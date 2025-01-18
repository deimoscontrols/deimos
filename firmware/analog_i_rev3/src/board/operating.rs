use super::*;

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use irq::{handler, scope};
use smoltcp::socket::udp;

use deimos_shared::peripherals::analog_i_rev_3::*;
use deimos_shared::states::{ByteStruct, ByteStructLen};

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
const WRAP_SPAN: i64 = (i32::MAX as i64) * 2;

impl<'a> Board<'a> {
    pub fn operate(&mut self) -> BoardState {
        // Pause systick until we are ready
        self.systick.disable_interrupt();
        self.systick.disable_counter();
        self.watchdog.feed();

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

        // Reset output state
        self.set_pwm(&OperatingRoundtripInput::default());

        //    Transition flags
        let transition_connecting = AtomicBool::new(false);

        //    Storage
        let mut udp_output = OperatingRoundtripOutput::default();
        let mut udp_input: OperatingRoundtripInput = OperatingRoundtripInput::default();
        let payload = &mut [0_u8; OperatingRoundtripOutput::BYTE_LEN];
        let mut loss_of_contact_persistence_counter = 0;

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
                let end_of_cycle = self.time_ns + self.dt_ns as i64 + udp_input.phase_delta_ns;

                // If we have lost contact with the controller, go back to connecting
                let contact_lost =
                    loss_of_contact_persistence_counter >= self.loss_of_contact_limit;
                transition_connecting.fetch_or(contact_lost, Ordering::Relaxed);

                // Preemptively increment loss-of-contact counter
                // so that it increments even if we do not complete the cycle on time
                // and clear the phase delta so that we do not repeatedly apply the same
                // delta if we miss an input packet
                loss_of_contact_persistence_counter += 1;
                udp_input.phase_delta_ns = 0; // Only zero the phase portion, but preserve period adjustment

                // Get latest ADC readings
                for i in 0..ADC_SAMPLES.len() {
                    udp_output.adc_voltages[i] = ADC_SAMPLES[i].load(Ordering::Relaxed);
                }

                // Get latest timer input readings
                // and unwrap from i32 values to one i64
                let encoder_val = COUNTER_SAMPLES[0].load(Ordering::Relaxed) as i64;
                let encoder_wraps = COUNTER_WRAPS[0].load(Ordering::Relaxed) as i64;
                udp_output.encoder = encoder_val + encoder_wraps * WRAP_SPAN;

                let pulse_counter_val = COUNTER_SAMPLES[1].load(Ordering::Relaxed) as i64;
                let pulse_counter_wraps = COUNTER_WRAPS[1].load(Ordering::Relaxed) as i64;
                udp_output.pulse_counter = pulse_counter_val + pulse_counter_wraps * WRAP_SPAN;

                udp_output.frequency_meas[0] = FREQ_SAMPLES[0].load(Ordering::Relaxed);
                udp_output.frequency_meas[1] = FREQ_SAMPLES[1].load(Ordering::Relaxed);

                // UDP send
                if let Some(meta) = self.controller {
                    udp_output.metrics.cycle_time_ns = self.time_ns;
                    udp_output.metrics.id = udp_output.metrics.id.wrapping_add(1);
                    udp_output.metrics.sent_time_ns = self.board_time(subcycle_res_ns);

                    udp_output.write_bytes(payload);

                    match self
                        .net
                        .sockets
                        .get_mut::<udp::Socket>(self.net.udp_handle)
                        .send_slice(payload, meta)
                    {
                        Ok(_) => {}
                        // If we are unable to transmit for any reason, return to connecting
                        Err(_) => {
                            transition_connecting.store(true, Ordering::Relaxed);
                            self.watchdog.feed();
                            return;
                        }
                    }
                } else {
                    // If the controller is deconfigured, go back to connecting
                    transition_connecting.store(true, Ordering::Relaxed);
                    self.watchdog.feed();
                    return;
                }

                for _ in 0..2 {
                    // Poll to receive next input
                    // We have to do this at least twice per cycle to clear
                    // buffering inputs if we were behind and are becoming sync'd
                    self.net.poll(self.board_time(subcycle_res_ns));
                    match self
                        .net
                        .sockets
                        .get_mut::<udp::Socket>(self.net.udp_handle)
                        .recv()
                    {
                        Ok((recv_buf, meta)) if Some(meta) == self.controller => {
                            if recv_buf.len() == OperatingRoundtripInput::BYTE_LEN {
                                // Parse
                                udp_input = OperatingRoundtripInput::read_bytes(&recv_buf);

                                if udp_input.id > udp_output.metrics.last_input_id {
                                    // Set last received packet id
                                    udp_output.metrics.last_input_id = udp_input.id;

                                    // Reset loss-of-contact counter
                                    loss_of_contact_persistence_counter = 0;
                                }
                            }
                        }
                        Err(_) => {}
                        _ => {}
                    };
                }

                // Set target phase adjustment
                self.systick_adjust(udp_input.phase_delta_ns + udp_input.period_delta_ns);

                // Write GPIO state based on last received inputs
                self.set_pwm(&udp_input);

                // Maintain IP address configuration or go back to connecting
                let ip_address_ok = self.poll_dhcp();
                transition_connecting.fetch_or(!ip_address_ok, Ordering::Relaxed);

                // Get overall cycle timing margin and put it in the output
                udp_output.metrics.cycle_time_margin_ns =
                    end_of_cycle - self.board_time(subcycle_res_ns);

                self.watchdog.feed();
            }
        );

        // Create a scope and register the systick interrupt handler.
        scope(|s| {
            s.register(interrupts::SysTick, systick_handler);

            let mut transition: bool;
            'wait_for_transition: loop {
                transition = transition_connecting.load(Ordering::Relaxed);
                if transition {
                    break 'wait_for_transition;
                }

                cortex_m::asm::wfi(); // Wait for interrupt
            }
        });

        return BoardState::Connecting;
    }
}
