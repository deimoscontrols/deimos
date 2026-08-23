use stm32h7xx_hal::{
    gpio::{Output, Pin},
    prelude::*,
    pwm::{Alignment, ComplementaryDisabled, ComplementaryImpossible, Pwm},
    stm32::*,
    time::Hertz,
    traits::DacOut,
};

use deimos_shared::peripherals::deimos_daq_rev7::{
    DAC_CHANNEL_COUNT,
    calc::{LinearCalibration, dac_code},
};

pub struct Outputs {
    pub pwm0: Pwm<TIM3, 1, ComplementaryImpossible>,
    pub pwm1: Pwm<TIM12, 0, ComplementaryImpossible>,
    pub pwm2: Pwm<TIM16, 0, ComplementaryDisabled>,
    pub pwm3: Pwm<TIM17, 0, ComplementaryDisabled>,
    /// Timer input clocks cached during startup, in `cycle/s` with shape `(4,)`.
    pub pwm_clock_hz: [Hertz; 4],
    pub dac1: stm32h7xx_hal::dac::C1<DAC, stm32h7xx_hal::dac::Enabled>,
    pub dac2: stm32h7xx_hal::dac::C2<DAC, stm32h7xx_hal::dac::Enabled>,
    pub do0: Pin<'D', 2, Output>,
    pub do1: Pin<'D', 3, Output>,
    pub do2: Pin<'D', 4, Output>,
    pub do3: Pin<'D', 5, Output>,
}

// Set PWM frequency and duty cycle
pub fn set_outputs(
    outputs: &mut Outputs,
    pwm_duty_frac: &[f32; 4],
    pwm_freq_hz: &[u32; 4],
    dac_v: &[f32; DAC_CHANNEL_COUNT],
    dac_cals: &[LinearCalibration; DAC_CHANNEL_COUNT],
    gpio: u8,
) {
    {
        let i = 0;
        let clk = outputs.pwm_clock_hz[i];
        let pwm = &mut outputs.pwm0;

        let duty = pwm_duty_frac[i];
        let freq = pwm_freq_hz[i].max(2).Hz(); // 0Hz causes breakage

        // Set freq
        let tim = unsafe { &*TIM3::ptr() };
        let (period, prescale) = calculate_frequency_16bit(clk, freq, Alignment::Left);
        // Write prescale
        tim.psc.write(|w| w.psc().bits(prescale as u16));
        // Write period
        tim.arr.write(|w| w.arr().bits(period as u16));

        // Set duty cycle
        let duty = (duty * (pwm.get_max_duty() as f32)) as u16;
        pwm.set_duty(duty);
    }

    {
        let i = 1;
        let clk = outputs.pwm_clock_hz[i];
        let pwm = &mut outputs.pwm1;

        let duty = pwm_duty_frac[i];
        let freq = pwm_freq_hz[i].max(2).Hz(); // 0Hz causes breakage

        // Set freq
        let tim = unsafe { &*TIM12::ptr() };
        let (period, prescale) = calculate_frequency_16bit(clk, freq, Alignment::Left);
        // Write prescale
        tim.psc.write(|w| w.psc().bits(prescale as u16));
        // Write period
        tim.arr.write(|w| w.arr().bits(period as u16));

        // Set duty cycle
        let duty = (duty * (pwm.get_max_duty() as f32)) as u16;
        pwm.set_duty(duty);
    }

    {
        let i = 2;
        let clk = outputs.pwm_clock_hz[i];
        let pwm = &mut outputs.pwm2;

        let duty = pwm_duty_frac[i];
        let freq = pwm_freq_hz[i].max(2).Hz(); // 0Hz causes breakage

        // Set freq
        let tim = unsafe { &*TIM16::ptr() };
        let (period, prescale) = calculate_frequency_16bit(clk, freq, Alignment::Left);
        // Write prescale
        tim.psc.write(|w| w.psc().bits(prescale as u16));
        // Write period
        tim.arr.write(|w| w.arr().bits(period as u16));

        // Set duty cycle
        let duty = (duty * (pwm.get_max_duty() as f32)) as u16;
        pwm.set_duty(duty);
    }

    {
        let i = 3;
        let clk = outputs.pwm_clock_hz[i];
        let pwm = &mut outputs.pwm3;

        let duty = pwm_duty_frac[i];
        let freq = pwm_freq_hz[i].max(2).Hz(); // 0Hz causes breakage

        // Set freq
        let tim = unsafe { &*TIM17::ptr() };
        let (period, prescale) = calculate_frequency_16bit(clk, freq, Alignment::Left);
        // Write prescale
        tim.psc.write(|w| w.psc().bits(prescale as u16));
        // Write period
        tim.arr.write(|w| w.arr().bits(period as u16));

        // Set duty cycle
        let duty = (duty * (pwm.get_max_duty() as f32)) as u16;
        pwm.set_duty(duty);
    }

    outputs.dac1.set_value(dac_code(dac_v[0], &dac_cals[0]));
    outputs.dac2.set_value(dac_code(dac_v[1], &dac_cals[1]));

    if gpio & (1 << 0) != 0 {
        outputs.do0.set_high();
    } else {
        outputs.do0.set_low();
    }

    if gpio & (1 << 1) != 0 {
        outputs.do1.set_high();
    } else {
        outputs.do1.set_low();
    }

    if gpio & (1 << 2) != 0 {
        outputs.do2.set_high();
    } else {
        outputs.do2.set_low();
    }

    if gpio & (1 << 3) != 0 {
        outputs.do3.set_high();
    } else {
        outputs.do3.set_low();
    }
}

// Period and prescaler calculator for 32-bit timers
// Returns (arr, psc)
fn calculate_frequency_32bit(base_freq: Hertz, freq: Hertz, alignment: Alignment) -> (u32, u16) {
    let divisor = if let Alignment::Center = alignment {
        freq.raw() * 2
    } else {
        freq.raw()
    };

    // Round to the nearest period
    let arr = (base_freq.raw() + (divisor >> 1)) / divisor - 1;

    (arr, 0)
}

// Period and prescaler calculator for 16-bit timers
// Returns (arr, psc)
// Returns as (u32, u16) to be compatible but arr will always be a valid u16
fn calculate_frequency_16bit(base_freq: Hertz, freq: Hertz, alignment: Alignment) -> (u32, u16) {
    let ideal_period = calculate_frequency_32bit(base_freq, freq, alignment).0 + 1;

    // Division factor is (PSC + 1)
    let prescale = (ideal_period - 1) / (1 << 16);

    // This will always fit in a 16-bit value because u32::MAX / (1 << 16) fits in a 16 bit

    // Round to the nearest period
    let period = (ideal_period + (prescale >> 1)) / (prescale + 1) - 1;

    // Dividing a `u32` period into 16-bit prescaler-sized chunks bounds both
    // results to `u16`; retain `period` as `u32` only for the HAL API.

    (period, prescale as u16)
}
