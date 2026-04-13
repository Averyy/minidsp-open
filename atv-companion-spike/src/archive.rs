use std::{collections::BTreeSet, io::Cursor};

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use plist::{Dictionary as PlistDictionary, Integer as PlistInteger, Uid, Value as PlistValue};
use serde_json::{Map, Value as JsonValue, json};

pub fn enrich_archived_payloads(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(map) => {
            if let Some(decoded) = decode_archive_object(map) {
                return decoded;
            }

            let mut out = Map::new();
            for (key, value) in map {
                out.insert(key.clone(), enrich_archived_payloads(value));
            }
            JsonValue::Object(out)
        }
        JsonValue::Array(values) => {
            JsonValue::Array(values.iter().map(enrich_archived_payloads).collect())
        }
        other => other.clone(),
    }
}

fn decode_archive_object(map: &Map<String, JsonValue>) -> Option<JsonValue> {
    if map.get("_type").and_then(JsonValue::as_str) != Some("bytes") {
        return None;
    }

    let encoded = map.get("base64")?.as_str()?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let start = raw.windows(8).position(|window| window == b"bplist00")?;
    let decoded = decode_keyed_archive(&raw[start..]).ok()?;

    let mut out = map.clone();
    out.insert("decoded_archive".to_owned(), decoded);
    if start > 0 {
        out.insert(
            "decoded_archive_trimmed_prefix_bytes".to_owned(),
            JsonValue::from(start as u64),
        );
    }
    Some(JsonValue::Object(out))
}

fn decode_keyed_archive(bytes: &[u8]) -> Result<JsonValue> {
    let plist =
        PlistValue::from_reader(Cursor::new(bytes)).context("failed to parse binary plist")?;
    let root = plist
        .as_dictionary()
        .ok_or_else(|| anyhow!("archive root was not a dictionary"))?;

    let objects = root
        .get("$objects")
        .and_then(PlistValue::as_array)
        .ok_or_else(|| anyhow!("archive missing $objects"))?;
    let top = root
        .get("$top")
        .and_then(PlistValue::as_dictionary)
        .ok_or_else(|| anyhow!("archive missing $top"))?;
    let root_ref = top
        .get("root")
        .or_else(|| top.values().next())
        .ok_or_else(|| anyhow!("archive top missing root reference"))?;

    let mut resolver = KeyedArchiveResolver::new(objects);
    resolver.resolve_value(root_ref)
}

struct KeyedArchiveResolver<'a> {
    objects: &'a [PlistValue],
    resolving: BTreeSet<usize>,
}

impl<'a> KeyedArchiveResolver<'a> {
    fn new(objects: &'a [PlistValue]) -> Self {
        Self {
            objects,
            resolving: BTreeSet::new(),
        }
    }

    fn resolve_value(&mut self, value: &PlistValue) -> Result<JsonValue> {
        match value {
            PlistValue::Array(values) => {
                let mut out = Vec::with_capacity(values.len());
                for value in values {
                    out.push(self.resolve_value(value)?);
                }
                Ok(JsonValue::Array(out))
            }
            PlistValue::Boolean(value) => Ok(JsonValue::Bool(*value)),
            PlistValue::Data(bytes) => Ok(json!({
                "_type": "plist_data",
                "base64": base64::engine::general_purpose::STANDARD.encode(bytes),
                "hex": hex::encode(bytes),
            })),
            PlistValue::Date(value) => Ok(JsonValue::String(value.to_xml_format())),
            PlistValue::Dictionary(dict) => self.resolve_dictionary(dict),
            PlistValue::Integer(value) => Ok(integer_to_json(value)),
            PlistValue::Real(value) => Ok(JsonValue::from(*value)),
            PlistValue::String(value) => Ok(JsonValue::String(value.clone())),
            PlistValue::Uid(uid) => self.resolve_uid(uid),
            _ => Ok(JsonValue::Null),
        }
    }

    fn resolve_dictionary(&mut self, dict: &PlistDictionary) -> Result<JsonValue> {
        let mut out = Map::new();
        if let Some(class_name) = dict
            .get("$class")
            .and_then(|value| self.class_name_for(value))
        {
            out.insert("_class".to_owned(), JsonValue::String(class_name));
        }

        for (key, value) in dict {
            if key == "$class" {
                continue;
            }
            out.insert(key.clone(), self.resolve_value(value)?);
        }
        Ok(JsonValue::Object(out))
    }

    fn class_name_for(&self, value: &PlistValue) -> Option<String> {
        let PlistValue::Uid(uid) = value else {
            return None;
        };
        let index = uid.get() as usize;
        let PlistValue::Dictionary(dict) = self.objects.get(index)? else {
            return None;
        };
        dict.get("$classname")
            .and_then(PlistValue::as_string)
            .map(str::to_owned)
    }

    fn resolve_uid(&mut self, uid: &Uid) -> Result<JsonValue> {
        let index = uid.get() as usize;
        if !self.resolving.insert(index) {
            return Ok(json!({ "_ref": index }));
        }

        let value = self
            .objects
            .get(index)
            .ok_or_else(|| anyhow!("archive uid {} out of range", index))?;
        let resolved = match value {
            PlistValue::String(value) if value == "$null" => JsonValue::Null,
            other => self.resolve_value(other)?,
        };
        self.resolving.remove(&index);
        Ok(resolved)
    }
}

fn integer_to_json(value: &PlistInteger) -> JsonValue {
    if let Some(signed) = value.as_signed() {
        JsonValue::from(signed)
    } else if let Some(unsigned) = value.as_unsigned() {
        JsonValue::from(unsigned)
    } else {
        JsonValue::Null
    }
}
