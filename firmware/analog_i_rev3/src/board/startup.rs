use super::*;

use core::time::Duration;

use smoltcp::time::Instant;
use stm32h7xx_hal::{
    adc,
    delay::Delay,
    ethernet,
    ethernet::PHY,
    gpio::{Output, Pin},
    rcc::rec::AdcClkSel,
    rcc::ResetEnable,
    stm32::*,
    timer::GetClk,
};

impl<'a> Board<'a> {
    /// Configure power, clocks, and peripherals
    pub fn new(store: &'a mut NetStorageStatic<'a>) -> (Self, Sampler) {
        // Power setup
        let dp = stm32::Peripherals::take().unwrap();
        let mut cp = stm32::CorePeripherals::take().unwrap();
        let pwr = dp.PWR.constrain().vos0(&dp.SYSCFG);
        let pwrcfg = pwr.freeze();
        //    Power up SRAM3, where ethernet buffers are stored
        dp.RCC.ahb2enr.modify(|_, w| w.sram3en().set_bit());

        // Clock setup
        let rcc = dp.RCC.constrain();
        let mut ccdr = rcc
            .use_hse(48.MHz()) // Set expected external clock freq
            .bypass_hse() // Use external clock signal directly
            .sys_ck(400.MHz())
            .hclk(200.MHz())
            .pll2_p_ck(24.MHz()) // Default adc_ker_ck_input
            .freeze(pwrcfg, &dp.SYSCFG);
        //    Make sure clock setup was exact
        assert_eq!(ccdr.clocks.sysclk().raw(), 400_000_000);
        assert_eq!(ccdr.clocks.hclk().raw(), 200_000_000);
        assert_eq!(ccdr.clocks.pclk1().raw(), 100_000_000);
        assert_eq!(ccdr.clocks.pclk2().raw(), 100_000_000);
        assert_eq!(ccdr.clocks.pclk4().raw(), 100_000_000);

        // Instruction caching
        cp.SCB.enable_icache();

        // Watchdog
        let mut watchdog = IndependentWatchdog::new(dp.IWDG);

        // ADCs are initialized before we start parting out the peripherals
        // because they need to use systick (or another timer) for a delay briefly
        let (adc1, adc2, adc3, systick) = {
            // Switch adc_ker_ck_input multiplexer to per_ck
            ccdr.peripheral.kernel_adc_clk_mux(AdcClkSel::Per);

            // Temporarily use SYSTICK as a delay provider, for initialization only
            let mut delay = Delay::new(cp.SYST, ccdr.clocks);

            // Sample-hold duration and sample frequency are tuned together
            // with a signal generator attached to analog inputs
            // to maximize overall samplerate without
            // * Introducing excessive DC crosstalk (at too low duration)
            //   * This is not an issue of settling time, but is improved by increasing ADC input capacitance
            //   * It is notably not resolved by oversampling and averaging, or by any amount of sample-hold time
            //     without adequate input capacitance
            //   * The problem is most likely caused by charge leakage between ADC mux stages,
            //     which can be outcompeted by a large enough input capacitor as long as that capacitor
            //     is not so large that it violates maximum slew rate and current output for the instrumentation
            //     amplifier that drives it, and as long as there is adequate time for the charge leakage to
            //     equilibrate with the capacitor
            // * Breaking cycle timing (too high [duration * rate])
            let sample_time = adc::AdcSampleTime::T_16;
            let resolution = adc::Resolution::SixteenBit;

            // Set oversampling and left-shifting for all ADCs
            // Shift left to make room for more averaging resolution and oversample
            // dp.ADC1.cfgr2.write(|w| w.lshift().bits(4).osvr().bits(8));
            // dp.ADC2.cfgr2.write(|w| w.lshift().bits(4).osvr().bits(8));
            // dp.ADC3.cfgr2.write(|w| w.lshift().bits(4).osvr().bits(8));

            // Set up ADC1 and ADC2
            let (mut adc1, mut adc2) = adc::adc12(
                dp.ADC1,
                dp.ADC2,
                50.MHz(),
                &mut delay,
                ccdr.peripheral.ADC12,
                &ccdr.clocks,
            );

            // Set up ADC3
            let mut adc3 = adc::Adc::adc3(
                dp.ADC3,
                50.MHz(),
                &mut delay,
                ccdr.peripheral.ADC3,
                &ccdr.clocks,
            );

            adc1.set_resolution(resolution);
            adc1.set_sample_time(sample_time);

            adc2.set_resolution(resolution);
            adc2.set_sample_time(sample_time);

            adc3.set_resolution(resolution);
            adc3.set_sample_time(sample_time);

            let adc1 = adc1.enable();
            let adc2 = adc2.enable();
            let adc3 = adc3.enable();

            (adc1, adc2, adc3, delay.free())
        };

        // Initialize GPIO
        let gpioa = dp.GPIOA.split(ccdr.peripheral.GPIOA);
        let gpiob = dp.GPIOB.split(ccdr.peripheral.GPIOB);
        let gpioc = dp.GPIOC.split(ccdr.peripheral.GPIOC);
        let gpioe = dp.GPIOE.split(ccdr.peripheral.GPIOE);
        let gpiof = dp.GPIOF.split(ccdr.peripheral.GPIOF);
        let gpiog = dp.GPIOG.split(ccdr.peripheral.GPIOG);
        let mut led0: Pin<'E', 5, Output> = gpioe.pe5.into_push_pull_output();
        let mut led1: Pin<'E', 4, Output> = gpioe.pe4.into_push_pull_output();
        let mut led2: Pin<'E', 3, Output> = gpioe.pe3.into_push_pull_output();
        let mut led3: Pin<'E', 2, Output> = gpioe.pe2.into_push_pull_output();
        led0.set_low();
        led1.set_low();
        led2.set_low();
        led3.set_low();

        let pwm2_pin = gpioc.pc7.into_alternate();
        let pwm5_pin = gpiob.pb14.into_alternate();
        let pwm6_pin = gpiob.pb8.into_alternate();
        let pwm7_pin = gpiob.pb9.into_alternate();

        //
        // Quadrature encoder input
        //

        // CC1S=’01’ (TIMx_CCMR1 register, TI1FP1 mapped on TI1).
        // • CC2S=’01’ (TIMx_CCMR2 register, TI1FP2 mapped on TI2).
        // • CC1P=’0’ and CC1NP=’0’ (TIMx_CCER register, TI1FP1 non-inverted, TI1FP1=TI1).
        // • CC2P=’0’ and CC2NP=’0’ (TIMx_CCER register, TI1FP2 non-inverted, TI1FP2= TI2).
        // • SMS=’011’ (TIMx_SMCR register, both inputs are active on both rising and falling
        // edges).
        // • CEN=’1’ (TIMx_CR1 register, Counter enabled)
        //
        // Counter value contains position index
        let _encoder0_pin0: Pin<'E', 9, stm32h7xx_hal::gpio::Alternate<1>> =
            gpioe.pe9.into_alternate();
        let _encoder0_pin1: Pin<'E', 11, stm32h7xx_hal::gpio::Alternate<1>> =
            gpioe.pe11.into_alternate();
        TIM1::get_clk(&ccdr.clocks).unwrap();
        ccdr.peripheral.TIM1.enable().reset();
        // External clock, gated mode and encoder mode can work only if the CEN bit has been previously set by software
        // TIM1_CH1 and TIM1_CH2 as input
        dp.TIM1.ccmr1_input().write(|w| w.cc1s().ti1()); // 01: CC1 channel is configured as input, IC1 is mapped on TI1
        dp.TIM1.ccmr1_input().write(|w| w.cc2s().ti2()); // 01: CC2 channel is configured as input, IC2 is mapped on TI2
        dp.TIM1.smcr.write(|w| w.sms().encoder_mode_3());
        dp.TIM1.cr1.write(|w| w.cen().enabled());
        let encoder = dp.TIM1;

        //
        // Pulse Counter
        //

        // TIM8 CH1 Pulse Counter
        // Read cnt for latest edge count (rising + falling)
        let _counter0_pin: Pin<'C', 6, stm32h7xx_hal::gpio::Alternate<3>> =
            gpioc.pc6.into_alternate();

        TIM8::get_clk(&ccdr.clocks).unwrap();
        ccdr.peripheral.TIM8.enable().reset();
        dp.TIM8.ccmr1_input().write(|w| w.cc1s().ti1()); // Select input
        dp.TIM8.ccmr1_input().write(|w| w.ic1f().no_filter()); // cycle persistence filter
        dp.TIM8
            .smcr
            .write(|w| w.ts().ti1f_ed().sms().ext_clock_mode()); // Trigger on input 1
        dp.TIM8.cr1.write(|w| w.cen().set_bit()); // Enable counter
        let pulse_counter = dp.TIM8;

        //
        // Frequency inputs
        //

        // TIM4 CH1
        // Read ccr1 for latest period
        // Using second CCR with the same channel input does not work; needs its own input channel
        let _pwmi1_pin: Pin<'B', 6, stm32h7xx_hal::gpio::Alternate<2>> = gpiob.pb6.into_alternate();
        TIM4::get_clk(&ccdr.clocks).unwrap();
        ccdr.peripheral.TIM4.enable().reset();
        dp.TIM4.psc.write(|w| w.psc().bits(7)); // 8x prescale -> about 400Hz min freq, 80ns res
        dp.TIM4.ccmr1_input().write(|w| w.cc1s().ti1()); // Compare/capture channel input
        dp.TIM4.smcr.write(|w| w.ts().ti1fp1().sms().reset_mode()); // Trigger input CH2, reset mode
        dp.TIM4.ccer.write(|w| w.cc1e().set_bit()); // Enable capture output
        dp.TIM4.cr1.write(|w| w.cen().enabled()); // Enable counter
        let frequency_inp0 = dp.TIM4;

        // TIM15 CH2
        // Read ccr1 for latest period
        // Using second CCR with the same channel input does not work; needs its own input channel
        let _pwmi0_pin: Pin<'E', 6, stm32h7xx_hal::gpio::Alternate<4>> = gpioe.pe6.into_alternate(); // TIM15 CH2
        TIM15::get_clk(&ccdr.clocks).unwrap();
        ccdr.peripheral.TIM15.enable().reset();
        // Both capture-compares on CH2 input
        dp.TIM15.psc.write(|w| w.psc().bits(7)); // 8x prescale -> about 400Hz min freq, 80ns res
        dp.TIM15.ccmr1_input().write(|w| w.cc1s().ti2()); // Compare/capture channel input
        unsafe {
            dp.TIM15
                .smcr
                .write(|w| w.ts_2_0().bits(0b110).sms().bits(0b100));
        } // Trigger input CH2, reset mode
        dp.TIM15.ccer.write(|w| w.cc1e().set_bit()); // Enable capture output
        dp.TIM15.cr1.write(|w| w.cen().enabled()); // Enable counter
        let frequency_inp1 = dp.TIM15;

        //
        // PWMs
        //

        dp.TIM3.cr1.write(|w| w.arpe().set_bit());
        let mut pwm2 = dp
            .TIM3
            .pwm(pwm2_pin, 100.kHz(), ccdr.peripheral.TIM3, &ccdr.clocks);
        pwm2.set_duty(0);
        pwm2.enable();

        dp.TIM12.cr1.write(|w| w.arpe().set_bit());
        let mut pwm5 = dp
            .TIM12
            .pwm(pwm5_pin, 100.kHz(), ccdr.peripheral.TIM12, &ccdr.clocks);
        pwm5.set_duty(0);
        pwm5.enable();

        dp.TIM16.cr1.write(|w| w.arpe().set_bit());
        let mut pwm6 = dp
            .TIM16
            .pwm(pwm6_pin, 100.kHz(), ccdr.peripheral.TIM16, &ccdr.clocks);
        pwm6.set_duty(0);
        pwm6.enable();

        dp.TIM17.cr1.write(|w| w.arpe().set_bit());
        let mut pwm7 = dp
            .TIM17
            .pwm(pwm7_pin, 100.kHz(), ccdr.peripheral.TIM17, &ccdr.clocks);
        pwm7.set_duty(0);
        pwm7.enable();

        //
        // GPIO
        //
        let pwm_pins = PwmPins {
            pwm0: pwm2,
            pwm1: pwm5,
            pwm2: pwm6,
            pwm3: pwm7,
        };

        //
        // ADC
        //

        let ain0: Pin<'F', 3> = gpiof.pf3.into_analog();
        let ain1: Pin<'F', 4> = gpiof.pf4.into_analog();
        let ain2: Pin<'F', 5> = gpiof.pf5.into_analog();
        let ain3: Pin<'F', 6> = gpiof.pf6.into_analog();
        let ain4: Pin<'F', 7> = gpiof.pf7.into_analog();
        let ain5: Pin<'F', 8> = gpiof.pf8.into_analog();
        let ain6: Pin<'F', 9> = gpiof.pf9.into_analog();
        let ain7: Pin<'F', 10> = gpiof.pf10.into_analog();
        let ain8: Pin<'C', 0> = gpioc.pc0.into_analog();

        let ain9: Pin<'C', 2> = gpioc.pc2.into_analog();
        let ain10: Pin<'C', 3> = gpioc.pc3.into_analog();
        let ain11: Pin<'A', 0> = gpioa.pa0.into_analog();
        let ain12: Pin<'A', 3> = gpioa.pa3.into_analog();
        let ain13: Pin<'A', 4> = gpioa.pa4.into_analog();
        let ain14: Pin<'A', 5> = gpioa.pa5.into_analog();
        let ain15: Pin<'A', 6> = gpioa.pa6.into_analog();
        let ain16: Pin<'B', 0> = gpiob.pb0.into_analog();
        let ain17: Pin<'B', 1> = gpiob.pb1.into_analog();
        let ain18: Pin<'F', 11> = gpiof.pf11.into_analog();
        let ain19: Pin<'F', 12> = gpiof.pf12.into_analog();

        let adc_pins = AdcPins {
            ain0,
            ain1,
            ain2,
            ain3,
            ain4,
            ain5,
            ain6,
            ain7,
            ain8,
            ain9,
            ain10,
            ain11,
            ain12,
            ain13,
            ain14,
            ain15,
            ain16,
            ain17,
            ain18,
            ain19,
        };

        // Set up sampling interrupt
        let sample_timer =
            dp.TIM2
                .timer(ADC_SAMPLE_FREQ_HZ.Hz(), ccdr.peripheral.TIM2, &ccdr.clocks);

        let adc = Sampler::new(
            &ccdr.clocks,
            adc1,
            adc2,
            adc3,
            adc_pins,
            sample_timer,
            encoder,
            pulse_counter,
            frequency_inp0,
            frequency_inp1,
        );

        //
        // Ethernet
        //

        // Ethernet pins
        let rmii_ref_clk = gpioa.pa1.into_alternate();
        let rmii_mdio = gpioa.pa2.into_alternate();
        let rmii_mdc = gpioc.pc1.into_alternate();
        let rmii_crs_dv = gpioa.pa7.into_alternate();
        let rmii_rxd0 = gpioc.pc4.into_alternate();
        let rmii_rxd1 = gpioc.pc5.into_alternate();
        let rmii_tx_en = gpiog.pg11.into_alternate();
        let rmii_txd0 = gpiog.pg13.into_alternate();
        let rmii_txd1 = gpiob.pb13.into_alternate();

        // Initialise ethernet...
        let mac_addr = smoltcp::wire::EthernetAddress::from_bytes(&MAC_ADDRESS);
        let (eth_dma, eth_mac) = unsafe {
            ethernet::new(
                dp.ETHERNET_MAC,
                dp.ETHERNET_MTL,
                dp.ETHERNET_DMA,
                (
                    rmii_ref_clk,
                    rmii_mdio,
                    rmii_mdc,
                    rmii_crs_dv,
                    rmii_rxd0,
                    rmii_rxd1,
                    rmii_tx_en,
                    rmii_txd0,
                    rmii_txd1,
                ),
                &mut DES_RING,
                mac_addr,
                ccdr.peripheral.ETH1MAC,
                &ccdr.clocks,
            )
        };

        // Initialise ethernet PHY
        let mut lan8742a = ethernet::phy::LAN8742A::new(eth_mac);
        lan8742a.phy_reset();
        lan8742a.phy_init();

        // Build ethernet interface
        let net: Net<'a> = Net::new(store, eth_dma, mac_addr.into(), Instant::ZERO);

        // Set up sub-cycle timer
        // TIM2 and TIM5 have 32-bit counters and 16-bit prescalers
        let subcycle_rate_hz = TIM5::get_clk(&ccdr.clocks).unwrap().to_Hz();
        let mut subcycle_timer = dp.TIM5.timer(1.Hz(), ccdr.peripheral.TIM5, &ccdr.clocks);

        // Defaults
        let dt_ns: u32 = 250_000; // Default, subject to clock res
        let clocks = ccdr.clocks;
        let state = BoardState::Connecting;
        let time_ns = 0;
        let controller = None;
        let configuring_timeout_ms = 0;
        let loss_of_contact_limit = 0;
        subcycle_timer.set_timeout(Duration::from_nanos((dt_ns as u64) * 2)); // Just needs to be at least as long as dt_ns
        watchdog.start(500.millis()); // Can't be updated later

        (
            Self {
                state,
                led0,
                led1,
                led2,
                led3,
                time_ns,
                dt_ns,
                systick,
                clocks,
                subcycle_timer,
                subcycle_rate_hz,
                watchdog,
                net,
                controller,
                configuring_timeout_ms,
                loss_of_contact_limit,
                pwm_pins,
            },
            adc,
        )
    }
}
