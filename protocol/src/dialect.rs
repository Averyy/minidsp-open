//! Dialect layer: old (2-byte addr, 5.23 fixed-point) vs new (3-byte addr, float32).
//!
//! The dialect determines how addresses and values are encoded on the wire.
//! Old dialect: ADAU1445 (SigmaDSP) devices — 2-byte addresses, 5.23 fixed-point values.
//! New dialect: SHARC (ADSP21489) devices — 3-byte addresses, IEEE float32 LE values.

/// Address encoding format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrLen {
    /// 2-byte address (legacy ADAU1445 devices).
    Two,
    /// 3-byte address (SHARC devices).
    Three,
}

/// Value encoding format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueFormat {
    /// 5.23 fixed-point, big-endian (legacy).
    FixedPoint,
    /// IEEE 754 float32, little-endian (SHARC).
    Float32,
}

/// Wire dialect for a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dialect {
    pub addr_len: AddrLen,
    pub value_format: ValueFormat,
}

impl Dialect {
    /// New (SHARC) dialect: 3-byte addresses, float32 values.
    pub const NEW: Self = Self {
        addr_len: AddrLen::Three,
        value_format: ValueFormat::Float32,
    };

    /// Old (ADAU1445) dialect: 2-byte addresses, fixed-point values.
    pub const OLD: Self = Self {
        addr_len: AddrLen::Two,
        value_format: ValueFormat::FixedPoint,
    };
}

/// Encode a DSP address for the wire. The high bit (0x80) is the write flag.
pub fn encode_addr(addr: u16, write: bool, dialect: &Dialect) -> Vec<u8> {
    let write_bit = if write { 0x80 } else { 0x00 };
    match dialect.addr_len {
        AddrLen::Two => {
            vec![((addr >> 8) as u8) | write_bit, addr as u8]
        }
        AddrLen::Three => {
            vec![
                ((addr >> 8) as u8) | write_bit,
                addr as u8,
                0x00, // third byte, always 0 for addresses < 65536
            ]
        }
    }
}

/// Decode a DSP address from wire bytes.
/// Returns `(address, is_write)`.
pub fn decode_addr(data: &[u8], dialect: &Dialect) -> Option<(u16, bool)> {
    match dialect.addr_len {
        AddrLen::Two => {
            if data.len() < 2 {
                return None;
            }
            let write = data[0] & 0x80 != 0;
            let addr = ((data[0] & 0x7F) as u16) << 8 | data[1] as u16;
            Some((addr, write))
        }
        AddrLen::Three => {
            if data.len() < 3 {
                return None;
            }
            let write = data[0] & 0x80 != 0;
            let addr = ((data[0] & 0x7F) as u16) << 8 | data[1] as u16;
            Some((addr, write))
        }
    }
}

/// Encode a float value for the wire.
pub fn encode_value(value: f32, dialect: &Dialect) -> [u8; 4] {
    match dialect.value_format {
        ValueFormat::Float32 => value.to_le_bytes(),
        ValueFormat::FixedPoint => float_to_fixed_point(value).to_be_bytes(),
    }
}

/// Decode a float value from wire bytes.
pub fn decode_value(data: &[u8; 4], dialect: &Dialect) -> f32 {
    match dialect.value_format {
        ValueFormat::Float32 => f32::from_le_bytes(*data),
        ValueFormat::FixedPoint => {
            fixed_point_to_float(u32::from_be_bytes(*data))
        }
    }
}

/// Convert IEEE float32 to 5.23 fixed-point (u32 big-endian on wire).
///
/// Format: 1 sign + 8 integer + 23 fractional bits. Range: [-256.0, +256.0).
pub fn float_to_fixed_point(value: f32) -> u32 {
    let scaled = value * (1 << 23) as f32;
    // Clamp to i32 range to prevent overflow
    let clamped = scaled.clamp(i32::MIN as f32, i32::MAX as f32);
    clamped as i32 as u32
}

/// Convert 5.23 fixed-point to IEEE float32.
pub fn fixed_point_to_float(raw: u32) -> f32 {
    (raw as i32) as f32 / (1 << 23) as f32
}

/// Encode mute value for the wire.
pub fn encode_mute(muted: bool, dialect: &Dialect) -> Vec<u8> {
    match dialect.value_format {
        ValueFormat::Float32 => {
            // New dialect: 2 bytes, 1=unmute, 2=mute
            let val: u16 = if muted { 2 } else { 1 };
            val.to_le_bytes().to_vec()
        }
        ValueFormat::FixedPoint => {
            // Old dialect: 4 bytes fixed-point, 0x00800000=unmute, 0x00000000=mute
            if muted {
                0x00000000u32.to_be_bytes().to_vec()
            } else {
                0x00800000u32.to_be_bytes().to_vec()
            }
        }
    }
}

/// Decode mute value from wire bytes.
pub fn decode_mute(data: &[u8], dialect: &Dialect) -> Option<bool> {
    match dialect.value_format {
        ValueFormat::Float32 => {
            if data.len() < 2 {
                return None;
            }
            let val = u16::from_le_bytes([data[0], data[1]]);
            Some(val == 2)
        }
        ValueFormat::FixedPoint => {
            if data.len() < 4 {
                return None;
            }
            let val = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            Some(val == 0x00000000)
        }
    }
}

/// Encode polarity/invert value.
pub fn encode_invert(inverted: bool, dialect: &Dialect) -> Vec<u8> {
    match dialect.value_format {
        ValueFormat::Float32 => {
            let val: u16 = if inverted { 1 } else { 0 };
            val.to_le_bytes().to_vec()
        }
        ValueFormat::FixedPoint => {
            // Old: inverted = 0xFF800000, non-inverted = 0x00800000
            if inverted {
                0xFF800000u32.to_be_bytes().to_vec()
            } else {
                0x00800000u32.to_be_bytes().to_vec()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_roundtrip() {
        let values = [1.0f32, -1.0, 0.0, 0.5, -0.5, 127.999, -128.0];
        for &v in &values {
            let fp = float_to_fixed_point(v);
            let back = fixed_point_to_float(fp);
            assert!((v - back).abs() < 0.0001, "roundtrip failed for {v}: got {back}");
        }
    }

    #[test]
    fn addr_encode_new_dialect() {
        let addr_bytes = encode_addr(0x0042, true, &Dialect::NEW);
        assert_eq!(addr_bytes, vec![0x80, 0x42, 0x00]);
    }

    #[test]
    fn addr_encode_old_dialect() {
        let addr_bytes = encode_addr(0x0042, true, &Dialect::OLD);
        assert_eq!(addr_bytes, vec![0x80, 0x42]);
    }

    #[test]
    fn addr_decode_roundtrip() {
        for &d in &[Dialect::NEW, Dialect::OLD] {
            let encoded = encode_addr(0x1234, true, &d);
            let (addr, write) = decode_addr(&encoded, &d).unwrap();
            assert_eq!(addr, 0x1234);
            assert!(write);

            let encoded = encode_addr(0x0100, false, &d);
            let (addr, write) = decode_addr(&encoded, &d).unwrap();
            assert_eq!(addr, 0x0100);
            assert!(!write);
        }
    }

    #[test]
    fn mute_encoding() {
        // New dialect
        let muted = encode_mute(true, &Dialect::NEW);
        assert_eq!(muted, vec![0x02, 0x00]);
        let unmuted = encode_mute(false, &Dialect::NEW);
        assert_eq!(unmuted, vec![0x01, 0x00]);

        // Old dialect
        let muted = encode_mute(true, &Dialect::OLD);
        assert_eq!(muted, vec![0x00, 0x00, 0x00, 0x00]);
        let unmuted = encode_mute(false, &Dialect::OLD);
        assert_eq!(unmuted, vec![0x00, 0x80, 0x00, 0x00]);
    }

    #[test]
    fn value_encode_float32() {
        let val = encode_value(1.0, &Dialect::NEW);
        assert_eq!(val, 1.0f32.to_le_bytes());
    }

    #[test]
    fn value_encode_fixed_point() {
        let val = encode_value(1.0, &Dialect::OLD);
        // 1.0 in 5.23 = 0x00800000
        assert_eq!(val, [0x00, 0x80, 0x00, 0x00]);
    }
}
