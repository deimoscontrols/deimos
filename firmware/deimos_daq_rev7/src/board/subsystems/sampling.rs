use deimos_shared::peripherals::deimos_daq_rev7::{
    AdcFilter, AdcFilterState, AdcFractionalDelayFilter, AdcFractionalDelayFilterState,
    FREQUENCY_CHANNEL_COUNT, FREQUENCY_INPUT_VALID_TIMEOUT_NS, adc_filter_bank,
    adc_fractional_delay_filter_bank, timing::unwrap_u16_delta,
};
use nb::block;
use stm32h7xx_hal::{adc, gpio::Pin, rcc::CoreClocks, stm32::*, timer::GetClk};

use crate::board::{
    ADC_CHANNEL_COUNT, ADC_OVERSAMPLE_MIN_SAMPLES_PER_CYCLE, ADC_OVERSAMPLE_TARGET_HZ, VREF,
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
    /// Unwrapped quadrature-encoder count.
    pub encoder: i64,
    /// Unwrapped pulse count.
    pub pulse_counter: i64,
    /// Measured input frequencies in `Hz` with shape `(FREQUENCY_CHANNEL_COUNT,)`.
    pub frequency_meas: [f32; FREQUENCY_CHANNEL_COUNT],
}

/// Unwrap a `u16` hardware counter into an ordinary `i64` accumulator.
///
/// The compiled sampling cutovers and supported edge-rate assertion keep each
/// real change strictly below half of the explicit `2^16` counter modulus.
/// The accumulator itself wraps after `2^64` counts rather than retaining an
/// unreachable checked-add panic path in the sampling IRQ.
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

/// Latest valid period-capture result for one frequency input.
///
/// A nonzero capture refreshes the retained frequency. Polls without a new
/// capture leave it unchanged until its age reaches
/// [`FREQUENCY_INPUT_VALID_TIMEOUT_NS`], at which point it returns to zero.
///
/// References:
///   \[1\] STMicroelectronics, *RM0433 STM32H742, STM32H743/753 and
///   STM32H750 Value Line advanced Arm-based 32-bit MCUs*, general-purpose
///   timer status and capture/compare register descriptions.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrequencyInputState {
    /// Board time when the latest nonzero period was observed, in `ns`.
    last_valid_capture_time_ns: i64,
    /// Most recently calculated valid frequency in `Hz`, or zero after timeout.
    latest_frequency_hz: f32,
}

impl FrequencyInputState {
    /// Discard the retained capture at an operating ownership change.
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Consume one optional newly captured period and apply the validity timeout.
    ///
    /// Args:
    ///   captured_period: Newly captured edge period in `timer tick`, or `None`
    ///     when `CC1IF` did not indicate a new capture.
    ///   sample_time_ns: Board time at which the capture register is observed,
    ///     in `ns`.
    ///   frequency_scaling: Timer tick rate in `tick/s`.
    ///
    /// Returns:
    ///   Latest valid frequency in `Hz`, or zero before the first valid capture
    ///   and after the capture-validity timeout.
    #[inline(always)]
    fn update(
        &mut self,
        captured_period: Option<u16>,
        sample_time_ns: i64,
        frequency_scaling: f32,
    ) -> f32 {
        if let Some(period) = captured_period
            && period != 0
        {
            self.latest_frequency_hz = frequency_scaling / period as f32;
            self.last_valid_capture_time_ns = sample_time_ns;
        }

        if sample_time_ns.wrapping_sub(self.last_valid_capture_time_ns)
            >= FREQUENCY_INPUT_VALID_TIMEOUT_NS
        {
            self.latest_frequency_hz = 0.0;
        }
        self.latest_frequency_hz
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
    pub adc_filters: [AdcFilter; ADC_CHANNEL_COUNT],
    pub adc_filter_states: [AdcFilterState; ADC_CHANNEL_COUNT],
    pub adc_filters_fractional_delay: [AdcFractionalDelayFilter; ADC_CHANNEL_COUNT],
    pub adc_filters_fractional_delay_states: [AdcFractionalDelayFilterState; ADC_CHANNEL_COUNT],
    sampled_inputs: SampledInputs,

    // Counter and frequency
    pub encoder: (TIM1, Unroller),
    pub pulse_counter: (TIM8, Unroller),
    pub pwmi0: (TIM4, FrequencyInputState),
    pub pwmi1: (TIM15, FrequencyInputState),
    pub frequency_scaling: f32,
}

impl Sampler {
    pub fn new(
        clocks: &CoreClocks,
        adc1: adc::Adc<ADC1, adc::Enabled>,
        adc2: adc::Adc<ADC2, adc::Enabled>,
        adc3: adc::Adc<ADC3, adc::Enabled>,
        adc_pins: AdcPins,
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
        // Operating entry replaces these coefficients before the first sample.
        let cutoff_ratio = 1.0 / f64::from(ADC_OVERSAMPLE_MIN_SAMPLES_PER_CYCLE);
        let adc_filters = adc_filter_bank(cutoff_ratio).unwrap();
        let adc_filter_states = [adc_filters[0].reset_state(); ADC_CHANNEL_COUNT];
        let sampled_inputs = SampledInputs {
            adc: AdcSampleGroup {
                values: [0.0_f32; ADC_CHANNEL_COUNT],
                sample_time_ns: 0,
            },
            encoder: 0,
            pulse_counter: 0,
            frequency_meas: [0.0; FREQUENCY_CHANNEL_COUNT],
        };

        // Fractional delay filters for synthetic simultaneous sampling
        //   Each ADC group starts as soon as the previous one is done.
        let adc_filters_fractional_delay =
            adc_fractional_delay_filter_bank(ADC_OVERSAMPLE_TARGET_HZ as f64).unwrap();
        let adc_filters_fractional_delay_states =
            [adc_filters_fractional_delay[0].reset_state(); ADC_CHANNEL_COUNT];

        //
        // Set up frequency input adc_scalings
        //
        let t4clk_hz = TIM4::get_clk(clocks).unwrap().to_Hz();
        let t4psc = pwmi0.psc.read().psc().bits() + 1;
        let frequency_scaling = ((t4clk_hz as f64) / (t4psc as f64)) as f32;

        Self {
            adc1,
            adc2,
            adc3,
            adc_pins,
            adc_scalings,
            adc_filters,
            adc_filter_states,
            adc_filters_fractional_delay,
            adc_filters_fractional_delay_states,
            sampled_inputs,
            encoder: (encoder, Unroller::new(0)),
            pulse_counter: (pulse_counter, Unroller::new(0)),
            pwmi0: (pwmi0, FrequencyInputState::default()),
            pwmi1: (pwmi1, FrequencyInputState::default()),
            frequency_scaling,
        }
    }

    /// Replace the ADC IIR filters with ones at a new cutoff ratio.
    /// This runs during Operating entry before the SysTick sampler is enabled.
    /// Also clears the encoder and pulse counter.
    pub fn update_cutoff(&mut self, cutoff_ratio: f64) {
        let filter_bank = adc_filter_bank(cutoff_ratio).unwrap();

        self.adc_filters
            .iter_mut()
            .zip(self.adc_filter_states.iter_mut())
            .enumerate()
            .for_each(|(i, (filter, state))| {
                // Get the most recent existing sample to initialize the filter
                // and, if it is in an error state, reset it to zero.
                let mut init_val = self.sampled_inputs.adc.values[i];
                if !init_val.is_finite() {
                    init_val = 0.0;
                }

                *filter = filter_bank[i];
                filter.set_steady_state(state, [init_val]);
            });

        self.reset_counter_inputs();
    }

    /// Reset sampled counter and frequency-input state at an ownership change.
    fn reset_counter_inputs(&mut self) {
        self.pwmi0.0.cnt.reset();
        self.pwmi0.0.ccr1().reset();
        self.pwmi0.0.sr.reset();
        self.pwmi1.0.cnt.reset();
        self.pwmi1.0.ccr1().reset();
        self.pwmi1.0.sr.reset();
        self.pwmi0.1.reset();
        self.pwmi1.1.reset();

        self.encoder.0.cnt.reset();
        self.pulse_counter.0.cnt.reset(); // Does not use a compare-and-capture
        self.encoder.1.reset(0);
        self.pulse_counter.1.reset(0);
    }

    /// Configure filters for synchronous sampling owned by the publishing IRQ.
    ///
    /// Args:
    ///   sample_rate_hz: ADC-group rate in `sample/s`.
    ///   iir_cutoff_ratio: Publishing-rate cutoff divided by the ADC-group rate,
    ///     or `None` when the direct path will not step the IIR.
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
            let value = if self.sampled_inputs.adc.values[index].is_finite() {
                self.sampled_inputs.adc.values[index]
            } else {
                0.0
            };
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
            self.adc_filters[index].set_steady_state(&mut self.adc_filter_states[index], [sample]);
            self.sampled_inputs.adc.values[index] = sample;
        }
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
            self.sampled_inputs.adc.values[index] = if APPLY_IIR {
                self.adc_filters[index].step(&mut self.adc_filter_states[index], [delayed_sample])
                    [0]
            } else {
                delayed_sample
            };
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

    /// Record the ADC timestamp and capture counter/frequency inputs at its cadence.
    ///
    /// Args:
    ///   sample_time_ns: Acquisition-start board timestamp in `ns`.
    #[inline(always)]
    fn update_sampled_inputs(&mut self, sample_time_ns: i64) {
        self.sampled_inputs.adc.sample_time_ns = sample_time_ns;

        // Get latest timer input readings, unwrapping integer counts
        let encoder_val: u16 = self.encoder.0.cnt.read().cnt().bits().into();
        self.sampled_inputs.encoder = self.encoder.1.update(encoder_val);

        let pulse_counter_val: u16 = self.pulse_counter.0.cnt.read().cnt().bits().into();
        self.sampled_inputs.pulse_counter = self.pulse_counter.1.update(pulse_counter_val);

        // In input-capture mode, reading CCR1 after observing CC1IF consumes
        // that flag. Polling the flag first prevents an empty or previously
        // consumed register value from replacing the latest valid frequency.
        let fcnt0 = self
            .pwmi0
            .0
            .sr
            .read()
            .cc1if()
            .bit_is_set()
            .then(|| self.pwmi0.0.ccr1().read().ccr().bits());
        self.sampled_inputs.frequency_meas[0] =
            self.pwmi0
                .1
                .update(fcnt0, sample_time_ns, self.frequency_scaling);

        let fcnt1 = self
            .pwmi1
            .0
            .sr
            .read()
            .cc1if()
            .bit_is_set()
            .then(|| self.pwmi1.0.ccr1().read().ccr().bits());
        self.sampled_inputs.frequency_meas[1] =
            self.pwmi1
                .1
                .update(fcnt1, sample_time_ns, self.frequency_scaling);
    }

    /// Borrow the group most recently completed by this sampler.
    ///
    /// Returns:
    ///   Coherent ADC, counter, and frequency inputs. Operating consumes this
    ///   reference before the sampler can be stepped again.
    #[inline(always)]
    pub(in crate::board) fn sampled_inputs(&self) -> &SampledInputs {
        &self.sampled_inputs
    }
}
