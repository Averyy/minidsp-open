use minidsp_protocol::device::{CapabilityTier, generated_official, probe};

#[test]
fn modern_probe_coverage_matches_official_assets() {
    for device in generated_official::OFFICIAL_MODERN_DEVICES {
        let profile = probe::identify(device.hw_id, device.dsp_version);
        assert_eq!(
            profile.hw_id, device.hw_id,
            "profile hw_id mismatch for official device {} / dsp {}",
            device.hw_id, device.dsp_version
        );
        assert_eq!(
            profile.dsp_version, device.dsp_version,
            "profile dsp_version mismatch for official device {} / dsp {}",
            device.hw_id, device.dsp_version
        );
        assert_eq!(
            profile.tier, CapabilityTier::Full,
            "official modern device {} / dsp {} regressed below Full tier",
            device.hw_id, device.dsp_version
        );
        assert!(
            profile.dsp_table.is_some(),
            "official modern device {} / dsp {} is missing a dsp table",
            device.hw_id, device.dsp_version
        );
        assert_eq!(
            profile.sources.count(),
            device.source_count,
            "source-count mismatch for official device {} / dsp {} ({})",
            device.hw_id, device.dsp_version, device.product_name
        );
        assert_eq!(
            profile.has_dirac,
            generated_official::OFFICIAL_DIRAC_DSP_VERSIONS.contains(&device.dsp_version),
            "dirac flag mismatch for official device {} / dsp {} ({})",
            device.hw_id, device.dsp_version, device.product_name
        );
        assert_eq!(
            profile.has_display, device.has_display,
            "display flag mismatch for official device {} / dsp {} ({})",
            device.hw_id, device.dsp_version, device.product_name
        );
        assert_eq!(
            profile.has_dre, device.has_dre,
            "DRE flag mismatch for official device {} / dsp {} ({})",
            device.hw_id, device.dsp_version, device.product_name
        );
        assert_eq!(
            profile.has_volume_offsets, device.has_volume_offsets,
            "volume-offset flag mismatch for official device {} / dsp {} ({})",
            device.hw_id, device.dsp_version, device.product_name
        );
    }
}

#[test]
fn official_usb_product_hints_are_accepted_by_shared_usb_policy() {
    for entry in generated_official::OFFICIAL_USB_PRODUCT_HINTS {
        assert!(
            probe::is_official_usb_device(entry.vid, entry.pid),
            "official USB product hint {:04x}:{:04x} ({}) is rejected by shared USB policy",
            entry.vid,
            entry.pid,
            entry.label
        );
    }
}

#[test]
fn shared_usb_policy_rejects_unknown_minidsp_vid_pid_pairs() {
    assert!(
        !probe::is_official_usb_device(probe::USB_VID_MINIDSP, 0x9999),
        "unknown MiniDSP VID/PID pair should not be accepted without an official asset-backed PID hint"
    );
    assert!(
        !probe::is_official_usb_device(probe::USB_VID_LEGACY, 0x9999),
        "unknown legacy VID/PID pair should not be accepted without an official asset-backed PID hint"
    );
}

#[test]
fn desktop_fallback_models_match_official_modern_devices_grouped_by_dsp() {
    let mut expected: Vec<(u8, Vec<&'static str>)> = generated_official::OFFICIAL_MODERN_DEVICES
        .iter()
        .fold(Vec::<(u8, Vec<&'static str>)>::new(), |mut acc, device| {
            if let Some((_, models)) = acc.iter_mut().find(|(dsp, _)| *dsp == device.dsp_version) {
                if !models.contains(&device.product_name) {
                    models.push(device.product_name);
                }
            } else {
                acc.push((device.dsp_version, vec![device.product_name]));
            }
            acc
        });
    expected.sort_by_key(|(dsp, _)| *dsp);
    for (_, models) in &mut expected {
        models.sort_unstable();
    }

    let mut actual: Vec<(u8, Vec<&'static str>)> = generated_official::OFFICIAL_DESKTOP_FALLBACK_MODELS
        .iter()
        .map(|entry| {
            let mut models = entry.models.to_vec();
            models.sort_unstable();
            (entry.dsp_id, models)
        })
        .collect();
    actual.sort_by_key(|(dsp, _)| *dsp);

    assert_eq!(
        actual, expected,
        "desktop fallback model groups should stay aligned with official modern deviceIdList rows"
    );
}

#[test]
fn minidsp_rs_reference_profiles_are_covered_by_probe_table() {
    for reference in generated_official::MINIDSP_RS_REFERENCE_PROFILES {
        let profile = probe::identify(reference.hw_id, reference.dsp_version);
        assert_eq!(
            profile.hw_id, reference.hw_id,
            "probe hw_id mismatch for minidsp-rs reference {} / dsp {} ({})",
            reference.hw_id, reference.dsp_version, reference.name
        );
        assert_eq!(
            profile.dsp_version, reference.dsp_version,
            "probe dsp_version mismatch for minidsp-rs reference {} / dsp {} ({})",
            reference.hw_id, reference.dsp_version, reference.name
        );
        assert!(
            reference.covered_by_rust,
            "generated reference unexpectedly claims uncovered minidsp-rs row {} / dsp {} ({})",
            reference.hw_id, reference.dsp_version, reference.name
        );
        assert!(
            reference.rust_profile.is_some(),
            "generated reference unexpectedly lacks a rust profile for minidsp-rs row {} / dsp {} ({})",
            reference.hw_id, reference.dsp_version, reference.name
        );
        let expected_name_match = profile.name == reference.name;
        assert_eq!(
            reference.name_match, expected_name_match,
            "generated name-match flag drifted for minidsp-rs row {} / dsp {} ({})",
            reference.hw_id, reference.dsp_version, reference.name
        );
    }
}
