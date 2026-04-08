use std::f64::consts::PI;
use std::fs;
use std::path::PathBuf;

const FAST_SIN_TABLE_SIZE: usize = 512;

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR missing"));
    let output = out_dir.join("fast_sin_lut.rs");

    let mut source = String::from("const FAST_SIN_LUT: [f32; 513] = [\n");
    for idx in 0..=FAST_SIN_TABLE_SIZE {
        let phase = (idx as f64 * 2.0 * PI) / FAST_SIN_TABLE_SIZE as f64;
        source.push_str(&format!("    {:.9}f32,\n", phase.sin() as f32));
    }
    source.push_str("];\n");

    fs::write(output, source).expect("failed to write fast_sin_lut.rs");
}
