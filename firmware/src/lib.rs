#![no_std]

extern crate alloc;

#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
pub mod display;
pub mod net;
#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
pub mod panel_bus;
