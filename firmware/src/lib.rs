#![no_std]

extern crate alloc;

pub mod device;
#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
pub mod display;
pub mod net;
#[cfg(any(feature = "inkplate6", feature = "inkplate6v2"))]
pub mod panel_bus;
pub mod portal;
pub mod time;

/// Firmware version: `git describe` at build time (see build.rs), else the package version.
pub const VERSION: &str = env!("INKPRINT_VERSION");
