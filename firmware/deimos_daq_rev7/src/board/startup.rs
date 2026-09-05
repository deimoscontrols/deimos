use super::*;

use core::ptr::addr_of_mut;
use cortex_m::peripheral::scb::SystemHandler;
use deimos_shared::states::ByteStruct;

use smoltcp::time::Instant;
use stm32h7xx_hal::{
    adc,
    delay::Delay,
    ethernet,
    ethernet::PHY,
    gpio::{Output, Pin},
    qei::QeiExt,
    rcc::ResetEnable,
    rcc::rec::AdcClkSel,
    timer::GetClk,
    traits::DacOut,
};

impl<'a> Board<'a> {
    /// Configure power, clocks, and peripherals
    pub fn new(store: &'a mut NetStorageStatic<'a>) -> (Self, Sampler) {
        let calibration =
            Calibration::read_bytes(include_bytes!(concat!(env!("OUT_DIR"), "/calibration.in")));
        assert!(calibration.is_valid());
        // Power setup
        let dp = stm32::Peripherals::take().unwrap();
        let mut cp = stm32::CorePeripherals::take().unwrap();
        // CYCCNT provides the core-cycle-resolution clock used within each
        // sampling/communication IRQ. It is read directly and requires no
        // timer peripheral, reload, or interrupt.
        cp.DCB.enable_trace();
        // This is a runtime hardware capability rather than a build-time
        // numeric bound. Fail during startup in every build if a future target
        // lacks the counter required by the operating clock.
        assert!(cortex_m::peripheral::DWT::has_cycle_counter());
        cp.DWT.enable_cycle_counter();
        let pwr = dp.PWR.constrain().vos0(&dp.SYSCFG);
        let pwrcfg = pwr.freeze();
        //    Power up SRAM3, where ethernet buffers are stored
        dp.RCC.ahb2enr.modify(|_, w| w.sram3en().set_bit());

        // Clock setup
        let rcc = dp.RCC.constrain();
        let mut ccdr = rcc
            .use_hse(48.MHz()) // Set expected external clock freq
            .bypass_hse() // Use external clock signal directly
            .sys_ck(CORE_RATE_HZ.Hz())
            .hclk(200.MHz())
            .pll2_p_ck(24.MHz()) // Default adc_ker_ck_input
            .freeze(pwrcfg, &dp.SYSCFG);
        //    Make sure clock setup was exact
        assert_eq!(ccdr.clocks.sysclk().raw(), CORE_RATE_HZ);
        assert_eq!(ccdr.clocks.c_ck().raw(), CORE_RATE_HZ);
        assert_eq!(ccdr.clocks.hclk().raw(), 200_000_000);
        assert_eq!(ccdr.clocks.pclk1().raw(), 100_000_000);
        assert_eq!(ccdr.clocks.pclk2().raw(), 100_000_000);
        assert_eq!(ccdr.clocks.pclk4().raw(), 100_000_000);

        // Instruction caching
        cp.SCB.enable_icache();

        // SysTick owns both synchronous acquisition and communication while the
        // board is operating. Give it the highest configurable exception
        // priority so unrelated peripheral work cannot add sample jitter.
        unsafe {
            cp.SCB.set_priority(SystemHandler::SysTick, 0x00);
        }

        // Watchdog reboots the board if the board freezes for any reason
        let mut watchdog = IndependentWatchdog::new(dp.IWDG);

        // Temporarily use SYSTICK as a delay provider, for initialization only
        let mut delay = Delay::new(cp.SYST, ccdr.clocks);

        // ADCs are initialized before we start parting out the peripherals
        // because they need to use systick (or another timer) for a delay briefly
        let (adc1, adc2, adc3) = {
            // Switch adc_ker_ck_input multiplexer to per_ck
            ccdr.peripheral.kernel_adc_clk_mux(AdcClkSel::Per);

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
            // This is disabled because it increases ADC channel crosstalk and shows minimal benefit
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

            (adc1, adc2, adc3)
        };

        // Initialize GPIO
        let gpioa = dp.GPIOA.split(ccdr.peripheral.GPIOA);
        let gpiob = dp.GPIOB.split(ccdr.peripheral.GPIOB);
        let gpioc = dp.GPIOC.split(ccdr.peripheral.GPIOC);
        let gpiod = dp.GPIOD.split(ccdr.peripheral.GPIOD);
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

        // QeiExt configures encoder mode 3, counting both edges of both
        // channels. Release the wrapper after configuration so the sampler can
        // retain its existing raw-register access pattern.
        let encoder = dp
            .TIM1
            .qei(
                (gpioe.pe9.into_alternate(), gpioe.pe11.into_alternate()),
                ccdr.peripheral.TIM1,
            )
            .release()
            .0;

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
        //
        // Using TIM4 CCR2 in any capacity - even just having it enabled and not connected to any reset trigger,
        // let alone using it to measure pulse width - causes failures across multiple timer modules (TIM4
        // CCMR1 fails to trigger, and TIM15 CH2 prescale becomes misconfigured).
        let _pwmi0_pin: Pin<'B', 6, stm32h7xx_hal::gpio::Alternate<2>> = gpiob.pb6.into_alternate();
        TIM4::get_clk(&ccdr.clocks).unwrap();
        ccdr.peripheral.TIM4.enable().reset();
        dp.TIM4.psc.write(|w| w.psc().bits(7)); // 8x prescale -> about 400Hz min freq, 80ns res
        dp.TIM4.ccmr1_input().write(|w| w.cc1s().ti1()); // Compare/capture channel input for period
        dp.TIM4.smcr.write(|w| w.ts().ti1fp1().sms().reset_mode()); // Trigger input, reset mode
        dp.TIM4.ccer.write(|w| w.cc1e().set_bit()); // Enable capture output
        dp.TIM4.cr1.write(|w| w.cen().enabled()); // Enable counter
        let frequency_inp0 = dp.TIM4;

        // TIM15 CH2
        // Read ccr1 for latest period
        // Using second CCR with the same channel input does not work; needs its own input channel
        let _pwmi1_pin: Pin<'E', 6, stm32h7xx_hal::gpio::Alternate<4>> = gpioe.pe6.into_alternate(); // TIM15 CH2
        TIM15::get_clk(&ccdr.clocks).unwrap();
        ccdr.peripheral.TIM15.enable().reset();
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
        // DAC
        //

        let (dac1, dac2) = {
            let pa4: Pin<'A', 4> = gpioa.pa4.into_analog();
            let pa5: Pin<'A', 5> = gpioa.pa5.into_analog();

            // Set up DAC registers
            let (dac1, dac2) = dp.DAC.dac((pa4, pa5), ccdr.peripheral.DAC12);

            // Calibrate
            let mut dac1: stm32h7xx_hal::dac::C1<DAC, stm32h7xx_hal::dac::Enabled> =
                dac1.calibrate_buffer(&mut delay).enable();
            let mut dac2: stm32h7xx_hal::dac::C2<DAC, stm32h7xx_hal::dac::Enabled> =
                dac2.calibrate_buffer(&mut delay).enable();

            dac1.set_value(0);
            dac2.set_value(0);

            (dac1, dac2)
        };

        //
        // GPIO
        //
        let di0: Pin<'D', 0, stm32h7xx_hal::gpio::Input> = gpiod.pd0.into_pull_down_input();
        let di1: Pin<'D', 1, stm32h7xx_hal::gpio::Input> = gpiod.pd1.into_pull_down_input();
        let do0: Pin<'D', 2, Output> = gpiod.pd2.into_push_pull_output();
        let do1: Pin<'D', 3, Output> = gpiod.pd3.into_push_pull_output();
        let do2: Pin<'D', 4, Output> = gpiod.pd4.into_push_pull_output();
        let do3: Pin<'D', 5, Output> = gpiod.pd5.into_push_pull_output();

        //
        // Outputs
        //
        let outputs = Outputs {
            pwm0: pwm2,
            pwm1: pwm5,
            pwm2: pwm6,
            pwm3: pwm7,
            // Clock discovery is fallible in the generic HAL but fixed by this
            // startup clock tree. Cache it once so output updates do not carry
            // four `unwrap` paths through every publishing IRQ.
            pwm_clock_hz: [
                TIM3::get_clk(&ccdr.clocks).unwrap(),
                TIM12::get_clk(&ccdr.clocks).unwrap(),
                TIM16::get_clk(&ccdr.clocks).unwrap(),
                TIM17::get_clk(&ccdr.clocks).unwrap(),
            ],
            dac1,
            dac2,
            do0,
            do1,
            do2,
            do3,
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
        // let ain13: Pin<'A', 4> = gpioa.pa4.into_analog();  // Consumed for DAC
        // let ain14: Pin<'A', 5> = gpioa.pa5.into_analog();
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
            // ain13,
            // ain14,
            ain15,
            ain16,
            ain17,
            ain18,
            ain19,
        };

        let adc = Sampler::new(
            &ccdr.clocks,
            adc1,
            adc2,
            adc3,
            adc_pins,
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
            let des_ring_ptr = addr_of_mut!(DES_RING);
            (*des_ring_ptr).write(ethernet::DesRing::new());

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
                (*des_ring_ptr).assume_init_mut(),
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
        let net: Net<'a> = Net::new(store, eth_dma, mac_addr, Instant::ZERO);

        // Restore systick for use as main cycle timer
        let systick = delay.free();

        // Defaults
        let dt_ns: u32 = 250_000; // Default, subject to clock res
        let clocks = ccdr.clocks;
        let state = BoardState::Connecting;
        let time_ns = 0;
        let controller = None;
        let configuring_timeout_ms = 0;
        let loss_of_contact_limit = 0;
        watchdog.start(500.millis()); // Can't be updated later

        (
            Self {
                state,
                led0,
                led1,
                led2,
                led3,
                di0,
                di1,
                time_ns,
                dt_ns,
                systick,
                clocks,
                watchdog,
                net,
                controller,
                configuring_timeout_ms,
                loss_of_contact_limit,
                modbus: ModbusTcpServer::new(),
                calibration,
                outputs,
            },
            adc,
        )
    }
}
