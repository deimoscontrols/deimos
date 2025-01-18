use super::*;

use core::sync::atomic::{AtomicBool, Ordering};

use smoltcp::socket::udp;
use smoltcp::wire::IpListenEndpoint;

use irq::{handler, scope};

use deimos_shared::{
    peripherals::analog_i_rev_3::operating_roundtrip::OperatingRoundtripInput, PERIPHERAL_RX_PORT,
};

impl<'a> Board<'a> {
    /// Acquire an IP address
    pub fn connect(&mut self) -> BoardState {
        // Initialize
        self.set_pwm(&OperatingRoundtripInput::default());
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

        handler!(
            systick_handler = || {
                self.time_ns += self.dt_ns as i64;
                self.net.poll(self.time_ns);
                let ip_address_ok = self.poll_dhcp();
                transition_binding.store(ip_address_ok, Ordering::Relaxed);
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
