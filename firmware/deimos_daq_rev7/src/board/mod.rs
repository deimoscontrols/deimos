use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use core::time::Duration;

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
mod operating;
mod startup;

// Peripherals with their own state and logic
pub mod subsystems;
pub use subsystems::interrupts;
use subsystems::net::*;
use subsystems::output::*;
use subsystems::sampling::*;

use deimos_shared::peripherals::deimos_daq_rev7::operating_roundtrip::OperatingRoundtripInput;
use deimos_shared::peripherals::deimos_daq_rev7::{
    adc_samples_per_report, expected_adc_sample_rate_hz, MAX_ADC_SAMPLE_RATE_HZ,
};
/// Model number
pub const MODEL_NUMBER: u64 =
    deimos_shared::peripherals::model_numbers::DEIMOS_DAQ_REV_7_MODEL_NUMBER;

/// ADC voltage reference
pub const VREF: f32 = 2.5;

/// Locally administered MAC address
pub const MAC_ADDRESS: [u8; 6] = *include_bytes!("../../static/macaddr.in");

/// Unique serial number
pub const SERIAL_NUMBER: u64 = u64::from_le_bytes(*include_bytes!("../../static/serialnumber.in"));

/// Ethernet descriptor rings are a global singleton
#[link_section = ".sram3.eth"]
static mut DES_RING: ethernet::DesRing<4, 4> = ethernet::DesRing::new();

/// Storage for the latest ADC samples
pub static ADC_SAMPLES: [AtomicF32; 18] = array_macro::array![_ => AtomicF32::new(0.0); 18];

/// Storage for latest unrolled counter samples
/// These are only integer-unwrapped, not filtered
pub static COUNTER_SAMPLES: [AtomicI32; 2] = array_macro::array![_ => AtomicI32::new(0); 2];

/// Storage for number of times (and direction) that the I32 counter has wrapped
pub static COUNTER_WRAPS: [AtomicI32; 2] = array_macro::array![_ => AtomicI32::new(0); 2];

/// Storage for the latest frequency samples
/// These see the same filter as ADC samples
pub static FREQ_SAMPLES: [AtomicF32; 2] = array_macro::array![_ => AtomicF32::new(0.0); 2];

/// ADC filter cutoff ratio
/// Ideally, this would be an AtomicF64, but the STM32H7 doesn't have 64-bit atomics
/// and the loss of resolution due to casting to/from 64-bit is not too bad here
pub static ADC_CUTOFF_RATIO: AtomicF32 = AtomicF32::new(0.1);

/// Flag for comm loop to indicate to sampling loop
/// that a new ADC filter cutoff should be incorporated
pub static NEW_ADC_CUTOFF: AtomicBool = AtomicBool::new(false);

/// Accumulated time spent sampling and filtering since last comm cycle
pub static ACCUMULATED_SAMPLING_TIME_NS: AtomicU32 = AtomicU32::new(0);

/// Number of ADC sample ticks to wait between comm releases.
pub static COMM_INTERVAL_SAMPLES: AtomicU32 = AtomicU32::new(30);

/// Nominal communication cycle interval configured by the controller.
pub static COMM_INTERVAL_NS: AtomicU32 = AtomicU32::new(1_000_000);

/// Number of comm releases emitted by the sample scheduler.
pub static COMM_RELEASE_COUNT: AtomicU32 = AtomicU32::new(0);

/// Number of comm releases that occurred while a previous comm handler was still active.
pub static COMM_OVERRUN_COUNT: AtomicU32 = AtomicU32::new(0);

/// Whether a PendSV comm handler is currently active.
pub static COMM_RUNNING: AtomicBool = AtomicBool::new(false);

/// Requested nominal ADC sample rate for filter and timing updates.
pub static TARGET_ADC_SAMPLE_RATE_HZ: AtomicU32 = AtomicU32::new(MAX_ADC_SAMPLE_RATE_HZ);

/// Flag for the sample loop to rebuild any sample-rate-dependent state.
pub static NEW_ADC_SAMPLE_RATE: AtomicBool = AtomicBool::new(false);

/// Latest requested persistent timing adjustment from the controller.
pub static REQUESTED_PERIOD_DELTA_NS: AtomicI32 = AtomicI32::new(0);

/// Latest requested one-shot timing adjustment from the controller.
pub static REQUESTED_PHASE_DELTA_NS: AtomicI32 = AtomicI32::new(0);

/// Indicates that a fresh timing request has been published for the sample ISR.
pub static TIMING_REQUEST_UPDATED: AtomicBool = AtomicBool::new(false);

#[derive(PartialEq, Eq)]
pub enum BoardState {
    Connecting,
    Binding,
    Configuring,
    Operating,
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
    pub last_comm_release_count: u32,
    pub clocks: CoreClocks,
    pub subcycle_timer: Timer<TIM5>,
    pub subcycle_rate_hz: u32,
    pub watchdog: IndependentWatchdog,

    // Ethernet
    pub net: Net<'a>,
    pub controller: Option<UdpMetadata>,
    pub configuring_timeout_ms: u16,
    pub loss_of_contact_limit: u16,

    // I/O
    pub outputs: Outputs,
}

/// Clears the `COMM_RUNNING` latch when a deferred comm handler returns.
struct CommCycleGuard;

impl Drop for CommCycleGuard {
    fn drop(&mut self) {
        COMM_RUNNING.store(false, Ordering::Relaxed);
    }
}

impl<'a> Board<'a> {
    pub fn run(&mut self) -> ! {
        self.state = BoardState::Connecting;
        loop {
            self.state = match self.state {
                BoardState::Connecting => self.connect(),
                BoardState::Binding => self.bind(),
                BoardState::Configuring => self.configure(),
                BoardState::Operating => self.operate(),
            }
        }
    }

    fn board_time(&self, subcycle_res_ns: u32) -> i64 {
        self.time_ns + (self.subcycle_timer.counter() * subcycle_res_ns) as i64
    }

    /// Reset the shared comm scheduler to a new nominal reporting cadence.
    fn configure_comm_schedule(&mut self, dt_ns: u32) {
        let samples_per_cycle = adc_samples_per_report(dt_ns);
        let nominal_sample_rate_hz = expected_adc_sample_rate_hz(dt_ns);
        let mut last_release_count = 0;

        self.subcycle_timer
            .set_timeout(Duration::from_nanos(2 * u64::from(dt_ns)));

        cortex_m::interrupt::free(|_| {
            last_release_count = COMM_RELEASE_COUNT.load(Ordering::Relaxed);
            COMM_INTERVAL_SAMPLES.store(samples_per_cycle, Ordering::Relaxed);
            COMM_INTERVAL_NS.store(dt_ns, Ordering::Relaxed);
            TARGET_ADC_SAMPLE_RATE_HZ.store(nominal_sample_rate_hz, Ordering::Relaxed);
            REQUESTED_PERIOD_DELTA_NS.store(0, Ordering::Relaxed);
            REQUESTED_PHASE_DELTA_NS.store(0, Ordering::Relaxed);
            TIMING_REQUEST_UPDATED.store(false, Ordering::Relaxed);
            NEW_ADC_SAMPLE_RATE.store(true, Ordering::Relaxed);
        });
        self.last_comm_release_count = last_release_count;
    }

    /// Clear any deferred comm bookkeeping before a new state registers its PendSV handler.
    fn reset_comm_pending(&self) {
        cortex_m::interrupt::free(|_| {
            COMM_OVERRUN_COUNT.store(0, Ordering::Relaxed);
            COMM_RUNNING.store(false, Ordering::Relaxed);
            TIMING_REQUEST_UPDATED.store(false, Ordering::Relaxed);
            cortex_m::peripheral::SCB::clear_pendsv();
        });
    }

    /// Mark the start of one deferred comm cycle and update the board timebase.
    fn begin_comm_cycle(&mut self) -> CommCycleGuard {
        COMM_RUNNING.store(true, Ordering::Relaxed);

        self.subcycle_timer.apply_freq();
        self.subcycle_timer.resume();

        let release_count = COMM_RELEASE_COUNT.load(Ordering::Relaxed);
        let releases = release_count
            .wrapping_sub(self.last_comm_release_count)
            .max(1);
        self.last_comm_release_count = release_count;
        self.time_ns += i64::from(releases) * i64::from(self.dt_ns);

        CommCycleGuard
    }

    // Set GPIO high/low or PWM duty cycle.
    fn set_outputs(&mut self, input: &OperatingRoundtripInput) {
        set_outputs(
            &mut self.outputs,
            &input.pwm_duty_frac,
            &input.pwm_freq_hz,
            &input.dac_v,
            input.gpio,
            &self.clocks,
        );
    }

    fn read_gpio_inputs(&self) -> u8 {
        (self.di0.is_high() as u8) | ((self.di1.is_high() as u8) << 1)
    }
}
