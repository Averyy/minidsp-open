//! Device definitions — address tables, capabilities, and identification.
//!
//! Two-stage identification:
//! 1. `hw_id` (from ReadHardwareId) → device family (2x4 HD, Flex, SHD, etc.)
//! 2. `dsp_version` (from EEPROM 0xFFA1) → DSP structure (address table)
//!
//! Together they identify the exact device profile.

pub mod dsp;
pub mod generated_official;
pub mod probe;

use crate::dialect::Dialect;
use crate::source::SourceMap;

/// Capability tier — determines what operations are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapabilityTier {
    /// Volume, mute, source, preset only. No address table.
    Universal,
    /// DSP version known, partial address table. Some PEQ may work.
    DspKnown,
    /// Full address table with all parameters mapped.
    Full,
}

/// Complete device profile — everything needed to control a device.
#[derive(Debug, Clone)]
pub struct DeviceProfile {
    /// Human-readable product name.
    pub name: &'static str,
    /// Hardware ID (from ReadHardwareId).
    pub hw_id: u8,
    /// DSP version (from EEPROM 0xFFA1).
    pub dsp_version: u8,
    /// Wire dialect (address length, value encoding).
    pub dialect: Dialect,
    /// Source map (index → Source variant).
    pub sources: &'static SourceMap,
    /// Number of input channels.
    pub inputs: u8,
    /// Number of output channels.
    pub outputs: u8,
    /// Number of presets (typically 4).
    pub presets: u8,
    /// DSP address table.
    pub dsp_table: Option<&'static dsp::DspTable>,
    /// Capability tier.
    pub tier: CapabilityTier,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Has Dirac Live support.
    pub has_dirac: bool,
    /// Has FIR filter support.
    pub has_fir: bool,
    /// Has display (brightness/idle controls).
    pub has_display: bool,
    /// Has DRE (Dynamic Range Enhancement) option.
    pub has_dre: bool,
    /// Per-source volume offsets supported.
    pub has_volume_offsets: bool,
    /// Device exposes USB audio playback in addition to HID control.
    pub has_usb_audio: bool,
}
