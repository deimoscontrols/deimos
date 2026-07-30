//! Counter arithmetic for rev7 ADC acquisition timestamps.
//!
//! The firmware uses SysTick as both the communication-cycle boundary and the
//! counter within that cycle. This module contains only the target-independent
//! arithmetic; interrupt priority, masking, and pending-bit handling remain in
//! the firmware.
//!
//! References:
//!   \[1\] Arm, *Cortex-M7 Devices Generic User Guide*, DDI 0489D, 2018,
//!   sections 4.4 and 4.5.

/// Maximum number of clock/counter capture attempts in one sampling interrupt.
pub const MAX_CAPTURE_ATTEMPTS: usize = 2;

/// Cycle base and reload value for the SysTick interval currently in progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquisitionClock {
    /// Board time at the start of the active SysTick interval, in `ns`.
    pub cycle_start_ns: i64,
    /// SysTick reload used by the active interval, in counter ticks minus one.
    pub active_reload: u32,
}

impl AcquisitionClock {
    /// Construct a clock at the start of one SysTick interval.
    ///
    /// Args:
    ///   cycle_start_ns: Board time at the interval boundary, in `ns`.
    ///   active_reload: Applied SysTick reload, in counter ticks minus one.
    ///
    /// Returns:
    ///   Acquisition clock for the interval which has just started.
    pub const fn new(cycle_start_ns: i64, active_reload: u32) -> Self {
        Self {
            cycle_start_ns,
            active_reload,
        }
    }

    /// Advance across the completed interval and record the newly active reload.
    ///
    /// Args:
    ///   new_active_reload: Reload applied to the interval which has just
    ///     started, in counter ticks minus one.
    ///   systick_tick_period_ns: Exact SysTick counter period in `ns/tick`.
    pub fn advance(&mut self, new_active_reload: u32, systick_tick_period_ns: u32) {
        self.cycle_start_ns += completed_interval_ns(self.active_reload, systick_tick_period_ns);
        self.active_reload = new_active_reload;
    }

    /// Convert a verified counter snapshot into an acquisition timestamp.
    ///
    /// The caller must ensure the counter and this clock came from the same
    /// SysTick interval. SysTick counts downward from `active_reload`; a pending
    /// wrap must therefore be rejected before this function is called.
    ///
    /// Args:
    ///   current_count: Current SysTick down-counter value, in `tick`.
    ///   systick_tick_period_ns: Exact SysTick counter period in `ns/tick`.
    ///
    /// Returns:
    ///   Board timestamp for the counter observation, in `ns`.
    pub fn timestamp_ns(&self, current_count: u32, systick_tick_period_ns: u32) -> i64 {
        debug_assert!(current_count <= self.active_reload);
        self.cycle_start_ns
            + i64::from(self.active_reload - current_count) * i64::from(systick_tick_period_ns)
    }
}

/// Return the duration represented by one completed SysTick interval.
///
/// SysTick reload values are encoded as `ticks - 1`, so the zero reload value
/// still represents one timer tick.
///
/// Args:
///   active_reload: Applied SysTick reload, in counter ticks minus one.
///   systick_tick_period_ns: Exact SysTick counter period in `ns/tick`.
///
/// Returns:
///   Completed interval duration in `ns`.
pub fn completed_interval_ns(active_reload: u32, systick_tick_period_ns: u32) -> i64 {
    i64::from(active_reload + 1) * i64::from(systick_tick_period_ns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum RolloverPoint {
        None,
        PendingBeforeCopy,
        PendingBetweenClockAndCounter,
        PendingAfterCounter,
    }

    #[derive(Clone, Copy)]
    struct ModeledAttempt {
        clock: AcquisitionClock,
        current_count: u32,
        rollover: RolloverPoint,
    }

    fn modeled_capture(
        attempts: [ModeledAttempt; MAX_CAPTURE_ATTEMPTS],
        last_sample_time_ns: i64,
        nominal_sample_period_ns: i64,
        tick_period_ns: u32,
    ) -> (i64, usize) {
        for (index, attempt) in attempts.into_iter().enumerate() {
            match attempt.rollover {
                RolloverPoint::None => {
                    return (
                        attempt
                            .clock
                            .timestamp_ns(attempt.current_count, tick_period_ns),
                        index + 1,
                    );
                }
                RolloverPoint::PendingBeforeCopy
                | RolloverPoint::PendingBetweenClockAndCounter
                | RolloverPoint::PendingAfterCounter => {}
            }
        }
        (
            last_sample_time_ns + nominal_sample_period_ns,
            MAX_CAPTURE_ATTEMPTS,
        )
    }

    #[test]
    fn reload_and_counter_arithmetic_includes_ticks_minus_one_convention() {
        let mut clock = AcquisitionClock::new(7_000_000, 49_999);
        assert_eq!(completed_interval_ns(clock.active_reload, 20), 1_000_000);
        assert_eq!(clock.timestamp_ns(39_999, 20), 7_200_000);

        clock.advance(9_999, 20);
        assert_eq!(clock.cycle_start_ns, 8_000_000);
        assert_eq!(clock.active_reload, 9_999);
        assert_eq!(clock.timestamp_ns(4_999, 20), 8_100_000);
    }

    #[test]
    fn pending_before_or_during_capture_retries_with_the_new_interval() {
        let old = AcquisitionClock::new(1_000_000, 49_999);
        let new = AcquisitionClock::new(2_000_000, 49_999);

        for rollover in [
            RolloverPoint::PendingBeforeCopy,
            RolloverPoint::PendingBetweenClockAndCounter,
            RolloverPoint::PendingAfterCounter,
        ] {
            let (timestamp, attempts) = modeled_capture(
                [
                    ModeledAttempt {
                        clock: old,
                        current_count: 0,
                        rollover,
                    },
                    ModeledAttempt {
                        clock: new,
                        current_count: 39_999,
                        rollover: RolloverPoint::None,
                    },
                ],
                1_900_000,
                30_300,
                20,
            );
            assert_eq!(timestamp, 2_200_000);
            assert_eq!(attempts, 2);
        }
    }

    #[test]
    fn two_pending_attempts_use_monotonic_nominal_period_fallback() {
        let clock = AcquisitionClock::new(1_000_000, 49_999);
        let (timestamp, attempts) = modeled_capture(
            [
                ModeledAttempt {
                    clock,
                    current_count: 0,
                    rollover: RolloverPoint::PendingBeforeCopy,
                },
                ModeledAttempt {
                    clock,
                    current_count: 0,
                    rollover: RolloverPoint::PendingAfterCounter,
                },
            ],
            1_234_500,
            30_300,
            20,
        );
        assert_eq!(timestamp, 1_264_800);
        assert_eq!(attempts, MAX_CAPTURE_ATTEMPTS);
    }
}
