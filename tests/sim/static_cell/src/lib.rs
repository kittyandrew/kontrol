//! Stands in for `static_cell` in the simulation only. app.rs takes its TLS and TCP buffers from
//! `ConstStaticCell`s inside `run`, and the real crate panics on a second take. A simulated watchdog reset
//! calls `run` again in the same process, so each take here hands out a fresh copy of the initial value.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

pub struct ConstStaticCell<T> {
    val: UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T: Send> Sync for ConstStaticCell<T> {}

impl<T> ConstStaticCell<T> {
    pub const fn new(value: T) -> Self {
        Self { val: UnsafeCell::new(MaybeUninit::new(value)) }
    }

    // A bitwise copy of the const initial value, which `take` never hands out itself: each boot gets state as
    // fresh as the board's after a reset. The copy leaks, as the board's RAM would never be freed either.
    #[allow(clippy::mut_from_ref)]
    pub fn take(&'static self) -> &'static mut T {
        let copy: T = unsafe { core::ptr::read((*self.val.get()).as_ptr()) };
        Box::leak(Box::new(copy))
    }
}
