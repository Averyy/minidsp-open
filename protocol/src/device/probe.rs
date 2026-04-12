//! Device identification: (hw_id, dsp_version) → DeviceProfile.

use crate::device::dsp::*;
use crate::device::generated_official;
use crate::device::{CapabilityTier, DeviceProfile};
use crate::dialect::Dialect;
use crate::source::*;

/// USB VID for MiniDSP devices (SHARC and newer legacy).
pub const USB_VID_MINIDSP: u16 = 0x2752;

/// USB VID for legacy PIC-based MiniDSP devices.
pub const USB_VID_LEGACY: u16 = 0x04D8;

/// USB PID for legacy devices.
pub const USB_PID_LEGACY: u16 = 0x003F;

/// Returns true when a USB VID/PID pair is treated as a MiniDSP candidate by
/// the official extracted app assets.
pub fn is_official_usb_device(vid: u16, pid: u16) -> bool {
    generated_official::OFFICIAL_USB_PRODUCT_HINTS
        .iter()
        .any(|entry| entry.vid == vid && entry.pid == pid)
}

/// Identify a device from its hardware ID and DSP version.
///
/// This is the two-stage identification:
/// 1. `hw_id` determines the device family and source map
/// 2. `dsp_version` determines the DSP structure (address table)
pub fn identify(hw_id: u8, dsp_version: u8) -> DeviceProfile {
    match (hw_id, dsp_version) {
        // SHARC devices — new dialect
        (10, 100) => profile_2x4hd(),
        (10, 101) => profile_ddrc24(),
        (27, 100) => profile_flex(),
        (27, 101) => profile_flex_dl(),
        (14, _) => profile_shd(),
        (17, _) => profile_shd_studio(17),
        (18, _) => profile_shd_studio(18),
        (21, _) => profile_shd_power(),
        (30, 110) => profile_flex_eight(),
        (30, 111) => profile_flex_eight_dl(),
        (31, 113) => profile_flex_ht_113(),
        (31, 115) => profile_flex_ht_115(),
        (32, 113) => profile_flex_htx_113(),
        (32, 115) => profile_flex_htx_115(),

        // Legacy devices — old dialect (ADAU1445, fixed-point)
        (1, 51) => profile_10x10hd(),
        (1, _) => profile_4x10hd(),
        (2, 54) => profile_nanodigi_2x8(),
        (2, 22) => profile_2x4_nonhd(),

        // Legacy devices — new dialect (SHARC despite legacy hw_id)
        (4, _) => profile_msharc_4x8(),
        (6, 95) => profile_ddrc_88bm(),
        (11, 97) => profile_cdsp_8x12v2(),

        // Basic-support-only devices
        (7, _) => profile_nanoavr(),
        (8, _) => profile_nanoavr_hda(),

        // Unknown — universal basic support
        _ => profile_generic(hw_id, dsp_version),
    }
}

/// Get product name for a (hw_id, dsp_version) pair.
/// Used for probe output and device list.
pub fn product_name(hw_id: u8, dsp_version: u8) -> &'static str {
    match (hw_id, dsp_version) {
        (10, 100) => "2x4HD",
        (10, 101) => "DDRC-24",
        (27, 100) => "Flex",
        (27, 101) => "Flex (DL)",
        (14, _) => "SHD",
        (17, _) => "SHD Studio",
        (18, _) => "SHD Studio",
        (21, _) => "SHD Power",
        (30, 110) => "Flex Eight",
        (30, 111) => "Flex Eight (DL)",
        (31, 113) => "Flex HT",
        (31, 115) => "Flex HT",
        (32, 113) => "Flex HTx",
        (32, 115) => "Flex HTx",
        (1, 51) => "10x10 HD",
        (1, _) => "4x10 HD",
        (2, 54) => "NanoDigi 2x8",
        (2, 22) => "miniDSP 2x4",
        (4, _) => "miniSHARC 4x8",
        (6, 95) => "DDRC-88BM",
        (11, 97) => "C-DSP 8x12 v2",
        (7, _) => "nanoAVR",
        (8, _) => "nanoAVR HDA",
        _ => "Unknown MiniDSP",
    }
}

// -- Profile constructors --

fn profile_2x4hd() -> DeviceProfile {
    DeviceProfile {
        name: "2x4HD",
        hw_id: 10,
        dsp_version: 100,
        dialect: Dialect::NEW,
        sources: &SOURCES_2X4HD,
        inputs: 2,
        outputs: 4,
        presets: 4,
        dsp_table: Some(&DSP_100),
        tier: CapabilityTier::Full,
        sample_rate: 96_000,
        has_dirac: false,
        has_fir: true,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: true,
        has_unreliable_selector_readback: true,
    }
}

fn profile_ddrc24() -> DeviceProfile {
    DeviceProfile {
        name: "DDRC-24",
        hw_id: 10,
        dsp_version: 101,
        dialect: Dialect::NEW,
        sources: &SOURCES_2X4HD,
        inputs: 2,
        outputs: 4,
        presets: 4,
        dsp_table: Some(&DSP_101),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: true,
        has_fir: false,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

fn profile_flex() -> DeviceProfile {
    DeviceProfile {
        name: "Flex",
        hw_id: 27,
        dsp_version: 100,
        dialect: Dialect::NEW,
        sources: &SOURCES_FLEX,
        inputs: 2,
        outputs: 4,
        presets: 4,
        dsp_table: Some(&DSP_100),
        tier: CapabilityTier::Full,
        sample_rate: 96_000,
        has_dirac: false,
        has_fir: true,
        has_display: true,
        has_dre: false,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: true,
    }
}

fn profile_flex_dl() -> DeviceProfile {
    DeviceProfile {
        name: "Flex (DL)",
        hw_id: 27,
        dsp_version: 101,
        dialect: Dialect::NEW,
        sources: &SOURCES_FLEX,
        inputs: 2,
        outputs: 4,
        presets: 4,
        dsp_table: Some(&DSP_101),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: true,
        has_fir: false,
        has_display: true,
        has_dre: false,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

fn profile_shd() -> DeviceProfile {
    DeviceProfile {
        name: "SHD",
        hw_id: 14,
        dsp_version: 105,
        dialect: Dialect::NEW,
        sources: &SOURCES_SHD,
        inputs: 2,
        outputs: 4,
        presets: 4,
        dsp_table: Some(&DSP_105),
        tier: CapabilityTier::Full,
        sample_rate: 96_000,
        has_dirac: true,
        has_fir: false,
        has_display: true,
        has_dre: false,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

fn profile_shd_studio(hw_id: u8) -> DeviceProfile {
    DeviceProfile {
        name: "SHD Studio",
        hw_id,
        dsp_version: 105,
        dialect: Dialect::NEW,
        sources: &SOURCES_SHD_STUDIO,
        inputs: 2,
        outputs: 4,
        presets: 4,
        dsp_table: Some(&DSP_105),
        tier: CapabilityTier::Full,
        sample_rate: 96_000,
        has_dirac: true,
        has_fir: false,
        has_display: true,
        has_dre: false,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

fn profile_shd_power() -> DeviceProfile {
    DeviceProfile {
        name: "SHD Power",
        hw_id: 21,
        dsp_version: 105,
        dialect: Dialect::NEW,
        sources: &SOURCES_SHD_POWER,
        inputs: 2,
        outputs: 4,
        presets: 4,
        dsp_table: Some(&DSP_105),
        tier: CapabilityTier::Full,
        sample_rate: 96_000,
        has_dirac: true,
        has_fir: false,
        has_display: true,
        has_dre: false,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

fn profile_flex_eight() -> DeviceProfile {
    DeviceProfile {
        name: "Flex Eight",
        hw_id: 30,
        dsp_version: 110,
        dialect: Dialect::NEW,
        sources: &SOURCES_FLEX_EIGHT,
        inputs: 2,
        outputs: 8,
        presets: 4,
        dsp_table: Some(&DSP_110),
        tier: CapabilityTier::Full,
        sample_rate: 96_000,
        has_dirac: false,
        has_fir: true,
        has_display: true,
        has_dre: true,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

fn profile_flex_eight_dl() -> DeviceProfile {
    DeviceProfile {
        name: "Flex Eight (DL)",
        hw_id: 30,
        dsp_version: 111,
        dialect: Dialect::NEW,
        sources: &SOURCES_FLEX_EIGHT,
        inputs: 2,
        outputs: 8,
        presets: 4,
        dsp_table: Some(&DSP_111),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: true,
        has_fir: false,
        has_display: true,
        has_dre: true,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

fn profile_flex_ht_113() -> DeviceProfile {
    DeviceProfile {
        name: "Flex HT",
        hw_id: 31,
        dsp_version: 113,
        dialect: Dialect::NEW,
        sources: &SOURCES_FLEX_HT,
        inputs: 8,
        outputs: 8,
        presets: 4,
        dsp_table: Some(&DSP_113),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: true,
        has_fir: false,
        has_display: true,
        has_dre: true,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

fn profile_flex_ht_115() -> DeviceProfile {
    DeviceProfile {
        name: "Flex HT",
        hw_id: 31,
        dsp_version: 115,
        dialect: Dialect::NEW,
        sources: &SOURCES_FLEX_HT,
        inputs: 8,
        outputs: 8,
        presets: 4,
        dsp_table: Some(&DSP_115),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: false,
        has_fir: false,
        has_display: true,
        has_dre: true,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

fn profile_flex_htx_113() -> DeviceProfile {
    DeviceProfile {
        name: "Flex HTx",
        hw_id: 32,
        dsp_version: 113,
        dialect: Dialect::NEW,
        sources: &SOURCES_FLEX_HTX,
        inputs: 8,
        outputs: 8,
        presets: 4,
        dsp_table: Some(&DSP_113),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: true,
        has_fir: false,
        has_display: true,
        has_dre: false,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

fn profile_flex_htx_115() -> DeviceProfile {
    DeviceProfile {
        name: "Flex HTx",
        hw_id: 32,
        dsp_version: 115,
        dialect: Dialect::NEW,
        sources: &SOURCES_FLEX_HTX,
        inputs: 8,
        outputs: 8,
        presets: 4,
        dsp_table: Some(&DSP_115),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: false,
        has_fir: false,
        has_display: true,
        has_dre: false,
        has_volume_offsets: true,
        has_usb_audio: true,
        has_unreliable_selector_readback: false,
    }
}

// -- Legacy device profiles --

fn profile_4x10hd() -> DeviceProfile {
    DeviceProfile {
        name: "4x10 HD",
        hw_id: 1,
        dsp_version: 0,
        dialect: Dialect::OLD,
        sources: &SOURCES_4X10HD,
        inputs: 4,
        outputs: 10,
        presets: 4,
        dsp_table: Some(&LEGACY_4X10HD),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: false,
        has_fir: false,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: false,
        has_unreliable_selector_readback: false,
    }
}

fn profile_10x10hd() -> DeviceProfile {
    DeviceProfile {
        name: "10x10 HD",
        hw_id: 1,
        dsp_version: 51,
        dialect: Dialect::OLD,
        sources: &SOURCES_4X10HD,
        inputs: 10,
        outputs: 10,
        presets: 4,
        dsp_table: Some(&LEGACY_10X10HD),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: false,
        has_fir: false,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: false,
        has_unreliable_selector_readback: false,
    }
}

fn profile_nanodigi_2x8() -> DeviceProfile {
    DeviceProfile {
        name: "NanoDigi 2x8",
        hw_id: 2,
        dsp_version: 54,
        dialect: Dialect::OLD,
        sources: &SOURCES_LEGACY_2,
        inputs: 4,
        outputs: 10,
        presets: 4,
        dsp_table: Some(&LEGACY_NANODIGI_2X8),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: false,
        has_fir: false,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: false,
        has_unreliable_selector_readback: false,
    }
}

fn profile_2x4_nonhd() -> DeviceProfile {
    DeviceProfile {
        name: "miniDSP 2x4",
        hw_id: 2,
        dsp_version: 22,
        dialect: Dialect::OLD,
        sources: &SOURCES_LEGACY_2,
        inputs: 2,
        outputs: 4,
        presets: 4,
        dsp_table: Some(&LEGACY_M2X4),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: false,
        has_fir: false,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: false,
        has_unreliable_selector_readback: false,
    }
}

fn profile_msharc_4x8() -> DeviceProfile {
    DeviceProfile {
        name: "miniSHARC 4x8",
        hw_id: 4,
        dsp_version: 0,
        dialect: Dialect::NEW,
        sources: &SOURCES_4X10HD,
        inputs: 4,
        outputs: 8,
        presets: 4,
        dsp_table: Some(&LEGACY_MSHARC_4X8),
        tier: CapabilityTier::Full,
        sample_rate: 96_000,
        has_dirac: false,
        has_fir: true,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: false,
        has_unreliable_selector_readback: false,
    }
}

fn profile_ddrc_88bm() -> DeviceProfile {
    DeviceProfile {
        name: "DDRC-88BM",
        hw_id: 6,
        dsp_version: 95,
        dialect: Dialect::NEW,
        sources: &SOURCES_LEGACY_2,
        inputs: 8,
        outputs: 8,
        presets: 4,
        dsp_table: Some(&LEGACY_DDRC_88BM),
        tier: CapabilityTier::Full,
        sample_rate: 48_000,
        has_dirac: true,
        has_fir: false,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: false,
        has_unreliable_selector_readback: false,
    }
}

fn profile_cdsp_8x12v2() -> DeviceProfile {
    DeviceProfile {
        name: "C-DSP 8x12 v2",
        hw_id: 11,
        dsp_version: 97,
        dialect: Dialect::NEW,
        sources: &SOURCES_LEGACY_2,
        inputs: 8,
        outputs: 12,
        presets: 4,
        dsp_table: Some(&LEGACY_CDSP_8X12_V2),
        tier: CapabilityTier::Full,
        sample_rate: 96_000,
        has_dirac: false,
        has_fir: false,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: false,
        has_unreliable_selector_readback: false,
    }
}

fn profile_nanoavr() -> DeviceProfile {
    DeviceProfile {
        name: "nanoAVR",
        hw_id: 7,
        dsp_version: 0,
        dialect: Dialect::OLD,
        sources: &SOURCES_LEGACY_2,
        inputs: 0,
        outputs: 0,
        presets: 4,
        dsp_table: None,
        tier: CapabilityTier::Universal,
        sample_rate: 48_000,
        has_dirac: false,
        has_fir: false,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: false,
        has_unreliable_selector_readback: false,
    }
}

fn profile_nanoavr_hda() -> DeviceProfile {
    DeviceProfile {
        name: "nanoAVR HDA",
        hw_id: 8,
        dsp_version: 0,
        dialect: Dialect::OLD,
        sources: &SOURCES_LEGACY_2,
        inputs: 0,
        outputs: 0,
        presets: 4,
        dsp_table: None,
        tier: CapabilityTier::Universal,
        sample_rate: 48_000,
        has_dirac: false,
        has_fir: false,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: false,
        has_unreliable_selector_readback: false,
    }
}

fn profile_generic(hw_id: u8, dsp_version: u8) -> DeviceProfile {
    DeviceProfile {
        name: "Unknown MiniDSP",
        hw_id,
        dsp_version,
        dialect: Dialect::NEW,
        sources: &SOURCES_2X4HD,
        inputs: 2,
        outputs: 4,
        presets: 4,
        dsp_table: None,
        tier: CapabilityTier::Universal,
        sample_rate: 48_000,
        has_dirac: false,
        has_fir: false,
        has_display: false,
        has_dre: false,
        has_volume_offsets: false,
        has_usb_audio: false,
        has_unreliable_selector_readback: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_2x4hd() {
        let p = identify(10, 100);
        assert_eq!(p.name, "2x4HD");
        assert_eq!(p.dialect, Dialect::NEW);
        assert_eq!(p.outputs, 4);
        assert!(p.dsp_table.is_some());
        assert_eq!(p.tier, CapabilityTier::Full);
    }

    #[test]
    fn identify_flex_eight() {
        let p = identify(30, 110);
        assert_eq!(p.name, "Flex Eight");
        assert_eq!(p.outputs, 8);
        assert!(p.has_fir);
        assert!(!p.has_dirac);
    }

    #[test]
    fn identify_flex_ht_old() {
        let p = identify(31, 113);
        assert_eq!(p.name, "Flex HT");
        assert!(p.has_dirac);
        assert_eq!(p.inputs, 8);
    }

    #[test]
    fn identify_flex_ht_new() {
        let p = identify(31, 115);
        assert_eq!(p.name, "Flex HT");
        assert!(!p.has_dirac); // Dirac replaced by input PEQ
        assert!(p.dsp_table.unwrap().inputs.len() == 8); // Has input PEQ
    }

    #[test]
    fn identify_shd_power() {
        let p = identify(21, 105);
        assert_eq!(p.name, "SHD Power");
        assert_eq!(p.sources.count(), 5); // No analog inputs
    }

    #[test]
    fn identify_shd_studio_variant_18() {
        let p = identify(18, 105);
        assert_eq!(p.name, "SHD Studio");
        assert_eq!(p.sources.count(), 5);
        assert_eq!(p.hw_id, 18);
    }

    #[test]
    fn identify_unknown() {
        let p = identify(99, 99);
        assert_eq!(p.name, "Unknown MiniDSP");
        assert_eq!(p.tier, CapabilityTier::Universal);
    }

    #[test]
    fn all_sharc_devices_have_full_tier() {
        let sharc_devices = [
            (10, 100),
            (10, 101),
            (27, 100),
            (27, 101),
            (14, 105),
            (17, 105),
            (18, 105),
            (21, 105),
            (30, 110),
            (30, 111),
            (31, 113),
            (31, 115),
            (32, 113),
            (32, 115),
        ];
        for (hw, dsp) in sharc_devices {
            let p = identify(hw, dsp);
            assert_eq!(
                p.tier,
                CapabilityTier::Full,
                "device hw_id={hw} dsp={dsp} ({}) should be Full tier",
                p.name
            );
        }
    }

    /// Every known device profile has consistent internal state.
    #[test]
    fn all_profiles_are_internally_consistent() {
        let all_devices: &[(u8, u8)] = &[
            // Modern SHARC
            (10, 100),
            (10, 101),
            (27, 100),
            (27, 101),
            (14, 105),
            (17, 105),
            (18, 105),
            (21, 105),
            (30, 110),
            (30, 111),
            (31, 113),
            (31, 115),
            (32, 113),
            (32, 115),
            // Legacy
            (1, 0),
            (1, 51),
            (2, 54),
            (2, 22),
            (4, 0),
            (6, 95),
            (11, 97),
            // Basic
            (7, 0),
            (8, 0),
            // Unknown
            (99, 99),
        ];
        for &(hw, dsp) in all_devices {
            let p = identify(hw, dsp);
            // Every profile must have a name
            assert!(!p.name.is_empty(), "({hw},{dsp}) missing name");
            // Full tier profiles must have a DSP table
            if p.tier == CapabilityTier::Full {
                assert!(
                    p.dsp_table.is_some(),
                    "({hw},{dsp}) {} is Full tier but has no DSP table",
                    p.name
                );
            }
            // If DSP table exists, table entries must not exceed profile channel counts.
            // Table inputs/outputs can be fewer than profile channels (e.g. no input PEQ
            // on DDRC-24 despite having 2 physical input channels).
            if let Some(table) = p.dsp_table {
                assert!(
                    table.inputs.len() <= p.inputs as usize,
                    "({hw},{dsp}) {} has more DSP table inputs ({}) than profile channels ({})",
                    p.name,
                    table.inputs.len(),
                    p.inputs
                );
                assert_eq!(
                    table.outputs.len(),
                    p.outputs as usize,
                    "({hw},{dsp}) {} output count mismatch: profile={} table={}",
                    p.name,
                    p.outputs,
                    table.outputs.len()
                );
            }
            // Must have at least 1 preset
            assert!(p.presets >= 1, "({hw},{dsp}) {} has 0 presets", p.name);
            // New dialect devices use Float32
            if p.dialect == Dialect::NEW {
                assert_eq!(
                    p.dialect.value_format,
                    crate::dialect::ValueFormat::Float32,
                    "({hw},{dsp}) {} NEW dialect should be Float32",
                    p.name
                );
            }
            // unreliable_selector_readback should only be true for DSP100
            if p.has_unreliable_selector_readback {
                assert_eq!(
                    p.dsp_version, 100,
                    "({hw},{dsp}) {} has unreliable_selector_readback but dsp_version != 100",
                    p.name
                );
            }
            // USB audio devices should use new dialect
            if p.has_usb_audio {
                assert_eq!(
                    p.dialect,
                    Dialect::NEW,
                    "({hw},{dsp}) {} has USB audio but old dialect",
                    p.name
                );
            }
        }
    }

    /// Legacy devices use old dialect (except SHARC-era legacy).
    #[test]
    fn legacy_adau_devices_use_old_dialect() {
        let legacy_adau = [(1, 0), (1, 51), (2, 54), (2, 22)];
        for (hw, dsp) in legacy_adau {
            let p = identify(hw, dsp);
            assert_eq!(
                p.dialect,
                Dialect::OLD,
                "({hw},{dsp}) {} should use old dialect",
                p.name
            );
            assert!(
                !p.has_usb_audio,
                "({hw},{dsp}) {} should not have USB audio",
                p.name
            );
            assert!(
                !p.has_unreliable_selector_readback,
                "({hw},{dsp}) {} should not have unreliable selector readback",
                p.name
            );
        }
    }

    /// SHARC-era legacy devices (miniSHARC, DDRC-88BM, C-DSP 8x12 v2) use new dialect.
    #[test]
    fn sharc_era_legacy_devices_use_new_dialect() {
        let sharc_legacy = [(4, 0), (6, 95), (11, 97)];
        for (hw, dsp) in sharc_legacy {
            let p = identify(hw, dsp);
            assert_eq!(
                p.dialect,
                Dialect::NEW,
                "({hw},{dsp}) {} should use new dialect",
                p.name
            );
        }
    }
}
