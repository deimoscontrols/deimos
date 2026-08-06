use core::mem::MaybeUninit;
use cortex_m::peripheral::{SYST, syst::SystClkSource};

use stm32h7xx_hal::{
    ethernet,
    gpio::{Input, Output, Pin},
    independent_watchdog::IndependentWatchdog,
    prelude::*,
    rcc::CoreClocks,
    stm32,
    stm32::*,
};

use smoltcp::socket::udp::UdpMetadata;

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
    ADC_CHANNEL_COUNT, ADC_IIR_CUTOFF_TO_REPORT_RATE, ADC_OVERSAMPLE_TARGET_HZ, MODEL_NUMBER, VREF,
};
use deimos_shared::peripherals::deimos_daq_rev7::{
    calc::Calibration,
    packets::{ModbusInitialConfig, OperatingOutputSettings},
    timing::{AcquisitionClock, UniformIntervalScheduler, bounded_cycle_timing_correction_ns},
};

/// Locally administered MAC address
pub const MAC_ADDRESS: [u8; 6] = *include_bytes!("../../static/macaddr.in");

/// Unique serial number
pub const SERIAL_NUMBER: u64 = u64::from_le_bytes(*include_bytes!("../../static/serialnumber.in"));

/// Configured CPU core-clock rate in `cycle/s`.
const CORE_RATE_HZ: u32 = 400_000_000;
/// Divider from the CPU core clock to SysTick's external reference clock.
const SYSTICK_EXTERNAL_DIVIDER: u32 = 8;
/// Configured SysTick external-reference rate in `tick/s`.
const SYSTICK_RATE_HZ: u32 = CORE_RATE_HZ / SYSTICK_EXTERNAL_DIVIDER;
/// DWT nanoseconds-per-core-cycle scale in unsigned Q16 fixed point.
const DWT_NS_PER_CYCLE_Q16: u32 = ((1_000_000_000_u64 << 16) / CORE_RATE_HZ as u64) as u32;

// These clock relationships are part of the fixed firmware build. Checking
// them here keeps the invariants active even though embedded release builds do
// not execute `debug_assert!` calls.
const _: () = assert!(CORE_RATE_HZ > 0);
const _: () = assert!(CORE_RATE_HZ % SYSTICK_EXTERNAL_DIVIDER == 0);
const _: () = assert!(1_000_000_000 % SYSTICK_RATE_HZ == 0);
const _: () = assert!(DWT_NS_PER_CYCLE_Q16 > 0);

/// Ethernet descriptor rings are a global singleton
#[unsafe(link_section = ".sram3.eth")]
static mut DES_RING: MaybeUninit<ethernet::DesRing<4, 4>> = MaybeUninit::uninit();

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
    pub watchdog: IndependentWatchdog,

    // Ethernet
    pub net: Net<'a>,
    pub controller: Option<UdpMetadata>,
    pub configuring_timeout_ms: u16,
    pub loss_of_contact_limit: u16,
    /// Fixed-storage Modbus/TCP framing and response state.
    modbus: ModbusTcpServer,

    // Embedded measurement calibration.
    pub calibration: Calibration,

    // I/O
    pub outputs: Outputs,
}

impl<'a> Board<'a> {
    pub fn run(&mut self, sampler: &mut Sampler) -> ! {
        self.state = BoardState::Connecting;
        loop {
            self.state = match self.state {
                BoardState::Connecting => self.connect(),
                BoardState::Binding => self.bind(),
                BoardState::Configuring => self.configure(),
                BoardState::OperatingDeimos => self.operate(OperatingMode::Deimos, sampler),
                BoardState::OperatingModbus(initial_config) => {
                    self.operate(OperatingMode::Modbus(initial_config), sampler)
                }
            }
        }
    }

    /// Return the clock rate driving SysTick when its external source is selected.
    ///
    /// On STM32H7, [`SystClkSource::External`] selects the processor reference
    /// clock, which is the CPU core clock (`c_ck`) divided by eight. Keeping
    /// that platform-specific relationship here ensures reload and timestamp
    /// calculations use the clock which actually advances the SysTick counter.
    ///
    /// Returns:
    ///   SysTick counter rate in `tick/s`.
    ///
    /// References:
    ///   STMicroelectronics, *RM0433 STM32H742, STM32H743/753 and STM32H750
    ///   Value Line advanced Arm-based 32-bit MCUs*, RCC and SysTick clock
    ///   descriptions.
    #[inline(always)]
    fn systick_rate_hz(&self) -> u32 {
        self.clocks.c_ck().raw() / SYSTICK_EXTERNAL_DIVIDER
    }

    /// Adjust systick counter's reload toward target delta
    /// relative to nominal dt_ns, without restarting
    fn systick_adjust(&mut self, delta_ns: i64) {
        let reload = self
            .systick_interval_ticks(delta_ns)
            .saturating_sub(1)
            .max(1);
        self.systick.set_reload(reload);
    }

    /// Convert one bounded publishing-interval correction to SysTick ticks.
    fn systick_interval_ticks(&self, delta_ns: i64) -> u32 {
        // Keep this final clamp at the timer boundary even though each
        // transport already combines its requested correction through the
        // same helper. This prevents any future caller from bypassing the
        // +/-10% execution-margin policy.
        let bounded_delta_ns = bounded_cycle_timing_correction_ns(self.dt_ns, delta_ns, 0);
        let adjusted_ns = (i64::from(self.dt_ns) + bounded_delta_ns) as u64;
        adjusted_ns
            .saturating_mul(u64::from(self.systick_rate_hz()))
            .saturating_div(1_000_000_000)
            .clamp(1, u64::from(u32::MAX)) as u32
    }

    /// Build a constant-space schedule for one corrected publishing interval.
    fn sample_interval_scheduler(
        &self,
        delta_ns: i64,
        samples_per_cycle: u32,
    ) -> UniformIntervalScheduler {
        UniformIntervalScheduler::new(self.systick_interval_ticks(delta_ns), samples_per_cycle)
    }

    /// Create an acquisition clock for the newly enabled SysTick interval.
    fn acquisition_clock_init(&self) -> AcquisitionClock {
        AcquisitionClock::new(self.time_ns, SYST::get_reload())
    }

    /// Return the exact SysTick counter period in `ns/tick`.
    fn systick_tick_period_ns(&self) -> u32 {
        // Startup checks the live core clock against `CORE_RATE_HZ`, while the
        // build-time assertion above proves this division is exact.
        1_000_000_000 / self.systick_rate_hz()
    }

    /// Configure SYSTICK for `self.dt_ns` timebase
    fn systick_init(&mut self) {
        let reload = self.systick_interval_ticks(0).saturating_sub(1).max(1);
        self.systick_init_reload(reload);
    }

    /// Configure SysTick with an explicit first interval.
    fn systick_init_reload(&mut self, reload: u32) {
        self.systick.disable_interrupt();
        self.systick.disable_counter();

        // "External" clock here means external to the cpu core,
        // but still part of the same chip. This is SYSCK on an 8x divider,
        // which allows us to sacrifice some resolution at high control frequencies
        // in order to be able to access lower frequencies.
        self.systick.set_clock_source(SystClkSource::External);

        self.systick.set_reload(reload);

        self.systick.clear_current();
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
        );
    }

    fn read_gpio_inputs(&self) -> u8 {
        (self.di0.is_high() as u8) | ((self.di1.is_high() as u8) << 1)
    }
}
