#![no_std]

extern crate alloc;

pub mod net;
#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
pub mod panel_bus;
