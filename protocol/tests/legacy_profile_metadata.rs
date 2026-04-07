use minidsp_protocol::device::{generated_official, probe};
use minidsp_protocol::dialect::Dialect;

#[test]
fn legacy_source_backed_profiles_match_generated_metadata() {
    for device in generated_official::OFFICIAL_LEGACY_DEVICES {
        let dsp_version = device.dsp_version.unwrap_or(0);
        let profile = probe::identify(device.hw_id, dsp_version);

        assert_eq!(
            profile.name, device.product_name,
            "legacy profile name mismatch for hw {} dsp {:?}",
            device.hw_id, device.dsp_version
        );

        let expected_dialect = match device.dialect {
            "old" => Dialect::OLD,
            "new" => Dialect::NEW,
            other => panic!("unexpected legacy dialect {other}"),
        };
        assert_eq!(
            profile.dialect, expected_dialect,
            "legacy profile dialect mismatch for {}",
            device.product_name
        );

        if device.meter_input_count > 0 {
            assert_eq!(
                profile.inputs as usize, device.meter_input_count,
                "legacy profile input count drifted from source-backed meter metadata for {}",
                device.product_name
            );
        } else if device.dialect == "old" && device.inferred_input_channels > 0 {
            assert_eq!(
                profile.inputs as usize, device.inferred_input_channels,
                "legacy profile input count drifted from source-backed inferred topology for {}",
                device.product_name
            );
        } else if matches!(device.product_name, "4x10 HD" | "10x10 HD") {
            assert_eq!(
                profile.inputs as usize, device.unique_input_label_text_count,
                "legacy profile input count drifted from source-backed label metadata for {}",
                device.product_name
            );
        }

        if device.meter_output_count > 0 {
            assert_eq!(
                profile.outputs as usize, device.meter_output_count,
                "legacy profile output count drifted from source-backed meter metadata for {}",
                device.product_name
            );
        } else if device.dialect == "old" && device.inferred_output_channels > 0 {
            assert_eq!(
                profile.outputs as usize, device.inferred_output_channels,
                "legacy profile output count drifted from source-backed inferred topology for {}",
                device.product_name
            );
        }

        assert_eq!(
            profile.has_fir,
            device.fir_count > 0,
            "legacy FIR capability mismatch for {}",
            device.product_name
        );
    }
}
