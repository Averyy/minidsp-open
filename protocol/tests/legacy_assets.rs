use minidsp_protocol::device::probe;
use serde_json::Value;

fn legacy_matrix() -> Value {
    serde_json::from_str(include_str!("../../docs/generated/device-support-matrix.json"))
        .expect("generated legacy matrix JSON should parse")
}

#[test]
fn cached_legacy_config_sources_cover_known_legacy_profiles() {
    let matrix = legacy_matrix();
    let configs = matrix["legacy_configs"]
        .as_array()
        .expect("legacy_configs should be an array");

    let expected = [
        ("4x10 HD", 1_u64, None),
        ("10x10 HD", 1_u64, Some(51_u64)),
        ("miniDSP 2x4", 2_u64, Some(22_u64)),
        ("NanoDigi 2x8", 2_u64, Some(54_u64)),
        ("miniSHARC 4x8", 4_u64, None),
        ("DDRC-88BM", 6_u64, Some(95_u64)),
        ("C-DSP 8x12 v2", 11_u64, Some(97_u64)),
    ];

    for (name, hw_id, dsp_version) in expected {
        let row = configs.iter().find(|row| {
            row["name"].as_str() == Some(name)
                && row["hw_id"].as_u64() == Some(hw_id)
                && row["dsp_version"].as_u64() == dsp_version
        });
        assert!(row.is_some(), "missing cached legacy config row for {name}");
    }
}

#[test]
fn legacy_profiles_with_cached_sources_are_not_generic() {
    let checks = [
        (1_u8, 51_u8, "10x10 HD"),
        (2_u8, 22_u8, "miniDSP 2x4"),
        (2_u8, 54_u8, "NanoDigi 2x8"),
        (6_u8, 95_u8, "DDRC-88BM"),
        (11_u8, 97_u8, "C-DSP 8x12 v2"),
    ];

    for (hw_id, dsp_version, expected_name) in checks {
        let profile = probe::identify(hw_id, dsp_version);
        assert_eq!(
            profile.name, expected_name,
            "cached legacy source should resolve to the expected probe profile for hw {hw_id} dsp {dsp_version}"
        );
    }
}
