use core::sync::atomic::Ordering;

use nb::block;
use stm32h7xx_hal::{
    adc,
    timer::{GetClk, Timer},
    gpio::Pin,
    rcc::CoreClocks,
    stm32::*,
};

use flaw::{
    butter2,
    MedianFilter,
    generated::butter::butter2::{MAX_CUTOFF_RATIO, MIN_CUTOFF_RATIO},
    SisoIirFilter,
};

use super::super::{
    ADC_CUTOFF_RATIO, ADC_SAMPLES, COUNTER_SAMPLES, COUNTER_WRAPS, FREQ_SAMPLES, NEW_ADC_CUTOFF,
    VREF,
};

/// Above this size of change, 16-bit counters are assumed to have wrapped.
/// If the real count rate is between a half and full u16::MAX per update,
/// we have no way of telling the difference between that and wrapping,
/// and the output will alias.
/// With an unrolling samplerate of 40kHz, a 2.6GHz counting rate can be accommodated,
/// which is much higher than the timer peripheral capability.
const WRAPPING_LIM_U16: i32 = (u16::MAX / 2) as i32;

const WRAPPING_LIM_I32: i64 = i32::MAX as i64;  // Max value is half-span for signed int

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
    pub ain13: Pin<'A', 4>,
    pub ain14: Pin<'A', 5>,
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
    pub adc_scalings: [f32; 20],
    pub adc_filters: [SisoIirFilter<2>; 20],
    pub adc_values: [f32; 20],

    // Timing
    pub timer: Timer<TIM2>,

    // Counter and frequency
    pub encoder: (TIM1, Unroller),
    pub pulse_counter: (TIM8, Unroller),
    pub frequency_inp0: (TIM4, MedianFilter<u16, 3>),
    pub frequency_inp1: (TIM15, MedianFilter<u16, 3>),
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
        frequency_inp0: TIM4,
        frequency_inp1: TIM15,
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
            adc2_scaling, // 13
            adc1_scaling, // 14
            adc2_scaling, // 15
            adc1_scaling, // 16
            adc2_scaling, // 17
            adc1_scaling, // 18
            adc1_scaling, // 19
        ];

        let cutoff_ratio = ADC_CUTOFF_RATIO.load(Ordering::Relaxed) as f64;
        let adc_filters = [butter2(cutoff_ratio).unwrap(); 20];
        let adc_values = [0.0_f32; 20];

        //
        // Set up frequency input adc_scalings
        //
        let tclk_hz = TIM4::get_clk(clocks).unwrap().to_Hz();
        let tpsc = frequency_inp0.psc.read().psc().bits() + 1;
        let frequency_scaling = ((tclk_hz as f64) / (tpsc as f64)) as f32;

        Self {
            adc1,
            adc2,
            adc3,
            adc_pins,
            adc_scalings,
            adc_filters,
            adc_values,
            timer,
            encoder: (encoder, Unroller::new(0)),
            pulse_counter: (pulse_counter, Unroller::new(0)),
            frequency_inp0: (frequency_inp0, MedianFilter::<u16, 3>::new(0)),
            frequency_inp1: (frequency_inp1, MedianFilter::<u16, 3>::new(0)),
            frequency_scaling,
        }
    }

    /// Replace the adc_filters with ones at a new cutoff ratio.
    /// This should be done with the sample timer paused to avoid interruptions.
    /// Also clears the encoder and pulse counter.
    pub fn update_cutoff(&mut self, cutoff_ratio: f64) {
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
                let _ = core::mem::replace(old_filter, new_filter);
            });

        // Reset counters and encoder
        self.frequency_inp0.0.cnt.reset();
        self.frequency_inp0.0.ccr1().reset();
        self.frequency_inp1.0.cnt.reset();
        self.frequency_inp1.0.ccr1().reset();
        self.frequency_inp0.1 = MedianFilter::<u16, 3>::new(0);
        self.frequency_inp1.1 = MedianFilter::<u16, 3>::new(0);

        self.encoder.0.cnt.reset();
        self.pulse_counter.0.cnt.reset(); // Does not use a compare-and-capture
        self.encoder.1.reset(0);
        self.pulse_counter.1.reset(0);
    }

    pub fn sample(&mut self) {
        self.timer.clear_irq();

        if NEW_ADC_CUTOFF.load(Ordering::Relaxed) {
            // Make sure we don't run out of time and trigger another cycle
            self.timer.pause();

            // Clip to filter coeff table limits
            let mut new_cutoff = ADC_CUTOFF_RATIO.load(Ordering::Relaxed) as f64;
            new_cutoff = new_cutoff.min(MAX_CUTOFF_RATIO).max(MIN_CUTOFF_RATIO);

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
        self.adc2.start_conversion(&mut self.adc_pins.ain13);
        self.adc3.start_conversion(&mut self.adc_pins.ain2);
        b[11] = block!(self.adc1.read_sample()).unwrap();
        b[13] = block!(self.adc2.read_sample()).unwrap();
        b[2] = block!(self.adc3.read_sample()).unwrap();

        self.adc1.start_conversion(&mut self.adc_pins.ain14);
        self.adc2.start_conversion(&mut self.adc_pins.ain15);
        self.adc3.start_conversion(&mut self.adc_pins.ain3);
        b[14] = block!(self.adc1.read_sample()).unwrap();
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
        self.adc_filters
            .iter_mut()
            .enumerate()
            .zip(self.adc_scalings.iter())
            .for_each(|((i, f), s)| {
                self.adc_values[i] = f.update(b[i] as f32 * s);
            });

        // Send to shared storage
        for i in 0..ADC_SAMPLES.len() {
            ADC_SAMPLES[i].store(self.adc_values[i], Ordering::Relaxed);
        }

        // Get latest timer input readings,
        // unwrapping integer counts
        let encoder_val: u16 = self.encoder.0.cnt.read().cnt().bits().into();
        let (encoder_unwrapped, encoder_wraps) = self.encoder.1.update(encoder_val);
        COUNTER_SAMPLES[0].store(encoder_unwrapped, Ordering::Relaxed);
        COUNTER_WRAPS[0].store(encoder_wraps, Ordering::Relaxed);

        let pulse_counter_val: u16 = self.pulse_counter.0.cnt.read().cnt().bits().into();
        let (pulse_counter_unwrapped, pulse_counter_wraps) =
            self.pulse_counter.1.update(pulse_counter_val);
        COUNTER_SAMPLES[1].store(pulse_counter_unwrapped, Ordering::Relaxed);
        COUNTER_WRAPS[1].store(pulse_counter_wraps, Ordering::Relaxed);

        // Frequency input readings use a median filter on the raw counter value
        // in order to filter out the random values produced when the incoming signal
        // is at a lower frequency than what the timer can track
        let frequency_inp0_val;
        let fcnt0_raw = self.frequency_inp0.0.ccr1().read().ccr().bits();
        let fcnt0 = self.frequency_inp0.1.update(fcnt0_raw);
        if fcnt0 < 1 {
            frequency_inp0_val = 0.0;
        } else {
            frequency_inp0_val = self.frequency_scaling / fcnt0 as f32;
        }
        FREQ_SAMPLES[0].store(frequency_inp0_val, Ordering::Relaxed);

        let frequency_inp1_val;
        let fcnt1_raw = self.frequency_inp1.0.ccr1().read().ccr().bits();
        let fcnt1 = self.frequency_inp1.1.update(fcnt1_raw);
        if fcnt1 < 1 {
            frequency_inp1_val = 0.0;
        } else {
            frequency_inp1_val = self.frequency_scaling / fcnt1 as f32;
        }
        FREQ_SAMPLES[1].store(frequency_inp1_val, Ordering::Relaxed);
    }
}
