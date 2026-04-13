use std::{
    collections::BTreeSet,
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::model::{MetadataEntry, NormalizedMetadata};

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogueSearchEntry {
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "sortTitle")]
    pub sort_title: String,
    #[serde(default, rename = "altTitle")]
    pub alt_title: String,
    #[serde(default)]
    pub runtime: String,
    #[serde(default)]
    pub year: String,
    #[serde(default, rename = "content_type")]
    pub content_type: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default, rename = "audioTypes")]
    pub audio_types: Vec<String>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub edition: String,
    #[serde(default)]
    pub underlying: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchCandidate {
    pub digest: String,
    pub title: String,
    pub year: Option<String>,
    pub content_type: Option<String>,
    pub runtime_minutes: Option<f64>,
    pub duration_delta_seconds: Option<f64>,
    pub audio_types: Vec<String>,
    pub source: Option<String>,
    pub edition: Option<String>,
    pub confidence: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchRecord {
    pub source_label: String,
    pub tolerance_secs: u64,
    pub metadata: NormalizedMetadata,
    pub normalized_title: Option<String>,
    pub matched: bool,
    pub ambiguous: bool,
    pub best: Option<MatchCandidate>,
    pub candidates: Vec<MatchCandidate>,
}

#[derive(Debug, Clone)]
pub struct MetadataSample {
    pub source_label: String,
    pub entry: MetadataEntry,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CatalogueFile {
    Array(Vec<CatalogueSearchEntry>),
    Wrapped { entries: Vec<CatalogueSearchEntry> },
}

#[derive(Debug, Deserialize)]
struct SummarySampleFile {
    device_id: String,
    observed_at_ms: u64,
    normalized: NormalizedMetadata,
}

pub fn load_catalogue(path: &Path) -> Result<Vec<CatalogueSearchEntry>> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let catalogue = serde_json::from_slice::<CatalogueFile>(&bytes)
        .with_context(|| format!("failed to decode {}", path.display()))?;
    let entries = match catalogue {
        CatalogueFile::Array(entries) => entries,
        CatalogueFile::Wrapped { entries } => entries,
    };
    if entries.is_empty() {
        bail!("catalogue {} is empty", path.display());
    }
    Ok(entries)
}

pub fn normalize_title(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut prev_space = true;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            prev_space = false;
        } else if ch == '&' {
            if !prev_space {
                normalized.push(' ');
            }
            normalized.push_str("and");
            prev_space = false;
        } else if !prev_space {
            normalized.push(' ');
            prev_space = true;
        }
    }
    normalized.trim().to_owned()
}

fn metadata_title_keys(value: &str) -> Vec<String> {
    let normalized = normalize_title(value);
    let mut keys = BTreeSet::new();
    keys.insert(normalized.clone());

    if let Some(stripped) = normalized.strip_suffix(" and the movie") {
        let stripped = stripped.trim();
        if !stripped.is_empty() {
            keys.insert(stripped.to_owned());
        }
    }

    if let Some(stripped) = normalized.strip_suffix(" the movie") {
        let stripped = stripped.trim();
        if !stripped.is_empty() {
            keys.insert(stripped.to_owned());
        }
    }

    keys.into_iter().collect()
}

fn catalogue_title_keys(entry: &CatalogueSearchEntry) -> Vec<String> {
    let mut keys = BTreeSet::new();
    for value in [&entry.title, &entry.sort_title, &entry.alt_title] {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            keys.insert(normalize_title(trimmed));
        }
    }

    if let Some(underlying) = parse_underlying_title(&entry.underlying) {
        keys.insert(normalize_title(&underlying));
    }

    keys.into_iter().collect()
}

fn parse_underlying_title(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let end = trimmed
        .find(" (")
        .or_else(|| trimmed.find(" BEQ"))
        .unwrap_or(trimmed.len());
    let title = trimmed[..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_owned())
    }
}

pub fn load_latest_titled_entry(path: &Path) -> Result<MetadataSample> {
    let file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut latest: Option<MetadataEntry> = None;
    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("failed to read {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: MetadataEntry = match serde_json::from_str(&line) {
            Ok(entry) => entry,
            Err(err) => {
                debug!(
                    "skipping malformed metadata line {} in {}: {err}",
                    line_no + 1,
                    path.display()
                );
                continue;
            }
        };
        if is_matchable_entry(&entry) {
            latest = Some(entry);
        }
    }
    let entry = latest
        .ok_or_else(|| anyhow!("no titled now-playing entries found in {}", path.display()))?;
    Ok(MetadataSample {
        source_label: path.display().to_string(),
        entry,
    })
}

pub fn load_sample_file(path: &Path) -> Result<MetadataSample> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let entry = match serde_json::from_slice::<MetadataEntry>(&bytes) {
        Ok(entry) => entry,
        Err(_) => {
            let summary: SummarySampleFile = serde_json::from_slice(&bytes)
                .with_context(|| format!("failed to decode {}", path.display()))?;
            MetadataEntry {
                device_id: summary.device_id,
                observed_at_ms: summary.observed_at_ms,
                kind: "now_playing_event".to_owned(),
                raw: serde_json::json!({}),
                normalized: summary.normalized,
            }
        }
    };
    Ok(MetadataSample {
        source_label: path.display().to_string(),
        entry,
    })
}

pub fn load_debug_samples(debug_dir: &Path) -> Result<Vec<MetadataSample>> {
    let mut paths = fs::read_dir(debug_dir)
        .with_context(|| format!("failed to read {}", debug_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to enumerate {}", debug_dir.display()))?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();

    let mut samples = Vec::new();
    for path in paths {
        let sample = load_sample_file(&path)?;
        if is_matchable_entry(&sample.entry) {
            samples.push(sample);
        }
    }
    if samples.is_empty() {
        bail!("no titled debug samples found in {}", debug_dir.display());
    }
    Ok(samples)
}

pub fn build_adhoc_sample(title: String, duration_secs: f64) -> MetadataSample {
    MetadataSample {
        source_label: format!("adhoc:{title}"),
        entry: MetadataEntry {
            device_id: "adhoc".to_owned(),
            observed_at_ms: 0,
            kind: "adhoc".to_owned(),
            raw: serde_json::json!({}),
            normalized: NormalizedMetadata {
                title: Some(title),
                duration: Some(duration_secs),
                ..NormalizedMetadata::default()
            },
        },
    }
}

pub fn match_metadata(
    metadata: &NormalizedMetadata,
    catalogue: &[CatalogueSearchEntry],
    tolerance_secs: u64,
) -> MatchRecord {
    let normalized_title = metadata.title.as_deref().map(normalize_title);
    let title_keys = metadata
        .title
        .as_deref()
        .map(metadata_title_keys)
        .unwrap_or_default();
    let mut candidates = Vec::new();
    let mut seen_digests = BTreeSet::new();

    if let Some(duration_secs) = metadata.duration {
        for entry in catalogue {
            if !entry_matches_any_title(entry, &title_keys) {
                continue;
            }

            let runtime_minutes = parse_runtime_minutes(&entry.runtime);
            let runtime_secs = runtime_minutes.map(|minutes| minutes * 60.0);
            let Some(runtime_secs) = runtime_secs else {
                continue;
            };

            let delta = (duration_secs - runtime_secs).abs();
            if delta > tolerance_secs as f64 {
                continue;
            }
            if !seen_digests.insert(entry.digest.clone()) {
                continue;
            }

            let closeness = 1.0 - (delta / tolerance_secs as f64);
            let confidence = (0.70 + (0.30 * closeness)).clamp(0.0, 1.0);
            candidates.push(MatchCandidate {
                digest: entry.digest.clone(),
                title: entry.title.clone(),
                year: non_empty(&entry.year),
                content_type: non_empty(&entry.content_type),
                runtime_minutes,
                duration_delta_seconds: Some(delta),
                audio_types: entry.audio_types.clone(),
                source: non_empty(&entry.source),
                edition: non_empty(&entry.edition),
                confidence,
                reason: format!(
                    "exact normalized title match; runtime delta {:.0}s within {}s window",
                    delta, tolerance_secs
                ),
            });
        }
    }

    candidates.sort_by(|left, right| {
        left.duration_delta_seconds
            .partial_cmp(&right.duration_delta_seconds)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right
                    .confidence
                    .partial_cmp(&left.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.digest.cmp(&right.digest))
    });

    let ambiguous = candidates.len() > 1;
    if ambiguous {
        for candidate in &mut candidates {
            candidate.confidence = (candidate.confidence - 0.20).max(0.0);
        }
    }

    MatchRecord {
        source_label: String::new(),
        tolerance_secs,
        metadata: metadata.clone(),
        normalized_title,
        matched: !candidates.is_empty(),
        ambiguous,
        best: candidates.first().cloned(),
        candidates,
    }
}

pub fn is_matchable_entry(entry: &MetadataEntry) -> bool {
    entry.kind.starts_with("now_playing")
        && entry.normalized.title.is_some()
        && entry.normalized.duration.is_some()
}

fn entry_matches_any_title(entry: &CatalogueSearchEntry, expected: &[String]) -> bool {
    if expected.is_empty() {
        return false;
    }

    let candidates = catalogue_title_keys(entry);
    expected
        .iter()
        .any(|expected_key| candidates.iter().any(|candidate| candidate == expected_key))
}

fn parse_runtime_minutes(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{CatalogueSearchEntry, match_metadata, normalize_title};
    use crate::model::NormalizedMetadata;

    #[test]
    fn normalize_title_collapses_punctuation() {
        assert_eq!(
            normalize_title("Example Film B"),
            "peaky blinders the immortal man"
        );
        assert_eq!(normalize_title("F1: The Movie"), "f1 the movie");
    }

    #[test]
    fn exact_title_and_runtime_match_succeeds() {
        let catalogue = vec![CatalogueSearchEntry {
            title: "1917".to_owned(),
            sort_title: "1917".to_owned(),
            alt_title: String::new(),
            runtime: "119".to_owned(),
            year: "2019".to_owned(),
            content_type: "film".to_owned(),
            digest: "abc".to_owned(),
            audio_types: vec!["Atmos".to_owned()],
            source: "Disc".to_owned(),
            edition: String::new(),
            underlying: String::new(),
        }];
        let metadata = NormalizedMetadata {
            title: Some("1917".to_owned()),
            duration: Some(7140.0),
            ..NormalizedMetadata::default()
        };

        let record = match_metadata(&metadata, &catalogue, 900);
        assert!(record.matched);
        assert!(!record.ambiguous);
        assert_eq!(
            record.best.as_ref().map(|value| value.digest.as_str()),
            Some("abc")
        );
    }

    #[test]
    fn duration_window_rejects_far_runtime() {
        let catalogue = vec![CatalogueSearchEntry {
            title: "1917".to_owned(),
            sort_title: "1917".to_owned(),
            alt_title: String::new(),
            runtime: "119".to_owned(),
            year: "2019".to_owned(),
            content_type: "film".to_owned(),
            digest: "abc".to_owned(),
            audio_types: vec!["Atmos".to_owned()],
            source: "Disc".to_owned(),
            edition: String::new(),
            underlying: String::new(),
        }];
        let metadata = NormalizedMetadata {
            title: Some("1917".to_owned()),
            duration: Some(9000.0),
            ..NormalizedMetadata::default()
        };

        let record = match_metadata(&metadata, &catalogue, 900);
        assert!(!record.matched);
    }

    #[test]
    fn movie_suffix_fallback_matches_catalogue_short_title() {
        let catalogue = vec![CatalogueSearchEntry {
            title: "F1".to_owned(),
            sort_title: "f1".to_owned(),
            alt_title: String::new(),
            runtime: "156".to_owned(),
            year: "2025".to_owned(),
            content_type: "film".to_owned(),
            digest: "f1".to_owned(),
            audio_types: vec!["Atmos".to_owned()],
            source: "Disc".to_owned(),
            edition: String::new(),
            underlying: "F1 (2025) BEQ Atmos".to_owned(),
        }];
        let metadata = NormalizedMetadata {
            title: Some("F1 & The Movie".to_owned()),
            duration: Some(9699.928001),
            ..NormalizedMetadata::default()
        };

        let record = match_metadata(&metadata, &catalogue, 900);
        assert!(record.matched);
        assert_eq!(
            record.best.as_ref().map(|value| value.digest.as_str()),
            Some("f1")
        );
    }

    #[test]
    fn latest_entry_skips_malformed_jsonl_lines() {
        let path = temp_test_file("matcher-jsonl");
        fs::write(
            &path,
            concat!(
                "{\"not\":\"valid metadata entry\"}\n",
                "{bad json\n",
                "{\"device_id\":\"dev\",\"observed_at_ms\":1,\"kind\":\"now_playing_event\",\"raw\":{},\"normalized\":{\"title\":\"1917\",\"duration\":7140.0}}\n"
            ),
        )
        .expect("write temp jsonl");

        let sample = super::load_latest_titled_entry(&path).expect("sample should load");
        assert_eq!(sample.entry.normalized.title.as_deref(), Some("1917"));

        let _ = fs::remove_file(path);
    }

    fn temp_test_file(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}.jsonl"))
    }
}
