use super::*;

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use deimos_shared::peripherals::deimos_daq_rev7::*;
use deimos_shared::states::{ByteStruct, ByteStructLen};
use irq::{handler, scope};

use super::modbus::{MAX_ADUS_PER_CYCLE, ModbusSocketBudget, ReceiveStatus};

/// Conservative minimum publishing-IRQ margin for test-instrumented images.
///
/// One SysTick handler is the only writer, so relaxed load/store semantics are
/// sufficient and avoid an exclusive-update loop in the measured path.
#[cfg(feature = "timing-watermark")]
#[unsafe(no_mangle)]
pub static MIN_PUBLISHING_CYCLE_MARGIN_NS: core::sync::atomic::AtomicI32 =
    core::sync::atomic::AtomicI32::new(i32::MAX);

/// Minimum oversampled sample-only SysTick margin in an instrumented image.
#[cfg(feature = "timing-watermark")]
#[unsafe(no_mangle)]
pub static MIN_SAMPLE_ONLY_MARGIN_NS: core::sync::atomic::AtomicI32 =
    core::sync::atomic::AtomicI32::new(i32::MAX);

/// Minimum cycle-owned sample-plus-communication SysTick margin.
#[cfg(feature = "timing-watermark")]
#[unsafe(no_mangle)]
pub static MIN_SAMPLE_COMM_MARGIN_NS: core::sync::atomic::AtomicI32 =
    core::sync::atomic::AtomicI32::new(i32::MAX);

/// Mutable protocol and conversion state shared by both sampling topologies.
struct OperatingState {
    mode: OperatingMode,
    current_modbus_config: ModbusInitialConfig,
    output: OperatingSnapshot,
    input: OperatingRoundtripInput,
    loss_of_contact_counter: u16,
    board_temperature_filter: AdcFilter,
    board_temperature_filter_state: AdcFilterState,
}

impl<'a> Board<'a> {
    /// Run one operating state with a topology selected at entry.
    ///
    /// Args:
    ///   mode: Transport and resolved entry configuration.
    ///   sampler: ADC/counter sampler lent exclusively to SysTick for the
    ///     duration of this invocation.
    ///
    /// Returns:
    ///   Next persistent state after timeout, transport failure, or rate change.
    pub(super) fn operate(&mut self, mode: OperatingMode, sampler: &mut Sampler) -> BoardState {
        self.systick.disable_interrupt();
        self.systick.disable_counter();
        self.watchdog.feed();

        let (initial_outputs, current_modbus_config) = match mode {
            // Deimos supplies a fresh output command through each roundtrip, so
            // every entry begins from the safe output state.
            OperatingMode::Deimos => (
                OperatingOutputSettings::default(),
                ModbusInitialConfig::default(),
            ),
            // Modbus commands are retained across reads and rate-change
            // re-entry, making the entry configuration the initial live state.
            OperatingMode::Modbus(initial_config) => {
                self.dt_ns = initial_config.dt_ns;
                self.loss_of_contact_limit = initial_config.loss_of_contact_limit;
                (initial_config.outputs, initial_config)
            }
        };

        // Modbus re-entry retains the selected TCP session. A vanished session
        // uses the ordinary reconnect path and safe output handling.
        if matches!(mode, OperatingMode::Modbus(_)) && !self.net.tcp_is_active() {
            return BoardState::Connecting;
        }

        self.led0.set_high();
        self.led1.set_high();
        self.led2.set_high();
        self.led3.set_high();

        // TIM5 measures elapsed time within each SysTick handler. Its count is
        // used for packet timestamps and for the execution-margin watermark;
        // a two-cycle range prevents a normal handler from reaching its limit.
        self.subcycle_timer
            .set_timeout(Duration::from_nanos(2 * u64::from(self.dt_ns)));
        let subcycle_scale = u64::from(self.subcycle_timer.inner().psc.read().psc().bits()) + 1;
        let subcycle_res_ns =
            (subcycle_scale * 1_000_000_000 / u64::from(self.subcycle_rate_hz)) as u32;

        self.set_outputs(&initial_outputs);
        // The SysTick closure requests transitions through these flags while
        // the foreground remains asleep in the scoped-IRQ wait loop. Relaxed
        // access is sufficient on this single-core target; associated state is
        // inspected only after the handler is unregistered at scope exit.
        let transition_connecting = AtomicBool::new(false);
        let transition_modbus_reentry = AtomicBool::new(false);

        // The reporting period determines both the number of acquisitions per
        // published snapshot and whether the ADC path includes the IIR stage.
        let reporting_rate_hz = 1.0e9 / f64::from(self.dt_ns);
        // UNWRAP: Both protocols validate `dt_ns` against the supported
        // cycle-rate range before entering an operating state.
        let sampling_policy = adc_sampling_policy(reporting_rate_hz).unwrap();
        sampler.configure_synchronous(
            sampling_policy.sample_rate_hz,
            sampling_policy.iir_cutoff_ratio,
        );

        // Prime every filter history from one real acquisition group before
        // publishing. Steady initial values prevent startup transients in both
        // raw channels and downstream board-temperature compensation.
        sampler.prime_synchronous(self.time_ns);

        // Board temperature is converted to kelvin before filtering because it
        // compensates the other engineering conversions. This separate 1 Hz
        // filter runs once per published snapshot, independent of the ADC
        // acquisition topology selected above.
        // UNWRAP: A 1 Hz cutoff is valid at every supported reporting rate.
        let board_temperature_filter = adc_filter_bank(1.0 / reporting_rate_hz).unwrap()[0];
        let initial_sample_group = &sampler.sampled_inputs().adc;
        let initial_board_temperature_k =
            board_temperature_k_f32(&initial_sample_group.values, &self.calibration);
        let mut board_temperature_filter_state = board_temperature_filter.reset_state();

        // Seed the engineering-value filter at its measured initial value so
        // the first published temperature is already at steady state. A
        // non-finite conversion cannot be a useful filter state, so zero is the
        // deterministic fallback until a finite measurement arrives.
        board_temperature_filter.set_steady_state(
            &mut board_temperature_filter_state,
            [if initial_board_temperature_k.is_finite() {
                initial_board_temperature_k
            } else {
                0.0
            }],
        );

        // All protocol-independent mutable state lives together so either IRQ
        // topology can call the same engineering, I/O, and publishing cycle.
        // Modbus configuration is retained here to support a rate-change
        // re-entry while preserving the last commanded output values.
        let mut state = OperatingState {
            mode,
            current_modbus_config,
            output: OperatingSnapshot::default(),
            input: OperatingRoundtripInput {
                outputs: initial_outputs,
                ..OperatingRoundtripInput::default()
            },
            loss_of_contact_counter: 0,
            board_temperature_filter,
            board_temperature_filter_state,
        };

        // Oversampled operation spaces acquisitions across each reporting
        // period and applies the IIR before publication. Direct operation runs
        // one fractional-delay-only acquisition and publishes in the same IRQ.
        match sampling_policy.mode {
            AdcSamplingMode::Oversampled => self.operate_synchronous_oversampled(
                sampler,
                &mut state,
                &transition_connecting,
                &transition_modbus_reentry,
                subcycle_res_ns,
                sampling_policy.samples_per_cycle,
            ),
            AdcSamplingMode::Direct => self.operate_synchronous_direct(
                sampler,
                &mut state,
                &transition_connecting,
                &transition_modbus_reentry,
                subcycle_res_ns,
            ),
        }

        // The selected IRQ scope returns only after a handler requests a state
        // transition. Stop SysTick before inspecting that request and handing
        // the sampler back to the top-level state machine.
        self.systick.disable_interrupt();
        self.systick.disable_counter();

        // Rate changes re-enter Modbus operation with the updated period,
        // timeout, and held outputs. Every other exit returns to discovery and
        // configuration through Connecting.
        if transition_connecting.load(Ordering::Relaxed) {
            BoardState::Connecting
        } else if transition_modbus_reentry.load(Ordering::Relaxed) {
            match self.modbus.take_reentry_config() {
                Some(config) => BoardState::OperatingModbus(config),
                None => BoardState::Connecting,
            }
        } else {
            BoardState::Connecting
        }
    }

    /// Run the rounded number of ADC groups nearest the internal rate target.
    ///
    /// SysTick divides each publishing cycle into uniformly spaced acquisition
    /// intervals. Intermediate interrupts only sample; the final interrupt also
    /// performs engineering conversion, communication, and output updates.
    ///
    /// Args:
    ///   sampler: ADC and counter sampler owned by the SysTick scope.
    ///   state: Shared protocol and engineering state for this operating entry.
    ///   transition_connecting: IRQ-to-foreground reconnect request.
    ///   transition_modbus_reentry: IRQ-to-foreground rate-change request.
    ///   subcycle_res_ns: TIM5 counter resolution in `ns/tick`.
    ///   samples_per_cycle: Number of ADC groups in `sample/cycle`.
    #[allow(clippy::too_many_arguments)]
    fn operate_synchronous_oversampled(
        &mut self,
        sampler: &mut Sampler,
        state: &mut OperatingState,
        transition_connecting: &AtomicBool,
        transition_modbus_reentry: &AtomicBool,
        subcycle_res_ns: u32,
        samples_per_cycle: u32,
    ) {
        // The quotient/remainder scheduler represents the complete reporting
        // period without allocating an interval table. Its intervals sum to
        // exactly one cycle and differ by no more than one SysTick tick.
        let mut scheduler = self.sample_interval_scheduler(0, samples_per_cycle);
        let first_reload = scheduler.next_ticks() - 1;
        self.systick_init_reload(first_reload);
        // AcquisitionClock accumulates the applied reload durations so sample
        // timestamps include the one-tick scheduler rounding pattern.
        let mut acquisition_clock = self.acquisition_clock_init();
        let systick_tick_period_ns = self.systick_tick_period_ns();
        let mut samples_remaining = samples_per_cycle;

        handler!(
            systick_handler = || {
                // SysTick reloads before entering the handler. Advance over the
                // completed interval, then associate the current down-counter
                // with the interval which has just begun.
                acquisition_clock.advance(SYST::get_reload(), systick_tick_period_ns);
                self.restart_subcycle_timer();
                let sample_time_ns =
                    acquisition_clock.timestamp_ns(SYST::get_current(), systick_tick_period_ns);
                sampler.sample_synchronous_iir(sample_time_ns);
                samples_remaining -= 1;

                // Sampling-only interrupts schedule the next acquisition and
                // return promptly, spreading CPU work across the cycle.
                if samples_remaining > 0 {
                    let next_reload = scheduler.next_ticks() - 1;
                    self.systick.set_reload(next_reload);
                    // Margin is the time from completion of this handler to the
                    // next scheduled acquisition.
                    let margin_ns = reload_duration_ns(next_reload, systick_tick_period_ns)
                        - self.subcycle_elapsed_ns(subcycle_res_ns);
                    record_sample_only_margin(margin_ns);
                    self.watchdog.feed();
                    return;
                }

                // The final filtered group is the coherent input snapshot for
                // this publishing cycle. Communication may request a bounded
                // correction to the duration of the following cycle.
                let correction_ns = self.operating_cycle(
                    state,
                    sampler.sampled_inputs(),
                    transition_connecting,
                    transition_modbus_reentry,
                    subcycle_res_ns,
                );
                // Rebuild the schedule once per cycle so the correction is
                // distributed uniformly over all of its acquisitions.
                scheduler = self.sample_interval_scheduler(correction_ns, samples_per_cycle);
                let next_reload = scheduler.next_ticks() - 1;
                self.systick.set_reload(next_reload);
                samples_remaining = samples_per_cycle;
                // Publication margin includes sampling, conversions, network
                // service, and output updates performed by this handler.
                let margin_ns = reload_duration_ns(next_reload, systick_tick_period_ns)
                    - self.subcycle_elapsed_ns(subcycle_res_ns);
                state.output.metrics.cycle_time_margin_ns = margin_ns;
                record_publication_margin(margin_ns);
                record_sample_comm_margin(margin_ns);
                self.watchdog.feed();
            }
        );

        scope(|s| {
            s.register(interrupts::SysTick, systick_handler);
            wait_for_operating_transition(transition_connecting, transition_modbus_reentry);
        });
    }

    /// Run one fractional-delay-only ADC group per published snapshot.
    ///
    /// At one acquisition per cycle there is no higher-rate stream for the IIR
    /// to antialias before decimation, so sampling and publication share every
    /// SysTick interrupt and only channel-alignment filtering is applied.
    ///
    /// Args:
    ///   sampler: ADC and counter sampler owned by the SysTick scope.
    ///   state: Shared protocol and engineering state for this operating entry.
    ///   transition_connecting: IRQ-to-foreground reconnect request.
    ///   transition_modbus_reentry: IRQ-to-foreground rate-change request.
    ///   subcycle_res_ns: TIM5 counter resolution in `ns/tick`.
    #[allow(clippy::too_many_arguments)]
    fn operate_synchronous_direct(
        &mut self,
        sampler: &mut Sampler,
        state: &mut OperatingState,
        transition_connecting: &AtomicBool,
        transition_modbus_reentry: &AtomicBool,
        subcycle_res_ns: u32,
    ) {
        self.systick_init();
        let mut acquisition_clock = self.acquisition_clock_init();
        let systick_tick_period_ns = self.systick_tick_period_ns();

        handler!(
            systick_handler = || {
                // The timestamp represents the start of the acquisition, while
                // the subcycle timer below measures all subsequent IRQ work.
                acquisition_clock.advance(SYST::get_reload(), systick_tick_period_ns);
                self.restart_subcycle_timer();
                let sample_time_ns =
                    acquisition_clock.timestamp_ns(SYST::get_current(), systick_tick_period_ns);
                sampler.sample_synchronous_fractional_only(sample_time_ns);
                let correction_ns = self.operating_cycle(
                    state,
                    sampler.sampled_inputs(),
                    transition_connecting,
                    transition_modbus_reentry,
                    subcycle_res_ns,
                );
                // With one sample per cycle, the timing correction applies
                // directly to the next SysTick interval.
                self.systick_adjust(correction_ns);
                let margin_ns = self
                    .systick_interval_duration_ns(correction_ns, systick_tick_period_ns)
                    - self.subcycle_elapsed_ns(subcycle_res_ns);
                state.output.metrics.cycle_time_margin_ns = margin_ns;
                record_publication_margin(margin_ns);
                record_sample_comm_margin(margin_ns);
                self.watchdog.feed();
            }
        );

        scope(|s| {
            s.register(interrupts::SysTick, systick_handler);
            wait_for_operating_transition(transition_connecting, transition_modbus_reentry);
        });
    }

    /// Perform the single shared engineering, transport, and output cycle.
    ///
    /// Args:
    ///   state: Mutable protocol, snapshot, command, and filter state.
    ///   sampled_inputs: Coherent inputs from the latest ADC group.
    ///   transition_connecting: IRQ-to-foreground reconnect request.
    ///   transition_modbus_reentry: IRQ-to-foreground rate-change request.
    ///   subcycle_res_ns: TIM5 counter resolution in `ns/tick`.
    ///
    /// Returns:
    ///   Bounded transport timing correction requested for the next publishing
    ///   interval, in `ns`.
    #[inline(never)]
    fn operating_cycle(
        &mut self,
        state: &mut OperatingState,
        sampled_inputs: &SampledInputs,
        transition_connecting: &AtomicBool,
        transition_modbus_reentry: &AtomicBool,
        subcycle_res_ns: u32,
    ) -> i64 {
        // `time_ns` is the nominal publishing epoch used by the network stack;
        // sample timestamps separately follow the applied SysTick intervals.
        self.time_ns += i64::from(self.dt_ns);

        // Every cycle begins provisionally unanswered. A fresh Deimos input or
        // accepted Modbus request resets the counter below; saturation makes an
        // indefinitely absent controller safe without introducing overflow.
        let contact_lost = state.loss_of_contact_counter >= self.loss_of_contact_limit;
        transition_connecting.fetch_or(contact_lost, Ordering::Relaxed);
        state.loss_of_contact_counter = state.loss_of_contact_counter.saturating_add(1);
        // Phase correction is a one-cycle request. Period correction and output
        // settings remain held in `state.input` until replaced.
        state.input.phase_delta_ns = 0;

        // Convert and filter board temperature first because the same coherent
        // value compensates every temperature-dependent analog conversion in
        // this snapshot.
        let adc_sample_group = &sampled_inputs.adc;
        state.output.sample_time_ns = adc_sample_group.sample_time_ns;
        let unfiltered_board_temperature_k =
            board_temperature_k_f32(&adc_sample_group.values, &self.calibration);
        let filtered_board_temperature_k = state.board_temperature_filter.step(
            &mut state.board_temperature_filter_state,
            [unfiltered_board_temperature_k],
        )[0];
        populate_analog_snapshot_f32(
            &mut state.output,
            &adc_sample_group.values,
            &self.calibration,
            filtered_board_temperature_k,
        );

        state.output.encoder = sampled_inputs.encoder;
        state.output.pulse_counter = sampled_inputs.pulse_counter;
        state.output.frequency_meas = sampled_inputs.frequency_meas;
        state.output.gpio = self.read_gpio_inputs();
        state.output.metrics.id = state.output.metrics.id.wrapping_add(1);
        // `sent_time_ns` is captured after engineering conversion and as close
        // as practical to transport service; `sample_time_ns` remains the ADC
        // acquisition timestamp copied above.
        state.output.metrics.sent_time_ns = self.board_time(subcycle_res_ns);

        match state.mode {
            OperatingMode::Deimos => {
                let Some(meta) = self.controller else {
                    transition_connecting.store(true, Ordering::Relaxed);
                    return 0;
                };
                if self
                    .net
                    .udp_send_with(OperatingSnapshot::BYTE_LEN, meta, |buf| {
                        state.output.write_bytes(buf);
                        OperatingSnapshot::BYTE_LEN
                    })
                    .is_err()
                {
                    transition_connecting.store(true, Ordering::Relaxed);
                    return 0;
                }

                // Send the coherent snapshot before accepting the command for
                // this roundtrip. The accepted input then controls the outputs
                // and timing correction applied at the end of the same IRQ.
                // Two bounded receives clear buffered inputs while the active
                // roundtrip timing controller converges on phase lock.
                for _ in 0..2 {
                    self.net.poll(self.board_time(subcycle_res_ns));
                    match self.net.udp_recv() {
                        Ok((recv_buf, meta))
                            if Some(meta) == self.controller
                                && recv_buf.len() == OperatingRoundtripInput::BYTE_LEN =>
                        {
                            let candidate = OperatingRoundtripInput::read_bytes(recv_buf);
                            if !candidate.is_valid() {
                                continue;
                            }
                            state.input = candidate;
                            if state.input.id > state.output.metrics.last_input_id {
                                state.output.metrics.last_input_id = state.input.id;
                                state.loss_of_contact_counter = 0;
                            }
                        }
                        _ => {}
                    }
                }
            }
            OperatingMode::Modbus(_) => {
                // Bound Ethernet-device and socket work independently so link
                // traffic cannot consume the complete control-cycle margin.
                let mut net_budget = NetPollBudget::modbus_cycle();
                self.net
                    .poll_bounded(self.board_time(subcycle_res_ns), &mut net_budget);
                if self.net.tcp_connection_ended() {
                    transition_connecting.store(true, Ordering::Relaxed);
                    return 0;
                }

                let mut socket_budget = ModbusSocketBudget::new();
                // Keep the published snapshot immutable while draining both
                // requests. Contact metadata is committed only after service,
                // so two FC23 responses in this cycle expose the same sample.
                let mut last_accepted_request = None;
                for _ in 0..MAX_ADUS_PER_CYCLE {
                    if self.modbus.response_pending() {
                        // A partially enqueued response has priority over the
                        // next request, preserving request/response ordering.
                        if self
                            .modbus
                            .send_response(&mut self.net, &mut socket_budget)
                            .is_err()
                        {
                            transition_connecting.store(true, Ordering::Relaxed);
                            return 0;
                        }
                        if self.modbus.response_pending() {
                            break;
                        }
                    }

                    let receive_status =
                        if self.modbus.request_complete() || self.net.tcp_can_recv() {
                            self.modbus.receive(&mut self.net, &mut socket_budget)
                        } else {
                            ReceiveStatus::Incomplete
                        };
                    match receive_status {
                        ReceiveStatus::Complete => match self.modbus.process_operating_request(
                            &state.output,
                            state.current_modbus_config,
                            state.loss_of_contact_counter,
                        ) {
                            Ok(outcome) if outcome.accepted => {
                                let rate_changed = outcome.config.dt_ns != self.dt_ns;
                                // Every accepted request yields a complete
                                // retained configuration. Writes update it and
                                // reads carry it through, so the last accepted
                                // request determines the held state.
                                state.current_modbus_config = outcome.config;
                                self.loss_of_contact_limit = outcome.config.loss_of_contact_limit;
                                state.input.outputs = outcome.config.outputs;
                                last_accepted_request = Some((
                                    u64::from(outcome.transaction_id),
                                    self.board_time(subcycle_res_ns),
                                ));
                                state.loss_of_contact_counter = 0;
                                if rate_changed {
                                    // Re-entry reconstructs the sampler and
                                    // SysTick schedule for the new cycle rate.
                                    self.modbus.set_reentry_config(outcome.config);
                                }
                            }
                            Ok(_) => {}
                            Err(_) => {
                                transition_connecting.store(true, Ordering::Relaxed);
                                return 0;
                            }
                        },
                        ReceiveStatus::Malformed | ReceiveStatus::Disconnected => {
                            transition_connecting.store(true, Ordering::Relaxed);
                            return 0;
                        }
                        ReceiveStatus::Incomplete => break,
                    }
                    if self.modbus.response_pending()
                        && self
                            .modbus
                            .send_response(&mut self.net, &mut socket_budget)
                            .is_err()
                    {
                        transition_connecting.store(true, Ordering::Relaxed);
                        return 0;
                    }
                    // Backpressure preserves the response for a later cycle.
                    // A rate change exits through the existing operating
                    // entrypoint, so neither condition may consume another ADU.
                    if self.modbus.response_pending() || self.modbus.reentry_pending() {
                        break;
                    }
                }
                if let Some((transaction_id, received_time_ns)) = last_accepted_request {
                    // Report only committed application requests; malformed or
                    // unsupported ADUs do not count as controller contact.
                    state.output.metrics.last_input_id = transaction_id;
                    state.output.metrics.last_input_received_time_ns = received_time_ns;
                }

                self.net
                    .poll_bounded(self.board_time(subcycle_res_ns), &mut net_budget);
                // Send the rate-change response before leaving this IRQ scope,
                // then let `operate` rebuild timing from the retained config.
                if self.modbus.reentry_pending() && !self.modbus.response_pending() {
                    transition_modbus_reentry.store(true, Ordering::Relaxed);
                }
            }
        }

        // Both protocols converge here: the most recently accepted complete
        // command is applied once, after transport processing for this cycle.
        self.set_outputs(&state.input.outputs);
        // Address loss is handled like controller loss so the top-level state
        // machine can reacquire a usable interface configuration.
        if self.net.step_address(self.time_ns, AddressMode::Operating) == AddressStatus::Missing {
            transition_connecting.store(true, Ordering::Relaxed);
        }

        match state.mode {
            // Deimos carries persistent period and one-shot phase corrections
            // in each validated roundtrip input.
            OperatingMode::Deimos => bounded_cycle_timing_correction_ns(
                self.dt_ns,
                state.input.period_delta_ns,
                state.input.phase_delta_ns,
            ),
            // A rate-changing write exits this Operating invocation. Preserve
            // its pending one-shot phase term in the re-entry configuration
            // instead of consuming it on an interval which will be disabled.
            OperatingMode::Modbus(_) if self.modbus.reentry_pending() => 0,
            OperatingMode::Modbus(_) => state.current_modbus_config.take_timing_correction_ns(),
        }
    }

    /// Restart the TIM5 duration counter at the beginning of an IRQ.
    #[inline]
    fn restart_subcycle_timer(&mut self) {
        self.subcycle_timer.apply_freq();
        self.subcycle_timer.resume();
    }

    /// Return elapsed handler time measured by TIM5, in `ns`.
    #[inline]
    fn subcycle_elapsed_ns(&self, subcycle_res_ns: u32) -> i64 {
        i64::from(self.subcycle_timer.counter()) * i64::from(subcycle_res_ns)
    }

    /// Return the applied duration of one corrected publishing interval.
    fn systick_interval_duration_ns(&self, correction_ns: i64, systick_tick_period_ns: u32) -> i64 {
        i64::from(self.systick_interval_ticks(correction_ns)) * i64::from(systick_tick_period_ns)
    }
}

/// Wait in the foreground until one IRQ requests an operating transition.
fn wait_for_operating_transition(
    transition_connecting: &AtomicBool,
    transition_modbus_reentry: &AtomicBool,
) {
    // This is the deliberate unbounded state-machine wait: all per-cycle work
    // remains in SysTick, and each pass sleeps until an interrupt changes state.
    loop {
        if transition_connecting.load(Ordering::Relaxed)
            || transition_modbus_reentry.load(Ordering::Relaxed)
        {
            return;
        }
        cortex_m::asm::wfi();
    }
}

/// Convert one SysTick reload (`ticks - 1`) to an interval in `ns`.
#[inline(always)]
fn reload_duration_ns(reload: u32, systick_tick_period_ns: u32) -> i64 {
    i64::from(reload + 1) * i64::from(systick_tick_period_ns)
}

#[cfg(feature = "timing-watermark")]
/// Update one single-writer minimum without an exclusive atomic operation.
fn record_minimum(target: &core::sync::atomic::AtomicI32, margin_ns: i64) {
    let margin = margin_ns.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    if margin < target.load(Ordering::Relaxed) {
        target.store(margin, Ordering::Relaxed);
    }
}

/// Record the minimum margin of an IRQ which publishes a snapshot.
fn record_publication_margin(margin_ns: i64) {
    #[cfg(feature = "timing-watermark")]
    record_minimum(&MIN_PUBLISHING_CYCLE_MARGIN_NS, margin_ns);
    #[cfg(not(feature = "timing-watermark"))]
    let _ = margin_ns;
}

/// Record the minimum margin of an oversampled acquisition-only IRQ.
fn record_sample_only_margin(margin_ns: i64) {
    #[cfg(feature = "timing-watermark")]
    record_minimum(&MIN_SAMPLE_ONLY_MARGIN_NS, margin_ns);
    #[cfg(not(feature = "timing-watermark"))]
    let _ = margin_ns;
}

/// Record the minimum margin of an IRQ containing sampling and communication.
fn record_sample_comm_margin(margin_ns: i64) {
    #[cfg(feature = "timing-watermark")]
    record_minimum(&MIN_SAMPLE_COMM_MARGIN_NS, margin_ns);
    #[cfg(not(feature = "timing-watermark"))]
    let _ = margin_ns;
}
