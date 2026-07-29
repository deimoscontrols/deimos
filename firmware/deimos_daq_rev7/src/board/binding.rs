use super::*;

use core::sync::atomic::{AtomicBool, Ordering};

use irq::{handler, scope};

use deimos_shared::peripherals::PeripheralId;
use deimos_shared::peripherals::deimos_daq_rev7::{
    Rev7BindingInput, Rev7BindingOutput, operating_roundtrip::OperatingOutputSettings,
};
use deimos_shared::states::{ByteStruct, ByteStructLen};

use super::modbus::{ModbusSocketBudget, ReceiveStatus};

impl<'a> Board<'a> {
    /// Bind to a controller
    pub fn bind(&mut self) -> BoardState {
        // Initialize
        self.set_outputs(&OperatingOutputSettings::default());
        self.dt_ns = 1_000_000;
        self.systick_init();
        self.watchdog.feed();

        // Unbind if previously bound
        self.controller = None;

        // Modbus is deliberately undiscoverable until a complete firmware
        // calibration has been installed. Connection setup reset the socket;
        // entering Binding is the only place which exposes TCP port 502.
        let modbus_enabled = self.calibration.is_calibrated();
        if modbus_enabled && self.net.tcp_listen().is_err() {
            return BoardState::Connecting;
        }

        // Set status LEDs
        self.led0.set_high();
        self.led1.set_low();
        self.led2.set_low();
        self.led3.set_low();

        // Transition flags
        let transition_connecting = AtomicBool::new(false);
        let transition_configuring = AtomicBool::new(false);
        let transition_modbus = AtomicBool::new(false);

        handler!(
            systick_handler = || {
                self.time_ns += self.dt_ns as i64;

                unsafe {
                    (&*IWDG::ptr()).kr.write(|w| w.bits(1));
                }

                // If we're already bound, just wait
                if transition_configuring.load(Ordering::Relaxed)
                    || transition_modbus.load(Ordering::Relaxed)
                {
                    self.watchdog.feed();
                    return;
                };

                // Process incoming and outgoing packets
                let mut net_budget = NetPollBudget::modbus_cycle();
                if modbus_enabled {
                    self.net.poll_bounded(self.time_ns, &mut net_budget);
                } else {
                    self.net.poll(self.time_ns);
                }

                // Keep setup traffic on one stable address; otherwise restart discovery.
                if self
                    .net
                    .step_address(self.time_ns, AddressMode::SessionSetup)
                    == AddressStatus::Missing
                {
                    transition_connecting.store(true, Ordering::Relaxed);
                    self.watchdog.feed();
                    return;
                }

                if transition_connecting.load(Ordering::Relaxed) {
                    self.watchdog.feed();
                    return;
                }

                // Check for a controller trying to bind
                if let Ok((recv_buf, meta)) = self.net.udp_recv() {
                    if recv_buf.len() == Rev7BindingInput::BYTE_LEN {
                        let binding_input = Rev7BindingInput::read_bytes(recv_buf);
                        if binding_input.is_valid() {
                            // Store the controller's address
                            self.controller = Some(meta);
                            self.configuring_timeout_ms = binding_input.configuring_timeout_ms;

                            // Respond to the controller
                            let binding_response = Rev7BindingOutput::new(PeripheralId {
                                model_number: MODEL_NUMBER,
                                serial_number: SERIAL_NUMBER,
                            });
                            match self
                                .net
                                .udp_send_with(Rev7BindingOutput::BYTE_LEN, meta, |buf| {
                                    binding_response.write_bytes(buf);
                                    Rev7BindingOutput::BYTE_LEN
                                }) {
                                Ok(_) => {}
                                Err(_) => {
                                    // If we are unable to send a UDP packet for any reason,
                                    // go back to connecting and start over
                                    transition_connecting.store(true, Ordering::Relaxed);
                                    self.watchdog.feed();
                                    return;
                                }
                            }
                            if modbus_enabled {
                                self.net.poll_bounded(self.time_ns, &mut net_budget);
                            } else {
                                self.net.poll(self.time_ns);
                            }

                            // UDP won protocol selection. Disable the unused
                            // TCP service before Configuring enters its legacy
                            // unbounded network-poll path.
                            self.modbus.reset();
                            self.net.reset_tcp_socket();

                            // Set flag to continue to Configuring
                            transition_configuring.store(true, Ordering::Relaxed);
                            self.watchdog.feed();
                            return;
                        }
                    }
                }

                if modbus_enabled {
                    let mut socket_budget = ModbusSocketBudget::new();
                    let response_was_pending = self.modbus.response_pending();
                    if response_was_pending {
                        if self
                            .modbus
                            .send_response(&mut self.net, &mut socket_budget)
                            .is_err()
                        {
                            self.modbus.reset();
                            let _ = self.net.tcp_relisten();
                        }
                    } else if self.net.tcp_can_recv() {
                        match self.modbus.receive(&mut self.net, &mut socket_budget) {
                            ReceiveStatus::Complete => {
                                match self.modbus.inspect_binding_request() {
                                    Ok(outcome) if outcome.accepted => {
                                        transition_modbus.store(true, Ordering::Relaxed);
                                    }
                                    Ok(_) => {
                                        if self
                                            .modbus
                                            .send_response(&mut self.net, &mut socket_budget)
                                            .is_err()
                                        {
                                            self.modbus.reset();
                                            let _ = self.net.tcp_relisten();
                                        }
                                    }
                                    Err(_) => {
                                        self.modbus.reset();
                                        let _ = self.net.tcp_relisten();
                                    }
                                }
                            }
                            ReceiveStatus::Malformed => {
                                // Modbus/TCP has no stream resynchronization marker. Abort only
                                // this connection; the next connection starts at byte zero again.
                                self.modbus.reset();
                                let _ = self.net.tcp_relisten();
                            }
                            ReceiveStatus::Disconnected => {
                                self.modbus.reset();
                                let _ = self.net.tcp_relisten();
                            }
                            ReceiveStatus::Incomplete => {}
                        }
                    } else if self.net.tcp_connection_ended() {
                        self.modbus.reset();
                        let _ = self.net.tcp_relisten();
                    }

                    // Flush newly queued response bytes without exceeding the
                    // same two-frame network budget used by the ingress poll.
                    self.net.poll_bounded(self.time_ns, &mut net_budget);
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
                    || transition_configuring.load(Ordering::Relaxed)
                    || transition_modbus.load(Ordering::Relaxed);

                if transition {
                    break 'wait_for_transition;
                }

                cortex_m::asm::wfi(); // Wait for interrupt
            }
        });

        if transition_configuring.load(Ordering::Relaxed) {
            return BoardState::Configuring;
        } else if transition_modbus.load(Ordering::Relaxed) {
            return BoardState::OperatingModbus(self.modbus.take_binding_config().unwrap());
        } else {
            return BoardState::Connecting;
        }
    }
}
