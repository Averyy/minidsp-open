//! MiniDSP wire protocol — encoding, commands, device definitions.
//!
//! Pure Rust with zero platform-specific code: the same crate builds for
//! desktop (Linux/macOS/Windows) and for ESP32-S3 on ESP-IDF. It needs `std`
//! (the default `std` feature); building without it isn't supported.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod biquad;
pub mod commands;
pub mod default_preset;
pub mod device;
pub mod dialect;
pub mod discovery;
pub mod eeprom;
pub mod packet;
pub mod playback;
pub mod rattle;
pub mod source;
pub mod status;
