//! Output formatting — text and JSON compatible with minidsp-rs.

use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::status::StatusSummary;

/// Print status as text (default output mode).
pub fn print_status_text(summary: &StatusSummary, profile: &DeviceProfile) {
    println!("MiniDSP [{}]", profile.name);
    println!("  Preset: {}", summary.master.preset);
    println!("  Source: {}", summary.master.source);
    println!("  Volume: {:.1} dB", summary.master.volume);
    println!(
        "  Mute:   {}",
        if summary.master.mute { "on" } else { "off" }
    );
    if let Some(dirac) = summary.master.dirac {
        println!("  Dirac:  {}", if dirac { "on" } else { "off" });
    }
    if !summary.input_levels.is_empty() {
        print!("  Input levels: ");
        for (i, &level) in summary.input_levels.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!("{:.1}", level);
        }
        println!();
    }
    if !summary.output_levels.is_empty() {
        print!("  Output levels: ");
        for (i, &level) in summary.output_levels.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!("{:.1}", level);
        }
        println!();
    }
}

/// Print status as JSON.
///
/// `jsonline` matches upstream compact single-line output, while `json`
/// matches upstream pretty-printed JSON.
pub fn print_status_json(
    summary: &StatusSummary,
    pretty: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let rendered = if pretty {
        serde_json::to_string_pretty(summary)?
    } else {
        serde_json::to_string(summary)?
    };
    println!("{rendered}");
    Ok(())
}
