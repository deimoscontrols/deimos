pub use systick::Interrupt::PendSV;
/// Core exceptions and peripheral interrupts are scoped through separate IRQ enums.
pub use systick::Interrupt::SysTick;
pub use timer::Interrupt::TIM2;

mod systick {
    use irq::scoped_interrupts;
    use rt::exception;
    scoped_interrupts! {
        /// Exception interrupts that can be overridden by the user at runtime
        ///
        /// Core exception handlers that can be overridden by the firmware at runtime.
        #[allow(missing_docs)]
        pub enum Interrupt {
            PendSV,
            SysTick,
        }

        use #[exception];
    }
}

mod timer {
    use irq::scoped_interrupts;
    use stm32h7xx_hal::interrupt;

    scoped_interrupts! {
        /// Exception interrupts that can be overridden by the user at runtime
        ///
        /// Peripheral interrupt handlers that can be overridden by the firmware at runtime.
        #[allow(missing_docs)]
        pub enum Interrupt {
            TIM2,
        }

        use #[interrupt];
    }
}
