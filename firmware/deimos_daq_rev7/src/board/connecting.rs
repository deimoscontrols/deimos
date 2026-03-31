use super::*;

use core::sync::atomic::{AtomicBool, Ordering};

use irq::{handler, scope};

use deimos_shared::peripherals::deimos_daq_rev7::operating_roundtrip::OperatingRoundtripInput;

impl<'a> Board<'a> {
    /// Ensure the board has a usable IPv4 address before entering `Binding`.
    pub fn connect(&mut self) -> BoardState {
        // Initialize
        self.set_outputs(&OperatingRoundtripInput::default());
        self.dt_ns = 1_000_000;
        self.reset_comm_pending();
        self.configure_comm_schedule(self.dt_ns);
        self.watchdog.feed();

        // Unbind if previously bound
        self.controller = None;
        self.net.reset_udp_socket();

        // Set status LEDs
        self.led0.set_low();
        self.led1.set_low();
        self.led2.set_low();
        self.led3.set_low();

        // Poll once so an already-available DHCP lease can win immediately; otherwise
        // optimistically claim the next fallback candidate without waiting.
        self.net.poll(self.time_ns);
        match self.net.poll_ip_config(self.time_ns, true) {
            IpConfigStatus::Ready | IpConfigStatus::DhcpApplied => return BoardState::Binding,
            IpConfigStatus::Missing | IpConfigStatus::DhcpDeferred => {
                if self.net.try_claim_fallback(self.time_ns, MAC_ADDRESS) {
                    self.watchdog.feed();
                    return BoardState::Binding;
                }
            }
        }

        let transition_binding = AtomicBool::new(false);

        handler!(
            pendsv_handler = || {
                let _comm_cycle = self.begin_comm_cycle();
                self.net.poll(self.time_ns);

                match self.net.poll_ip_config(self.time_ns, true) {
                    IpConfigStatus::Ready | IpConfigStatus::DhcpApplied => {
                        transition_binding.store(true, Ordering::Relaxed);
                    }
                    IpConfigStatus::Missing | IpConfigStatus::DhcpDeferred => {
                        if self.net.try_claim_fallback(self.time_ns, MAC_ADDRESS) {
                            transition_binding.store(true, Ordering::Relaxed);
                        }
                    }
                }

                self.watchdog.feed();
            }
        );

        scope(|s| {
            s.register(interrupts::PendSV, pendsv_handler);

            loop {
                if transition_binding.load(Ordering::Relaxed) {
                    break;
                }

                cortex_m::asm::wfi();
            }
        });

        return BoardState::Binding;
    }
}
