use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32};
use cortex_m::peripheral::syst::SystClkSource;

use stm32h7xx_hal::{
    ethernet,
    gpio::{Output, Pin},
    independent_watchdog::IndependentWatchdog,
    prelude::*,
    rcc::CoreClocks,
    stm32,
    stm32::*,
    timer::Timer,
};

use smoltcp::{
    iface::Interface,
    socket::{dhcpv4, udp::UdpMetadata},
    wire::{IpCidr, Ipv4Address, Ipv4Cidr},
};

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

use deimos_shared::peripherals::deimos_daq_rev6::operating_roundtrip::OperatingRoundtripInput;

/// Model number
pub const MODEL_NUMBER: u64 =
    deimos_shared::peripherals::model_numbers::DEIMOS_DAQ_REV_6_MODEL_NUMBER;

/// ADC sample frequency
pub const ADC_SAMPLE_FREQ_HZ: u32 = 33_000;

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

#[derive(PartialEq, Eq)]
pub enum BoardState {
    Connecting,
    Binding,
    Configuring,
    Operating,
}

/// Result of evaluating the board's current IPv4 configuration state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IpConfigStatus {
    /// No usable IPv4 address is currently configured.
    Missing,
    /// The current IPv4 configuration remains usable and unchanged.
    Ready,
    /// A DHCP configuration was just applied and callers should treat the address as changed.
    DhcpApplied,
    /// A DHCP configuration was observed but intentionally deferred until reconnect.
    DhcpDeferred,
}

pub struct Board<'a> {
    state: BoardState,

    // Misc
    pub led0: Pin<'E', 5, Output>,
    pub led1: Pin<'E', 4, Output>,
    pub led2: Pin<'E', 3, Output>,
    pub led3: Pin<'E', 2, Output>,

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
                BoardState::Operating => self.operate(),
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
        self.systick.enable_counter();
        self.systick.enable_interrupt();
    }

    // Set GPIO high/low or PWM duty cycle.
    fn set_outputs(&mut self, input: &OperatingRoundtripInput) {
        set_outputs(
            &mut self.outputs,
            &input.pwm_duty_frac,
            &input.pwm_freq_hz,
            &input.dac_v,
            &self.clocks,
        );
    }

    /// Remove any configured IPv4 address and route state from the interface.
    fn clear_ipv4_config(&mut self) {
        clear_ipv4_addr(&mut self.net.iface);
        self.net.iface.routes_mut().remove_default_ipv4_route();
        self.net.ip_assignment = IpAssignment::Unconfigured;
    }

    /// Install the deterministic static fallback address derived from the board MAC.
    fn apply_static_fallback(&mut self) -> Ipv4Cidr {
        let cidr = static_fallback_cidr(MAC_ADDRESS);
        set_ipv4_addr(&mut self.net.iface, cidr);
        self.net.iface.routes_mut().remove_default_ipv4_route();
        self.net.ip_assignment = IpAssignment::StaticFallback(cidr);
        self.led0.set_high();
        cidr
    }

    /// Apply a DHCP-provided address and optional default route immediately.
    fn apply_dhcp_config(&mut self, config: PendingDhcpConfig) {
        set_ipv4_addr(&mut self.net.iface, config.address);
        if let Some(router) = config.router {
            self.net
                .iface
                .routes_mut()
                .add_default_ipv4_route(router)
                .unwrap();
        } else {
            self.net.iface.routes_mut().remove_default_ipv4_route();
        }
        self.net.ip_assignment = IpAssignment::Dhcp(config.address);
        self.net.pending_dhcp = None;
        self.led0.set_high();
    }

    /// Poll DHCP and reconcile the board's active IPv4 configuration.
    ///
    /// When `allow_dhcp_swap` is `false`, any newly acquired DHCP configuration is
    /// remembered in `pending_dhcp` instead of being applied immediately. This is used
    /// during `Operating` to avoid changing the board address mid-run.
    fn poll_ip_config(&mut self, allow_dhcp_swap: bool) -> IpConfigStatus {
        if allow_dhcp_swap {
            if let Some(config) = self.net.pending_dhcp.take() {
                self.apply_dhcp_config(config);
                return IpConfigStatus::DhcpApplied;
            }
        }

        let event = self
            .net
            .sockets
            .get_mut::<dhcpv4::Socket>(self.net.dhcp_handle)
            .poll();

        match event {
            Some(dhcpv4::Event::Configured(config)) => {
                let config = PendingDhcpConfig {
                    address: config.address,
                    router: config.router,
                };
                match self.net.ip_assignment {
                    IpAssignment::StaticFallback(_) if !allow_dhcp_swap => {
                        self.net.pending_dhcp = Some(config);
                        self.led0.set_high();
                        IpConfigStatus::DhcpDeferred
                    }
                    IpAssignment::Dhcp(_) => {
                        self.apply_dhcp_config(config);
                        IpConfigStatus::Ready
                    }
                    _ => {
                        self.apply_dhcp_config(config);
                        IpConfigStatus::DhcpApplied
                    }
                }
            }
            Some(dhcpv4::Event::Deconfigured) => {
                self.net.pending_dhcp = None;
                match self.net.ip_assignment {
                    IpAssignment::Dhcp(_) => {
                        self.clear_ipv4_config();
                        self.led0.set_low();
                        IpConfigStatus::Missing
                    }
                    IpAssignment::StaticFallback(_) => {
                        self.led0.set_high();
                        IpConfigStatus::Ready
                    }
                    IpAssignment::Unconfigured => {
                        self.clear_ipv4_config();
                        self.led0.set_low();
                        IpConfigStatus::Missing
                    }
                }
            }
            None => match self.net.ip_assignment {
                IpAssignment::Unconfigured => match self.net.iface.ipv4_addr() {
                    Some(x) if x != Ipv4Address::UNSPECIFIED => {
                        self.led0.set_high();
                        IpConfigStatus::Ready
                    }
                    _ => {
                        self.led0.set_low();
                        IpConfigStatus::Missing
                    }
                },
                IpAssignment::StaticFallback(cidr) => {
                    if self.net.iface.ipv4_addr() != Some(cidr.address()) {
                        set_ipv4_addr(&mut self.net.iface, cidr);
                    }
                    self.led0.set_high();
                    IpConfigStatus::Ready
                }
                IpAssignment::Dhcp(cidr) => {
                    if self.net.iface.ipv4_addr() != Some(cidr.address()) {
                        set_ipv4_addr(&mut self.net.iface, cidr);
                    }
                    self.led0.set_high();
                    IpConfigStatus::Ready
                }
            },
        }
    }
}

/// Mutate the first IP address to match the one supplied
/// TODO: eliminate unwrap
fn set_ipv4_addr(iface: &mut Interface, cidr: Ipv4Cidr) {
    iface.update_ip_addrs(|addrs| {
        addrs.clear();
        addrs.push(IpCidr::Ipv4(cidr)).unwrap();
    });
}

/// Remove all IPv4 addresses from the interface.
fn clear_ipv4_addr(iface: &mut Interface) {
    iface.update_ip_addrs(|addrs| addrs.clear());
}
