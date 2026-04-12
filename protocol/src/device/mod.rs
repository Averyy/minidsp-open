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

pub use dsp::DelayRange;

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
    /// Runtime selector/status reads via `0x14` are not authoritative on this
    /// device family.  When true, the client keeps verified shadow state for
    /// routing, bypass, FIR tap count, and compressor fields and prefers it
    /// over live reads that are known to collapse or return garbage values.
    pub has_unreliable_selector_readback: bool,
}

impl DeviceProfile {
    /// Effective delay range for this profile. Returns the `DspTable` delay
    /// range when available, otherwise the conservative
    /// [`DelayRange::fallback`].
    pub fn delay_range(&self) -> DelayRange {
        self.dsp_table
            .map(|t| t.delay_range)
            .unwrap_or_else(DelayRange::fallback)
    }

    /// Clamp `delay_ms` into the family's supported delay range. NaN and
    /// infinity are forced to the minimum.
    pub fn clamp_delay_ms(&self, delay_ms: f32) -> f32 {
        self.delay_range().clamp(delay_ms)
    }

    /// Convert a delay in milliseconds to a sample count, clamped by this
    /// family's range first. Rounds to the nearest integer. The returned
    /// value is always `<= delay_range.max_ms * sample_rate / 1000`, so any
    /// downstream samples-as-u32 conversion cannot overflow the DSP.
    pub fn delay_ms_to_samples(&self, delay_ms: f32) -> u32 {
        let clamped = self.clamp_delay_ms(delay_ms);
        let samples = clamped * self.sample_rate as f32 / 1000.0;
        samples.round().max(0.0) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use probe::identify;

    #[test]
    fn delay_ms_to_samples_clamps_per_family() {
        // 2x4 HD (DSP100) runs at 96 kHz with 0..80 ms range.
        let p = identify(10, 100);
        assert_eq!(p.delay_ms_to_samples(-5.0), 0);
        assert_eq!(p.delay_ms_to_samples(0.0), 0);
        // 40 ms * 96 kHz = 3840 samples.
        assert_eq!(p.delay_ms_to_samples(40.0), 3840);
        // 80 ms * 96 kHz = 7680 samples.
        assert_eq!(p.delay_ms_to_samples(80.0), 7680);
        // Out-of-range (200 ms) clamps to 80 ms.
        assert_eq!(p.delay_ms_to_samples(200.0), 7680);
        // NaN clamps to the minimum (0 samples).
        assert_eq!(p.delay_ms_to_samples(f32::NAN), 0);
        // +Infinity clamps to max.
        assert_eq!(p.delay_ms_to_samples(f32::INFINITY), 7680);
    }

    #[test]
    fn delay_ms_to_samples_respects_tighter_dirac_family_range() {
        // DDRC-24 (DSP101) runs at 48 kHz with 0..30 ms range.
        let p = identify(10, 101);
        // 100 ms clamps to 30 ms → 30 * 48 = 1440 samples.
        assert_eq!(p.delay_ms_to_samples(100.0), 1440);
        assert_eq!(p.delay_ms_to_samples(30.0), 1440);
    }
}
