use super::*;

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use deimos_shared::peripherals::deimos_daq_rev7::*;
use deimos_shared::states::{ByteStruct, ByteStructLen};
use irq::{handler, scope};

use super::modbus::{ModbusSocketBudget, ReceiveStatus};

/// When an i32 wraps, what is the size of the jump in value?
/// Counter values will eventually be converted to an f64, so it's useful to think about the implications.
/// 64-bit float which has integer resolution out to 2**53, so
/// if the total value exceeds 2**53, individual steps may become unrepresentable,
/// although the total value will continue to track as close as possible out to 2**62
/// where the number of wraps will wrap. Only the incoming fully-representable integer
/// values are used for each update, so there is no accumulation of floating-point error.
///
/// Some reference points
/// * f64 has integer resolution out to 2**53, so this is ok in terms of resolution
/// * 50MHz is the fastest possible rate that the counter peripherals can reach in any configuration
/// * 2**53 count is about 5 years at 50MHz before losing exact resolution
/// * 2**62 count is about 2900 years at 50MHz before wrapping
const WRAP_SPAN: i64 = 1_i64 << 32;

impl<'a> Board<'a> {
    /// Runs the common engineering-snapshot operating loop for one protocol.
    ///
    /// Args:
    ///   mode: Per-invocation transport selection and resolved entry values.
    ///
    /// Returns:
    ///   The next persistent board state after timeout or transport failure.
    pub(super) fn operate(&mut self, mode: OperatingMode) -> BoardState {
        // Pause systick until we are ready
        self.systick.disable_interrupt();
        self.systick.disable_counter();
        self.watchdog.feed();

        // Resolve all mode-specific entry state before enabling the cycle IRQ.
        // Deimos configuration already installed its period, timeout, and ADC
        // cutoff. Modbus carries those values explicitly so rate-change
        // re-entry can preserve the complete output state.
        let initial_outputs = match mode {
            OperatingMode::Deimos => OperatingOutputSettings::default(),
            OperatingMode::Modbus(initial_config) => {
                self.dt_ns = initial_config.dt_ns;
                self.loss_of_contact_limit = initial_config.loss_of_contact_limit;

                let reporting_rate_hz = 1.0e9 / self.dt_ns as f64;
                let cutoff_ratio = reporting_rate_hz / ADC_SAMPLE_FREQ_HZ as f64;
                ADC_CUTOFF_RATIO.store(cutoff_ratio as f32, Ordering::Relaxed);
                NEW_ADC_CUTOFF.store(true, Ordering::Relaxed);

                initial_config.outputs
            }
        };
        let mut current_modbus_config = match mode {
            OperatingMode::Deimos => ModbusInitialConfig::default(),
            OperatingMode::Modbus(initial_config) => initial_config,
        };

        // A Modbus operating invocation owns the connection selected in
        // Binding, including across a rate-change re-entry. A vanished session
        // uses the normal safe-output reconnect path.
        if matches!(mode, OperatingMode::Modbus(_)) && !self.net.tcp_is_active() {
            return BoardState::Connecting;
        }

        // Init
        //    Set status LEDs
        self.led0.set_high();
        self.led1.set_high();
        self.led2.set_high();
        self.led3.set_high();

        //    Set up sub-cycle timer
        self.subcycle_timer
            .set_timeout(Duration::from_nanos(2 * self.dt_ns as u64)); // Just needs to be at least as long as dt_ns
        let subcycle_scale = self.subcycle_timer.inner().psc.read().psc().bits() as u64 + 1; // Register values index from 0b0 -> prescale = 1
        let subcycle_res_ns =
            (subcycle_scale * 1_000_000_000 / (self.subcycle_rate_hz as u64)) as u32;

        // Deimos enters with safe defaults; Modbus re-entry restores its last
        // complete settings without an intermediate output glitch.
        self.set_outputs(&initial_outputs);

        //    Transition flags
        let transition_connecting = AtomicBool::new(false);
        let transition_modbus_reentry = AtomicBool::new(false);

        //    Storage
        let mut operating_output = OperatingSnapshot::default();
        let mut deimos_input = OperatingRoundtripInput::default();
        deimos_input.outputs = initial_outputs;
        let mut loss_of_contact_persistence_counter = 0;

        // Board temperature is the cold-junction estimate and is filtered once per
        // publishing cycle. Filter construction remains outside the interrupt hot path.
        let reporting_rate_hz = 1.0e9 / self.dt_ns as f64;
        let board_temperature_filter = adc_filter_bank(1.0 / reporting_rate_hz).unwrap()[0];
        let initial_samples = latest_adc_samples();
        let initial_board_temperature_k =
            board_temperature_k_f32(&initial_samples, &self.calibration);
        let mut board_temperature_filter_state = board_temperature_filter.reset_state();
        board_temperature_filter.set_steady_state(
            &mut board_temperature_filter_state,
            [if initial_board_temperature_k.is_finite() {
                initial_board_temperature_k
            } else {
                0.0
            }],
        );

        // Sampling runs in every board state. Discard time accumulated before
        // this operating interval so the first completed-cycle margin contains
        // only work attributable to the new operating session.
        ACCUMULATED_SAMPLING_TIME_NS.store(0, Ordering::Relaxed);

        // Set up main cycle
        self.systick_init();

        //    Interrupt handler
        handler!(
            systick_handler = || {
                // Restart subcycle counter
                self.subcycle_timer.apply_freq();
                self.subcycle_timer.resume();

                // Increment cycle time
                self.time_ns += self.dt_ns as i64;
                let phase_delta_ns = match mode {
                    OperatingMode::Deimos => deimos_input.phase_delta_ns,
                    OperatingMode::Modbus(_) => 0,
                };
                let end_of_cycle = self.time_ns + self.dt_ns as i64 + phase_delta_ns;

                // If we have lost contact with the controller, go back to connecting
                let contact_lost =
                    loss_of_contact_persistence_counter >= self.loss_of_contact_limit;
                transition_connecting.fetch_or(contact_lost, Ordering::Relaxed);

                // Preemptively increment loss-of-contact counter
                // so that it increments even if we do not complete the cycle on time
                // and clear the phase delta so that we do not repeatedly apply the same
                // delta if we miss an input packet
                loss_of_contact_persistence_counter += 1;
                // Only Deimos uses timing corrections. Zero its one-cycle
                // phase portion while preserving the period adjustment.
                deimos_input.phase_delta_ns = 0;

                // Read one coherent ADC group and convert it to the common engineering snapshot.
                let adc_samples = latest_adc_samples();
                let unfiltered_board_temperature_k =
                    board_temperature_k_f32(&adc_samples, &self.calibration);
                let filtered_board_temperature_k = board_temperature_filter.step(
                    &mut board_temperature_filter_state,
                    [unfiltered_board_temperature_k],
                )[0];
                populate_analog_snapshot_f32(
                    &mut operating_output,
                    &adc_samples,
                    &self.calibration,
                    filtered_board_temperature_k,
                );

                // Get latest timer input readings
                // and unwrap from i32 values to one i64
                let encoder_val = COUNTER_SAMPLES[0].load(Ordering::Relaxed) as i64;
                let encoder_wraps = COUNTER_WRAPS[0].load(Ordering::Relaxed) as i64;
                operating_output.encoder = encoder_val + encoder_wraps * WRAP_SPAN;

                let pulse_counter_val = COUNTER_SAMPLES[1].load(Ordering::Relaxed) as i64;
                let pulse_counter_wraps = COUNTER_WRAPS[1].load(Ordering::Relaxed) as i64;
                operating_output.pulse_counter =
                    pulse_counter_val + pulse_counter_wraps * WRAP_SPAN;

                operating_output.frequency_meas[0] = FREQ_SAMPLES[0].load(Ordering::Relaxed);
                operating_output.frequency_meas[1] = FREQ_SAMPLES[1].load(Ordering::Relaxed);
                operating_output.gpio = self.read_gpio_inputs();
                operating_output.metrics.cycle_time_ns = self.time_ns;
                operating_output.metrics.id = operating_output.metrics.id.wrapping_add(1);
                operating_output.metrics.sent_time_ns = self.board_time(subcycle_res_ns);

                match mode {
                    OperatingMode::Deimos => {
                        // Deimos publishes every snapshot as an unsolicited UDP
                        // response and requires its bound controller to remain.
                        let Some(meta) = self.controller else {
                            transition_connecting.store(true, Ordering::Relaxed);
                            self.watchdog.feed();
                            return;
                        };
                        match self
                            .net
                            .udp_send_with(OperatingSnapshot::BYTE_LEN, meta, |buf| {
                                operating_output.write_bytes(buf);
                                OperatingSnapshot::BYTE_LEN
                            }) {
                            Ok(_) => {}
                            Err(_) => {
                                transition_connecting.store(true, Ordering::Relaxed);
                                self.watchdog.feed();
                                return;
                            }
                        }

                        for _ in 0..2 {
                            // Poll at least twice to clear buffered inputs while
                            // the roundtrip controller converges on phase lock.
                            self.net.poll(self.board_time(subcycle_res_ns));
                            match self.net.udp_recv() {
                                Ok((recv_buf, meta)) if Some(meta) == self.controller => {
                                    if recv_buf.len() == OperatingRoundtripInput::BYTE_LEN {
                                        let candidate =
                                            OperatingRoundtripInput::read_bytes(recv_buf);
                                        if !candidate.is_valid() {
                                            continue;
                                        }
                                        deimos_input = candidate;

                                        if deimos_input.id > operating_output.metrics.last_input_id
                                        {
                                            operating_output.metrics.last_input_id =
                                                deimos_input.id;
                                            loss_of_contact_persistence_counter = 0;
                                        }
                                    }
                                }
                                Err(_) => {}
                                _ => {}
                            };
                        }

                        self.systick_adjust(
                            deimos_input.phase_delta_ns + deimos_input.period_delta_ns,
                        );
                    }
                    OperatingMode::Modbus(_) => {
                        let mut net_budget = NetPollBudget::modbus_cycle();
                        self.net
                            .poll_bounded(self.board_time(subcycle_res_ns), &mut net_budget);
                        if self.net.tcp_connection_ended() {
                            transition_connecting.store(true, Ordering::Relaxed);
                            self.watchdog.feed();
                            return;
                        }

                        let mut socket_budget = ModbusSocketBudget::new();
                        let response_was_pending = self.modbus.response_pending();
                        if response_was_pending {
                            if self
                                .modbus
                                .send_response(&mut self.net, &mut socket_budget)
                                .is_err()
                            {
                                transition_connecting.store(true, Ordering::Relaxed);
                                self.watchdog.feed();
                                return;
                            }
                        } else {
                            let receive_status =
                                if self.modbus.request_complete() || self.net.tcp_can_recv() {
                                    self.modbus.receive(&mut self.net, &mut socket_budget)
                                } else {
                                    ReceiveStatus::Incomplete
                                };
                            match receive_status {
                                ReceiveStatus::Complete => {
                                    match self.modbus.process_operating_request(
                                        &operating_output,
                                        current_modbus_config,
                                        loss_of_contact_persistence_counter,
                                    ) {
                                        Ok(outcome) => {
                                            if outcome.accepted {
                                                let rate_changed =
                                                    outcome.config.dt_ns != self.dt_ns;
                                                current_modbus_config = outcome.config;
                                                self.loss_of_contact_limit =
                                                    outcome.config.loss_of_contact_limit;
                                                deimos_input.outputs = outcome.config.outputs;
                                                operating_output.metrics.last_input_id =
                                                    u64::from(outcome.transaction_id);
                                                operating_output
                                                    .metrics
                                                    .last_input_received_time_ns =
                                                    self.board_time(subcycle_res_ns);
                                                loss_of_contact_persistence_counter = 0;
                                                if rate_changed {
                                                    self.modbus.set_reentry_config(outcome.config);
                                                }
                                            }
                                        }
                                        Err(_) => {
                                            transition_connecting.store(true, Ordering::Relaxed);
                                            self.watchdog.feed();
                                            return;
                                        }
                                    }
                                }
                                ReceiveStatus::Malformed | ReceiveStatus::Disconnected => {
                                    transition_connecting.store(true, Ordering::Relaxed);
                                    self.watchdog.feed();
                                    return;
                                }
                                ReceiveStatus::Incomplete => {}
                            }

                            if self.modbus.response_pending()
                                && self
                                    .modbus
                                    .send_response(&mut self.net, &mut socket_budget)
                                    .is_err()
                            {
                                transition_connecting.store(true, Ordering::Relaxed);
                                self.watchdog.feed();
                                return;
                            }
                        }

                        // A second bounded poll can emit a newly enqueued
                        // response, but shares the original two-frame budget.
                        self.net
                            .poll_bounded(self.board_time(subcycle_res_ns), &mut net_budget);
                        if self.modbus.reentry_pending() && !self.modbus.response_pending() {
                            transition_modbus_reentry.store(true, Ordering::Relaxed);
                        }
                    }
                }

                // Apply one complete settings object in either mode. Modbus
                // reads and omitted future writes leave this retained value
                // unchanged across cycles and rate-change re-entry.
                self.set_outputs(&deimos_input.outputs);

                // Keep operating on the current address and defer fallback-to-DHCP swaps.
                if self.net.step_address(self.time_ns, AddressMode::Operating)
                    == AddressStatus::Missing
                {
                    transition_connecting.store(true, Ordering::Relaxed);
                }

                // Get overall cycle timing margin and put it in the output
                let adc_sample_time_ns =
                    ACCUMULATED_SAMPLING_TIME_NS.fetch_and(0, Ordering::Relaxed) as i64;
                operating_output.metrics.cycle_time_margin_ns =
                    end_of_cycle - self.board_time(subcycle_res_ns) - adc_sample_time_ns;

                self.watchdog.feed();
            }
        );

        // Create a scope and register the systick interrupt handler.
        scope(|s| {
            s.register(interrupts::SysTick, systick_handler);

            let mut transition: bool;
            'wait_for_transition: loop {
                transition = transition_connecting.load(Ordering::Relaxed);
                transition |= transition_modbus_reentry.load(Ordering::Relaxed);
                if transition {
                    break 'wait_for_transition;
                }

                cortex_m::asm::wfi(); // Wait for interrupt
            }
        });

        if transition_connecting.load(Ordering::Relaxed) {
            BoardState::Connecting
        } else if transition_modbus_reentry.load(Ordering::Relaxed) {
            BoardState::OperatingModbus(self.modbus.take_reentry_config().unwrap())
        } else {
            BoardState::Connecting
        }
    }
}
