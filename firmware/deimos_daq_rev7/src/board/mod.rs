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
    timer::Timer,
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
    ADC_CHANNEL_COUNT, ADC_OVERSAMPLE_MIN_SAMPLES_PER_CYCLE, ADC_OVERSAMPLE_TARGET_HZ,
    MODEL_NUMBER, VREF,
};
use deimos_shared::peripherals::deimos_daq_rev7::{
    Rev7Calibration,
    acquisition::{AcquisitionClock, UniformIntervalScheduler},
    operating_roundtrip::{ModbusInitialConfig, OperatingOutputSettings},
};

/// Locally administered MAC address
pub const MAC_ADDRESS: [u8; 6] = *include_bytes!("../../static/macaddr.in");

/// Unique serial number
pub const SERIAL_NUMBER: u64 = u64::from_le_bytes(*include_bytes!("../../static/serialnumber.in"));

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

    fn board_time(&self, subcycle_res_ns: u32) -> i64 {
        self.time_ns + (self.subcycle_timer.counter() * subcycle_res_ns) as i64
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
        let c_ck_mhz = self.clocks.c_ck().to_MHz() / 8;
        let delta_ns_max = (self.dt_ns / 10) as i64;
        let bounded_delta_ns = delta_ns.clamp(-delta_ns_max, delta_ns_max);
        let adjusted_ns = (i64::from(self.dt_ns) + bounded_delta_ns) as u64;
        adjusted_ns
            .saturating_mul(u64::from(c_ck_mhz))
            .saturating_div(1000)
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
        let systick_rate_hz = self.clocks.c_ck().raw() / 8;
        debug_assert_eq!(1_000_000_000 % systick_rate_hz, 0);
        1_000_000_000 / systick_rate_hz
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
            &self.clocks,
        );
    }

    fn read_gpio_inputs(&self) -> u8 {
        (self.di0.is_high() as u8) | ((self.di1.is_high() as u8) << 1)
    }
}
