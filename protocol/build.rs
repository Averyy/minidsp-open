use std::f64::consts::PI;
use std::fs;
use std::path::{Path, PathBuf};

const FAST_SIN_TABLE_SIZE: usize = 512;
const DEFAULT_PRESET_DSPS: &[u8] = &[100, 110, 115];

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
        source.push_str(&format!("    {:.9}f32,\n", phase.sin() as f32));
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

fn write_default_preset_images(out_dir: &Path) {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing"));
    let repo_root = manifest_dir
        .parent()
        .expect("protocol crate should live under repo root");
    let audio_fw_root = repo_root.join("app").join("Audio Firmware");
    println!("cargo:rerun-if-changed={}", audio_fw_root.display());

    let mut source = String::from(
        "// Generated from app/Audio Firmware/*/DSP_default_parameters.xml. Do not edit by hand.\n\n",
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

        source.push_str(&format!(
            "static DSP_{dsp_version}_PARAMS: [u8; {}] = [\n",
            params.len()
        ));
        source.push_str(&format_bytes(&params));
        source.push_str("];\n\n");

        source.push_str(&format!(
            "static DSP_{dsp_version}_BYPASS: [u8; {}] = [\n",
            bypass.len()
        ));
        source.push_str(&format_bytes(&bypass));
        source.push_str("];\n\n");

        source.push_str(&format!(
            "static DSP_{dsp_version}_IMAGE: DefaultPresetImage = DefaultPresetImage {{\n\
                 dsp_version: {dsp_version},\n\
                 params: &DSP_{dsp_version}_PARAMS,\n\
                 bypass: &DSP_{dsp_version}_BYPASS,\n\
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
    let mut params: Vec<u8> = Vec::new();
    let mut fir_status: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut bypass_records = Vec::new();

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

    for raw_line in content.lines() {
        let line = raw_line.trim();

        if params.is_empty() && line.starts_with("<params ") {
            let size = extract_attr(line, "size")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_else(|| panic!("missing/invalid params size in {}", path.display()));
            params.resize(size, 0);
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
                    continue;
                }
                if let Some(dec) = extract_tag_text(line, "dec") {
                    *words = Some(parse_dec_csv(dec));
                    continue;
                }
                if let Some(text) = extract_tag_text(line, "bypass") {
                    *bypassed = Some(text.parse::<u8>().map(|value| value != 0).unwrap_or(false));
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
                continue;
            }
            Some(Block::Fir {
                addr,
                channel,
                words,
            }) => {
                if let Some(hex) = extract_tag_text(line, "subpara") {
                    words.extend(parse_hex_csv(hex));
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
        }
    }

    let mut bypass = Vec::with_capacity(bypass_records.len() * 3);
    for record in bypass_records {
        bypass.push(record.type_byte | if record.bypassed { 0x80 } else { 0x00 });
        bypass.extend_from_slice(&record.addr.to_be_bytes());
    }

    (params, bypass)
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
    let start = addr as usize * 4;
    let end = start + words.len() * 4;
    if end > params.len() {
        panic!(
            "default preset write overruns params buffer at addr {} in {}",
            addr,
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

fn format_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(16) {
        out.push_str("    ");
        for (idx, byte) in chunk.iter().enumerate() {
            if idx > 0 {
                out.push(' ');
            }
            out.push_str(&format!("0x{byte:02X},"));
        }
        out.push('\n');
    }
    out
}
