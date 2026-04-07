use std::collections::HashSet;

use minidsp_protocol::device::{dsp::DspTable, probe};
use serde::Deserialize;

const DEVICE_MATRIX_JSON: &str = include_str!("../../docs/generated/device-support-matrix.json");

#[derive(Debug, Deserialize)]
struct DeviceMatrix {
    legacy_configs: Vec<LegacyConfigRow>,
}

#[derive(Debug, Deserialize)]
struct LegacyConfigRow {
    hw_id: u8,
    dsp_version: Option<u8>,
    name: String,
    items: Vec<LegacyItem>,
    filters: Vec<LegacyFilter>,
    fir_blocks: Vec<LegacyFirBlock>,
}

#[derive(Debug, Deserialize)]
struct LegacyItem {
    addr: u16,
}

#[derive(Debug, Deserialize)]
struct LegacyFilter {
    addr: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct LegacyFirBlock {
    addr: Option<u16>,
}

#[test]
fn source_backed_legacy_tables_reference_known_config_indexes() {
    let matrix: DeviceMatrix = serde_json::from_str(DEVICE_MATRIX_JSON).expect("parse device matrix json");

    for (hw_id, dsp_version, expected_name) in [
        (1_u8, 0_u8, "4x10 HD"),
        (1_u8, 51_u8, "10x10 HD"),
        (2_u8, 22_u8, "miniDSP 2x4"),
        (2_u8, 54_u8, "NanoDigi 2x8"),
        (4_u8, 0_u8, "miniSHARC 4x8"),
        (6_u8, 95_u8, "DDRC-88BM"),
        (11_u8, 97_u8, "C-DSP 8x12 v2"),
    ] {
        let row = matrix
            .legacy_configs
            .iter()
            .find(|row| row.hw_id == hw_id && row.dsp_version.unwrap_or(0) == dsp_version)
            .unwrap_or_else(|| panic!("missing generated legacy config row for hw {hw_id} dsp {dsp_version}"));
        let profile = probe::identify(hw_id, dsp_version);
        assert_eq!(profile.name, expected_name);

        let table = profile
            .dsp_table
            .unwrap_or_else(|| panic!("missing legacy dsp table for {expected_name}"));

        let known_indexes: HashSet<u16> = row
            .items
            .iter()
            .map(|item| item.addr)
            .chain(row.filters.iter().filter_map(|filter| filter.addr))
            .chain(row.fir_blocks.iter().filter_map(|fir| fir.addr))
            .collect();
        assert!(
            !known_indexes.is_empty(),
            "generated legacy metadata for {} is unexpectedly empty",
            row.name
        );
        assert_channel_control_indexes_known(expected_name, table, &known_indexes);
    }
}

fn assert_channel_control_indexes_known(
    profile_name: &str,
    table: &DspTable,
    known_indexes: &HashSet<u16>,
) {
    for (channel, input) in table.inputs.iter().enumerate() {
        if let Some(addr) = input.mute {
            assert_known_index(profile_name, "input.mute", channel, addr, known_indexes);
        }
        if let Some(addr) = input.gain {
            assert_known_index(profile_name, "input.gain", channel, addr, known_indexes);
        }
        if let Some(addr) = input.polarity {
            assert_known_index(profile_name, "input.polarity", channel, addr, known_indexes);
        }
        for (band, peq) in input.peq.iter().enumerate() {
            assert_known_index(profile_name, "input.peq", channel * 100 + band, peq.addr, known_indexes);
        }
        if let Some(fir) = input.fir {
            assert_known_index(profile_name, "input.fir.bypass", channel, fir.bypass, known_indexes);
            assert_known_index(profile_name, "input.fir.taps_count", channel, fir.taps_count, known_indexes);
            assert_known_index(profile_name, "input.fir.data_start", channel, fir.data_start, known_indexes);
        }
    }

    for (channel, output) in table.outputs.iter().enumerate() {
        assert_known_index(profile_name, "output.mute", channel, output.mute, known_indexes);
        assert_known_index(profile_name, "output.gain", channel, output.gain, known_indexes);
        if let Some(addr) = output.delay {
            assert_known_index(profile_name, "output.delay", channel, addr, known_indexes);
        }
        if let Some(addr) = output.polarity {
            assert_known_index(profile_name, "output.polarity", channel, addr, known_indexes);
        }
        for (band, peq) in output.peq.iter().enumerate() {
            assert_known_index(profile_name, "output.peq", channel * 100 + band, peq.addr, known_indexes);
        }
        if let Some(fir) = output.fir {
            assert_known_index(profile_name, "output.fir.bypass", channel, fir.bypass, known_indexes);
            assert_known_index(profile_name, "output.fir.taps_count", channel, fir.taps_count, known_indexes);
            assert_known_index(profile_name, "output.fir.data_start", channel, fir.data_start, known_indexes);
        }
        if let Some(crossover) = output.crossover {
            assert_known_index(profile_name, "output.crossover.lpf", channel, crossover.lpf_addr, known_indexes);
            assert_known_index(profile_name, "output.crossover.hpf", channel, crossover.hpf_addr, known_indexes);
        }
        if let Some(compressor) = output.compressor {
            assert_known_index(profile_name, "output.compressor.bypass", channel, compressor.bypass, known_indexes);
            assert_known_index(profile_name, "output.compressor.threshold", channel, compressor.threshold, known_indexes);
            assert_known_index(profile_name, "output.compressor.gain", channel, compressor.gain, known_indexes);
            assert_known_index(profile_name, "output.compressor.ratio", channel, compressor.ratio, known_indexes);
            assert_known_index(profile_name, "output.compressor.knee", channel, compressor.knee, known_indexes);
            assert_known_index(profile_name, "output.compressor.attack", channel, compressor.attack, known_indexes);
            assert_known_index(profile_name, "output.compressor.release", channel, compressor.release, known_indexes);
        }
    }

    for row in 0..table.routing.rows as usize {
        for col in 0..table.routing.cols as usize {
            let entry = row * table.routing.cols as usize + col;
            assert_known_index(
                profile_name,
                "routing.status",
                entry,
                table.routing.status_base + entry as u16,
                known_indexes,
            );
            assert_known_index(
                profile_name,
                "routing.gain",
                entry,
                table.routing.gain_base + entry as u16,
                known_indexes,
            );
            if let Some(base) = table.routing.polarity_base {
                assert_known_index(profile_name, "routing.polarity", entry, base + entry as u16, known_indexes);
            }
        }
    }

    for (channel, addr) in table.meters.input.iter().enumerate() {
        assert_known_index(profile_name, "meters.input", channel, *addr, known_indexes);
    }
    for (channel, addr) in table.meters.output.iter().enumerate() {
        assert_known_index(profile_name, "meters.output", channel, *addr, known_indexes);
    }
    for (channel, addr) in table.meters.compressor.iter().enumerate() {
        assert_known_index(profile_name, "meters.compressor", channel, *addr, known_indexes);
    }

    if let Some(bm) = table.bass_management {
        assert_known_index(profile_name, "bass_management.global_lpf", 0, bm.global_lpf_addr, known_indexes);
        assert_known_index(profile_name, "bass_management.global_hpf", 0, bm.global_hpf_addr, known_indexes);
        assert_known_index(profile_name, "bass_management.gain_status_base", 0, bm.gain_status_base, known_indexes);
        assert_known_index(profile_name, "bass_management.gain_base", 0, bm.gain_base, known_indexes);
        for (channel, (lpf, hpf)) in bm.per_channel_bpf.iter().enumerate() {
            assert_known_index(profile_name, "bass_management.channel_lpf", channel, *lpf, known_indexes);
            assert_known_index(profile_name, "bass_management.channel_hpf", channel, *hpf, known_indexes);
        }
    }
}

fn assert_known_index(
    profile_name: &str,
    field: &str,
    channel: usize,
    addr: u16,
    known_indexes: &HashSet<u16>,
) {
    assert!(
        known_indexes.contains(&addr),
        "{profile_name} field {field} entry {channel} uses addr {addr}, but that index is absent from source-backed legacy config metadata",
    );
}
