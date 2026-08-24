use deimos_shared::peripherals::deimos_daq_rev8::{
    AdcFilterBank, AdcFilterBankState, AdcFractionalDelayFilter, AdcFractionalDelayFilterState,
    ENCODER_CHANNEL_COUNT, adc_filter_bank, adc_fractional_delay_filter_bank,
    timing::unwrap_u16_delta,
};
use nb::block;
use stm32h7xx_hal::{adc, gpio::Pin, stm32::*};

use crate::board::{
    ADC_CHANNEL_COUNT, ADC_IIR_CUTOFF_TO_REPORT_RATE, ADC_OVERSAMPLE_TARGET_HZ, VREF,
};

/// One coherent, filtered ADC group owned by the operating SysTick sampler.
#[derive(Clone, Copy, Debug)]
pub(in crate::board) struct AdcSampleGroup {
    /// Filtered ADC output voltages in `V` with shape `(ADC_CHANNEL_COUNT,)`.
    pub values: [f32; ADC_CHANNEL_COUNT],
    /// Board time immediately before the first ADC conversion group, in `ns`.
    pub sample_time_ns: i64,
}

/// Latest sampled inputs consumed later in the same SysTick invocation.
///
/// SysTick has exclusive access to the sampler throughout Operating, so these
/// ordinary fields need no atomic handoff or double buffer.
#[derive(Clone, Copy, Debug)]
pub(in crate::board) struct SampledInputs {
    /// Coherent filtered ADC values and their acquisition timestamp.
    pub adc: AdcSampleGroup,
    /// Unwrapped quadrature-encoder counts in timer order TIM1, TIM8, TIM4, TIM3.
    pub encoder: [i64; ENCODER_CHANNEL_COUNT],
}

/// Unwrap a `u16` hardware counter into an ordinary `i64` accumulator.
///
/// The compiled sampling cutovers and supported edge-rate assertion keep each
/// real change strictly below half of the explicit `2^16` counter modulus.
/// Starting from zero, the first signed wrap from `i64::MAX` to `i64::MIN`
/// occurs after `2^63` net positive counts. The complete accumulator bit
/// pattern repeats after `2^64` counts. Wrapping arithmetic avoids retaining
/// an unreachable checked-add panic path in the sampling IRQ.
pub struct Unroller {
    prev: u16,
    acc: i64,
}

impl Unroller {
    fn new(v: u16) -> Self {
        Self {
            prev: v,
            acc: i64::from(v),
        }
    }

    fn reset(&mut self, v: u16) {
        self.prev = v;
        self.acc = i64::from(v);
    }

    fn update(&mut self, v: u16) -> i64 {
        let change = unwrap_u16_delta(self.prev, v);
        self.prev = v;
        self.acc = self.acc.wrapping_add(i64::from(change));
        self.acc
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
    pub adc_scalings: [f32; ADC_CHANNEL_COUNT],
    pub adc_filter: AdcFilterBank,
    pub adc_filter_state: AdcFilterBankState,
    pub adc_filters_fractional_delay: [AdcFractionalDelayFilter; ADC_CHANNEL_COUNT],
    pub adc_filters_fractional_delay_states: [AdcFractionalDelayFilterState; ADC_CHANNEL_COUNT],
    sampled_inputs: SampledInputs,

    // Quadrature encoders
    pub encoder0: (TIM1, Unroller),
    pub encoder1: (TIM8, Unroller),
    pub encoder2: (TIM4, Unroller),
    pub encoder3: (TIM3, Unroller),
}

impl Sampler {
    pub fn new(
        adc1: adc::Adc<ADC1, adc::Enabled>,
        adc2: adc::Adc<ADC2, adc::Enabled>,
        adc3: adc::Adc<ADC3, adc::Enabled>,
        adc_pins: AdcPins,
        encoder0: TIM1,
        encoder1: TIM8,
        encoder2: TIM4,
        encoder3: TIM3,
    ) -> Self {
        //
        // Set up ADC scalings, the shared-coefficient filter, and sample state.
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
        // Operating entry replaces these coefficients before the first sample.
        let cutoff_ratio = ADC_IIR_CUTOFF_TO_REPORT_RATE / 2.0;
        let adc_filter = adc_filter_bank(cutoff_ratio).unwrap();
        let adc_filter_state = adc_filter.reset_state();
        let sampled_inputs = SampledInputs {
            adc: AdcSampleGroup {
                values: [0.0_f32; ADC_CHANNEL_COUNT],
                sample_time_ns: 0,
            },
            encoder: [0; ENCODER_CHANNEL_COUNT],
        };

        // Fractional delay filters for synthetic simultaneous sampling
        //   Each ADC group starts as soon as the previous one is done.
        let adc_filters_fractional_delay =
            adc_fractional_delay_filter_bank(ADC_OVERSAMPLE_TARGET_HZ as f64).unwrap();
        let adc_filters_fractional_delay_states =
            [adc_filters_fractional_delay[0].reset_state(); ADC_CHANNEL_COUNT];

        Self {
            adc1,
            adc2,
            adc3,
            adc_pins,
            adc_scalings,
            adc_filter,
            adc_filter_state,
            adc_filters_fractional_delay,
            adc_filters_fractional_delay_states,
            sampled_inputs,
            encoder0: (encoder0, Unroller::new(0)),
            encoder1: (encoder1, Unroller::new(0)),
            encoder2: (encoder2, Unroller::new(0)),
            encoder3: (encoder3, Unroller::new(0)),
        }
    }

    /// Replace the ADC IIR coefficient set for every channel.
    /// This runs during Operating entry before the SysTick sampler is enabled.
    /// Also clears the encoder counters.
    pub fn update_cutoff(&mut self, cutoff_ratio: f64) {
        self.adc_filter = adc_filter_bank(cutoff_ratio).unwrap();
        // Seed directly from the most recent sample group; IEEE-754
        // exceptional values propagate without a sanitizing branch.
        self.adc_filter
            .set_steady_state(&mut self.adc_filter_state, self.sampled_inputs.adc.values);

        self.reset_counter_inputs();
    }

    /// Reset sampled encoder state at an ownership change.
    fn reset_counter_inputs(&mut self) {
        self.encoder0.0.cnt.reset();
        self.encoder1.0.cnt.reset();
        self.encoder2.0.cnt.reset();
        self.encoder3.0.cnt.reset();
        self.encoder0.1.reset(0);
        self.encoder1.1.reset(0);
        self.encoder2.1.reset(0);
        self.encoder3.1.reset(0);
    }

    /// Configure filters for synchronous sampling owned by the publishing IRQ.
    ///
    /// Args:
    ///   sample_rate_hz: ADC-group rate in `sample/s`.
    ///   iir_cutoff_ratio: ADC IIR cutoff divided by the ADC-group rate, or
    ///     `None` when the direct path will not step the IIR.
    pub fn configure_synchronous(&mut self, sample_rate_hz: f64, iir_cutoff_ratio: Option<f64>) {
        if let Some(cutoff_ratio) = iir_cutoff_ratio {
            self.update_cutoff(cutoff_ratio);
        } else {
            self.reset_counter_inputs();
        }
        self.configure_fractional_delay(sample_rate_hz);
    }

    /// Rebuild fractional-delay taps and initialize their histories steadily.
    fn configure_fractional_delay(&mut self, sample_rate_hz: f64) {
        self.adc_filters_fractional_delay =
            adc_fractional_delay_filter_bank(sample_rate_hz).unwrap();
        for (index, state) in self
            .adc_filters_fractional_delay_states
            .iter_mut()
            .enumerate()
        {
            let value = self.sampled_inputs.adc.values[index];
            *state = AdcFractionalDelayFilterState::filled([value]);
        }
    }

    /// Acquire one ADC group from the cycle IRQ and apply fractional delay + IIR.
    ///
    /// Args:
    ///   sample_time_ns: Acquisition-start board timestamp in `ns`.
    #[inline(never)]
    #[unsafe(link_section = ".itcm.sample")]
    pub fn sample_synchronous_iir(&mut self, sample_time_ns: i64) {
        self.sample_and_update::<true>(sample_time_ns);
    }

    /// Acquire one ADC group from the cycle IRQ and apply fractional delay only.
    ///
    /// Args:
    ///   sample_time_ns: Acquisition-start board timestamp in `ns`.
    #[inline(never)]
    #[unsafe(link_section = ".itcm.sample")]
    pub fn sample_synchronous_fractional_only(&mut self, sample_time_ns: i64) {
        self.sample_and_update::<false>(sample_time_ns);
    }

    /// Prime all ADC filter histories from one real acquisition group.
    ///
    /// This runs once before the operating SysTick scope begins. Initializing
    /// both the fractional-delay FIR and low-pass IIR to the measured voltages
    /// prevents a long startup transient, particularly in the downstream board-
    /// temperature calculation.
    ///
    /// Args:
    ///   sample_time_ns: Acquisition-start board timestamp in `ns`.
    pub fn prime_synchronous(&mut self, sample_time_ns: i64) {
        let raw_samples = self.acquire_raw_adc_values();
        for index in 0..self.sampled_inputs.adc.values.len() {
            let sample = raw_samples[index] as f32 * self.adc_scalings[index];
            self.adc_filters_fractional_delay_states[index] =
                AdcFractionalDelayFilterState::filled([sample]);
            self.sampled_inputs.adc.values[index] = sample;
        }
        self.adc_filter
            .set_steady_state(&mut self.adc_filter_state, self.sampled_inputs.adc.values);
        self.update_sampled_inputs(sample_time_ns);
    }

    /// Execute topology-independent acquisition, alignment, and state update.
    ///
    /// Args:
    ///   sample_time_ns: Acquisition-start board timestamp in `ns`.
    #[inline(always)]
    fn sample_and_update<const APPLY_IIR: bool>(&mut self, sample_time_ns: i64) {
        let raw_samples = self.acquire_raw_adc_values();
        for index in 0..self.sampled_inputs.adc.values.len() {
            let scaled_sample = raw_samples[index] as f32 * self.adc_scalings[index];
            let delayed_sample = self.adc_filters_fractional_delay[index].step(
                &mut self.adc_filters_fractional_delay_states[index],
                [scaled_sample],
            )[0];
            self.sampled_inputs.adc.values[index] = delayed_sample;
        }
        // `APPLY_IIR` is a const generic, so monomorphization removes this
        // branch and the unused path; it has no runtime cost.
        if APPLY_IIR {
            self.sampled_inputs.adc.values = self
                .adc_filter
                .step(&mut self.adc_filter_state, self.sampled_inputs.adc.values);
        }
        self.update_sampled_inputs(sample_time_ns);
    }

    /// Acquire one raw ADC group without stepping digital filters.
    ///
    /// Returns:
    ///   Raw ADC codes in `count` with shape `(ADC_CHANNEL_COUNT,)` and channel
    ///   order `ain0..ain12, ain15..ain19`.
    #[inline(always)]
    fn acquire_raw_adc_values(&mut self) -> [u32; ADC_CHANNEL_COUNT] {
        let mut b = [0_u32; ADC_CHANNEL_COUNT];

        // Sample
        // UNWRAP: Every read below follows `start_conversion` on the same ADC,
        // satisfying the HAL's internal `current_channel.expect` invariant.
        // UNWRAP: `block!` only waits out `WouldBlock`; the terminal error type
        // is `Infallible`, so the outer `Result::unwrap` cannot panic.
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
        b[15 - 2] = block!(self.adc2.read_sample()).unwrap();
        b[3] = block!(self.adc3.read_sample()).unwrap();

        self.adc1.start_conversion(&mut self.adc_pins.ain16);
        self.adc2.start_conversion(&mut self.adc_pins.ain17);
        self.adc3.start_conversion(&mut self.adc_pins.ain4);
        b[16 - 2] = block!(self.adc1.read_sample()).unwrap();
        b[17 - 2] = block!(self.adc2.read_sample()).unwrap();
        b[4] = block!(self.adc3.read_sample()).unwrap();

        self.adc1.start_conversion(&mut self.adc_pins.ain18);
        self.adc3.start_conversion(&mut self.adc_pins.ain5);
        b[18 - 2] = block!(self.adc1.read_sample()).unwrap();
        b[5] = block!(self.adc3.read_sample()).unwrap();

        self.adc1.start_conversion(&mut self.adc_pins.ain19);
        self.adc3.start_conversion(&mut self.adc_pins.ain6);
        b[19 - 2] = block!(self.adc1.read_sample()).unwrap();
        b[6] = block!(self.adc3.read_sample()).unwrap();

        self.adc3.start_conversion(&mut self.adc_pins.ain7);
        b[7] = block!(self.adc3.read_sample()).unwrap();

        b
    }

    /// Record the ADC timestamp and capture encoder inputs at its cadence.
    ///
    /// Args:
    ///   sample_time_ns: Acquisition-start board timestamp in `ns`.
    #[inline(always)]
    fn update_sampled_inputs(&mut self, sample_time_ns: i64) {
        self.sampled_inputs.adc.sample_time_ns = sample_time_ns;

        // Get latest timer input readings, unwrapping the 16-bit counters.
        let encoder0: u16 = self.encoder0.0.cnt.read().cnt().bits().into();
        let encoder1: u16 = self.encoder1.0.cnt.read().cnt().bits().into();
        let encoder2: u16 = self.encoder2.0.cnt.read().cnt().bits().into();
        let encoder3: u16 = self.encoder3.0.cnt.read().cnt().bits().into();
        self.sampled_inputs.encoder[0] = self.encoder0.1.update(encoder0);
        self.sampled_inputs.encoder[1] = self.encoder1.1.update(encoder1);
        self.sampled_inputs.encoder[2] = self.encoder2.1.update(encoder2);
        self.sampled_inputs.encoder[3] = self.encoder3.1.update(encoder3);
    }

    /// Borrow the group most recently completed by this sampler.
    ///
    /// Returns:
    ///   Coherent ADC and encoder inputs. Operating consumes this
    ///   reference before the sampler can be stepped again.
    #[inline(always)]
    pub(in crate::board) fn sampled_inputs(&self) -> &SampledInputs {
        &self.sampled_inputs
    }
}
