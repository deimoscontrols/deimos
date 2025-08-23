use stm32h7xx_hal::{
    prelude::*,
    pwm::{Alignment, ComplementaryDisabled, ComplementaryImpossible, Pwm},
    rcc::CoreClocks,
    stm32::*,
    time::Hertz,
    timer::GetClk, traits::DacOut,
};

const DAC_SCALING: f32 = 4095.0 / 2.5;

pub struct Outputs {
    pub pwm0: Pwm<TIM3, 1, ComplementaryImpossible>,
    pub pwm1: Pwm<TIM12, 0, ComplementaryImpossible>,
    pub pwm2: Pwm<TIM16, 0, ComplementaryDisabled>,
    pub pwm3: Pwm<TIM17, 0, ComplementaryDisabled>,
    pub dac1: stm32h7xx_hal::dac::C1<DAC, stm32h7xx_hal::dac::Enabled>,
    pub dac2: stm32h7xx_hal::dac::C2<DAC, stm32h7xx_hal::dac::Enabled>,
}

// Set PWM frequency and duty cycle
pub fn set_outputs(
    outputs: &mut Outputs,
    pwm_duty_frac: &[f32; 4],
    pwm_freq_hz: &[u32; 4],
    dac_v: &[f32; 2],
    clocks: &CoreClocks,
) {
    {
        let i = 0;
        let pwm = &mut outputs.pwm0;

        let duty = pwm_duty_frac[i];
        let freq = pwm_freq_hz[i].max(2).Hz(); // 0Hz causes breakage

        // Set freq
        let tim = unsafe { &*TIM3::ptr() };
        let clk = TIM3::get_clk(clocks).unwrap();
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
        let pwm = &mut outputs.pwm1;

        let duty = pwm_duty_frac[i];
        let freq = pwm_freq_hz[i].max(2).Hz(); // 0Hz causes breakage

        // Set freq
        let tim = unsafe { &*TIM12::ptr() };
        let clk = TIM12::get_clk(clocks).unwrap();
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
        let pwm = &mut outputs.pwm2;

        let duty = pwm_duty_frac[i];
        let freq = pwm_freq_hz[i].max(2).Hz(); // 0Hz causes breakage

        // Set freq
        let tim = unsafe { &*TIM16::ptr() };
        let clk = TIM16::get_clk(clocks).unwrap();
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
        let pwm = &mut outputs.pwm3;

        let duty = pwm_duty_frac[i];
        let freq = pwm_freq_hz[i].max(2).Hz(); // 0Hz causes breakage

        // Set freq
        let tim = unsafe { &*TIM17::ptr() };
        let clk = TIM17::get_clk(clocks).unwrap();
        let (period, prescale) = calculate_frequency_16bit(clk, freq, Alignment::Left);
        // Write prescale
        tim.psc.write(|w| w.psc().bits(prescale as u16));
        // Write period
        tim.arr.write(|w| w.arr().bits(period as u16));

        // Set duty cycle
        let duty = (duty * (pwm.get_max_duty() as f32)) as u16;
        pwm.set_duty(duty);
    }

    outputs.dac1.set_value((dac_v[0] * DAC_SCALING) as u16);
    outputs.dac2.set_value((dac_v[1] * DAC_SCALING) as u16);
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

    // It should be impossible to fail these asserts
    assert!(period <= 0xFFFF);
    assert!(prescale <= 0xFFFF);

    (period, prescale as u16)
}
