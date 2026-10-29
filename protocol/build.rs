use std::collections::HashMap;
use std::f64::consts::PI;
use std::fs;
use std::path::{Path, PathBuf};

const FAST_SIN_TABLE_SIZE: usize = 512;
const DEFAULT_PRESET_DSPS: &[u8] = &[100, 101, 105, 110, 111, 113, 115];

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR missing"));
    write_fast_sin_lut(&out_dir);
    write_default_preset_images(&out_dir);
}

fn write_fast_sin_lut(out_dir: &Path) {
    let output = out_dir.join("fast_sin_lut.rs");
    let mut source = String::from("const FAST_SIN_LUT: [f32; 513] = [\n");
    for idx in 0..=FAST_SIN_TABLE_SIZE {
        let phase = (idx as f64 * 2.0 * PI) / FAST_SIN_TABLE_SIZE as f64;
        // Preserve the original nine-decimal rounding, including signed zero,
        // then emit the resulting f32 in its shortest round-trip form.
        let value: f32 = format!("{:.9}", phase.sin() as f32)
            .parse()
            .expect("formatted sine is a valid f32");
        source.push_str(&format!("    {value:?}f32,\n"));
    }
    source.push_str("];\n");

    fs::write(output, source).expect("failed to write fast_sin_lut.rs");
}

#[derive(Debug, Clone, Copy)]
struct BypassRecord {
    type_byte: u8,
    addr: u16,
    bypassed: bool,
}

#[derive(Debug, Clone)]
struct IndexedWords {
    idx: usize,
    words: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
struct IndexedWord {
    idx: usize,
    word: u32,
}

#[derive(Debug, Clone)]
struct DiracDefaults {
    first_channel_offset: usize,
    channel_size: usize,
    channel_count: usize,
    digital_gain_status: IndexedWords,
    digital_gain_db: IndexedWords,
    meters: IndexedWords,
    delay: IndexedWord,
    digital_gain_polarity: IndexedWords,
    mux_bypass: IndexedWord,
    dirac_delay: IndexedWord,
    mixer_fir: IndexedWord,
    mixer_iir: IndexedWords,
    fir_ctrl: IndexedWord,
    fir_taps: IndexedWords,
    iir_taps: IndexedWords,
}

#[derive(Debug, Default)]
struct DiracDefaultsBuilder {
    digital_gain_status: Option<IndexedWords>,
    digital_gain_db: Option<IndexedWords>,
    meters: Option<IndexedWords>,
    delay: Option<IndexedWord>,
    digital_gain_polarity: Option<IndexedWords>,
    mux_bypass: Option<IndexedWord>,
    dirac_delay: Option<IndexedWord>,
    mixer_fir: Option<IndexedWord>,
    mixer_iir: Option<IndexedWords>,
    fir_ctrl: Option<IndexedWord>,
    fir_taps: Option<IndexedWords>,
    iir_taps: Option<IndexedWords>,
}

fn write_default_preset_images(out_dir: &Path) {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing"));
    let audio_fw_root = manifest_dir.join("assets").join("audio_firmware");
    println!("cargo:rerun-if-changed={}", audio_fw_root.display());

    let mut source = String::from(
        "// Generated from protocol/assets/audio_firmware/*/DSP_default_parameters.xml. Do not edit by hand.\n\n",
    );
    source.push_str(
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
         pub struct DefaultPresetImage {\n\
             pub dsp_version: u8,\n\
             pub params: &'static [u8],\n\
             pub bypass: &'static [u8],\n\
         }\n\n",
    );

    for &dsp_version in DEFAULT_PRESET_DSPS {
        let xml_path = audio_fw_root
            .join(dsp_version.to_string())
            .join("DSP_default_parameters.xml");
        println!("cargo:rerun-if-changed={}", xml_path.display());
        let (params, bypass) = parse_default_preset_xml(&xml_path);
        let params_file = format!("dsp_{dsp_version}_params.bin");
        let bypass_file = format!("dsp_{dsp_version}_bypass.bin");
        fs::write(out_dir.join(&params_file), &params)
            .unwrap_or_else(|err| panic!("failed to write {params_file}: {err}"));
        fs::write(out_dir.join(&bypass_file), &bypass)
            .unwrap_or_else(|err| panic!("failed to write {bypass_file}: {err}"));

        source.push_str(&format!(
            "static DSP_{dsp_version}_PARAMS: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{params_file}\"));\n"
        ));
        source.push('\n');
        source.push_str(&format!(
            "static DSP_{dsp_version}_BYPASS: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{bypass_file}\"));\n"
        ));
        source.push('\n');

        source.push_str(&format!(
            "static DSP_{dsp_version}_IMAGE: DefaultPresetImage = DefaultPresetImage {{\n\
                 dsp_version: {dsp_version},\n\
                 params: DSP_{dsp_version}_PARAMS,\n\
                 bypass: DSP_{dsp_version}_BYPASS,\n\
             }};\n\n"
        ));
    }

    source.push_str(
        "pub fn default_preset_image_for_dsp(dsp_version: u8) -> Option<&'static DefaultPresetImage> {\n\
             match dsp_version {\n",
    );
    for &dsp_version in DEFAULT_PRESET_DSPS {
        source.push_str(&format!(
            "        {dsp_version} => Some(&DSP_{dsp_version}_IMAGE),\n"
        ));
    }
    source.push_str("        _ => None,\n    }\n}\n");

    fs::write(out_dir.join("default_preset_generated.rs"), source)
        .expect("failed to write default_preset_generated.rs");
}

fn parse_default_preset_xml(path: &Path) -> (Vec<u8>, Vec<u8>) {
    let content = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    let lines: Vec<&str> = content.lines().collect();
    let mut params: Vec<u8> = Vec::new();
    let mut fir_status: HashMap<String, u32> = HashMap::new();
    let mut bypass_records = Vec::new();
    let mut dirac_defaults: Option<DiracDefaults> = None;

    enum Block {
        Filter {
            addr: u16,
            type_name: String,
            words: Option<Vec<u32>>,
            bypassed: Option<bool>,
        },
        Fir {
            addr: u16,
            channel: String,
            words: Vec<u32>,
        },
    }

    let mut current_block: Option<Block> = None;
    let mut line_idx = 0;

    while line_idx < lines.len() {
        let line = lines[line_idx].trim();

        if params.is_empty() && line.starts_with("<params ") {
            let size = extract_attr(line, "size")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_else(|| panic!("missing/invalid params size in {}", path.display()));
            params.resize(size, 0);
            line_idx += 1;
            continue;
        }

        if line.starts_with("<dirac ") {
            if dirac_defaults.is_some() {
                panic!("multiple dirac blocks found in {}", path.display());
            }
            dirac_defaults = Some(parse_dirac_block(&lines, &mut line_idx, path));
            line_idx += 1;
            continue;
        }

        match &mut current_block {
            Some(Block::Filter {
                addr,
                type_name,
                words,
                bypassed,
            }) => {
                if let Some(hex) = extract_tag_text(line, "hex") {
                    *words = Some(parse_hex_csv(hex));
                    line_idx += 1;
                    continue;
                }
                if let Some(dec) = extract_tag_text(line, "dec") {
                    *words = Some(parse_dec_csv(dec));
                    line_idx += 1;
                    continue;
                }
                if let Some(text) = extract_tag_text(line, "bypass") {
                    *bypassed = Some(text.parse::<u8>().map(|value| value != 0).unwrap_or(false));
                    line_idx += 1;
                    continue;
                }
                if line.starts_with("</filter>") {
                    let filter_words = words.take().unwrap_or_else(|| {
                        panic!(
                            "filter block missing coefficients at addr {} in {}",
                            addr,
                            path.display()
                        )
                    });
                    write_words_le(&mut params, *addr, &filter_words, path);
                    bypass_records.push(BypassRecord {
                        type_byte: bypass_type_byte(type_name),
                        addr: *addr,
                        bypassed: bypassed.unwrap_or(false),
                    });
                    current_block = None;
                }
                line_idx += 1;
                continue;
            }
            Some(Block::Fir {
                addr,
                channel,
                words,
            }) => {
                if let Some(hex) = extract_tag_text(line, "subpara") {
                    words.extend(parse_hex_csv(hex));
                    line_idx += 1;
                    continue;
                }
                if line.starts_with("</fir>") {
                    write_words_le(&mut params, *addr, words, path);
                    let status_key = format!("FIR_{}_0_status", channel);
                    let bypassed = fir_status
                        .get(&status_key)
                        .copied()
                        .map(|value| value == 3)
                        .unwrap_or(false);
                    bypass_records.push(BypassRecord {
                        type_byte: 0x04,
                        addr: *addr,
                        bypassed,
                    });
                    current_block = None;
                }
                line_idx += 1;
                continue;
            }
            None => {}
        }

        if line.starts_with("<item ") {
            let idx = extract_attr(line, "idx")
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or_else(|| panic!("item idx missing in {}", path.display()));
            let name = extract_attr(line, "name").unwrap_or_default();
            let hex = extract_tag_text(line, "item").unwrap_or_else(|| {
                panic!("item payload missing for idx {idx} in {}", path.display())
            });
            let value = parse_hex_u32(hex, path);
            write_words_le(&mut params, idx, &[value], path);
            if name.starts_with("FIR_") && name.ends_with("_status") {
                fir_status.insert(name, value);
            }
            line_idx += 1;
            continue;
        }

        if line.starts_with("<filter ") {
            let addr = extract_attr(line, "idx")
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or_else(|| panic!("filter idx missing in {}", path.display()));
            let type_name = extract_attr(line, "type")
                .unwrap_or_else(|| panic!("filter type missing in {}", path.display()));
            current_block = Some(Block::Filter {
                addr,
                type_name,
                words: None,
                bypassed: None,
            });
            line_idx += 1;
            continue;
        }

        if line.starts_with("<fir ") {
            let addr = extract_attr(line, "idx")
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or_else(|| panic!("fir idx missing in {}", path.display()));
            let channel = extract_attr(line, "chn")
                .unwrap_or_else(|| panic!("fir channel missing in {}", path.display()));
            current_block = Some(Block::Fir {
                addr,
                channel,
                words: Vec::new(),
            });
            line_idx += 1;
            continue;
        }

        line_idx += 1;
    }

    if let Some(dirac) = &dirac_defaults {
        append_dirac_defaults(&mut params, dirac, path);
    }

    let mut bypass = Vec::with_capacity(bypass_records.len() * 3);
    for record in bypass_records {
        bypass.push(record.type_byte | if record.bypassed { 0x80 } else { 0x00 });
        bypass.extend_from_slice(&record.addr.to_be_bytes());
    }

    (params, bypass)
}

impl DiracDefaultsBuilder {
    fn build(
        self,
        first_channel_offset: usize,
        channel_size: usize,
        channel_count: usize,
        path: &Path,
    ) -> DiracDefaults {
        DiracDefaults {
            first_channel_offset,
            channel_size,
            channel_count,
            digital_gain_status: self.digital_gain_status.unwrap_or_else(|| {
                panic!("dirac digital gain status missing in {}", path.display())
            }),
            digital_gain_db: self
                .digital_gain_db
                .unwrap_or_else(|| panic!("dirac digital gain db missing in {}", path.display())),
            meters: self
                .meters
                .unwrap_or_else(|| panic!("dirac meters missing in {}", path.display())),
            delay: self
                .delay
                .unwrap_or_else(|| panic!("dirac delay missing in {}", path.display())),
            digital_gain_polarity: self.digital_gain_polarity.unwrap_or_else(|| {
                panic!("dirac digital gain polarity missing in {}", path.display())
            }),
            mux_bypass: self
                .mux_bypass
                .unwrap_or_else(|| panic!("dirac mux_bypass missing in {}", path.display())),
            dirac_delay: self
                .dirac_delay
                .unwrap_or_else(|| panic!("dirac dirac_delay missing in {}", path.display())),
            mixer_fir: self
                .mixer_fir
                .unwrap_or_else(|| panic!("dirac mixer_fir missing in {}", path.display())),
            mixer_iir: self
                .mixer_iir
                .unwrap_or_else(|| panic!("dirac mixer_iir missing in {}", path.display())),
            fir_ctrl: self
                .fir_ctrl
                .unwrap_or_else(|| panic!("dirac fir ctrl missing in {}", path.display())),
            fir_taps: self
                .fir_taps
                .unwrap_or_else(|| panic!("dirac fir taps missing in {}", path.display())),
            iir_taps: self
                .iir_taps
                .unwrap_or_else(|| panic!("dirac iir taps missing in {}", path.display())),
        }
    }
}

fn parse_dirac_block(lines: &[&str], line_idx: &mut usize, path: &Path) -> DiracDefaults {
    let line = lines[*line_idx].trim();
    let first_channel_offset = parse_required_attr_usize(line, "addr", path, "dirac");
    let channel_size = parse_required_attr_usize(line, "size", path, "dirac");
    let channel_count = parse_required_attr_usize(line, "chns", path, "dirac");
    let mut builder = DiracDefaultsBuilder::default();

    while *line_idx + 1 < lines.len() {
        *line_idx += 1;
        let line = lines[*line_idx].trim();

        if line.is_empty() || line.starts_with("<!--") {
            continue;
        }
        if line.starts_with("</dirac>") {
            return builder.build(first_channel_offset, channel_size, channel_count, path);
        }
        if line.starts_with("<digital_gains ") {
            parse_dirac_digital_gains(lines, line_idx, path, &mut builder);
            continue;
        }
        if line.starts_with("<meters ") {
            parse_dirac_meters(lines, line_idx, path, &mut builder);
            continue;
        }
        if line.starts_with("<delay ") {
            builder.delay = Some(parse_dirac_scalar_tag(line, "delay", path));
            continue;
        }
        if line.starts_with("<mux_bypass ") {
            builder.mux_bypass = Some(parse_dirac_scalar_tag(line, "mux_bypass", path));
            continue;
        }
        if line.starts_with("<dirac_delay ") {
            builder.dirac_delay = Some(parse_dirac_scalar_tag(line, "dirac_delay", path));
            continue;
        }
        if line.starts_with("<mixer_fir ") {
            builder.mixer_fir = Some(parse_dirac_scalar_tag(line, "mixer_fir", path));
            continue;
        }
        if line.starts_with("<mixer_iir ") {
            builder.mixer_iir = Some(parse_dirac_repeated_words_tag(line, "mixer_iir", path));
            continue;
        }
        if line.starts_with("<fir>") {
            parse_dirac_fir(lines, line_idx, path, &mut builder);
            continue;
        }
        if line.starts_with("<iir ") {
            parse_dirac_iir(lines, line_idx, path, &mut builder);
            continue;
        }
    }

    panic!("unterminated dirac block in {}", path.display());
}

fn parse_dirac_digital_gains(
    lines: &[&str],
    line_idx: &mut usize,
    path: &Path,
    builder: &mut DiracDefaultsBuilder,
) {
    let line = lines[*line_idx].trim();
    let count = parse_required_attr_usize(line, "cnt", path, "dirac digital_gains");
    let mut status = None;
    let mut db = None;
    let mut polarity = None;

    while *line_idx + 1 < lines.len() {
        *line_idx += 1;
        let line = lines[*line_idx].trim();

        if line.is_empty() || line.starts_with("<!--") {
            continue;
        }
        if line.starts_with("</digital_gains>") {
            builder.digital_gain_status = Some(status.unwrap_or_else(|| {
                panic!("dirac digital_gains status missing in {}", path.display())
            }));
            builder.digital_gain_db =
                Some(db.unwrap_or_else(|| {
                    panic!("dirac digital_gains db missing in {}", path.display())
                }));
            builder.digital_gain_polarity = Some(polarity.unwrap_or_else(|| {
                panic!("dirac digital_gains polarity missing in {}", path.display())
            }));
            return;
        }
        if let Some(text) = extract_tag_text(line, "status") {
            status = Some(IndexedWords {
                idx: parse_required_attr_usize(line, "idx", path, "dirac digital_gains status"),
                words: repeat_words(
                    count,
                    parse_required_dec_u32(text, path, "dirac digital_gains status"),
                ),
            });
            continue;
        }
        if let Some(text) = extract_tag_text(line, "db") {
            db = Some(IndexedWords {
                idx: parse_required_attr_usize(line, "idx", path, "dirac digital_gains db"),
                words: repeat_words(
                    count,
                    parse_required_dec_u32(text, path, "dirac digital_gains db"),
                ),
            });
            continue;
        }
        if let Some(text) = extract_tag_text(line, "polarity") {
            polarity = Some(IndexedWords {
                idx: parse_required_attr_usize(line, "idx", path, "dirac digital_gains polarity"),
                words: repeat_words(
                    count,
                    parse_required_dec_u32(text, path, "dirac digital_gains polarity"),
                ),
            });
        }
    }

    panic!(
        "unterminated dirac digital_gains block in {}",
        path.display()
    );
}

fn parse_dirac_meters(
    lines: &[&str],
    line_idx: &mut usize,
    path: &Path,
    builder: &mut DiracDefaultsBuilder,
) {
    let line = lines[*line_idx].trim();
    let count = parse_required_attr_usize(line, "cnt", path, "dirac meters");
    let idx = parse_required_attr_usize(line, "idx", path, "dirac meters");
    let mut meter_values = Vec::new();

    while *line_idx + 1 < lines.len() {
        *line_idx += 1;
        let line = lines[*line_idx].trim();

        if line.is_empty() || line.starts_with("<!--") {
            continue;
        }
        if line.starts_with("</meters>") {
            let words = if meter_values.len() == 1 {
                repeat_words(count, meter_values[0])
            } else if meter_values.len() == count {
                meter_values
            } else {
                panic!(
                    "dirac meters count mismatch (expected 1 or {count}, got {}) in {}",
                    meter_values.len(),
                    path.display()
                );
            };
            builder.meters = Some(IndexedWords { idx, words });
            return;
        }
        if let Some(text) = extract_tag_text(line, "meter") {
            meter_values.push(parse_required_dec_u32(text, path, "dirac meter"));
        }
    }

    panic!("unterminated dirac meters block in {}", path.display());
}

fn parse_dirac_fir(
    lines: &[&str],
    line_idx: &mut usize,
    path: &Path,
    builder: &mut DiracDefaultsBuilder,
) {
    let mut ctrl = None;
    let mut taps = None;

    while *line_idx + 1 < lines.len() {
        *line_idx += 1;
        let line = lines[*line_idx].trim();

        if line.is_empty() || line.starts_with("<!--") {
            continue;
        }
        if line.starts_with("</fir>") {
            builder.fir_ctrl = Some(
                ctrl.unwrap_or_else(|| panic!("dirac fir ctrl missing in {}", path.display())),
            );
            builder.fir_taps = Some(
                taps.unwrap_or_else(|| panic!("dirac fir taps missing in {}", path.display())),
            );
            return;
        }
        if let Some(text) = extract_tag_text(line, "ctrl") {
            ctrl = Some(IndexedWord {
                idx: parse_required_attr_usize(line, "idx", path, "dirac fir ctrl"),
                word: parse_required_dec_u32(text, path, "dirac fir ctrl"),
            });
            continue;
        }
        if let Some(text) = extract_tag_text(line, "taps") {
            let size = parse_required_attr_usize(line, "size", path, "dirac fir taps");
            let idx = parse_required_attr_usize(line, "idx", path, "dirac fir taps");
            let fill = parse_required_dec_u32(text, path, "dirac fir taps");
            let fraction = parse_hex_u32(
                &parse_required_attr(line, "iirFraction", path, "dirac fir taps"),
                path,
            );
            let mut words = repeat_words(size, fill);
            if words.is_empty() {
                panic!("dirac fir taps cannot be empty in {}", path.display());
            }
            match parse_required_attr(line, "iirFractionLocation", path, "dirac fir taps").as_str()
            {
                "begin" => words[0] = fraction,
                "end" => *words.last_mut().unwrap() = fraction,
                other => panic!(
                    "unexpected dirac fir iirFractionLocation '{other}' in {}",
                    path.display()
                ),
            }
            taps = Some(IndexedWords { idx, words });
        }
    }

    panic!("unterminated dirac fir block in {}", path.display());
}

fn parse_dirac_iir(
    lines: &[&str],
    line_idx: &mut usize,
    path: &Path,
    builder: &mut DiracDefaultsBuilder,
) {
    let open_line = lines[*line_idx].trim();
    let fraction = parse_hex_u32(
        &parse_required_attr(open_line, "fraction", path, "dirac iir"),
        path,
    );
    let mut taps = None;

    while *line_idx + 1 < lines.len() {
        *line_idx += 1;
        let line = lines[*line_idx].trim();

        if line.is_empty() || line.starts_with("<!--") {
            continue;
        }
        if line.starts_with("</iir>") {
            builder.iir_taps = Some(
                taps.unwrap_or_else(|| panic!("dirac iir taps missing in {}", path.display())),
            );
            return;
        }
        if let Some(text) = extract_tag_text(line, "taps") {
            let size = parse_required_attr_usize(line, "size", path, "dirac iir taps");
            let dimension = parse_required_attr_usize(line, "dimension", path, "dirac iir taps");
            let fraction_dim =
                parse_required_attr_usize(line, "iirFractionDim", path, "dirac iir taps");
            let idx = parse_required_attr_usize(line, "idx", path, "dirac iir taps");
            let fill = parse_required_dec_u32(text, path, "dirac iir taps");
            let mut words = repeat_words(size * dimension, fill);
            for section in 0..size {
                let fraction_idx = fraction_dim + section * dimension;
                if fraction_idx >= words.len() {
                    panic!(
                        "dirac iir fraction index {} overruns taps buffer in {}",
                        fraction_idx,
                        path.display()
                    );
                }
                words[fraction_idx] = fraction;
            }
            taps = Some(IndexedWords { idx, words });
        }
    }

    panic!("unterminated dirac iir block in {}", path.display());
}

fn append_dirac_defaults(params: &mut Vec<u8>, dirac: &DiracDefaults, path: &Path) {
    if params.len() > dirac.first_channel_offset {
        panic!(
            "dirac start {} precedes params length {} in {}",
            dirac.first_channel_offset,
            params.len(),
            path.display()
        );
    }

    if params.len() < dirac.first_channel_offset {
        params.resize(dirac.first_channel_offset, 0);
    }
    params.resize(
        dirac.first_channel_offset + dirac.channel_count * dirac.channel_size,
        0,
    );

    for channel_idx in 0..dirac.channel_count {
        let channel_base = dirac.first_channel_offset + channel_idx * dirac.channel_size;
        write_channel_words_le(
            params,
            channel_base,
            dirac.channel_size,
            &dirac.digital_gain_status,
            "dirac digital_gain_status",
            path,
        );
        write_channel_words_le(
            params,
            channel_base,
            dirac.channel_size,
            &dirac.digital_gain_db,
            "dirac digital_gain_db",
            path,
        );
        write_channel_words_le(
            params,
            channel_base,
            dirac.channel_size,
            &dirac.meters,
            "dirac meters",
            path,
        );
        write_channel_word_le(
            params,
            channel_base,
            dirac.channel_size,
            dirac.delay,
            "dirac delay",
            path,
        );
        write_channel_words_le(
            params,
            channel_base,
            dirac.channel_size,
            &dirac.digital_gain_polarity,
            "dirac digital_gain_polarity",
            path,
        );
        write_channel_word_le(
            params,
            channel_base,
            dirac.channel_size,
            dirac.mux_bypass,
            "dirac mux_bypass",
            path,
        );
        write_channel_word_le(
            params,
            channel_base,
            dirac.channel_size,
            dirac.dirac_delay,
            "dirac dirac_delay",
            path,
        );
        write_channel_word_le(
            params,
            channel_base,
            dirac.channel_size,
            dirac.mixer_fir,
            "dirac mixer_fir",
            path,
        );
        write_channel_words_le(
            params,
            channel_base,
            dirac.channel_size,
            &dirac.mixer_iir,
            "dirac mixer_iir",
            path,
        );
        write_channel_word_le(
            params,
            channel_base,
            dirac.channel_size,
            dirac.fir_ctrl,
            "dirac fir_ctrl",
            path,
        );
        write_channel_words_le(
            params,
            channel_base,
            dirac.channel_size,
            &dirac.fir_taps,
            "dirac fir_taps",
            path,
        );
        write_channel_words_le(
            params,
            channel_base,
            dirac.channel_size,
            &dirac.iir_taps,
            "dirac iir_taps",
            path,
        );
    }
}

fn parse_required_attr(line: &str, attr: &str, path: &Path, context: &str) -> String {
    extract_attr(line, attr)
        .unwrap_or_else(|| panic!("missing {context} {attr} in {}", path.display()))
}

fn parse_required_attr_usize(line: &str, attr: &str, path: &Path, context: &str) -> usize {
    parse_required_attr(line, attr, path, context)
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("invalid {context} {attr} in {}", path.display()))
}

fn parse_required_dec_u32(text: &str, path: &Path, context: &str) -> u32 {
    text.trim()
        .parse::<u32>()
        .unwrap_or_else(|_| panic!("invalid {context} value in {}", path.display()))
}

fn parse_dirac_scalar_tag(line: &str, tag: &str, path: &Path) -> IndexedWord {
    let text = extract_tag_text(line, tag)
        .unwrap_or_else(|| panic!("missing dirac {tag} payload in {}", path.display()));
    IndexedWord {
        idx: parse_required_attr_usize(line, "idx", path, &format!("dirac {tag}")),
        word: parse_required_dec_u32(text, path, &format!("dirac {tag}")),
    }
}

fn parse_dirac_repeated_words_tag(line: &str, tag: &str, path: &Path) -> IndexedWords {
    let text = extract_tag_text(line, tag)
        .unwrap_or_else(|| panic!("missing dirac {tag} payload in {}", path.display()));
    let size = parse_required_attr_usize(line, "size", path, &format!("dirac {tag}"));
    IndexedWords {
        idx: parse_required_attr_usize(line, "idx", path, &format!("dirac {tag}")),
        words: repeat_words(
            size,
            parse_required_dec_u32(text, path, &format!("dirac {tag}")),
        ),
    }
}

fn repeat_words(len: usize, word: u32) -> Vec<u32> {
    vec![word; len]
}

fn extract_attr(line: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = line.find(&needle)? + needle.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

fn extract_tag_text<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start = line.find(&open)?;
    let content_start = line[start..].find('>')? + start + 1;
    let end = line[content_start..].find(&close)? + content_start;
    Some(line[content_start..end].trim())
}

fn parse_hex_csv(text: &str) -> Vec<u32> {
    text.split(',')
        .filter_map(|word| {
            let trimmed = word.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .map(|word| {
            u32::from_str_radix(word.trim_start_matches("0x"), 16)
                .unwrap_or_else(|_| panic!("invalid hex word: {word}"))
        })
        .collect()
}

fn parse_dec_csv(text: &str) -> Vec<u32> {
    text.split(',')
        .filter_map(|word| {
            let trimmed = word.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .map(|word| {
            word.parse::<f32>()
                .unwrap_or_else(|_| panic!("invalid decimal word: {word}"))
                .to_bits()
        })
        .collect()
}

fn parse_hex_u32(text: &str, path: &Path) -> u32 {
    u32::from_str_radix(text.trim().trim_start_matches("0x"), 16)
        .unwrap_or_else(|_| panic!("invalid hex value in {}", path.display()))
}

fn write_words_le(params: &mut [u8], addr: u16, words: &[u32], path: &Path) {
    let limit = params.len();
    write_words_le_at_offset(
        params,
        addr as usize * 4,
        words,
        limit,
        "default preset params",
        path,
    );
}

fn write_channel_word_le(
    params: &mut [u8],
    channel_base: usize,
    channel_size: usize,
    field: IndexedWord,
    field_name: &str,
    path: &Path,
) {
    write_words_le_at_offset(
        params,
        channel_base + field.idx * 4,
        &[field.word],
        channel_base + channel_size,
        field_name,
        path,
    );
}

fn write_channel_words_le(
    params: &mut [u8],
    channel_base: usize,
    channel_size: usize,
    field: &IndexedWords,
    field_name: &str,
    path: &Path,
) {
    write_words_le_at_offset(
        params,
        channel_base + field.idx * 4,
        &field.words,
        channel_base + channel_size,
        field_name,
        path,
    );
}

fn write_words_le_at_offset(
    params: &mut [u8],
    start: usize,
    words: &[u32],
    limit: usize,
    field_name: &str,
    path: &Path,
) {
    let end = start + words.len() * 4;
    if end > limit || end > params.len() {
        panic!(
            "{field_name} write overruns buffer at byte offset {start} in {}",
            path.display()
        );
    }
    let mut offset = start;
    for word in words {
        params[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        offset += 4;
    }
}

fn bypass_type_byte(type_name: &str) -> u8 {
    if type_name == "PEQ" { 0x01 } else { 0x04 }
}
