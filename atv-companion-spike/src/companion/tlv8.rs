use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Tag {
    Method = 0x00,
    Identifier = 0x01,
    Salt = 0x02,
    PublicKey = 0x03,
    Proof = 0x04,
    EncryptedData = 0x05,
    SeqNo = 0x06,
    Error = 0x07,
    Signature = 0x0A,
    Name = 0x11,
}

pub fn encode(items: impl IntoIterator<Item = (u8, Vec<u8>)>) -> Vec<u8> {
    let mut output = Vec::new();
    for (tag, value) in items {
        if value.is_empty() {
            output.extend([tag, 0]);
            continue;
        }
        for chunk in value.chunks(255) {
            output.push(tag);
            output.push(chunk.len() as u8);
            output.extend_from_slice(chunk);
        }
    }
    output
}

pub fn decode(data: &[u8]) -> BTreeMap<u8, Vec<u8>> {
    let mut output = BTreeMap::new();
    let mut offset = 0;
    while offset + 2 <= data.len() {
        let tag = data[offset];
        let len = data[offset + 1] as usize;
        offset += 2;
        if offset + len > data.len() {
            break;
        }
        output
            .entry(tag)
            .and_modify(|buffer: &mut Vec<u8>| {
                buffer.extend_from_slice(&data[offset..offset + len])
            })
            .or_insert_with(|| data[offset..offset + len].to_vec());
        offset += len;
    }
    output
}
