use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    NoOp = 0x01,
    PairSetupStart = 0x03,
    PairSetupNext = 0x04,
    PairVerifyStart = 0x05,
    PairVerifyNext = 0x06,
    OpackUnencrypted = 0x07,
    OpackEncrypted = 0x08,
    SessionStartRequest = 0x10,
    SessionStartResponse = 0x11,
    SessionData = 0x12,
}

impl FrameType {
    pub fn from_byte(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(Self::NoOp),
            0x03 => Ok(Self::PairSetupStart),
            0x04 => Ok(Self::PairSetupNext),
            0x05 => Ok(Self::PairVerifyStart),
            0x06 => Ok(Self::PairVerifyNext),
            0x07 => Ok(Self::OpackUnencrypted),
            0x08 => Ok(Self::OpackEncrypted),
            0x10 => Ok(Self::SessionStartRequest),
            0x11 => Ok(Self::SessionStartResponse),
            0x12 => Ok(Self::SessionData),
            other => bail!("unsupported frame type 0x{other:02x}"),
        }
    }
}

pub const MESSAGE_EVENT: i64 = 1;
pub const MESSAGE_REQUEST: i64 = 2;
pub const MESSAGE_RESPONSE: i64 = 3;

pub fn header_bytes(frame_type: FrameType, payload_len: usize) -> [u8; 4] {
    [
        frame_type as u8,
        ((payload_len >> 16) & 0xFF) as u8,
        ((payload_len >> 8) & 0xFF) as u8,
        (payload_len & 0xFF) as u8,
    ]
}

pub fn parse_header(header: [u8; 4]) -> Result<(FrameType, usize)> {
    let frame_type = FrameType::from_byte(header[0])?;
    let len = ((header[1] as usize) << 16) | ((header[2] as usize) << 8) | header[3] as usize;
    Ok((frame_type, len))
}
