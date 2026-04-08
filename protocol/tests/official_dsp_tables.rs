use std::collections::HashSet;

use minidsp_protocol::device::{dsp::DspTable, probe};
use serde::Deserialize;

const DEVICE_MATRIX_JSON: &str = include_str!("../../docs/generated/device-support-matrix.json");
// This fixture is generated from the extracted official app assets.

#[derive(Debug, Deserialize)]
struct DeviceMatrix {
    modern_devices: Vec<ModernDeviceRow>,
}

#[derive(Debug, Deserialize)]
struct ModernDeviceRow {
    hw_id: u8,
    dsp_version: u8,
    dsp_defaults: DspDefaults,
}

#[derive(Debug, Deserialize)]
struct DspDefaults {
    items: Vec<DspItem>,
    filters: Vec<DspFilter>,
}

#[derive(Debug, Deserialize)]
struct DspItem {
    idx: u16,
}

#[derive(Debug, Deserialize)]
struct DspFilter {
    idx: u16,
}

#[test]
fn modern_dsp_tables_reference_official_slot_indexes() {
    let matrix: DeviceMatrix =
        serde_json::from_str(DEVICE_MATRIX_JSON).expect("parse device matrix json");
    let mut seen_dsp_versions = HashSet::new();

    for row in matrix.modern_devices {
        if !seen_dsp_versions.insert(row.dsp_version) {
            continue;
        }
        let profile = probe::identify(row.hw_id, row.dsp_version);
        let table = profile
            .dsp_table
            .unwrap_or_else(|| panic!("missing dsp table for official dsp {}", row.dsp_version));
        let known_indexes: HashSet<u16> = row
            .dsp_defaults
            .items
            .into_iter()
            .map(|item| item.idx)
            .chain(
                row.dsp_defaults
                    .filters
                    .into_iter()
                    .map(|filter| filter.idx),
            )
            .collect();
        assert_channel_control_indexes_known(profile.name, row.dsp_version, table, &known_indexes);
    }
}

fn assert_channel_control_indexes_known(
    profile_name: &str,
    dsp_version: u8,
    table: &DspTable,
    known_indexes: &HashSet<u16>,
) {
    for (channel, input) in table.inputs.iter().enumerate() {
        if let Some(addr) = input.mute {
            assert_known_index(
                profile_name,
                dsp_version,
                "input.mute",
                channel,
                addr,
                known_indexes,
            );
        }
        if let Some(addr) = input.gain {
            assert_known_index(
                profile_name,
                dsp_version,
                "input.gain",
                channel,
                addr,
                known_indexes,
            );
        }
        if let Some(addr) = input.polarity {
            assert_known_index(
                profile_name,
                dsp_version,
                "input.polarity",
                channel,
                addr,
                known_indexes,
            );
        }
        for (band, peq) in input.peq.iter().enumerate() {
            assert_known_index(
                profile_name,
                dsp_version,
                "input.peq",
                channel * 100 + band,
                peq.addr,
                known_indexes,
            );
        }
        if let Some(fir) = input.fir {
            assert_known_index(
                profile_name,
                dsp_version,
                "input.fir.bypass",
                channel,
                fir.bypass,
                known_indexes,
            );
            assert_known_index(
                profile_name,
                dsp_version,
                "input.fir.taps_count",
                channel,
                fir.taps_count,
                known_indexes,
            );
        }
    }

    for (channel, output) in table.outputs.iter().enumerate() {
        assert_known_index(
            profile_name,
            dsp_version,
            "output.mute",
            channel,
            output.mute,
            known_indexes,
        );
        assert_known_index(
            profile_name,
            dsp_version,
            "output.gain",
            channel,
            output.gain,
            known_indexes,
        );
        if let Some(addr) = output.delay {
            assert_known_index(
                profile_name,
                dsp_version,
                "output.delay",
                channel,
                addr,
                known_indexes,
            );
        }
        if let Some(addr) = output.polarity {
            assert_known_index(
                profile_name,
                dsp_version,
                "output.polarity",
                channel,
                addr,
                known_indexes,
            );
        }
        for (band, peq) in output.peq.iter().enumerate() {
            assert_known_index(
                profile_name,
                dsp_version,
                "output.peq",
                channel * 100 + band,
                peq.addr,
                known_indexes,
            );
        }
        if let Some(crossover) = output.crossover {
            assert_known_index(
                profile_name,
                dsp_version,
                "output.crossover.lpf",
                channel,
                crossover.lpf_addr,
                known_indexes,
            );
            assert_known_index(
                profile_name,
                dsp_version,
                "output.crossover.hpf",
                channel,
                crossover.hpf_addr,
                known_indexes,
            );
        }
        if let Some(compressor) = output.compressor {
            assert_known_index(
                profile_name,
                dsp_version,
                "output.compressor.bypass",
                channel,
                compressor.bypass,
                known_indexes,
            );
            assert_known_index(
                profile_name,
                dsp_version,
                "output.compressor.threshold",
                channel,
                compressor.threshold,
                known_indexes,
            );
            assert_known_index(
                profile_name,
                dsp_version,
                "output.compressor.gain",
                channel,
                compressor.gain,
                known_indexes,
            );
            assert_known_index(
                profile_name,
                dsp_version,
                "output.compressor.ratio",
                channel,
                compressor.ratio,
                known_indexes,
            );
            assert_known_index(
                profile_name,
                dsp_version,
                "output.compressor.knee",
                channel,
                compressor.knee,
                known_indexes,
            );
            assert_known_index(
                profile_name,
                dsp_version,
                "output.compressor.attack",
                channel,
                compressor.attack,
                known_indexes,
            );
            assert_known_index(
                profile_name,
                dsp_version,
                "output.compressor.release",
                channel,
                compressor.release,
                known_indexes,
            );
        }
    }
}

fn assert_known_index(
    profile_name: &str,
    dsp_version: u8,
    field: &str,
    channel: usize,
    addr: u16,
    known_indexes: &HashSet<u16>,
) {
    assert!(
        known_indexes.contains(&addr),
        "{profile_name} dsp {dsp_version} field {field} entry {channel} uses addr {addr}, but that index is absent from official app metadata",
    );
}
