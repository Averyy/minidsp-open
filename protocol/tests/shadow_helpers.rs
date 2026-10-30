//! Shared pure helpers, checked against both original clients before extraction.
//! The temporary host equivalence harness used the actual desktop helpers and
//! verbatim ESP32 function bodies, since the ESP32 crate is not host-testable.
//! No divergences were found, so only shared-helper contract tests remain.

use minidsp_protocol::commands::{self, HardwareId};
use minidsp_protocol::device::{dsp, probe};
use minidsp_protocol::dialect::{self, AddrLen, Dialect, ValueFormat};
use minidsp_protocol::shadow::*;
use minidsp_protocol::status::{self, DeviceVersion};
use std::collections::HashSet;

fn next(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *seed
}

#[test]
fn shadow_defaults_cover_every_profile_peq_and_crossover() {
    // Exhaust the identification inputs, including unknown and basic profiles.
    for hw in 0..=u8::MAX {
        for dsp in 0..=u8::MAX {
            let profile = probe::identify(hw, dsp);
            let shadow = build_peq_shadow(&profile);
            let mut expected = HashSet::new();
            if let Some(table) = profile.dsp_table {
                expected.extend(
                    table
                        .inputs
                        .iter()
                        .flat_map(|i| i.peq.iter().map(|b| b.addr)),
                );
                for output in table.outputs {
                    expected.extend(output.peq.iter().map(|b| b.addr));
                    if let Some(crossover) = output.crossover {
                        for offset in [0, 5, 10, 15] {
                            expected.insert(crossover.lpf_addr + offset);
                            expected.insert(crossover.hpf_addr + offset);
                        }
                    }
                }
            }
            assert_eq!(shadow.keys().copied().collect::<HashSet<_>>(), expected);
            for entry in shadow.values() {
                assert_eq!(
                    entry.coeffs.map(f32::to_bits),
                    [1.0f32.to_bits(), 0, 0, 0, 0]
                );
                assert!(!entry.bypass);
            }
        }
    }
}

#[test]
fn scalar_sanitation_preserves_boundaries_and_special_values() {
    for raw in [0, 1, 2, 0x4a03_9901, u32::MAX] {
        assert_eq!(sanitize_shadow_preferred_scalar(raw), Some(0.0));
    }
    for value in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        -f32::MAX,
    ] {
        assert_eq!(sanitize_shadow_preferred_scalar(value.to_bits()), Some(0.0));
    }
    for value in [-10_000.0f32, 10_000.0, -9.5, 9.5, -0.0, f32::MIN_POSITIVE] {
        assert_eq!(sanitize_shadow_preferred_scalar(value.to_bits()), None);
    }
    for raw in [10_000.0f32.to_bits() + 1, (-10_000.0f32).to_bits() + 1] {
        assert_eq!(sanitize_shadow_preferred_scalar(raw), Some(0.0));
    }
    assert_eq!(sanitize_shadow_preferred_scalar(3), None);

    let mut seed = 0xdead_beef;
    for _ in 0..100_000 {
        let raw = next(&mut seed);
        match sanitize_shadow_preferred_scalar(raw) {
            Some(value) => {
                assert_eq!(value.to_bits(), 0);
                assert!(
                    raw <= 2
                        || !f32::from_bits(raw).is_finite()
                        || f32::from_bits(raw).abs() > 10_000.0
                );
            }
            None => {
                assert!(raw > 2);
                assert!(f32::from_bits(raw).is_finite());
                assert!(f32::from_bits(raw).abs() <= 10_000.0);
            }
        }
    }
}

#[test]
fn raw_biquad_writes_decode_coefficients_bit_exactly() {
    let mut seed = 0x2352_abcd;
    for dialect in [Dialect::NEW, Dialect::OLD] {
        for i in 0..512 {
            let addr = next(&mut seed) as u16;
            let coeffs = if i == 0 {
                [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0, 1.0]
            } else {
                std::array::from_fn(|_| f32::from_bits(next(&mut seed)))
            };
            let mut payload = commands::write_biquad(addr, &coeffs, &dialect);
            let expected = coeffs.map(|v| {
                dialect::decode_value(&dialect::encode_value(v, &dialect), &dialect).to_bits()
            });
            for len in 0..26 {
                assert!(parse_raw_write_biquad(&payload[..len], &dialect).is_none());
            }
            let (parsed_addr, parsed) = parse_raw_write_biquad(&payload, &dialect).unwrap();
            // Both original clients retain the legacy address write flag.
            let expected_addr = if dialect == Dialect::OLD {
                addr | 0x8000
            } else {
                addr
            };
            assert_eq!(parsed_addr, expected_addr);
            assert_eq!(parsed.map(f32::to_bits), expected);
            payload.extend_from_slice(&[0xaa, 0xff]);
            assert_eq!(
                parse_raw_write_biquad(&payload, &dialect)
                    .unwrap()
                    .1
                    .map(f32::to_bits),
                expected
            );
            payload[1] = 0x81;
            assert!(parse_raw_write_biquad(&payload, &dialect).is_none());
            payload[1] = 0x80;
            payload[0] = commands::CMD_WRITE_DSP;
            assert!(parse_raw_write_biquad(&payload, &dialect).is_none());
        }
    }
}

#[test]
fn raw_bypass_writes_keep_the_flag_and_primary_address() {
    let mut seed = 0xcafe_babe;
    for ctrl in 0..=u8::MAX {
        let addr = next(&mut seed) as u16;
        let payload = [
            commands::CMD_BYPASS_FILTER,
            ctrl,
            (addr >> 8) as u8,
            addr as u8,
            0xaa,
            0xff,
        ];
        assert_eq!(
            parse_raw_bypass_filter(&payload),
            Some((addr, ctrl >= 0x80))
        );
        for len in 0..6 {
            assert!(parse_raw_bypass_filter(&payload[..len]).is_none());
        }
    }
    assert!(parse_raw_bypass_filter(&[0xff; 6]).is_none());
    // Preserve the original parsers' minimum length, even for legacy commands.
    assert!(parse_raw_bypass_filter(&commands::bypass_filter(24, true, &Dialect::OLD)).is_none());
}

#[test]
fn raw_selector_writes_require_persisted_padded_sharc_values() {
    let mut seed = 0x1234_5678;
    for i in 0..1024 {
        let addr = next(&mut seed) as u16;
        let value = [0, 1, 2, u16::MAX, next(&mut seed) as u16][i % 5];
        let mut payload = commands::write_dsp_int(addr, value);
        assert_eq!(
            parse_raw_selector_write(&payload, &Dialect::NEW),
            Some((addr, value))
        );
        assert!(parse_raw_selector_write(&payload, &Dialect::OLD).is_none());
        for len in 0..8 {
            assert!(parse_raw_selector_write(&payload[..len], &Dialect::NEW).is_none());
        }
        payload[6] = 1;
        assert!(parse_raw_selector_write(&payload, &Dialect::NEW).is_none());
        payload[6] = 0;
        payload[7] = 1;
        assert!(parse_raw_selector_write(&payload, &Dialect::NEW).is_none());
        payload[7] = 0;
        payload[1] = 0xa0;
        assert!(parse_raw_selector_write(&payload, &Dialect::NEW).is_none());
        payload[1] = 0x80;
        payload[0] = 0xff;
        assert!(parse_raw_selector_write(&payload, &Dialect::NEW).is_none());
    }
}

#[test]
fn raw_linked_selectors_keep_primary_address_and_low_value_bits() {
    let mut seed = 0x9876_5432;
    for count in 0..=127 {
        for value in [0, 1, 2, 0xffff, 0xabcd_1234, u32::MAX, next(&mut seed)] {
            let addr = next(&mut seed) as u16;
            let linked: Vec<_> = (0..count).map(|_| next(&mut seed) as u16).collect();
            let mut payload = commands::write_dsp_linked_u32(addr, value, &linked);
            assert_eq!(
                parse_raw_linked_selector_write(&payload, &Dialect::NEW),
                Some((addr, value as u16))
            );
            assert!(parse_raw_linked_selector_write(&payload, &Dialect::OLD).is_none());
            for len in 0..payload.len() {
                assert!(parse_raw_linked_selector_write(&payload[..len], &Dialect::NEW).is_none());
            }
            payload.push(0xff);
            assert_eq!(
                parse_raw_linked_selector_write(&payload, &Dialect::NEW),
                Some((addr, value as u16))
            );
            payload[1] = 0x7f;
            assert!(parse_raw_linked_selector_write(&payload, &Dialect::NEW).is_none());
            payload[1] = 0x80;
            payload[0] = 0xff;
            assert!(parse_raw_linked_selector_write(&payload, &Dialect::NEW).is_none());
        }
    }
}

#[test]
fn arbitrary_payloads_are_safe_in_all_dialects() {
    let mut seed = 0x4321_dcba;
    for len in 0..=264 {
        for opcode in [
            0xff,
            commands::CMD_WRITE_DSP,
            commands::CMD_WRITE_BIQUAD,
            commands::CMD_BYPASS_FILTER,
        ] {
            let mut payload: Vec<_> = (0..len).map(|_| next(&mut seed) as u8).collect();
            if let Some(first) = payload.first_mut() {
                *first = opcode;
            }
            for addr_len in [AddrLen::Two, AddrLen::Three] {
                for value_format in [ValueFormat::FixedPoint, ValueFormat::Float32] {
                    let dialect = Dialect {
                        addr_len,
                        value_format,
                    };
                    let _ = parse_raw_write_biquad(&payload, &dialect);
                    let _ = parse_raw_bypass_filter(&payload);
                    let _ = parse_raw_selector_write(&payload, &dialect);
                    let _ = parse_raw_linked_selector_write(&payload, &dialect);
                }
            }
        }
    }
}

#[test]
fn device_info_keeps_identity_product_name_and_transport_url() {
    let mut seed = 0x0123_4567;
    for hw_id in 0..=u8::MAX {
        for dsp_version in 0..=u8::MAX {
            let hw = HardwareId {
                hw_id,
                fw_major: next(&mut seed) as u8,
                fw_minor: next(&mut seed) as u8,
            };
            let serial = next(&mut seed);
            let url = format!("usb://0x2752:0x{:04x}", next(&mut seed) as u16);
            let info = status::device_info(url.clone(), &hw, dsp_version, serial);
            assert_eq!(info.url, url);
            assert_eq!(
                info.version,
                DeviceVersion {
                    hw_id,
                    fw_major: hw.fw_major,
                    fw_minor: hw.fw_minor,
                    dsp_version,
                    serial
                }
            );
            assert_eq!(info.product_name, probe::product_name(hw_id, dsp_version));
        }
    }
    let hw = HardwareId {
        hw_id: 10,
        fw_major: 1,
        fw_minor: 2,
    };
    for url in [
        "",
        "tcp://localhost:5333",
        "ws://host/path",
        "mock://unicode/é",
    ] {
        let info = status::device_info(url.into(), &hw, 100, u32::MAX);
        assert_eq!(info.url, url);
        assert_eq!(info.product_name, "2x4HD");
    }
}

#[test]
fn default_peq_frequencies_match_official_labels() {
    assert_eq!(
        dsp::default_peq_frequencies(),
        &[20, 40, 80, 160, 320, 640, 1280, 2560, 5120, 10240]
    );
    assert!(std::ptr::eq(
        dsp::default_peq_frequencies(),
        dsp::default_peq_frequencies()
    ));
}
