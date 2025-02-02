use super::*;

use core::sync::atomic::{AtomicBool, Ordering};
use irq::{handler, scope};
use smoltcp::socket::udp;

use deimos_shared::{peripherals::analog_i_rev_4::OperatingRoundtripInput, states::configuring::*};

impl<'a> Board<'a> {
    pub fn configure(&mut self) -> BoardState {
        // Initialize
        self.set_pwm(&OperatingRoundtripInput::default());
        self.dt_ns = 1_000_000;
        self.systick_init();
        self.watchdog.feed();
        let end_of_configuring = self.time_ns + (self.configuring_timeout_ms as i64) * 1_000_000;

        // Set status LEDs
        self.led0.set_high();
        self.led1.set_high();
        self.led2.set_low();
        self.led3.set_low();

        // Transition flags
        let transition_connecting = AtomicBool::new(false);
        let transition_operating = AtomicBool::new(false);

        // UDP tx buffer
        let response_buf = &mut [0_u8; ConfiguringOutput::BYTE_LEN];
        let mut configured = false; // Whether we have received a good config packet
        let mut configured_time_ns = 0; // Time when we received a config packet
        let mut timeout_to_operating_ns = 0; // Time to wait after receiving config packet

        handler!(
            systick_handler = || {
                self.time_ns += self.dt_ns as i64;

                // Poll send/recv to process incoming packets
                self.net.poll(self.time_ns);

                // Maintain IP address configuration or go back to connecting
                let ip_address_ok = self.poll_dhcp();
                transition_connecting.fetch_or(!ip_address_ok, Ordering::Relaxed);

                // Make sure we have a controller bound or go back to connecting
                let controller_ok = self.controller.is_some();
                transition_connecting.fetch_or(!controller_ok, Ordering::Relaxed);

                if !configured {
                    // If we're past the scheduled timeout, go back to connecting
                    if self.time_ns >= end_of_configuring {
                        transition_connecting.store(true, Ordering::Relaxed);
                        self.watchdog.feed();
                        return;
                    }

                    // Check for configuration packets on UDP
                    let (recv_buf, _meta) = match self
                        .net
                        .sockets
                        .get_mut::<udp::Socket>(self.net.udp_handle)
                        .recv()
                    {
                        Ok((recv_buf, meta)) if Some(meta) == self.controller => (recv_buf, meta),
                        Err(_) => {
                            self.watchdog.feed();
                            return;
                        } // Exhausted buffer
                        _ => {
                            self.watchdog.feed();
                            return;
                        } // Source is not our controller, or we have no controller
                    };

                    // Parse received config
                    if recv_buf.len() == ConfiguringInput::BYTE_LEN {
                        // Mark the time
                        configured_time_ns = self.time_ns;

                        // TODO: Check inputs and NACK if necessary

                        // Parse and apply configuration
                        let config = ConfiguringInput::read_bytes(&recv_buf);
                        self.loss_of_contact_limit = config.loss_of_contact_limit;
                        self.dt_ns = config.dt_ns;
                        timeout_to_operating_ns = config.timeout_to_operating_ns;

                        // Set ADC filter cutoff
                        let reporting_rate = 1.0 / (self.dt_ns as f64 / 1e9); // Hz
                        let cutoff_ratio = reporting_rate / (ADC_SAMPLE_FREQ_HZ as f64); // Dimensionless
                        ADC_CUTOFF_RATIO.store(cutoff_ratio as f32, Ordering::Relaxed);
                        NEW_ADC_CUTOFF.store(true, Ordering::Relaxed); // Flag for ADC sample loop to update cutoff

                        // If we've made it this far, we're done configuring
                        self.led2.set_high();
                        configured = true;
                        self.systick_init(); // Set new systick freq _after_ fully configured
                    }
                }

                if configured {
                    if let Some(meta) = self.controller {
                        // Acknowledge configuration
                        let ack = ConfiguringOutput::default();
                        ack.write_bytes(&mut response_buf[..ConfiguringOutput::BYTE_LEN]);
                        match self
                            .net
                            .sockets
                            .get_mut::<udp::Socket>(self.net.udp_handle)
                            .send_slice(&response_buf[..ConfiguringOutput::BYTE_LEN], meta)
                        {
                            Ok(_) => {}
                            Err(_) => {
                                // If we are unable to send a UDP packet for any reason,
                                // go back to connecting and start over
                                transition_connecting.store(true, Ordering::Relaxed);
                                self.watchdog.feed();
                                return;
                            }
                        }

                        // Poll send/recv to push out response packet before we exit this state
                        self.net.poll(self.time_ns);
                    } else {
                        transition_connecting.store(true, Ordering::Relaxed);
                        self.watchdog.feed();
                        return;
                    }
                }

                // Time out into Operating state when ready
                if configured && self.time_ns - configured_time_ns >= timeout_to_operating_ns as i64
                {
                    transition_operating.store(true, Ordering::Relaxed);
                }

                self.watchdog.feed();
            }
        );

        // Create a scope and register the systick interrupt handler.
        scope(|s| {
            // Run
            s.register(interrupts::SysTick, systick_handler);

            // Transition when indicated by inner loop
            let mut transition;
            'wait_for_transition: loop {
                transition = transition_connecting.load(Ordering::Relaxed)
                    || transition_operating.load(Ordering::Relaxed);

                if transition {
                    break 'wait_for_transition;
                }

                cortex_m::asm::wfi(); // Wait for interrupt
            }
        });

        if transition_connecting.load(Ordering::Relaxed) {
            return BoardState::Connecting;
        }

        return BoardState::Operating;
    }
}
