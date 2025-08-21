use core::sync::atomic::Ordering;

use nb::block;
use stm32h7xx_hal::{
    adc,
    gpio::Pin,
    rcc::CoreClocks,
    stm32::*,
    timer::{GetClk, Timer},
};

use flaw::{
    butter1, butter2, generated::butter, polynomial_fractional_delay, MedianFilter, SisoFirFilter,
    SisoIirFilter,
};

use crate::board::{
    ACCUMULATED_SAMPLING_TIME_NS, ADC_CUTOFF_RATIO, ADC_SAMPLES, ADC_SAMPLE_FREQ_HZ,
    COUNTER_SAMPLES, COUNTER_WRAPS, FREQ_SAMPLES, NEW_ADC_CUTOFF, VREF,
};

/// Above this size of change, 16-bit counters are assumed to have wrapped.
/// If the real count rate is between a half and full u16::MAX per update,
/// we have no way of telling the difference between that and wrapping,
/// and the output will alias.
/// With an unrolling samplerate of 40kHz, a 2.6GHz counting rate can be accommodated,
/// which is much higher than the timer peripheral capability.
const WRAPPING_LIM_U16: i32 = (u16::MAX / 2) as i32;

const WRAPPING_LIM_I32: i64 = i32::MAX as i64; // Max value is half-span for signed int

/// Undo wrapping of U16 values and store in an I32 (which is also allowed to wrap).
/// This allows integer wrapping events to happen rarely enough to be handled in a larger
/// data type at the application level; sending wrapping u16 to the application will often result
/// in ambiguous aliased wrapping events under normal conditions.
pub struct Unroller {
    prev: i32,
    acc: i32,

    /// How many times the accumulator has wrapped
    /// and in what direction
    acc_wraps: i32,
}

impl Unroller {
    fn new(v: u16) -> Self {
        Self {
            prev: v as i32,
            acc: v as i32,
            acc_wraps: 0,
        }
    }

    fn reset(&mut self, v: u16) {
        let v = v as i32;
        self.prev = v;
        self.acc = v;
        self.acc_wraps = 0;
    }

    fn update(&mut self, v: u16) -> (i32, i32) {
        let latest = v as i32;
        let diff = latest.wrapping_sub(self.prev);

        // Unwrap u16 to i32
        let change: i32;

        if diff > WRAPPING_LIM_U16 {
            change = diff.wrapping_sub(u16::MAX as i32);
        } else if diff < -WRAPPING_LIM_U16 {
            change = diff.wrapping_add(u16::MAX as i32);
        } else {
            change = diff;
        }

        let latest_unwrapped = self.acc.wrapping_add(change);
        let latest_did_wrap = self.acc.checked_add(change).is_none();

        // Unwrap i32 to i64 if a wrap occurred,
        // and record which direction the wrap went
        let mut latest_acc_wrap = 0;
        if latest_did_wrap {
            let diff2 = (latest_unwrapped as i64).saturating_sub(self.prev as i64);
            if diff2 > WRAPPING_LIM_I32 {
                latest_acc_wrap = 1;
            } else if diff2 < -WRAPPING_LIM_I32 {
                latest_acc_wrap = -1;
            } else {
                latest_acc_wrap = 0;
            }
        }

        // Store
        self.prev = latest;
        self.acc = latest_unwrapped;
        //    If we actually manage to wrap the wrap counter, something has gone terribly wrong -
        //    this should take decades even at the timer's max rate
        //    In this case, panic and reboot is better than risking
        //    driving a controller to an unreasonable condition.
        //    Applications designed to run for several decades should have some
        //    reconnection logic to handle radiation upsets, power outages, network downtime, etc
        self.acc_wraps = self.acc_wraps.checked_add(latest_acc_wrap).unwrap();

        (latest_unwrapped, latest_acc_wrap)
    }
}

pub struct AdcPins {
    pub ain0: Pin<'F', 3>,
    pub ain1: Pin<'F', 4>,
    pub ain2: Pin<'F', 5>,
    pub ain3: Pin<'F', 6>,
    pub ain4: Pin<'F', 7>,
    pub ain5: Pin<'F', 8>,
    pub ain6: Pin<'F', 9>,
    pub ain7: Pin<'F', 10>,

    pub ain8: Pin<'C', 0>,
    pub ain9: Pin<'C', 2>,
    pub ain10: Pin<'C', 3>,

    pub ain11: Pin<'A', 0>,
    pub ain12: Pin<'A', 3>,
    // pub ain13: Pin<'A', 4>,
    // pub ain14: Pin<'A', 5>,
    pub ain15: Pin<'A', 6>,

    pub ain16: Pin<'B', 0>,
    pub ain17: Pin<'B', 1>,

    pub ain18: Pin<'F', 11>,
    pub ain19: Pin<'F', 12>,
}

pub struct Sampler {
    // Analog
    pub adc1: adc::Adc<ADC1, adc::Enabled>,
    pub adc2: adc::Adc<ADC2, adc::Enabled>,
    pub adc3: adc::Adc<ADC3, adc::Enabled>,
    pub adc_pins: AdcPins,
    pub adc_scalings: [f32; 18],
    pub adc_filters: [SisoIirFilter<2>; 18],
    pub adc_filters_low_rate: [SisoIirFilter<1>; 18],
    pub adc_filters_fractional_delay: [SisoFirFilter<3, f32>; 18],
    pub adc_filter_cutoff_ratio: f64,
    pub adc_values: [f32; 18],

    // Timing
    pub timer: Timer<TIM2>,
    pub tick_period_ns: u32,

    // Counter and frequency
    pub encoder: (TIM1, Unroller),
    pub pulse_counter: (TIM8, Unroller),
    pub pwmi0: (TIM4, MedianFilter<u16, 3>, MedianFilter<u16, 3>),
    pub pwmi1: (TIM15, MedianFilter<u16, 3>),
    pub frequency_scaling: f32,
}

impl Sampler {
    pub fn new(
        clocks: &CoreClocks,
        adc1: adc::Adc<ADC1, adc::Enabled>,
        adc2: adc::Adc<ADC2, adc::Enabled>,
        adc3: adc::Adc<ADC3, adc::Enabled>,
        adc_pins: AdcPins,
        timer: Timer<TIM2>,

        encoder: TIM1,
        pulse_counter: TIM8,
        pwmi0: TIM4,
        pwmi1: TIM15,
    ) -> Self {
        //
        // Set up ADC adc_scalings, adc_filters, and buffer
        //

        // Precalculate adc_scalings
        let adc1_scaling = (VREF as f64 / adc1.slope() as f64) as f32;
        let adc2_scaling = (VREF as f64 / adc2.slope() as f64) as f32;
        let adc3_scaling = (VREF as f64 / adc3.slope() as f64) as f32;

        let adc_scalings = [
            // 0-7
            adc3_scaling,
            adc3_scaling,
            adc3_scaling,
            adc3_scaling,
            adc3_scaling,
            adc3_scaling,
            adc3_scaling,
            adc3_scaling,
            adc1_scaling, // 8
            adc2_scaling, // 9
            adc1_scaling, // 10
            adc1_scaling, // 11
            adc2_scaling, // 12
            // adc2_scaling, // 13
            // adc1_scaling, // 14
            adc2_scaling, // 15
            adc1_scaling, // 16
            adc2_scaling, // 17
            adc1_scaling, // 18
            adc1_scaling, // 19
        ];

        // Low-pass filters
        let cutoff_ratio = ADC_CUTOFF_RATIO.load(Ordering::Relaxed) as f64;
        let adc_filters = [butter2(cutoff_ratio).unwrap(); 18];
        let adc_filters_low_rate = [butter1(cutoff_ratio).unwrap(); 18];
        let adc_values = [0.0_f32; 18];

        // Fractional delay filters for synthetic simultaneous sampling

        // Timing components
        let internal_sample_period = 1.0 / ADC_SAMPLE_FREQ_HZ as f64; // [s]
        let adc_clock_speed = 50e6; // [Hz]
        let adc_clock_period = 1.0 / adc_clock_speed; // [s]
        let adc_sample_hold_cycles = 16.5; // [dimensionless]
        let adc_sample_hold = adc_sample_hold_cycles * adc_clock_period; // [s]
        let adc_conversion_time = 7.5 * adc_clock_period; // [s] RM0433 25.4.13

        // Calculate fractional delay needed for each channel to align with the first sample group
        //   Each group starts as soon as the previous one is done
        let delay_per_group = adc_sample_hold + adc_conversion_time; // [s]

        let groups = (
            [8, 9, 0],
            [10, 12, 1],
            // [11, 13, 2],  // ain13,14 consumed for DAC
            // [14, 15, 3],
            [11, 2],
            [15, 3],
            [16, 17, 4],
            [18, 5],
            [19, 6],
            [7],
        );
        let mut delays = [0_f64; 20];

        let apply_delay = |delays: &mut [f64], group: &[usize], i: usize| {
            let delay = i as f64 * delay_per_group;
            for j in group {
                delays[*j] = delay;
            }
        };

        apply_delay(&mut delays, &groups.0, 0);
        apply_delay(&mut delays, &groups.1, 1);
        apply_delay(&mut delays, &groups.2, 2);
        apply_delay(&mut delays, &groups.3, 3);
        apply_delay(&mut delays, &groups.4, 4);
        apply_delay(&mut delays, &groups.5, 5);
        apply_delay(&mut delays, &groups.6, 6);
        apply_delay(&mut delays, &groups.7, 7);

        let mut adc_filters_fractional_delay: [SisoFirFilter<3, f32>; 18] =
            [SisoFirFilter::<3, f32>::new(&[0.0, 0.0, 0.0]); 18];
        for (i, filter) in adc_filters_fractional_delay.iter_mut().enumerate() {
            let delay_frac = (delays[i] / internal_sample_period) as f32;
            *filter = polynomial_fractional_delay(delay_frac);
        }

        //
        // Set up frequency input adc_scalings
        //
        let t4clk_hz = TIM4::get_clk(clocks).unwrap().to_Hz();
        let t4psc = pwmi0.psc.read().psc().bits() + 1;
        let frequency_scaling = ((t4clk_hz as f64) / (t4psc as f64)) as f32;

        //
        // Figure out how long each timer tick is in nanoseconds
        //
        let t2clk_hz = TIM2::get_clk(clocks).unwrap().to_Hz();
        let t2psc = timer.inner().psc.read().psc().bits() + 1;
        let tick_period_ns = ((t2psc as f64) / (t2clk_hz as f64) * 1e9) as u32; // [ns] ADC timer tick period

        Self {
            adc1,
            adc2,
            adc3,
            adc_pins,
            adc_scalings,
            adc_filters,
            adc_filters_low_rate,
            adc_filters_fractional_delay,
            adc_filter_cutoff_ratio: cutoff_ratio,
            adc_values,
            timer,
            tick_period_ns,
            encoder: (encoder, Unroller::new(0)),
            pulse_counter: (pulse_counter, Unroller::new(0)),
            pwmi0: (
                pwmi0,
                MedianFilter::<u16, 3>::new(0),
                MedianFilter::<u16, 3>::new(0),
            ),
            pwmi1: (pwmi1, MedianFilter::<u16, 3>::new(0)),
            frequency_scaling,
        }
    }

    /// Replace the adc_filters with ones at a new cutoff ratio.
    /// This should be done with the sample timer paused to avoid interruptions.
    /// Also clears the encoder and pulse counter.
    pub fn update_cutoff(&mut self, cutoff_ratio: f64) {
        // Store un-clipped cutoff so that we can check it later to decide which filter to use
        self.adc_filter_cutoff_ratio = cutoff_ratio;

        if cutoff_ratio >= butter::butter2::MIN_CUTOFF_RATIO {
            // Clip to table bounds for init
            let cutoff_ratio = cutoff_ratio
                .min(butter::butter2::MAX_CUTOFF_RATIO)
                .max(butter::butter2::MIN_CUTOFF_RATIO);

            // Prototype of the new filter, initialized to zero internal state.
            // Copying the filter is much faster than constructing a new one.
            let filter_proto = butter2(cutoff_ratio).unwrap();

            self.adc_filters
                .iter_mut()
                .enumerate()
                .for_each(|(i, old_filter)| {
                    // Get the most recent existing sample to initialize the filter
                    // and, if it is in an error state, reset it to zero
                    let mut init_val = ADC_SAMPLES[i].load(Ordering::Relaxed);
                    if !init_val.is_finite() {
                        init_val = 0.0;
                    }

                    // Construct and initialize the new filter
                    let mut new_filter = filter_proto;
                    new_filter.initialize(init_val);

                    // Swap the old and new adc_filters
                    *old_filter = new_filter;
                });
        } else {
            // Clip to table bounds for init
            let cutoff_ratio = cutoff_ratio
                .min(butter::butter1::MAX_CUTOFF_RATIO)
                .max(butter::butter1::MIN_CUTOFF_RATIO);

            // Prototype of the new filter, initialized to zero internal state.
            // Copying the filter is much faster than constructing a new one.
            let filter_proto = butter1(cutoff_ratio).unwrap();

            self.adc_filters_low_rate
                .iter_mut()
                .enumerate()
                .for_each(|(i, old_filter)| {
                    // Get the most recent existing sample to initialize the filter
                    // and, if it is in an error state, reset it to zero
                    let mut init_val = ADC_SAMPLES[i].load(Ordering::Relaxed);
                    if !init_val.is_finite() {
                        init_val = 0.0;
                    }

                    // Construct and initialize the new filter
                    let mut new_filter = filter_proto;
                    new_filter.initialize(init_val);

                    // Swap the old and new adc_filters
                    *old_filter = new_filter;
                });
        }

        // Reset counters and encoder
        self.pwmi0.0.cnt.reset();
        self.pwmi0.0.ccr1().reset();
        self.pwmi1.0.cnt.reset();
        self.pwmi1.0.ccr1().reset();
        self.pwmi0.1 = MedianFilter::<u16, 3>::new(0);
        self.pwmi1.1 = MedianFilter::<u16, 3>::new(0);

        self.encoder.0.cnt.reset();
        self.pulse_counter.0.cnt.reset(); // Does not use a compare-and-capture
        self.encoder.1.reset(0);
        self.pulse_counter.1.reset(0);
    }

    pub fn sample(&mut self) {
        self.timer.clear_irq();
        let start_time = self.timer.counter();

        if NEW_ADC_CUTOFF.load(Ordering::Relaxed) {
            // Make sure we don't run out of time and trigger another cycle
            self.timer.pause();

            // Load new cutoff
            let new_cutoff = ADC_CUTOFF_RATIO.load(Ordering::Relaxed) as f64;

            // Build new interpolated filter
            self.update_cutoff(new_cutoff);

            // Mark the cutoff update as complete only after the new adc_values are fully propagated
            // in case this interrupt handler gets preempted in the middle of a transaction
            NEW_ADC_CUTOFF.store(false, Ordering::Relaxed);

            // Start the ADC sample clock again
            self.timer.resume();

            // Skip this sample to avoid breaking timing guarantee
            return;
        }

        let mut b = [0_u32; 20];

        // Sample
        self.adc1.start_conversion(&mut self.adc_pins.ain8);
        self.adc2.start_conversion(&mut self.adc_pins.ain9);
        self.adc3.start_conversion(&mut self.adc_pins.ain0);
        b[8] = block!(self.adc1.read_sample()).unwrap();
        b[9] = block!(self.adc2.read_sample()).unwrap();
        b[0] = block!(self.adc3.read_sample()).unwrap();

        self.adc1.start_conversion(&mut self.adc_pins.ain10);
        self.adc2.start_conversion(&mut self.adc_pins.ain12);
        self.adc3.start_conversion(&mut self.adc_pins.ain1);
        b[10] = block!(self.adc1.read_sample()).unwrap();
        b[12] = block!(self.adc2.read_sample()).unwrap();
        b[1] = block!(self.adc3.read_sample()).unwrap();

        self.adc1.start_conversion(&mut self.adc_pins.ain11);
        // self.adc2.start_conversion(&mut self.adc_pins.ain13);
        self.adc3.start_conversion(&mut self.adc_pins.ain2);
        b[11] = block!(self.adc1.read_sample()).unwrap();
        // b[13] = block!(self.adc2.read_sample()).unwrap();
        b[2] = block!(self.adc3.read_sample()).unwrap();

        // self.adc1.start_conversion(&mut self.adc_pins.ain14);
        self.adc2.start_conversion(&mut self.adc_pins.ain15);
        self.adc3.start_conversion(&mut self.adc_pins.ain3);
        // b[14] = block!(self.adc1.read_sample()).unwrap();
        b[15] = block!(self.adc2.read_sample()).unwrap();
        b[3] = block!(self.adc3.read_sample()).unwrap();

        self.adc1.start_conversion(&mut self.adc_pins.ain16);
        self.adc2.start_conversion(&mut self.adc_pins.ain17);
        self.adc3.start_conversion(&mut self.adc_pins.ain4);
        b[16] = block!(self.adc1.read_sample()).unwrap();
        b[17] = block!(self.adc2.read_sample()).unwrap();
        b[4] = block!(self.adc3.read_sample()).unwrap();

        self.adc1.start_conversion(&mut self.adc_pins.ain18);
        self.adc3.start_conversion(&mut self.adc_pins.ain5);
        b[18] = block!(self.adc1.read_sample()).unwrap();
        b[5] = block!(self.adc3.read_sample()).unwrap();

        self.adc1.start_conversion(&mut self.adc_pins.ain19);
        self.adc3.start_conversion(&mut self.adc_pins.ain6);
        b[19] = block!(self.adc1.read_sample()).unwrap();
        b[6] = block!(self.adc3.read_sample()).unwrap();

        self.adc3.start_conversion(&mut self.adc_pins.ain7);
        b[7] = block!(self.adc3.read_sample()).unwrap();

        // Apply calibration, scaling, and filter
        // Use more stable order1 filter for lower output data rate if our
        // selected cutoff ratio is below the table bounds for the order2 filter.
        if self.adc_filter_cutoff_ratio >= butter::butter2::MIN_CUTOFF_RATIO {
            // Nominal filter for higher reporting rate (above about 40Hz)
            self.adc_filters_fractional_delay
                .iter_mut()
                .zip(self.adc_filters.iter_mut())
                .enumerate()
                .zip(self.adc_scalings.iter())
                .for_each(|((i, (f1, f2)), s)| {
                    self.adc_values[i] = f2.update(f1.update(b[i] as f32 * s));
                });
        } else {
            // Alternate filter for low data rate
            self.adc_filters_fractional_delay
                .iter_mut()
                .zip(self.adc_filters_low_rate.iter_mut())
                .enumerate()
                .zip(self.adc_scalings.iter())
                .for_each(|((i, (f1, f2)), s)| {
                    self.adc_values[i] = f2.update(f1.update(b[i] as f32 * s));
                });
        }

        // Send measurements to shared storage
        for i in 0..ADC_SAMPLES.len() {
            ADC_SAMPLES[i].store(self.adc_values[i], Ordering::Relaxed);
        }

        // Get latest timer input readings, unwrapping integer counts
        let encoder_val: u16 = self.encoder.0.cnt.read().cnt().bits().into();
        let (encoder_unwrapped, encoder_wraps) = self.encoder.1.update(encoder_val);
        COUNTER_SAMPLES[0].store(encoder_unwrapped, Ordering::Relaxed);
        COUNTER_WRAPS[0].store(encoder_wraps, Ordering::Relaxed);

        let pulse_counter_val: u16 = self.pulse_counter.0.cnt.read().cnt().bits().into();
        let (pulse_counter_unwrapped, pulse_counter_wraps) =
            self.pulse_counter.1.update(pulse_counter_val);
        COUNTER_SAMPLES[1].store(pulse_counter_unwrapped, Ordering::Relaxed);
        COUNTER_WRAPS[1].store(pulse_counter_wraps, Ordering::Relaxed);

        // PWM input readings use a median filter on the raw counter value
        // in order to filter out the semi-random values produced when the incoming signal
        // is at a lower frequency than what the timer can track

        // FREQ0
        let pwmi0_freq_val;
        let fcnt0_raw = self.pwmi0.0.ccr1().read().ccr().bits();
        let fcnt0 = self.pwmi0.1.update(fcnt0_raw);
        if fcnt0 < 1 {
            pwmi0_freq_val = 0.0;
        } else {
            pwmi0_freq_val = self.frequency_scaling / fcnt0 as f32;
        }
        FREQ_SAMPLES[0].store(pwmi0_freq_val, Ordering::Relaxed);

        // FREQ1
        let pwmi1_val;
        let fcnt1_raw = self.pwmi1.0.ccr1().read().ccr().bits();
        let fcnt1 = self.pwmi1.1.update(fcnt1_raw);
        if fcnt1 < 1 {
            pwmi1_val = 0.0;
        } else {
            pwmi1_val = self.frequency_scaling / fcnt1 as f32;
        }
        FREQ_SAMPLES[1].store(pwmi1_val, Ordering::Relaxed);

        // Log how much time was spent sampling
        let end_time = self.timer.counter();
        let scope_time = (end_time - start_time) * self.tick_period_ns;
        ACCUMULATED_SAMPLING_TIME_NS.fetch_add(scope_time, Ordering::Relaxed);
    }
}
