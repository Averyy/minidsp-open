use minidsp::client::{CachedPeqShadow, PresetShadowState};
use minidsp::snapshot::*;
use minidsp_protocol::device::probe::identify;

#[path = "../../app-esp32/src/bundle_update.rs"]
mod bundle;
#[path = "../../app-esp32/src/service_input.rs"]
mod input;
#[path = "../../app-esp32/src/snapshot_validation.rs"]
mod validation;

fn complete_snapshot() -> DeviceSnapshot {
    let profile = identify(10, 100);
    let table = profile.dsp_table.unwrap();
    let mut shadow = PresetShadowState::default();
    let band = CachedPeqShadow {
        coeffs: [1.0, 0.0, 0.0, 0.0, 0.0],
        bypass: true,
    };
    for input in table.inputs {
        if let Some(addr) = input.gain {
            shadow.scalar.insert(addr, 0);
        }
        if let Some(addr) = input.mute {
            shadow.selector.insert(addr, 2);
        }
        for peq in input.peq {
            shadow.peq.insert(peq.addr, band.clone());
        }
    }
    for output in table.outputs {
        shadow.scalar.insert(output.gain, 0);
        shadow.selector.insert(output.mute, 2);
        if let Some(addr) = output.delay {
            shadow.scalar.insert(addr, 0);
        }
        if let Some(addr) = output.polarity {
            shadow.selector.insert(addr, 0);
        }
        for peq in output.peq {
            shadow.peq.insert(peq.addr, band.clone());
        }
        if let Some(crossover) = output.crossover {
            for base in [crossover.lpf_addr, crossover.hpf_addr] {
                for index in 0..4 {
                    shadow.peq.insert(base + index * 5, band.clone());
                }
            }
        }
        if let Some(fir) = output.fir {
            shadow.selector.insert(fir.bypass, 3);
            shadow.selector.insert(fir.taps_count, 0);
        }
        if let Some(comp) = output.compressor {
            shadow.selector.insert(comp.bypass, 3);
            for addr in [
                comp.threshold,
                comp.gain,
                comp.ratio,
                comp.knee,
                comp.attack,
                comp.release,
            ] {
                shadow.scalar.insert(addr, 0);
            }
        }
    }
    for offset in 0..table.routing.rows as u16 * table.routing.cols as u16 {
        shadow
            .selector
            .insert(table.routing.status_base + offset, 2);
        shadow.scalar.insert(table.routing.gain_base + offset, 0);
    }
    serde_json::from_value(serde_json::json!({
        "formatVersion": 1, "capturedAt": "2026-04-16T00:00:00Z",
        "tool": {"name": "minidsp-open", "version": "0.1.0"},
        "device": {"serial": 912345, "hwId": 10, "dspVersion": 100, "fwMajor": 1,
            "fwMinor": 0, "productName": "2x4HD", "presetCount": 4},
        "transport": {"kind": "usb", "target": "test"},
        "master": {"activePreset": 0, "source": "Usb", "volume": -20.0, "mute": false},
        "presets": (0..4).map(|slot| serde_json::json!({
            "slot": slot, "token": "feef8fcf", "complete": true,
            "provenance": ["controller-shadow"], "completeness": {"strict": true},
            "shadow": shadow,
        })).collect::<Vec<_>>(),
        "completeness": {"strict": true},
    }))
    .unwrap()
}

#[test]
fn snapshot_preflight_accepts_complete_desktop_artifact_without_changing_schema() {
    let snapshot = complete_snapshot();
    validation::validate_snapshot_payload(&identify(10, 100), &snapshot).unwrap();
    let wire = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(wire["formatVersion"], 1);
    assert!(wire["presets"][0]["shadow"].is_object());
}

#[test]
fn snapshot_preflight_rejects_unsupported_schema_and_master_state() {
    let profile = identify(10, 100);
    let valid = complete_snapshot();
    let mut snapshot = valid.clone();
    snapshot.format_version = 2;
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    let mut snapshot = valid.clone();
    snapshot.master.active_preset = 4;
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    let mut snapshot = valid.clone();
    snapshot.master.source = "unknown".into();
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    let mut snapshot = valid;
    snapshot.master.volume = f32::NAN;
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
}

#[test]
fn snapshot_preflight_rejects_missing_or_duplicate_presets_and_forged_completeness() {
    let profile = identify(10, 100);
    let valid = complete_snapshot();
    let mut snapshot = valid.clone();
    snapshot.presets.pop();
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    let mut snapshot = valid.clone();
    snapshot.presets[3].slot = 0;
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    let mut snapshot = valid.clone();
    snapshot.presets[0].shadow = Some(PresetShadowState::default());
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    let mut snapshot = valid;
    snapshot.presets[0].complete = false;
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
}

#[test]
fn snapshot_preflight_checks_late_preset_addresses_and_tokens_before_restore() {
    let profile = identify(10, 100);
    let valid = complete_snapshot();
    for token in [None, Some("0000000€"), Some("0000000g")] {
        let mut snapshot = valid.clone();
        snapshot.presets[3].token = token.map(str::to_string);
        assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    }
    for kind in ["scalar", "selector", "peq"] {
        let mut snapshot = valid.clone();
        let shadow = snapshot.presets[3].shadow.as_mut().unwrap();
        match kind {
            "scalar" => {
                let addr = *shadow.scalar.keys().next().unwrap();
                shadow.scalar.remove(&addr);
            }
            "selector" => {
                let addr = *shadow.selector.keys().next().unwrap();
                shadow.selector.remove(&addr);
            }
            _ => {
                let addr = *shadow.peq.keys().next().unwrap();
                shadow.peq.remove(&addr);
            }
        }
        assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    }
    let mut snapshot = valid;
    snapshot.presets[3]
        .shadow
        .as_mut()
        .unwrap()
        .selector
        .insert(65000, 1);
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
}

#[test]
fn snapshot_preflight_checks_fir_tap_bounds_presence_lengths_and_duplicates() {
    let profile = identify(10, 100);
    let fir = profile.dsp_table.unwrap().outputs[0].fir.unwrap();
    let mut snapshot = complete_snapshot();
    snapshot.presets[0]
        .shadow
        .as_mut()
        .unwrap()
        .selector
        .insert(fir.taps_count, 2);
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    snapshot.presets[0].fir_blocks.push(SnapshotFirBlock {
        data_start: fir.data_start,
        coefficients: vec![1.0],
    });
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    snapshot.presets[0].fir_blocks[0].coefficients.push(0.0);
    validation::validate_snapshot_payload(&profile, &snapshot).unwrap();
    let block = snapshot.presets[0].fir_blocks[0].clone();
    snapshot.presets[0].fir_blocks.push(block);
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
    snapshot.presets[0].fir_blocks.pop();
    snapshot.presets[0]
        .shadow
        .as_mut()
        .unwrap()
        .selector
        .insert(fir.taps_count, fir.max_taps + 1);
    assert!(validation::validate_snapshot_payload(&profile, &snapshot).is_err());
}

#[path = "../../app-esp32/src/ezbeq_contract.rs"]
mod ezbeq_contract;

#[test]
fn ezbeq_patch_commands_apply_controls_before_entry_and_activate_last() {
    use ezbeq_contract::SlotStep::*;
    let patch = serde_json::json!({
        "id": "2", "entry": "digest", "active": true,
        "gains": [{"id": "1", "value": -8.0}],
        "mutes": [{"id": "2", "value": false}],
        "outputGains": [{"id": "1", "value": -3.0}],
        "outputMutes": [{"id": "1", "value": true}]
    });
    assert_eq!(
        ezbeq_contract::slot_steps(&patch).unwrap(),
        [Inputs, Outputs, Entry, Activate]
    );
    // An empty entry clears gains/mutes after the requested controls, as upstream does.
    let clear = serde_json::json!({"entry": "", "gains": [{"id": "1", "value": -8.0}]});
    assert_eq!(ezbeq_contract::slot_steps(&clear).unwrap(), [Inputs, Entry]);
}

#[test]
fn ezbeq_patch_active_false_also_activates_but_absent_active_does_not() {
    use ezbeq_contract::SlotStep::Activate;
    for active in [true, false] {
        assert_eq!(
            ezbeq_contract::slot_steps(&serde_json::json!({"id": "3", "active": active})).unwrap(),
            [Activate]
        );
    }
    assert!(
        ezbeq_contract::slot_steps(&serde_json::json!({"id": "3"}))
            .unwrap()
            .is_empty()
    );
    for active in [
        serde_json::Value::Null,
        serde_json::json!(1),
        serde_json::json!("false"),
    ] {
        assert!(ezbeq_contract::slot_steps(&serde_json::json!({"active": active})).is_err());
    }
}

#[test]
fn ezbeq_empty_overwrite_bypasses_only_selected_channels_without_rewriting_coefficients() {
    use ezbeq_contract::BandAction;
    let biquads = ezbeq_contract::parse_biquads("\n  \n").unwrap();
    let mut inputs = [[false; 10]; 2];
    let mut outputs = [[false; 10]; 4];
    // The route passes only selected inputs/outputs to this same action planner.
    for bands in [&mut inputs[1], &mut outputs[2]] {
        let actions = ezbeq_contract::band_actions(&biquads, true, bands.len());
        assert_eq!(actions.len(), 10);
        for action in actions {
            match action {
                BandAction::Bypass { index } => bands[index] = true,
                BandAction::Write { .. } => panic!("empty uploads must not rewrite coefficients"),
            }
        }
    }
    assert_eq!(inputs, [[false; 10], [true; 10]]);
    assert_eq!(outputs, [[false; 10], [false; 10], [true; 10], [false; 10]]);
    assert!(ezbeq_contract::band_actions(&biquads, false, 10).is_empty());
    let coefficient = [1.0, 0.0, 0.0, 1.9, -0.95];
    assert_eq!(
        ezbeq_contract::band_actions(&[coefficient], false, 10),
        [BandAction::Write {
            index: 0,
            coefficients: coefficient
        }]
    );
}

#[test]
fn bundle_plan_releases_shrinking_catalogues_before_new_metadata() {
    use bundle::PendingFile;
    let files = [
        PendingFile {
            name: "entry-metadata.jsonl",
            old_size: 0,
            new_size: 4 * 4096,
        },
        PendingFile {
            name: "database.json.gz",
            old_size: 8 * 4096,
            new_size: 4 * 4096,
        },
        PendingFile {
            name: "catalogue.bin",
            old_size: 10 * 4096,
            new_size: 4 * 4096,
        },
    ];
    let plan = bundle::replacement_plan(&files, 6 * 4096, 4096).unwrap();
    assert_eq!(plan.order, [2, 1, 0]);
    assert!(!plan.delete_before_download);
    // Even the first temporary copy will not fit here, but replacing in place does.
    let plan = bundle::replacement_plan(&files, 4096, 4096).unwrap();
    assert!(plan.delete_before_download);
    assert_eq!(plan.order, [2, 1, 0]);
}

#[test]
fn bundle_plan_rounds_blocks_preserves_margin_and_prefers_keep_old() {
    use bundle::PendingFile;
    let files = [PendingFile {
        name: "catalogue.bin",
        old_size: 4097,
        new_size: 4097,
    }];
    assert!(
        !bundle::replacement_plan(&files, 3 * 4096, 4096)
            .unwrap()
            .delete_before_download
    );
    assert!(
        bundle::replacement_plan(&files, 3 * 4096 - 1, 4096)
            .unwrap()
            .delete_before_download
    );
    let added = [PendingFile {
        name: "new",
        old_size: 0,
        new_size: 1,
    }];
    assert!(bundle::replacement_plan(&added, 8191, 4096).is_err());
    assert!(
        !bundle::replacement_plan(&added, 8192, 4096)
            .unwrap()
            .delete_before_download
    );
    let overflow = [PendingFile {
        name: "new",
        old_size: 0,
        new_size: u64::MAX,
    }];
    assert!(bundle::replacement_plan(&overflow, u64::MAX, 4096).is_err());
}

#[test]
fn bundle_plan_publishes_ui_index_last_in_both_modes_and_accounts_for_its_peak() {
    use bundle::PendingFile;
    let files = [
        PendingFile {
            name: "ui/index.html",
            old_size: 10 * 4096,
            new_size: 1,
        },
        PendingFile {
            name: "ui/static/main.js",
            old_size: 4096,
            new_size: 4096,
        },
    ];
    for available in [4096, 3 * 4096] {
        let plan = bundle::replacement_plan(&files, available, 4096).unwrap();
        assert_eq!(plan.order, [1, 0]);
        assert_eq!(plan.delete_before_download, available == 4096);
    }
    // The index's large shrink cannot fund assets before it publishes.
    let files = [
        PendingFile {
            name: "ui/index.html",
            old_size: 10 * 4096,
            new_size: 1,
        },
        PendingFile {
            name: "ui/static/main.js",
            old_size: 0,
            new_size: 4096,
        },
    ];
    assert!(bundle::replacement_plan(&files, 4096, 4096).is_err());
}
