//! MiniDSP wire protocol — encoding, commands, device definitions.
//!
//! Pure Rust with zero platform-specific code: the same crate builds for
//! desktop (Linux/macOS/Windows) and for ESP32-S3 on ESP-IDF. It needs `std`
//! (the default `std` feature); building without it isn't supported.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(feature = "auth")]
pub mod auth;
pub mod biquad;
pub mod commands;
pub mod default_preset;
pub mod device;
pub mod dialect;
pub mod discovery;
pub mod eeprom;
#[cfg(feature = "std")]
pub mod http_origin;
pub mod packet;
pub mod playback;
pub mod rattle;
pub mod settings;
pub mod shadow;
pub mod source;
pub mod status;
