//! Scheduling, timestamp, and counter arithmetic for rev7 synchronous acquisition.
//!
//! The firmware uses SysTick as both the communication-cycle boundary and the
//! counter within each sample interval. This module contains the
//! target-independent arithmetic used to distribute timer ticks, timestamp a
//! sample, and unwrap hardware counters. Sampling-policy selection lives in
//! the sibling `filters` module.
//!
//! References:
//!   \[1\] Arm, *Cortex-M7 Devices Generic User Guide*, DDI 0489D, 2018,
//!   sections 4.4 and 4.5.

use super::MAX_CYCLE_TIMING_CORRECTION_DIVISOR;

/// Bounded quotient/remainder distributor for one publishing interval.
///
/// Each call to [`Self::next_ticks`] returns one positive sample interval. A
/// complete schedule contains `sample_count` intervals whose sum is
/// `total_ticks`; individual intervals differ by at most one SysTick tick. The
/// remainder accumulator spreads longer intervals across the cycle without an
/// array sized by the potentially large low-rate sample count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UniformIntervalScheduler {
    base_ticks: u32,
    remainder: u32,
    sample_count: u32,
    error: u32,
}

impl UniformIntervalScheduler {
    /// Construct a scheduler for one corrected publishing interval.
    ///
    /// Args:
    ///   total_ticks: Applied publishing interval in SysTick `tick/cycle`.
    ///   sample_count: Number of ADC groups in the cycle, in `sample/cycle`.
    pub const fn new(total_ticks: u32, sample_count: u32) -> Self {
        // Normalize invalid inputs instead of retaining panic branches in the
        // publishing loop. Valid sampling policies are unchanged: they always
        // request at least one timer tick for every acquisition.
        let sample_count = if sample_count == 0 { 1 } else { sample_count };
        let total_ticks = if total_ticks < sample_count {
            sample_count
        } else {
            total_ticks
        };
        Self {
            base_ticks: total_ticks / sample_count,
            remainder: total_ticks % sample_count,
            sample_count,
            error: 0,
        }
    }

    /// Return the next interval in constant time.
    ///
    /// Returns:
    ///   Positive interval length in `tick/sample`.
    pub fn next_ticks(&mut self) -> u32 {
        let space_before_wrap = self.sample_count - self.error;
        if self.remainder >= space_before_wrap {
            self.error = self.remainder - space_before_wrap;
            self.base_ticks + 1
        } else {
            self.error += self.remainder;
            self.base_ticks
        }
    }
}

/// Unwrap one 16-bit hardware counter delta into its shortest signed change.
///
/// The caller's rate contract must keep the real change strictly below half of
/// the `2^16` modulus; an exact half-modulus change is inherently ambiguous.
pub const fn unwrap_u16_delta(previous: u16, latest: u16) -> i32 {
    const MODULUS: i32 = 1_i32 << 16;
    const HALF_MODULUS: i32 = MODULUS / 2;
    let difference = latest as i32 - previous as i32;
    if difference > HALF_MODULUS {
        difference - MODULUS
    } else if difference < -HALF_MODULUS {
        difference + MODULUS
    } else {
        difference
    }
}

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
        self.advance_intervals(1, new_active_reload, systick_tick_period_ns);
    }

    /// Advance across one or more equal completed intervals.
    ///
    /// This handles coalesced SysTick exceptions after a long-running handler.
    /// LOAD is retained whenever a deadline is missed, so every coalesced
    /// interval uses `active_reload`.
    ///
    /// Args:
    ///   interval_count: Number of completed intervals to advance.
    ///   new_active_reload: Reload applied to the interval currently counting
    ///     down, in counter ticks minus one.
    ///   systick_tick_period_ns: Exact SysTick counter period in `ns/tick`.
    pub fn advance_intervals(
        &mut self,
        interval_count: u32,
        new_active_reload: u32,
        systick_tick_period_ns: u32,
    ) {
        self.cycle_start_ns += i64::from(interval_count)
            * completed_interval_ns(self.active_reload, systick_tick_period_ns);
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
        // SysTick hardware constrains VAL to `0..=LOAD`. Make the subtraction's
        // release behavior explicit without evaluating an assertion in the IRQ.
        self.cycle_start_ns
            + i64::from(self.active_reload.wrapping_sub(current_count))
                * i64::from(systick_tick_period_ns)
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
    (i64::from(active_reload) + 1) * i64::from(systick_tick_period_ns)
}

/// Saturating-combine and clamp one requested cycle-timing correction.
///
/// Args:
///   dt_ns: Nominal publishing-cycle duration in `ns`.
///   period_delta_ns: Persistent period correction in `ns`.
///   phase_delta_ns: One-cycle phase correction in `ns`.
///
/// Returns:
///   Combined correction in `ns`, limited to `+/-10%` of `dt_ns`.
pub fn bounded_cycle_timing_correction_ns(
    dt_ns: u32,
    period_delta_ns: i64,
    phase_delta_ns: i64,
) -> i64 {
    let limit_ns = i64::from(dt_ns / MAX_CYCLE_TIMING_CORRECTION_DIVISOR);
    period_delta_ns
        .saturating_add(phase_delta_ns)
        .clamp(-limit_ns, limit_ns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_and_counter_arithmetic_includes_ticks_minus_one_convention() {
        let mut clock = AcquisitionClock::new(7_000_000, 49_999);
        assert_eq!(completed_interval_ns(clock.active_reload, 20), 1_000_000);
        assert_eq!(clock.timestamp_ns(39_999, 20), 7_200_000);

        clock.advance(9_999, 20);
        assert_eq!(clock.cycle_start_ns, 8_000_000);
        assert_eq!(clock.active_reload, 9_999);
        assert_eq!(clock.timestamp_ns(4_999, 20), 8_100_000);

        clock.advance_intervals(3, 19_999, 20);
        assert_eq!(clock.cycle_start_ns, 8_600_000);
        assert_eq!(clock.active_reload, 19_999);
    }

    #[test]
    fn interval_scheduler_preserves_ticks_and_is_uniform() {
        for sample_count in 1..=25 {
            for total_ticks in sample_count..=100 {
                let mut scheduler = UniformIntervalScheduler::new(total_ticks, sample_count);
                let mut sum = 0;
                let mut minimum = u32::MAX;
                let mut maximum = 0;
                for _ in 0..sample_count {
                    let ticks = scheduler.next_ticks();
                    sum += ticks;
                    minimum = minimum.min(ticks);
                    maximum = maximum.max(ticks);
                }
                assert_eq!(sum, total_ticks);
                assert!(minimum > 0);
                assert!(maximum - minimum <= 1);
            }
        }

        let mut low_rate = UniformIntervalScheduler::new(12_500_000, 2_250);
        let mut sum = 0_u32;
        for _ in 0..2_250 {
            sum += low_rate.next_ticks();
        }
        assert_eq!(sum, 12_500_000);

        // Invalid external inputs normalize to a one-tick minimum instead of
        // retaining a panic path in firmware callers.
        let mut zero_count = UniformIntervalScheduler::new(0, 0);
        assert_eq!(zero_count.next_ticks(), 1);
        let mut too_few_ticks = UniformIntervalScheduler::new(2, 4);
        assert_eq!((0..4).map(|_| too_few_ticks.next_ticks()).sum::<u32>(), 4);
    }

    #[test]
    fn counter_unrolling_uses_power_of_two_moduli_in_both_directions() {
        assert_eq!(unwrap_u16_delta(u16::MAX, 0), 1);
        assert_eq!(unwrap_u16_delta(0, u16::MAX), -1);
        assert_eq!(unwrap_u16_delta(100, 125), 25);
    }
}
