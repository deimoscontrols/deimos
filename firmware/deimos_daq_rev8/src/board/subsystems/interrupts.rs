/// The systick interrupt is handled separately from the peripheral interrupts,
/// so we have to use two different scoped interrupt systems to control both
pub use systick::Interrupt::SysTick;

mod systick {
    use irq::scoped_interrupts;
    use rt::exception;
    scoped_interrupts! {
        /// Exception interrupts that can be overridden by the user at runtime
        ///
        /// A SysTick exception is an exception the system timer generates when it
        /// reaches zero. Software can also generate a SysTick exception. In an OS
        /// environment, the processor can use this exception as system tick.
        #[allow(missing_docs)]
        pub enum Interrupt {
            SysTick,
        }

        use #[exception];
    }
}
