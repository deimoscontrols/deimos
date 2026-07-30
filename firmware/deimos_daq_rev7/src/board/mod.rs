use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ptr::{addr_of, addr_of_mut},
    sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering, compiler_fence},
};
use cortex_m::peripheral::{SCB, SYST, syst::SystClkSource};

use stm32h7xx_hal::{
    ethernet,
    gpio::{Input, Output, Pin},
    independent_watchdog::IndependentWatchdog,
    prelude::*,
    rcc::CoreClocks,
    stm32,
    stm32::*,
    timer::Timer,
};

use smoltcp::socket::udp::UdpMetadata;

use atomic_float::AtomicF32;

// State machine
mod binding;
mod configuring;
mod connecting;
mod modbus;
mod operating;
mod startup;

// Peripherals with their own state and logic
pub mod subsystems;
use modbus::ModbusTcpServer;
pub use subsystems::interrupts;
use subsystems::net::*;
use subsystems::output::*;
use subsystems::sampling::*;

pub use deimos_shared::peripherals::deimos_daq_rev7::{
    ADC_CHANNEL_COUNT, ADC_SAMPLE_FREQ_HZ, COUNTER_CHANNEL_COUNT, FREQUENCY_CHANNEL_COUNT,
    MODEL_NUMBER, VREF,
};
use deimos_shared::peripherals::deimos_daq_rev7::{
    Rev7Calibration,
    acquisition::{AcquisitionClock, MAX_CAPTURE_ATTEMPTS},
    operating_roundtrip::{ModbusInitialConfig, OperatingOutputSettings},
};

/// Locally administered MAC address
pub const MAC_ADDRESS: [u8; 6] = *include_bytes!("../../static/macaddr.in");

/// Unique serial number
pub const SERIAL_NUMBER: u64 = u64::from_le_bytes(*include_bytes!("../../static/serialnumber.in"));

/// Ethernet descriptor rings are a global singleton
#[unsafe(link_section = ".sram3.eth")]
static mut DES_RING: MaybeUninit<ethernet::DesRing<4, 4>> = MaybeUninit::uninit();

/// One ordinary ADC group copied out of the interrupt handoff by value.
#[derive(Clone, Copy, Debug)]
pub struct AdcSampleGroup {
    /// Filtered ADC output voltages in `V` with shape `(ADC_CHANNEL_COUNT,)`.
    pub values: [f32; ADC_CHANNEL_COUNT],
    /// Board time immediately before the first ADC conversion group, in `ns`.
    pub sample_time_ns: i64,
}

/// One atomically addressed slot in the ADC publication double buffer.
struct AtomicAdcSampleGroup {
    values: [AtomicF32; ADC_CHANNEL_COUNT],
    sample_time_lo: AtomicU32,
    sample_time_hi: AtomicU32,
}

impl AtomicAdcSampleGroup {
    const fn new() -> Self {
        Self {
            values: array_macro::array![_ => AtomicF32::new(0.0); ADC_CHANNEL_COUNT],
            sample_time_lo: AtomicU32::new(0),
            sample_time_hi: AtomicU32::new(0),
        }
    }
}

/// First of two ADC publication buffers.
///
/// This no-wait contract depends on the current single-core, single-writer,
/// higher-priority-reader interrupt topology. Reuse on a multicore target, with
/// a writer that can preempt the reader, or with another reader requires a
/// triple buffer or a stronger ownership protocol.
static ADC_SAMPLE_BUFFER_0: AtomicAdcSampleGroup = AtomicAdcSampleGroup::new();
/// Second ADC publication buffer.
static ADC_SAMPLE_BUFFER_1: AtomicAdcSampleGroup = AtomicAdcSampleGroup::new();
/// Selector written only by TIM2 after it has completed the inactive buffer.
static ADC_LATEST_BUFFER_IS_1: AtomicBool = AtomicBool::new(false);

/// Publish one complete sample group without exposing a partially written group.
///
/// TIM2 writes only the buffer not named by the selector, then publishes that
/// buffer with one selector store. The higher-priority communication interrupt
/// can therefore preempt this function without observing a partially written
/// group.
///
/// Args:
///   values: Filtered ADC output voltages in `V` with shape
///     `(ADC_CHANNEL_COUNT,)` and channel order `ain0..ain12, ain15..ain19`.
///   sample_time_ns: Board time immediately before the first ADC conversion
///     group, in `ns`.
#[inline]
pub fn publish_adc_samples(values: &[f32; ADC_CHANNEL_COUNT], sample_time_ns: i64) {
    let latest_is_1 = ADC_LATEST_BUFFER_IS_1.load(Ordering::Relaxed);
    let destination = if latest_is_1 {
        &ADC_SAMPLE_BUFFER_0
    } else {
        &ADC_SAMPLE_BUFFER_1
    };
    for (slot, value) in destination.values.iter().zip(values.iter()) {
        slot.store(*value, Ordering::Relaxed);
    }
    let sample_time_bits = sample_time_ns as u64;
    destination
        .sample_time_lo
        .store(sample_time_bits as u32, Ordering::Relaxed);
    destination
        .sample_time_hi
        .store((sample_time_bits >> 32) as u32, Ordering::Relaxed);
    // On this single-core target the compiler fences prevent reordering; no
    // strongly ordered atomic or hardware memory barrier is required.
    compiler_fence(Ordering::Release);
    ADC_LATEST_BUFFER_IS_1.store(!latest_is_1, Ordering::Relaxed);
}

/// Reads exactly one published ADC group by loading the selector once.
///
/// Returns:
///   Coherent filtered ADC output voltages and their acquisition timestamp.
#[inline]
pub fn latest_adc_sample_group() -> AdcSampleGroup {
    let latest_is_1 = ADC_LATEST_BUFFER_IS_1.load(Ordering::Relaxed);
    compiler_fence(Ordering::Acquire);
    let source = if latest_is_1 {
        &ADC_SAMPLE_BUFFER_1
    } else {
        &ADC_SAMPLE_BUFFER_0
    };
    let values = core::array::from_fn(|index| source.values[index].load(Ordering::Relaxed));
    let sample_time_bits = u64::from(source.sample_time_lo.load(Ordering::Relaxed))
        | (u64::from(source.sample_time_hi.load(Ordering::Relaxed)) << 32);
    AdcSampleGroup {
        values,
        sample_time_ns: sample_time_bits as i64,
    }
}

/// Clock state shared only between the higher-priority SysTick writer and the
/// TIM2 reader while TIM2 has interrupts masked.
struct AcquisitionTiming {
    clock: AcquisitionClock,
    last_sample_time_ns: i64,
    nominal_sample_period_ns: i64,
    systick_tick_period_ns: u32,
}

/// Interior-mutable storage for the topology-specific acquisition clock.
struct AcquisitionTimingCell(UnsafeCell<AcquisitionTiming>);

// SAFETY: This cell is not a general concurrent container. SysTick is the only
// clock writer and has higher priority than TIM2. TIM2 copies the clock only
// with interrupts masked; the state-machine context resets it only with all
// interrupts masked. Moving this firmware to multiple cores or allowing TIM2
// to preempt SysTick requires replacing this ownership protocol.
unsafe impl Sync for AcquisitionTimingCell {}

const DEFAULT_ADC_SAMPLE_PERIOD_NS: i64 =
    ((1_000_000_000_u64 + ADC_SAMPLE_FREQ_HZ as u64 / 2) / ADC_SAMPLE_FREQ_HZ as u64) as i64;

static ACQUISITION_TIMING: AcquisitionTimingCell =
    AcquisitionTimingCell(UnsafeCell::new(AcquisitionTiming {
        clock: AcquisitionClock::new(0, 0),
        last_sample_time_ns: -DEFAULT_ADC_SAMPLE_PERIOD_NS,
        nominal_sample_period_ns: DEFAULT_ADC_SAMPLE_PERIOD_NS,
        // Startup asserts a 400 MHz core clock; SysTick External is core / 8.
        systick_tick_period_ns: 20,
    }));

/// Record the rounded period produced by the configured TIM2 sample timer.
///
/// This is called once during startup, before TIM2 is unmasked, and supplies the
/// bounded capture fallback with the applied timer period rather than an
/// independently rounded nominal frequency.
///
/// Args:
///   sample_period_ns: Applied TIM2 period in `ns/sample`.
pub fn configure_adc_sample_period_ns(sample_period_ns: i64) {
    cortex_m::interrupt::free(|_| {
        compiler_fence(Ordering::SeqCst);
        let timing = ACQUISITION_TIMING.0.get();
        unsafe {
            addr_of_mut!((*timing).nominal_sample_period_ns).write(sample_period_ns);
        }
        compiler_fence(Ordering::SeqCst);
    });
}

/// Capture one ADC acquisition timestamp with at most two rollover retries.
///
/// Each attempt masks interrupts only while checking the SysTick pending bit,
/// copying the two-word acquisition clock, and reading the down-counter. If a
/// wrap was already pending or became pending during that copy, restoring
/// interrupts lets the higher-priority SysTick handler advance the clock before
/// the next and final attempt. Two rejected attempts use the monotonic nominal-
/// sample-period fallback and do not alter downstream packet structure.
///
/// Returns:
///   Board time immediately before the first ADC conversion group, in `ns`.
#[inline]
pub fn capture_adc_sample_time_ns() -> i64 {
    for _ in 0..MAX_CAPTURE_ATTEMPTS {
        let capture = cortex_m::interrupt::free(|_| {
            compiler_fence(Ordering::SeqCst);
            if SCB::is_pendst_pending() {
                return None;
            }

            let timing = ACQUISITION_TIMING.0.get();
            let clock = unsafe { addr_of!((*timing).clock).read() };
            let current_count = SYST::get_current();
            let pending_after_copy = SCB::is_pendst_pending();
            compiler_fence(Ordering::SeqCst);

            if pending_after_copy {
                None
            } else {
                Some((clock, current_count))
            }
        });

        if let Some((clock, current_count)) = capture {
            let timing = ACQUISITION_TIMING.0.get();
            let tick_period_ns = unsafe { addr_of!((*timing).systick_tick_period_ns).read() };
            let sample_time_ns = clock.timestamp_ns(current_count, tick_period_ns);
            unsafe {
                addr_of_mut!((*timing).last_sample_time_ns).write(sample_time_ns);
            }
            return sample_time_ns;
        }
    }

    let timing = ACQUISITION_TIMING.0.get();
    let last_sample_time_ns = unsafe { addr_of!((*timing).last_sample_time_ns).read() };
    let nominal_sample_period_ns = unsafe { addr_of!((*timing).nominal_sample_period_ns).read() };
    let sample_time_ns = last_sample_time_ns + nominal_sample_period_ns;
    unsafe {
        addr_of_mut!((*timing).last_sample_time_ns).write(sample_time_ns);
    }
    sample_time_ns
}

/// Storage for latest unrolled counter samples
/// These are only integer-unwrapped, not filtered
pub static COUNTER_SAMPLES: [AtomicI32; COUNTER_CHANNEL_COUNT] =
    array_macro::array![_ => AtomicI32::new(0); COUNTER_CHANNEL_COUNT];

/// Storage for number of times (and direction) that the I32 counter has wrapped
pub static COUNTER_WRAPS: [AtomicI32; COUNTER_CHANNEL_COUNT] =
    array_macro::array![_ => AtomicI32::new(0); COUNTER_CHANNEL_COUNT];

/// Storage for the latest frequency samples
/// These see the same filter as ADC samples
pub static FREQ_SAMPLES: [AtomicF32; FREQUENCY_CHANNEL_COUNT] =
    array_macro::array![_ => AtomicF32::new(0.0); FREQUENCY_CHANNEL_COUNT];

/// ADC filter cutoff ratio
/// Ideally, this would be an AtomicF64, but the STM32H7 doesn't have 64-bit atomics
/// and the loss of resolution due to casting to/from 64-bit is not too bad here
pub static ADC_CUTOFF_RATIO: AtomicF32 = AtomicF32::new(0.1);

/// Flag for comm loop to indicate to sampling loop
/// that a new ADC filter cutoff should be incorporated
pub static NEW_ADC_CUTOFF: AtomicBool = AtomicBool::new(false);

/// Accumulated time spent sampling and filtering since last comm cycle
pub static ACCUMULATED_SAMPLING_TIME_NS: AtomicU32 = AtomicU32::new(0);

/// Private per-invocation selector for the common operating implementation.
#[derive(Clone, Copy, Debug, PartialEq)]
enum OperatingMode {
    Deimos,
    Modbus(ModbusInitialConfig),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BoardState {
    Connecting,
    Binding,
    Configuring,
    OperatingDeimos,
    OperatingModbus(ModbusInitialConfig),
}

pub struct Board<'a> {
    state: BoardState,

    // Misc
    pub led0: Pin<'E', 5, Output>,
    pub led1: Pin<'E', 4, Output>,
    pub led2: Pin<'E', 3, Output>,
    pub led3: Pin<'E', 2, Output>,
    pub di0: Pin<'D', 0, Input>,
    pub di1: Pin<'D', 1, Input>,

    // Time
    pub time_ns: i64,
    pub dt_ns: u32,
    pub systick: stm32::SYST,
    pub clocks: CoreClocks,
    pub subcycle_timer: Timer<TIM5>,
    pub subcycle_rate_hz: u32,
    pub watchdog: IndependentWatchdog,

    // Ethernet
    pub net: Net<'a>,
    pub controller: Option<UdpMetadata>,
    pub configuring_timeout_ms: u16,
    pub loss_of_contact_limit: u16,
    /// Fixed-storage Modbus/TCP framing and response state.
    modbus: ModbusTcpServer,

    // Embedded measurement calibration.
    pub calibration: Rev7Calibration,

    // I/O
    pub outputs: Outputs,
}

impl<'a> Board<'a> {
    pub fn run(&mut self) -> ! {
        self.state = BoardState::Connecting;
        loop {
            self.state = match self.state {
                BoardState::Connecting => self.connect(),
                BoardState::Binding => self.bind(),
                BoardState::Configuring => self.configure(),
                BoardState::OperatingDeimos => self.operate(OperatingMode::Deimos),
                BoardState::OperatingModbus(initial_config) => {
                    self.operate(OperatingMode::Modbus(initial_config))
                }
            }
        }
    }

    fn board_time(&self, subcycle_res_ns: u32) -> i64 {
        self.time_ns + (self.subcycle_timer.counter() * subcycle_res_ns) as i64
    }

    /// Adjust systick counter's reload toward target delta
    /// relative to nominal dt_ns, without restarting
    fn systick_adjust(&mut self, delta_ns: i64) {
        // Using "external" systick clock (sysclk on 8x divider)
        let c_ck_mhz = self.clocks.c_ck().to_MHz() / 8;
        let delta_ns_max = (self.dt_ns / 10) as i64;
        let delta_ns = delta_ns.max(-delta_ns_max).min(delta_ns_max);
        let dt_adjusted_ns = (self.dt_ns as i64 + delta_ns) as u64;
        let reload = dt_adjusted_ns
            .saturating_mul(c_ck_mhz as u64)
            .saturating_div(1000)
            .saturating_sub(1)
            .max(1);

        self.systick.set_reload(reload as u32);
    }

    /// Reset acquisition time to the start of the newly enabled SysTick interval.
    ///
    /// This also places the fallback one nominal TIM2 period before the anchor,
    /// so even two immediately ambiguous captures produce a defined monotonic
    /// timestamp.
    fn acquisition_clock_init(&self) {
        let systick_rate_hz = self.clocks.c_ck().raw() / 8;
        debug_assert_eq!(1_000_000_000 % systick_rate_hz, 0);
        let systick_tick_period_ns = 1_000_000_000 / systick_rate_hz;
        let active_reload = SYST::get_reload();

        cortex_m::interrupt::free(|_| {
            compiler_fence(Ordering::SeqCst);
            let timing = ACQUISITION_TIMING.0.get();
            let nominal_sample_period_ns =
                unsafe { addr_of!((*timing).nominal_sample_period_ns).read() };
            unsafe {
                addr_of_mut!((*timing).clock)
                    .write(AcquisitionClock::new(self.time_ns, active_reload));
                addr_of_mut!((*timing).last_sample_time_ns)
                    .write(self.time_ns - nominal_sample_period_ns);
                addr_of_mut!((*timing).systick_tick_period_ns).write(systick_tick_period_ns);
            }
            compiler_fence(Ordering::SeqCst);
        });
    }

    /// Advance the acquisition clock at the beginning of one SysTick handler.
    ///
    /// The reload register already contains the value loaded for the interval
    /// which has just started. A later call to [`Self::systick_adjust`] only
    /// programs the following interval and cannot change this saved value.
    #[inline]
    fn acquisition_clock_advance(&self) {
        let timing = ACQUISITION_TIMING.0.get();
        let mut clock = unsafe { addr_of!((*timing).clock).read() };
        let tick_period_ns = unsafe { addr_of!((*timing).systick_tick_period_ns).read() };
        clock.advance(SYST::get_reload(), tick_period_ns);
        unsafe {
            addr_of_mut!((*timing).clock).write(clock);
        }
        // Keep the complete clock publication ahead of the rest of the handler
        // without emitting a Cortex-M hardware memory barrier.
        compiler_fence(Ordering::Release);
    }

    /// Configure SYSTICK for `self.dt_ns` timebase
    fn systick_init(&mut self) {
        self.systick.disable_interrupt();
        self.systick.disable_counter();

        // "External" clock here means external to the cpu core,
        // but still part of the same chip. This is SYSCK on an 8x divider,
        // which allows us to sacrifice some resolution at high control frequencies
        // in order to be able to access lower frequencies.
        self.systick.set_clock_source(SystClkSource::External);

        self.systick_adjust(0); // Set reload value

        self.systick.clear_current();
        self.acquisition_clock_init();
        self.systick.enable_counter();
        self.systick.enable_interrupt();
    }

    // Set GPIO high/low or PWM duty cycle.
    fn set_outputs(&mut self, settings: &OperatingOutputSettings) {
        set_outputs(
            &mut self.outputs,
            &settings.pwm_duty_frac,
            &settings.pwm_freq_hz,
            &settings.dac_v,
            settings.gpio,
            &self.clocks,
        );
    }

    fn read_gpio_inputs(&self) -> u8 {
        (self.di0.is_high() as u8) | ((self.di1.is_high() as u8) << 1)
    }
}
