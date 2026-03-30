use super::*;

use core::sync::atomic::{AtomicBool, Ordering};

use smoltcp::socket::udp;
use smoltcp::wire::IpListenEndpoint;

use irq::{handler, scope};

use deimos_shared::{
    peripherals::deimos_daq_rev6::operating_roundtrip::OperatingRoundtripInput, PERIPHERAL_RX_PORT,
};

impl<'a> Board<'a> {
    /// Acquire an IP address
    pub fn connect(&mut self) -> BoardState {
        // Initialize
        self.set_outputs(&OperatingRoundtripInput::default());
        self.dt_ns = 1_000_000;
        self.systick_init();
        self.watchdog.feed();

        // Unbind if previously bound
        self.controller = None;
        let udpsocket = self.net.sockets.get_mut::<udp::Socket>(self.net.udp_handle);
        udpsocket.close();
        udpsocket
            .bind(IpListenEndpoint {
                addr: None,
                port: PERIPHERAL_RX_PORT,
            })
            .unwrap();

        // Set status LEDs
        self.led0.set_low();
        self.led1.set_low();
        self.led2.set_low();
        self.led3.set_low();

        // Transition flags
        let transition_binding = AtomicBool::new(false);
        let fallback_deadline = self.time_ns + DHCP_FALLBACK_TIMEOUT_NS;

        handler!(
            systick_handler = || {
                self.time_ns += self.dt_ns as i64;
                self.net.poll(self.time_ns);
                let ip_config = self.poll_ip_config(true);
                match ip_config {
                    IpConfigStatus::Ready | IpConfigStatus::DhcpApplied => {
                        transition_binding.store(true, Ordering::Relaxed);
                    }
                    IpConfigStatus::Missing | IpConfigStatus::DhcpDeferred => {
                        if self.time_ns >= fallback_deadline {
                            self.apply_static_fallback();
                            transition_binding.store(true, Ordering::Relaxed);
                        }
                    }
                }
                self.watchdog.feed();
            }
        );

        // Create a scope and register the systick interrupt handler.
        scope(|s| {
            // Run
            s.register(interrupts::SysTick, systick_handler);

            // Transition when indicated by inner loop
            'wait_for_transition: loop {
                if transition_binding.load(Ordering::Relaxed) {
                    break 'wait_for_transition;
                }

                cortex_m::asm::wfi(); // Wait for interrupt
            }
        });

        return BoardState::Binding;
    }
}
