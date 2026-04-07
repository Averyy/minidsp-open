//! Audio source definitions per device.
//!
//! Source names and indices vary by hw_id. This module defines the mapping.

use core::fmt;

/// Audio input source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Analog,
    Toslink,
    Spdif,
    Usb,
    Bluetooth,
    Hdmi,
    AesEbu,
    /// SHD-specific analog inputs.
    AnalogSingleEnd,
    AnalogBalanced,
    Lan,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Lowercase for CLI/URL path usage
        match self {
            Source::Analog => write!(f, "analog"),
            Source::Toslink => write!(f, "toslink"),
            Source::Spdif => write!(f, "spdif"),
            Source::Usb => write!(f, "usb"),
            Source::Bluetooth => write!(f, "bluetooth"),
            Source::Hdmi => write!(f, "hdmi"),
            Source::AesEbu => write!(f, "aesebu"),
            Source::AnalogSingleEnd => write!(f, "analog_se"),
            Source::AnalogBalanced => write!(f, "analog_bal"),
            Source::Lan => write!(f, "lan"),
        }
    }
}

impl Source {
    /// PascalCase name for JSON serialization (serde compatibility with minidsp-rs).
    pub fn pascal_case(&self) -> &'static str {
        match self {
            Source::Analog => "Analog",
            Source::Toslink => "Toslink",
            Source::Spdif => "Spdif",
            Source::Usb => "Usb",
            Source::Bluetooth => "Bluetooth",
            Source::Hdmi => "Hdmi",
            Source::AesEbu => "Aesebu",
            Source::AnalogSingleEnd => "Rca",
            Source::AnalogBalanced => "Xlr",
            Source::Lan => "Lan",
        }
    }

    /// Parse from lowercase string (CLI/URL path).
    pub fn from_str_lowercase(s: &str) -> Option<Self> {
        match s {
            "analog" => Some(Source::Analog),
            "toslink" => Some(Source::Toslink),
            "spdif" => Some(Source::Spdif),
            "usb" => Some(Source::Usb),
            "bluetooth" => Some(Source::Bluetooth),
            "hdmi" => Some(Source::Hdmi),
            "aesebu" => Some(Source::AesEbu),
            "analog_se" | "rca" => Some(Source::AnalogSingleEnd),
            "analog_bal" | "xlr" => Some(Source::AnalogBalanced),
            "lan" => Some(Source::Lan),
            _ => None,
        }
    }

    /// Parse from PascalCase string (JSON).
    pub fn from_str_pascal(s: &str) -> Option<Self> {
        match s {
            "Analog" => Some(Source::Analog),
            "Toslink" => Some(Source::Toslink),
            "Spdif" => Some(Source::Spdif),
            "Usb" => Some(Source::Usb),
            "Bluetooth" => Some(Source::Bluetooth),
            "Hdmi" => Some(Source::Hdmi),
            "Aesebu" => Some(Source::AesEbu),
            "Rca" | "RCA" | "AnalogSingleEnd" => Some(Source::AnalogSingleEnd),
            "Xlr" | "XLR" | "AnalogBalanced" => Some(Source::AnalogBalanced),
            "Lan" => Some(Source::Lan),
            _ => None,
        }
    }
}

/// Source map for a specific device — maps source index to Source variant.
#[derive(Debug, Clone)]
pub struct SourceMap {
    entries: &'static [(u8, Source)],
}

impl SourceMap {
    pub const fn new(entries: &'static [(u8, Source)]) -> Self {
        Self { entries }
    }

    pub fn source_for_index(&self, idx: u8) -> Option<Source> {
        self.entries.iter().find(|(i, _)| *i == idx).map(|(_, s)| *s)
    }

    pub fn index_for_source(&self, source: Source) -> Option<u8> {
        self.entries.iter().find(|(_, s)| *s == source).map(|(i, _)| *i)
    }

    pub fn sources(&self) -> &[(u8, Source)] {
        self.entries
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }
}

// -- Per-device source maps --

pub static SOURCES_2X4HD: SourceMap = SourceMap::new(&[
    (0, Source::Analog),
    (1, Source::Toslink),
    (2, Source::Usb),
]);

pub static SOURCES_FLEX: SourceMap = SourceMap::new(&[
    (0, Source::Analog),
    (1, Source::Toslink),
    (2, Source::Spdif),
    (3, Source::Usb),
    (4, Source::Bluetooth),
]);

pub static SOURCES_FLEX_EIGHT: SourceMap = SourceMap::new(&[
    (1, Source::Toslink),
    (2, Source::Spdif),
    (3, Source::Usb),
    (4, Source::Bluetooth),
]);

pub static SOURCES_FLEX_HT: SourceMap = SourceMap::new(&[
    (1, Source::Toslink),
    (2, Source::Spdif),
    (3, Source::Usb),
    (4, Source::Hdmi),
]);

pub static SOURCES_FLEX_HTX: SourceMap = SourceMap::new(&[
    (0, Source::Analog),
    (1, Source::Toslink),
    (2, Source::Spdif),
    (3, Source::Usb),
    (4, Source::Hdmi),
]);

pub static SOURCES_SHD: SourceMap = SourceMap::new(&[
    (0, Source::Toslink),
    (1, Source::Spdif),
    (2, Source::AesEbu),
    (3, Source::AnalogSingleEnd),
    (4, Source::AnalogBalanced),
    (5, Source::Usb),
    (6, Source::Lan),
]);

pub static SOURCES_SHD_STUDIO: SourceMap = SourceMap::new(&[
    (0, Source::Toslink),
    (1, Source::Spdif),
    (2, Source::AesEbu),
    (3, Source::Usb),
    (4, Source::Lan),
]);

pub static SOURCES_SHD_POWER: SourceMap = SourceMap::new(&[
    (0, Source::Toslink),
    (1, Source::Spdif),
    (2, Source::AesEbu),
    (3, Source::Usb),
    (4, Source::Lan),
]);

// Legacy sources
pub static SOURCES_4X10HD: SourceMap = SourceMap::new(&[
    (0, Source::Spdif),
    (1, Source::Toslink),
    (2, Source::AesEbu),
]);

pub static SOURCES_LEGACY_2: SourceMap = SourceMap::new(&[
    (0, Source::Toslink),
    (1, Source::Spdif),
]);
