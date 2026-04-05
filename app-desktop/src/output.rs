//! Output formatting — text and JSON compatible with minidsp-rs.

use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::status::StatusSummary;

/// Print status as text (default output mode).
pub fn print_status_text(summary: &StatusSummary, profile: &DeviceProfile) {
    println!("MiniDSP [{}]", profile.name);
    println!("  Preset: {}", summary.master.preset);
    println!("  Source: {}", summary.master.source);
    println!("  Volume: {:.1} dB", summary.master.volume);
    println!("  Mute:   {}", if summary.master.mute { "on" } else { "off" });
    if let Some(dirac) = summary.master.dirac {
        println!("  Dirac:  {}", if dirac { "on" } else { "off" });
    }
    if !summary.input_levels.is_empty() {
        print!("  Input levels: ");
        for (i, &level) in summary.input_levels.iter().enumerate() {
            if i > 0 { print!(", "); }
            print!("{:.1}", level);
        }
        println!();
    }
    if !summary.output_levels.is_empty() {
        print!("  Output levels: ");
        for (i, &level) in summary.output_levels.iter().enumerate() {
            if i > 0 { print!(", "); }
            print!("{:.1}", level);
        }
        println!();
    }
}

/// Print status as JSON (minidsp -o jsonline compatible).
///
/// Uses serde_json for safe serialization — source names are properly escaped.
/// Format matches minidsp-rs StatusSummary:
/// ```json
/// {"master":{"preset":0,"source":"Toslink","volume":-8.0,"mute":false},"input_levels":[-131.36],"output_levels":[-140.2]}
/// ```
pub fn print_status_json(summary: &StatusSummary) -> Result<(), Box<dyn std::error::Error>> {
    let mut master = serde_json::json!({
        "preset": summary.master.preset,
        "source": summary.master.source,
        "volume": summary.master.volume,
        "mute": summary.master.mute,
    });
    if let Some(dirac) = summary.master.dirac {
        master.as_object_mut().unwrap().insert("dirac".to_string(), serde_json::json!(dirac));
    }

    let json = serde_json::json!({
        "master": master,
        "input_levels": summary.input_levels,
        "output_levels": summary.output_levels,
    });

    println!("{}", serde_json::to_string(&json)?);
    Ok(())
}
