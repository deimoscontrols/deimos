//! Peripheral side of control program
#![no_main]
#![no_std]

mod board;

extern crate cortex_m;
extern crate cortex_m_rt as rt;

use rt::{entry, exception};
use stm32h7xx_hal::{interrupt, pac, prelude::*, stm32, timer::Event};

use core::mem::MaybeUninit;
use core::panic::PanicInfo;
use core::ptr::addr_of_mut;
use core::sync::atomic::compiler_fence;

use irq::{handler, scope};
use smoltcp::iface::SocketStorage;

use crate::board::{subsystems::net::NetStorageStatic, Board};

// MaybeUninit allows us write code that is correct even if STORE is not
// initialised by the runtime
static mut STORE: MaybeUninit<NetStorageStatic> = MaybeUninit::uninit();

/// User program entrypoint after program is copied from flash into RAM
/// This routine is specified at the reset vector in the ISR vector table.
///
/// Copies global .data init from flash to SRAM and then zeros the bss segment.
#[entry]
unsafe fn main() -> ! {
    // Initialize runtime-defined exception handlers before running any
    // application code or doing anything that might trigger them
    handler!(systick_default_handler = || {});

    // Initialize static storage
    // unsafe: mutable reference to static storage, we only do this once
    let store: &mut NetStorageStatic<'_> = unsafe {
        let store_ptr = STORE.as_mut_ptr();

        // Initialise the socket_storage field. Using `write` instead of
        // assignment via `=` to not call `drop` on the old, uninitialised
        // value
        addr_of_mut!((*store_ptr).socket_storage).write([SocketStorage::EMPTY; 8]);

        // Now that all fields are initialised we can safely use
        // assume_init_mut to return a mutable reference to STORE
        STORE.assume_init_mut()
    };

    // Board startup
    let (mut board, mut sampler) = Board::new(store);

    // Set up the sample timer and build its interrupt handler,
    // waiting until after the sampler timer is configured to enable the
    // sampling interrupt.
    sampler.timer.reset_counter(); // Make sure we don't immediately trigger the interrupt during handoff
    sampler.timer.listen(Event::TimeOut);
    handler!(sampling_handler = || sampler.sample());

    scope(|sampling| {
        scope(|systick_default| {
            // Set default interrupt handlers
            systick_default.register(board::interrupts::SysTick, systick_default_handler);
            sampling.register(board::interrupts::TIM2, sampling_handler);

            // Unmask sample interrupt
            unsafe {
                pac::NVIC::unmask(interrupt::TIM2);
            }

            // Enter the state machine main loop,
            // which will control the systick interrupt handler
            board.run();
        })
    });

    loop {}
}

// #[interrupt]
// fn ETH() {
//     unsafe { ethernet::interrupt_handler() }
// }

#[exception]
unsafe fn HardFault(_ef: &cortex_m_rt::ExceptionFrame) -> ! {
    panic!();
}

#[exception]
unsafe fn DefaultHandler(_irqn: i16) {
    // panic!("Unhandled exception (IRQn = {})", irqn);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // Disable all interrupts, then reset

    cortex_m::interrupt::disable();
    let dp = unsafe { stm32::Peripherals::steal() };

    // Reset the eth mac, which is, by default, not reset by the CPU reset
    // because it's on domain 2
    dp.RCC.ahb1rstr.write(|w| w.eth1macrst().set_bit());

    // Data-and-instruction sync barrier to make sure the eth mac enters reset before the CPU
    cortex_m::asm::dsb();

    // Reset the CPU and restart the program
    dp.RCC.ahb3rstr.write(|w| w.cpurst().set_bit());

    // Data-and-instruction sync barrier to make sure the reset is written before busywaiting
    cortex_m::asm::dsb();

    // Must not return, but is in CPU reset now & program will restart from main
    loop {
        // Don't let the compiler move the loop before writing reset, which it could do to save time
        compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
}
