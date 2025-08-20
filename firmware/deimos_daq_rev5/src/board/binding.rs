use super::*;

use core::sync::atomic::{AtomicBool, Ordering};

use irq::{handler, scope};

use smoltcp::socket::udp;

use deimos_shared::peripherals::analog_i_rev_4::operating_roundtrip::OperatingRoundtripInput;
use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::binding::*;

impl<'a> Board<'a> {
    /// Bind to a controller
    pub fn bind(&mut self) -> BoardState {
        // Initialize
        self.set_pwm(&OperatingRoundtripInput::default());
        self.dt_ns = 1_000_000;
        self.systick_init();
        self.watchdog.feed();

        // Unbind if previously bound
        self.controller = None;

        // Set status LEDs
        self.led0.set_high();
        self.led1.set_low();
        self.led2.set_low();
        self.led3.set_low();

        // Transition flags
        let transition_connecting = AtomicBool::new(false);
        let transition_configuring = AtomicBool::new(false);

        // UDP tx buffer
        let response_buf = &mut [0_u8; BindingOutput::BYTE_LEN];

        handler!(
            systick_handler = || {
                self.time_ns += self.dt_ns as i64;

                unsafe {
                    (&*IWDG::ptr()).kr.write(|w| w.bits(1));
                }

                // If we're already bound, just wait
                if transition_configuring.load(Ordering::Relaxed) {
                    self.watchdog.feed();
                    return;
                };

                // Process incoming and outgoing packets
                self.net.poll(self.time_ns);

                // Make sure we still have an IP address & renew it on time
                let ip_address_ok = self.poll_dhcp();
                if !ip_address_ok || transition_connecting.load(Ordering::Relaxed) {
                    transition_connecting.store(true, Ordering::Relaxed);
                    self.watchdog.feed();
                    return;
                }

                // Check for a controller trying to bind
                let (recv_buf, meta) = match self
                    .net
                    .sockets
                    .get_mut::<udp::Socket>(self.net.udp_handle)
                    .recv()
                {
                    Ok((recv_buf, meta)) => (recv_buf, meta),
                    Err(_) => {
                        self.watchdog.feed();
                        return;
                    }
                };

                if recv_buf.len() == BindingInput::BYTE_LEN {
                    // Store the controller's address
                    self.controller = Some(meta);
                    let binding_input = BindingInput::read_bytes(recv_buf);
                    self.configuring_timeout_ms = binding_input.configuring_timeout_ms;

                    // Respond to the controller
                    let binding_response = BindingOutput {
                        peripheral_id: PeripheralId {
                            model_number: MODEL_NUMBER,
                            serial_number: SERIAL_NUMBER,
                        },
                    };
                    binding_response.write_bytes(response_buf);
                    match self
                        .net
                        .sockets
                        .get_mut::<udp::Socket>(self.net.udp_handle)
                        .send_slice(response_buf, meta)
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
                    self.net.poll(self.time_ns);

                    // Set flag to continue to Configuring
                    transition_configuring.store(true, Ordering::Relaxed);
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
                    || transition_configuring.load(Ordering::Relaxed);

                if transition {
                    break 'wait_for_transition;
                }

                cortex_m::asm::wfi(); // Wait for interrupt
            }
        });

        if transition_configuring.load(Ordering::Relaxed) {
            return BoardState::Configuring;
        } else {
            return BoardState::Connecting;
        }
    }
}
