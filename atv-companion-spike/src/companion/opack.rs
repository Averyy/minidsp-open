use std::collections::BTreeMap;

use anyhow::{Result, bail};
use base64::Engine;
use serde_json::{Map, Value as JsonValue, json};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Null,
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Uuid([u8; 16]),
    Array(Vec<Value>),
    Dict(BTreeMap<String, Value>),
}

impl Value {
    pub fn int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(value) => Some(value),
            _ => None,
        }
    }

    pub fn dict_get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Dict(entries) => entries.get(key),
            _ => None,
        }
    }

    pub fn to_json(&self) -> JsonValue {
        match self {
            Self::Bool(value) => JsonValue::Bool(*value),
            Self::Null => JsonValue::Null,
            Self::Int(value) => JsonValue::from(*value),
            Self::Float(value) => JsonValue::from(*value),
            Self::String(value) => JsonValue::String(value.clone()),
            Self::Bytes(value) => json!({
                "_type": "bytes",
                "hex": hex::encode(value),
                "base64": base64::engine::general_purpose::STANDARD.encode(value),
            }),
            Self::Uuid(bytes) => JsonValue::String(hex::encode(bytes)),
            Self::Array(values) => JsonValue::Array(values.iter().map(Value::to_json).collect()),
            Self::Dict(values) => {
                let mut map = Map::new();
                for (key, value) in values {
                    map.insert(key.clone(), value.to_json());
                }
                JsonValue::Object(map)
            }
        }
    }
}

pub fn encode(value: &Value) -> Vec<u8> {
    let mut encoder = Encoder { output: Vec::new() };
    encoder.write(value);
    encoder.output
}

pub fn decode(data: &[u8]) -> Result<Value> {
    let mut decoder = Decoder::new(data);
    decoder.read_value()
}

struct Encoder {
    output: Vec<u8>,
}

impl Encoder {
    fn write(&mut self, value: &Value) {
        match value {
            Value::Bool(true) => self.output.push(0x01),
            Value::Bool(false) => self.output.push(0x02),
            Value::Null => self.output.push(0x04),
            Value::Uuid(value) => {
                self.output.push(0x05);
                self.output.extend_from_slice(value);
            }
            Value::Int(value) => self.write_int(*value),
            Value::Float(value) => {
                self.output.push(0x36);
                self.output.extend_from_slice(&value.to_le_bytes());
            }
            Value::String(value) => self.write_string(value),
            Value::Bytes(value) => self.write_bytes(value),
            Value::Array(values) => {
                if values.len() <= 14 {
                    self.output.push(0xD0 + values.len() as u8);
                    for value in values {
                        self.write(value);
                    }
                } else {
                    self.output.push(0xDF);
                    for value in values {
                        self.write(value);
                    }
                    self.output.push(0x03);
                }
            }
            Value::Dict(values) => {
                if values.len() <= 14 {
                    self.output.push(0xE0 + values.len() as u8);
                    for (key, value) in values {
                        self.write(&Value::String(key.clone()));
                        self.write(value);
                    }
                } else {
                    self.output.push(0xEF);
                    for (key, value) in values {
                        self.write(&Value::String(key.clone()));
                        self.write(value);
                    }
                    self.output.push(0x03);
                }
            }
        }
    }

    fn write_int(&mut self, value: i64) {
        if (0..=39).contains(&value) {
            self.output.push(0x08 + value as u8);
        } else if (i8::MIN as i64..=i8::MAX as i64).contains(&value) {
            self.output.push(0x30);
            self.output.push(value as i8 as u8);
        } else if (i16::MIN as i64..=i16::MAX as i64).contains(&value) {
            self.output.push(0x31);
            self.output.extend_from_slice(&(value as i16).to_le_bytes());
        } else if (i32::MIN as i64..=i32::MAX as i64).contains(&value) {
            self.output.push(0x32);
            self.output.extend_from_slice(&(value as i32).to_le_bytes());
        } else {
            self.output.push(0x33);
            self.output.extend_from_slice(&value.to_le_bytes());
        }
    }

    fn write_string(&mut self, value: &str) {
        let bytes = value.as_bytes();
        match bytes.len() {
            0..=32 => self.output.push(0x40 + bytes.len() as u8),
            33..=0xFF => {
                self.output.push(0x61);
                self.output.push(bytes.len() as u8);
            }
            0x100..=0xFFFF => {
                self.output.push(0x62);
                self.output
                    .extend_from_slice(&(bytes.len() as u16).to_le_bytes());
            }
            _ => {
                self.output.push(0x64);
                self.output
                    .extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            }
        }
        self.output.extend_from_slice(bytes);
    }

    fn write_bytes(&mut self, value: &[u8]) {
        match value.len() {
            0..=32 => self.output.push(0x70 + value.len() as u8),
            33..=0xFF => {
                self.output.push(0x91);
                self.output.push(value.len() as u8);
            }
            0x100..=0xFFFF => {
                self.output.push(0x92);
                self.output
                    .extend_from_slice(&(value.len() as u16).to_le_bytes());
            }
            _ => {
                self.output.push(0x94);
                self.output
                    .extend_from_slice(&(value.len() as u32).to_le_bytes());
            }
        }
        self.output.extend_from_slice(value);
    }
}

struct Decoder<'a> {
    data: &'a [u8],
    offset: usize,
    objects: Vec<Value>,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            offset: 0,
            objects: Vec::new(),
        }
    }

    fn read_value(&mut self) -> Result<Value> {
        let tag = self.read_byte()?;
        self.read_tag(tag)
    }

    fn read_tag(&mut self, tag: u8) -> Result<Value> {
        let value = match tag {
            0x01 => Value::Bool(true),
            0x02 => Value::Bool(false),
            0x04 => Value::Null,
            0x05 => {
                let bytes = self.read_bytes(16)?;
                let mut uuid = [0_u8; 16];
                uuid.copy_from_slice(bytes);
                Value::Uuid(uuid)
            }
            0x08..=0x2F => Value::Int((tag - 0x08) as i64),
            0x30 => Value::Int(self.read_byte()? as i8 as i64),
            0x31 => {
                let mut bytes = [0_u8; 2];
                bytes.copy_from_slice(self.read_bytes(2)?);
                Value::Int(i16::from_le_bytes(bytes) as i64)
            }
            0x32 => {
                let mut bytes = [0_u8; 4];
                bytes.copy_from_slice(self.read_bytes(4)?);
                Value::Int(i32::from_le_bytes(bytes) as i64)
            }
            0x33 => {
                let mut bytes = [0_u8; 8];
                bytes.copy_from_slice(self.read_bytes(8)?);
                Value::Int(i64::from_le_bytes(bytes))
            }
            0x35 => {
                let mut bytes = [0_u8; 4];
                bytes.copy_from_slice(self.read_bytes(4)?);
                Value::Float(f32::from_le_bytes(bytes) as f64)
            }
            0x36 => {
                let mut bytes = [0_u8; 8];
                bytes.copy_from_slice(self.read_bytes(8)?);
                Value::Float(f64::from_le_bytes(bytes))
            }
            0x40..=0x60 => {
                let len = (tag - 0x40) as usize;
                let bytes = self.read_bytes(len)?;
                Value::String(String::from_utf8(bytes.to_vec())?)
            }
            0x61 => {
                let len = self.read_byte()? as usize;
                let bytes = self.read_bytes(len)?;
                Value::String(String::from_utf8(bytes.to_vec())?)
            }
            0x62 => {
                let mut bytes = [0_u8; 2];
                bytes.copy_from_slice(self.read_bytes(2)?);
                let len = u16::from_le_bytes(bytes) as usize;
                let bytes = self.read_bytes(len)?;
                Value::String(String::from_utf8(bytes.to_vec())?)
            }
            0x63 => {
                let len_bytes = self.read_bytes(3)?;
                let len = (len_bytes[0] as usize)
                    | ((len_bytes[1] as usize) << 8)
                    | ((len_bytes[2] as usize) << 16);
                let bytes = self.read_bytes(len)?;
                Value::String(String::from_utf8(bytes.to_vec())?)
            }
            0x64 => {
                let mut bytes = [0_u8; 4];
                bytes.copy_from_slice(self.read_bytes(4)?);
                let len = u32::from_le_bytes(bytes) as usize;
                let bytes = self.read_bytes(len)?;
                Value::String(String::from_utf8(bytes.to_vec())?)
            }
            0x70..=0x90 => Value::Bytes(self.read_bytes((tag - 0x70) as usize)?.to_vec()),
            0x91 => {
                let len = self.read_byte()? as usize;
                Value::Bytes(self.read_bytes(len)?.to_vec())
            }
            0x92 => {
                let mut bytes = [0_u8; 2];
                bytes.copy_from_slice(self.read_bytes(2)?);
                let len = u16::from_le_bytes(bytes) as usize;
                Value::Bytes(self.read_bytes(len)?.to_vec())
            }
            0x93 | 0x94 => {
                let mut bytes = [0_u8; 4];
                bytes.copy_from_slice(self.read_bytes(4)?);
                let len = u32::from_le_bytes(bytes) as usize;
                Value::Bytes(self.read_bytes(len)?.to_vec())
            }
            0xA0..=0xBF => {
                let index = (tag - 0xA0) as usize;
                self.objects
                    .get(index)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("invalid OPACK reference {index}"))?
            }
            0xC1 => {
                let index = self.read_byte()? as usize;
                self.objects
                    .get(index)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("invalid OPACK reference {index}"))?
            }
            0xC2 => {
                let mut bytes = [0_u8; 2];
                bytes.copy_from_slice(self.read_bytes(2)?);
                let index = u16::from_le_bytes(bytes) as usize;
                self.objects
                    .get(index)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("invalid OPACK reference {index}"))?
            }
            0xD0..=0xDE => {
                let count = (tag - 0xD0) as usize;
                let slot = self.reserve();
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(self.read_value()?);
                }
                self.fill(slot, Value::Array(values))
            }
            0xDF => {
                let slot = self.reserve();
                let mut values = Vec::new();
                loop {
                    let next = self.read_byte()?;
                    if next == 0x03 {
                        break;
                    }
                    values.push(self.read_tag(next)?);
                }
                self.fill(slot, Value::Array(values))
            }
            0xE0..=0xEE => {
                let count = (tag - 0xE0) as usize;
                let slot = self.reserve();
                let mut values = BTreeMap::new();
                for _ in 0..count {
                    let key = self.read_value()?;
                    let Value::String(key) = key else {
                        bail!("dictionary key was not a string");
                    };
                    values.insert(key, self.read_value()?);
                }
                self.fill(slot, Value::Dict(values))
            }
            0xEF => {
                let slot = self.reserve();
                let mut values = BTreeMap::new();
                loop {
                    let next = self.read_byte()?;
                    if next == 0x03 {
                        break;
                    }
                    let key = self.read_tag(next)?;
                    let Value::String(key) = key else {
                        bail!("dictionary key was not a string");
                    };
                    values.insert(key, self.read_value()?);
                }
                self.fill(slot, Value::Dict(values))
            }
            other => bail!("unsupported OPACK tag 0x{other:02x}"),
        };

        match value {
            Value::Bool(_) | Value::Null | Value::Int(0..=39) => Ok(value),
            _ => {
                self.objects.push(value.clone());
                Ok(value)
            }
        }
    }

    fn reserve(&mut self) -> usize {
        self.objects.push(Value::Null);
        self.objects.len() - 1
    }

    fn fill(&mut self, slot: usize, value: Value) -> Value {
        self.objects[slot] = value.clone();
        value
    }

    fn read_byte(&mut self) -> Result<u8> {
        if self.offset >= self.data.len() {
            bail!("unexpected end of OPACK data");
        }
        let byte = self.data[self.offset];
        self.offset += 1;
        Ok(byte)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.offset + len > self.data.len() {
            bail!("unexpected end of OPACK data");
        }
        let bytes = &self.data[self.offset..self.offset + len];
        self.offset += len;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, decode, encode};

    #[test]
    fn signed_integer_boundaries_round_trip() {
        for value in [
            127_i64,
            128,
            255,
            32_767,
            32_768,
            65_535,
            i32::MAX as i64,
            i32::MAX as i64 + 1,
        ] {
            let encoded = encode(&Value::Int(value));
            let decoded = decode(&encoded).expect("decode succeeds");
            assert_eq!(
                decoded,
                Value::Int(value),
                "value {value} should round-trip"
            );
        }
    }
}
