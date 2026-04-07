//! MiniDSP wire protocol — encoding, commands, device definitions.
//!
//! Pure Rust, `no_std` compatible (with `default-features = false`).
//! This crate contains zero platform-specific code and compiles on both
//! desktop (Linux/macOS/Windows) and ESP32-S3.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod packet;
pub mod commands;
pub mod device;
pub mod dialect;
pub mod biquad;
pub mod eeprom;
pub mod source;
pub mod status;
pub mod discovery;
