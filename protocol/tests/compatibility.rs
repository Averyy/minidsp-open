//! Compatibility tests — verify protocol behavior against minidsp-rs contract.
//!
//! These tests verify that our protocol implementation produces the exact same
//! wire bytes and parses the exact same responses as minidsp-rs would.
//! See docs/ref-compatibility.md for the canonical contract.

use minidsp_protocol::commands::*;
use minidsp_protocol::dialect::Dialect;
use minidsp_protocol::packet;
use minidsp_protocol::device::probe;
use minidsp_protocol::eeprom;
use minidsp_protocol::source::Source;
use minidsp_protocol::biquad;

// ============================================================
// CLI output format compatibility
// ============================================================

#[test]
fn jsonline_output_format() {
    // minidsp -o jsonline must produce this exact structure:
    // {"master":{"preset":0,"source":"Toslink","volume":-8.0,"mute":false},"input_levels":[-131.36],"output_levels":[-140.2]}
    //
    // Key requirements:
    // - "source" is PascalCase (Toslink, not toslink)
    // - "volume" is float (includes .0 for integers)
    // - "mute" is boolean
    // - "dirac" present only for Dirac devices, omitted otherwise
    // - levels are float arrays

    // Verify source PascalCase serialization
    assert_eq!(Source::Toslink.pascal_case(), "Toslink");
    assert_eq!(Source::Spdif.pascal_case(), "Spdif");
    assert_eq!(Source::Usb.pascal_case(), "Usb");
    assert_eq!(Source::AesEbu.pascal_case(), "Aesebu");
    assert_eq!(Source::Hdmi.pascal_case(), "Hdmi");
    assert_eq!(Source::Bluetooth.pascal_case(), "Bluetooth");
    assert_eq!(Source::Lan.pascal_case(), "Lan");
}

#[test]
fn source_lowercase_parsing() {
    // CLI and URL paths use lowercase: /source/toslink
    assert_eq!(Source::from_str_lowercase("toslink"), Some(Source::Toslink));
    assert_eq!(Source::from_str_lowercase("spdif"), Some(Source::Spdif));
    assert_eq!(Source::from_str_lowercase("usb"), Some(Source::Usb));
    assert_eq!(Source::from_str_lowercase("aesebu"), Some(Source::AesEbu));
    assert_eq!(Source::from_str_lowercase("hdmi"), Some(Source::Hdmi));
    assert_eq!(Source::from_str_lowercase("analog"), Some(Source::Analog));
    assert_eq!(Source::from_str_lowercase("bluetooth"), Some(Source::Bluetooth));
    // RCA/XLR aliases (SHD)
    assert_eq!(Source::from_str_lowercase("rca"), Some(Source::AnalogSingleEnd));
    assert_eq!(Source::from_str_lowercase("xlr"), Some(Source::AnalogBalanced));
}

#[test]
fn source_pascal_parsing() {
    // JSON body uses PascalCase
    assert_eq!(Source::from_str_pascal("Toslink"), Some(Source::Toslink));
    assert_eq!(Source::from_str_pascal("Spdif"), Some(Source::Spdif));
    assert_eq!(Source::from_str_pascal("Usb"), Some(Source::Usb));
    assert_eq!(Source::from_str_pascal("Aesebu"), Some(Source::AesEbu));
}

// ============================================================
// Probe output format compatibility
// ============================================================

#[test]
fn probe_output_format() {
    // minidsp probe must produce:
    // 0: Found 2x4HD with serial 912345 at usb:... [hw_id: 10, dsp_version: 100]
    let name = probe::product_name(10, 100);
    assert_eq!(name, "2x4HD");

    let name = probe::product_name(27, 100);
    assert_eq!(name, "Flex");

    let name = probe::product_name(14, 105);
    assert_eq!(name, "SHD");

    let name = probe::product_name(30, 110);
    assert_eq!(name, "Flex Eight");

    let name = probe::product_name(31, 113);
    assert_eq!(name, "Flex HT");

    let name = probe::product_name(32, 113);
    assert_eq!(name, "Flex HTx");
}

// ============================================================
// Protocol wire format compatibility
// ============================================================

#[test]
fn frame_encoding_matches_minidsp_rs() {
    // Frame: [length(self-inclusive)] [payload] [checksum]
    // ReadHardwareId: payload = [0x31]
    // length = 2 (1 + 1), checksum = (2 + 0x31) & 0xFF = 0x33
    let frame = packet::encode_frame(&[0x31]);
    assert_eq!(frame, vec![0x02, 0x31, 0x33]);
}

#[test]
fn hid_report_format() {
    // HID report: [0x00 report_id] [frame] [0xFF padding to 65 bytes]
    let report = packet::encode_hid(&[0x31]);
    assert_eq!(report[0], 0x00); // report ID
    assert_eq!(report[1], 0x02); // frame length
    assert_eq!(report[2], 0x31); // payload
    assert_eq!(report[3], 0x33); // checksum
    // Rest is 0xFF padding
    for i in 4..65 {
        assert_eq!(report[i], 0xFF, "byte {i} should be 0xFF padding");
    }
    assert_eq!(report.len(), 65);
}

#[test]
fn hid_response_uses_length_only_without_checksum() {
    let data = [
        0x0c, 0x31, 0x01, 0x55, 0x0a, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x21, 0xf6, 0x08, 0x5b,
    ];
    let payload = packet::decode_hid_response(&data).unwrap();
    assert_eq!(payload, vec![0x31, 0x01, 0x55, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn hid_ack_response_can_be_single_length_byte() {
    let data = [0x01, 0x42, 0x14, 0xfe, 0x30, 0x39, 0x00, 0x00];
    let payload = packet::decode_hid_response(&data).unwrap();
    assert!(payload.is_empty());
}

#[test]
fn set_volume_wire_format() {
    // SetVolume: [0x42, value]
    // value=20 means -10.0 dB (gain = -0.5 * value)
    let cmd = set_volume(20);
    assert_eq!(cmd, vec![0x42, 20]);

    // Verify dB conversion
    let raw = eeprom::MasterStatusRaw {
        preset: 0, source: 0, volume: 20, mute: 0,
    };
    assert!((raw.volume_db() - (-10.0)).abs() < 0.01);
}

#[test]
fn set_config_wire_format() {
    // SetConfig: [0x25, preset, reset_flag]
    let cmd = set_config(2, 0);
    assert_eq!(cmd, vec![0x25, 0x02, 0x00]);
}

#[test]
fn write_dsp_new_dialect_wire_format() {
    // WriteDSP new dialect: [0x13, 0x80(save), addr_hi|0x80, addr_lo, 0x00, value_le_4bytes]
    let cmd = write_dsp(0x0042, 1.0, true, &Dialect::NEW);
    assert_eq!(cmd[0], 0x13); // command
    assert_eq!(cmd[1], 0x80); // save flag
    assert_eq!(cmd[2], 0x80); // addr_hi with write bit
    assert_eq!(cmd[3], 0x42); // addr_lo
    assert_eq!(cmd[4], 0x00); // addr byte 3
    assert_eq!(&cmd[5..9], &1.0f32.to_le_bytes()); // value as float32 LE
}

#[test]
fn write_biquad_wire_format() {
    // WriteBiquad: [0x30, 0x80, addr(3 bytes), 0x00, 0x00, 5×float32]
    let bypass_coeffs = [1.0, 0.0, 0.0, 0.0, 0.0];
    let cmd = write_biquad(0x0100, &bypass_coeffs, &Dialect::NEW);
    assert_eq!(cmd[0], 0x30); // command
    assert_eq!(cmd[1], 0x80); // flag
    // addr: 0x0100 with write bit
    assert_eq!(cmd[2], 0x81); // addr_hi | 0x80
    assert_eq!(cmd[3], 0x00); // addr_lo
    assert_eq!(cmd[4], 0x00); // addr byte 3
    // padding
    assert_eq!(cmd[5], 0x00);
    assert_eq!(cmd[6], 0x00);
    // 5 coefficients × 4 bytes = 20 bytes
    assert_eq!(cmd.len(), 7 + 20);
    // First coeff (b0=1.0) in LE
    assert_eq!(&cmd[7..11], &1.0f32.to_le_bytes());
}

#[test]
fn read_floats_wire_format() {
    // ReadFloats: [0x14, addr_hi, addr_lo, addr_byte3, count]
    let cmd = read_floats(0x0FC0, 4, &Dialect::NEW);
    assert_eq!(cmd[0], 0x14);
    assert_eq!(cmd[1], 0x0F); // addr_hi (no write bit for reads)
    assert_eq!(cmd[2], 0xC0); // addr_lo
    assert_eq!(cmd[3], 0x00); // byte 3
    assert_eq!(cmd[4], 4);    // count
}

#[test]
fn read_floats_count_capped_at_14() {
    let cmd = read_floats(0x0000, 20, &Dialect::NEW);
    assert_eq!(cmd[cmd.len() - 1], 14); // capped
}

// ============================================================
// Response parsing compatibility
// ============================================================

#[test]
fn hardware_id_response_parsing() {
    // Response: [0x31, fw_major, fw_minor, hw_id]
    let resp = parse_response(&[0x31, 1, 85, 10], &Dialect::NEW);
    match resp {
        Response::HardwareId(id) => {
            assert_eq!(id.fw_major, 1);
            assert_eq!(id.fw_minor, 85);
            assert_eq!(id.hw_id, 10);
        }
        _ => panic!("expected HardwareId"),
    }
}

#[test]
fn flash_data_response_parsing() {
    // Response: [0x05, data...]
    let resp = parse_response(&[0x05, 0x00, 0x02, 0x00, 0x00], &Dialect::NEW);
    match resp {
        Response::FlashData(data) => {
            assert_eq!(data, vec![0x00, 0x02, 0x00, 0x00]);
        }
        _ => panic!("expected FlashData"),
    }
}

#[test]
fn preset_complete_response() {
    let resp = parse_response(&[0xAB], &Dialect::NEW);
    assert_eq!(resp, Response::PresetComplete);
}

#[test]
fn reset_complete_response() {
    let resp = parse_response(&[0xAA], &Dialect::NEW);
    assert_eq!(resp, Response::ResetComplete);
}

#[test]
fn overflow_response() {
    let resp = parse_response(&[0xFF], &Dialect::NEW);
    assert_eq!(resp, Response::Overflow);
}

// ============================================================
// EEPROM parsing compatibility
// ============================================================

#[test]
fn master_status_eeprom_byte_order() {
    // EEPROM 0xFFD8 returns: [preset, source, volume, mute]
    // Verified against miniDSP 2x4 HD simulator
    let data = [0x02, 0x01, 30, 0x01]; // preset=2, source=1(toslink), volume=30(-15dB), muted
    let status = eeprom::MasterStatusRaw::from_bytes(&data).unwrap();
    assert_eq!(status.preset, 2);
    assert_eq!(status.source, 1);
    assert_eq!(status.volume, 30);
    assert!(status.is_muted());
    assert!((status.volume_db() - (-15.0)).abs() < 0.01);
}

#[test]
fn serial_number_encoding() {
    // Serial at 0xFFFE: big-endian u16, actual = value + 900000
    let data = [0x30, 0x39]; // 0x3039 = 12345
    assert_eq!(eeprom::parse_serial(&data), Some(912345));
}

// ============================================================
// Device identification compatibility
// ============================================================

#[test]
fn all_minidsp_rs_probe_mappings() {
    // These must match minidsp-rs dev branch probe.rs exactly
    assert_eq!(probe::identify(10, 100).name, "2x4HD");
    assert_eq!(probe::identify(10, 101).name, "DDRC-24");
    assert_eq!(probe::identify(27, 100).name, "Flex");
    assert_eq!(probe::identify(27, 101).name, "Flex (DL)");
    assert_eq!(probe::identify(14, 105).name, "SHD");
    assert_eq!(probe::identify(32, 113).name, "Flex HTx");
}

#[test]
fn new_devices_not_in_minidsp_rs() {
    // These are NEW — we support them, minidsp-rs doesn't
    assert_eq!(probe::identify(17, 105).name, "SHD Studio");
    assert_eq!(probe::identify(21, 105).name, "SHD Power");
    assert_eq!(probe::identify(30, 110).name, "Flex Eight");
    assert_eq!(probe::identify(30, 111).name, "Flex Eight (DL)");
    assert_eq!(probe::identify(31, 113).name, "Flex HT");
    assert_eq!(probe::identify(31, 115).name, "Flex HT");
    assert_eq!(probe::identify(32, 115).name, "Flex HTx");
}

#[test]
fn legacy_device_probe_mappings() {
    assert_eq!(probe::identify(1, 51).name, "10x10 HD");
    assert_eq!(probe::identify(1, 0).name, "4x10 HD");
    assert_eq!(probe::identify(2, 54).name, "NanoDigi 2x8");
    assert_eq!(probe::identify(2, 22).name, "miniDSP 2x4");
    assert_eq!(probe::identify(4, 0).name, "miniSHARC 4x8");
    assert_eq!(probe::identify(6, 95).name, "DDRC-88BM");
    assert_eq!(probe::identify(11, 97).name, "C-DSP 8x12 v2");
}

#[test]
fn unknown_device_returns_generic() {
    let p = probe::identify(99, 99);
    assert_eq!(p.name, "Unknown MiniDSP");
    assert_eq!(p.tier, minidsp_protocol::device::CapabilityTier::Universal);
}

// ============================================================
// Source mapping compatibility
// ============================================================

#[test]
fn source_maps_match_docs() {
    use minidsp_protocol::source::*;

    // hw_id 10 (2x4 HD): 0:Analog, 1:Toslink, 2:USB
    assert_eq!(SOURCES_2X4HD.source_for_index(0), Some(Source::Analog));
    assert_eq!(SOURCES_2X4HD.source_for_index(1), Some(Source::Toslink));
    assert_eq!(SOURCES_2X4HD.source_for_index(2), Some(Source::Usb));
    assert_eq!(SOURCES_2X4HD.count(), 3);

    // hw_id 27 (Flex): 0:Analog, 1:Toslink, 2:SPDIF, 3:USB, 4:Bluetooth
    assert_eq!(SOURCES_FLEX.source_for_index(0), Some(Source::Analog));
    assert_eq!(SOURCES_FLEX.source_for_index(4), Some(Source::Bluetooth));
    assert_eq!(SOURCES_FLEX.count(), 5);

    // hw_id 30 (Flex Eight): NO analog, starts at 1
    assert_eq!(SOURCES_FLEX_EIGHT.source_for_index(0), None);
    assert_eq!(SOURCES_FLEX_EIGHT.source_for_index(1), Some(Source::Toslink));
    assert_eq!(SOURCES_FLEX_EIGHT.count(), 4);

    // hw_id 14 (SHD): 7 sources including analog balanced
    assert_eq!(SOURCES_SHD.source_for_index(3), Some(Source::AnalogSingleEnd));
    assert_eq!(SOURCES_SHD.source_for_index(4), Some(Source::AnalogBalanced));
    assert_eq!(SOURCES_SHD.source_for_index(6), Some(Source::Lan));
    assert_eq!(SOURCES_SHD.count(), 7);

    // hw_id 17 (SHD Studio): 5 sources (no analog)
    assert_eq!(SOURCES_SHD_STUDIO.count(), 5);
    assert_eq!(SOURCES_SHD_STUDIO.source_for_index(3), Some(Source::Usb));

    // hw_id 21 (SHD Power): 5 sources (no analog)
    assert_eq!(SOURCES_SHD_POWER.count(), 5);
}

#[test]
fn source_reverse_lookup() {
    use minidsp_protocol::source::*;

    assert_eq!(SOURCES_2X4HD.index_for_source(Source::Usb), Some(2));
    assert_eq!(SOURCES_FLEX.index_for_source(Source::Bluetooth), Some(4));
    assert_eq!(SOURCES_FLEX_EIGHT.index_for_source(Source::Analog), None); // Flex Eight has no analog
    assert_eq!(SOURCES_FLEX_HT.index_for_source(Source::Hdmi), Some(4));
}

// ============================================================
// Dialect compatibility
// ============================================================

#[test]
fn fixed_point_codec() {
    use minidsp_protocol::dialect::*;

    // 1.0 in 5.23 fixed-point = 0x00800000
    assert_eq!(float_to_fixed_point(1.0), 0x00800000);
    assert!((fixed_point_to_float(0x00800000) - 1.0).abs() < 0.0001);

    // -1.0 = 0xFF800000
    assert_eq!(float_to_fixed_point(-1.0), 0xFF800000);
    assert!((fixed_point_to_float(0xFF800000) - (-1.0)).abs() < 0.0001);

    // 0.0
    assert_eq!(float_to_fixed_point(0.0), 0x00000000);
}

#[test]
fn mute_encoding_compatibility() {
    use minidsp_protocol::dialect::*;

    // New dialect: muted=2, unmuted=1 (u16 LE)
    assert_eq!(encode_mute(true, &Dialect::NEW), vec![0x02, 0x00]);
    assert_eq!(encode_mute(false, &Dialect::NEW), vec![0x01, 0x00]);

    // Old dialect: muted=0x00000000, unmuted=0x00800000 (u32 BE)
    assert_eq!(encode_mute(true, &Dialect::OLD), vec![0x00, 0x00, 0x00, 0x00]);
    assert_eq!(encode_mute(false, &Dialect::OLD), vec![0x00, 0x80, 0x00, 0x00]);

    // Decode roundtrip
    assert!(decode_mute(&encode_mute(true, &Dialect::NEW), &Dialect::NEW).unwrap());
    assert!(!decode_mute(&encode_mute(false, &Dialect::NEW), &Dialect::NEW).unwrap());
    assert!(decode_mute(&encode_mute(true, &Dialect::OLD), &Dialect::OLD).unwrap());
    assert!(!decode_mute(&encode_mute(false, &Dialect::OLD), &Dialect::OLD).unwrap());
}

// ============================================================
// Biquad compatibility
// ============================================================

#[test]
fn biquad_bypass_coefficients() {
    // Bypass: b0=1, b1=0, b2=0, a1=0, a2=0
    assert!(biquad::is_bypass(&biquad::BYPASS));
    assert!(biquad::is_stable(&biquad::BYPASS));
}

#[test]
fn typical_ezbeq_filter_is_stable() {
    // Real PEQ coefficients from an ezBEQ filter pack
    let coeffs = [1.0003468, -1.9990215, 0.9986773, -1.9990215, 0.9990241];
    assert!(biquad::is_stable(&coeffs));
    assert!(!biquad::is_bypass(&coeffs));
}

#[test]
fn negative_coefficients_accepted() {
    // minidsp-rs #625: negative coefficients must be accepted
    // These are valid coefficients with negative values
    let coeffs = [-0.5, -1.5, 0.75, -1.2, 0.6];
    // Stability doesn't matter for this test — we just verify no panic/rejection
    let _ = biquad::is_stable(&coeffs);
}

// UDP Discovery tests are in daemon/src/discovery/mod.rs (different crate boundary)

// ============================================================
// DSP address table verification
// ============================================================

#[test]
fn dsp_100_address_table_verified() {
    use minidsp_protocol::device::dsp::DSP_100;

    // Verified against minidsp-rs and Device Console XML
    assert_eq!(DSP_100.inputs.len(), 2);
    assert_eq!(DSP_100.outputs.len(), 4);

    // Input 0: mute at 4032 (from ref-address-tables.md)
    assert_eq!(DSP_100.inputs[0].mute, Some(4032));
    assert_eq!(DSP_100.inputs[0].peq.len(), 10);

    // Output 0: mute at 4144
    assert_eq!(DSP_100.outputs[0].mute, 4144);
    assert_eq!(DSP_100.outputs[0].peq.len(), 10);

    // Routing matrix: 2x4
    assert_eq!(DSP_100.routing.rows, 2);
    assert_eq!(DSP_100.routing.cols, 4);
}

#[test]
fn dsp_110_flex_eight_addresses() {
    use minidsp_protocol::device::dsp::DSP_110;

    assert_eq!(DSP_110.inputs.len(), 2);
    assert_eq!(DSP_110.outputs.len(), 8);

    // Input 0 has FIR
    assert!(DSP_110.inputs[0].fir.is_some());
    assert_eq!(DSP_110.inputs[0].fir.unwrap().max_taps, 2048);

    // Output channel stride is 103
    assert_eq!(DSP_110.outputs[1].mute - DSP_110.outputs[0].mute, 103);
}

#[test]
fn dsp_113_bass_management() {
    use minidsp_protocol::device::dsp::DSP_113;

    assert!(DSP_113.bass_management.is_some());
    let bm = DSP_113.bass_management.unwrap();
    assert_eq!(bm.global_lpf_addr, 0);
    assert_eq!(bm.global_hpf_addr, 20);
    assert_eq!(bm.per_channel_bpf.len(), 8);
}

#[test]
fn dsp_115_has_input_peq_113_does_not() {
    use minidsp_protocol::device::dsp::{DSP_113, DSP_115};

    assert_eq!(DSP_113.inputs.len(), 0); // Old FW: no input PEQ
    assert_eq!(DSP_115.inputs.len(), 8); // New FW: 8 input channels with PEQ
    assert_eq!(DSP_115.inputs[0].peq.len(), 10); // 10 bands per channel
}

#[test]
fn dsp_115_input_channels_have_no_mute_gain() {
    use minidsp_protocol::device::dsp::DSP_115;

    // DSP 115 input PEQ was added in new firmware but input
    // mute/gain/polarity addresses aren't exposed in Device Console XML
    for input in DSP_115.inputs {
        assert!(input.mute.is_none());
        assert!(input.gain.is_none());
        assert!(input.polarity.is_none());
    }
}

// ============================================================
// File execution (-f) compatibility
// ============================================================

#[test]
fn biquad_coefficient_parsing() {
    // minidsp -f format: input 0 peq 0 set -- 1.0003468 -1.9990215 0.9986773 -1.9990215 0.9990241
    // The -- separator allows negative values
    let line = "input 0 peq 0 set -- 1.0003468 -1.9990215 0.9986773 -1.9990215 0.9990241";
    let parts: Vec<&str> = line.split_whitespace().collect();

    assert_eq!(parts[0], "input");
    assert_eq!(parts[1], "0");
    assert_eq!(parts[2], "peq");
    assert_eq!(parts[3], "0");
    assert_eq!(parts[4], "set");
    assert_eq!(parts[5], "--");

    // Parse coefficients
    let b0: f32 = parts[6].parse().unwrap();
    let b1: f32 = parts[7].parse().unwrap();
    let _b2: f32 = parts[8].parse().unwrap();
    let _a1: f32 = parts[9].parse().unwrap();
    let a2: f32 = parts[10].parse().unwrap();

    assert!((b0 - 1.0003468).abs() < 0.0001);
    assert!((b1 - (-1.9990215)).abs() < 0.0001);
    assert!((a2 - 0.9990241).abs() < 0.0001);
}

// ============================================================
// TCP frame compatibility
// ============================================================

#[test]
fn tcp_frame_with_zero_padding() {
    // TCP frames may have zero-byte padding between them
    let frame = packet::encode_frame(&[0x31]);
    let mut data = vec![0x00, 0x00, 0x00]; // 3 bytes padding
    data.extend_from_slice(&frame);
    let (payload, consumed) = packet::decode_tcp_frame(&data).unwrap();
    assert_eq!(payload, vec![0x31]);
    assert_eq!(consumed, 3 + frame.len());
}

#[test]
fn tcp_response_encoding() {
    // TCP server responses: length byte + payload (no checksum)
    let resp = packet::encode_tcp_response(&[0x31, 0x01, 0x55, 0x0A]);
    assert_eq!(resp[0], 5); // self-inclusive length: 4 + 1
    assert_eq!(&resp[1..], &[0x31, 0x01, 0x55, 0x0A]);
}
