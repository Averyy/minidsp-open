pub fn parse_digest_prefix(digest: &str) -> Result<[u8; 8], String> {
    if digest.len() < 16 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("digest must contain at least 16 hex characters".into());
    }
    let mut prefix = [0; 8];
    for (index, byte) in prefix.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digest[index * 2..index * 2 + 2], 16)
            .map_err(|_| "invalid hex digest")?;
    }
    Ok(prefix)
}

pub fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub fn read_body<E>(
    declared_len: Option<usize>,
    max_len: usize,
    mut read: impl FnMut(&mut [u8]) -> Result<usize, E>,
) -> Result<Vec<u8>, (u16, &'static str)> {
    if declared_len.is_some_and(|len| len > max_len) {
        return Err((413, "request body too large"));
    }
    let mut body = Vec::with_capacity(declared_len.unwrap_or(0));
    let mut buf = [0; 512];
    loop {
        let remaining = match declared_len {
            Some(len) => len - body.len(),
            None => max_len.saturating_sub(body.len()).saturating_add(1),
        };
        if remaining == 0 {
            return Ok(body);
        }
        let read_len = remaining.min(buf.len());
        let count = read(&mut buf[..read_len]).map_err(|_| (400, "failed to read request body"))?;
        if count == 0 {
            return if declared_len.is_some_and(|len| len != body.len()) {
                Err((400, "incomplete request body"))
            } else {
                Ok(body)
            };
        }
        if count > max_len.saturating_sub(body.len()) {
            return Err((413, "request body too large"));
        }
        body.extend_from_slice(&buf[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn digest_rejects_utf8_and_non_hex_without_panicking() {
        for digest in [
            "a€00000000000000",
            "000000000000000g",
            "0000",
            "0000000000000000é",
        ] {
            assert!(parse_digest_prefix(digest).is_err());
        }
        assert_eq!(
            parse_digest_prefix("0123456789AbCdEf").unwrap(),
            [1, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
        );
    }

    #[test]
    fn text_limit_preserves_char_boundaries() {
        let text = format!("{}é", "a".repeat(255));
        assert_eq!(truncate_utf8(&text, 256), &text[..255]);
        assert_eq!(truncate_utf8("€é", 3), "€");
    }

    #[test]
    fn body_reads_fragments_and_rejects_truncation_and_overflow() {
        let body = br#"{"entryId":"0123456789abcdef"}"#;
        let mut remaining = body.as_slice();
        assert_eq!(
            read_body(Some(body.len()), 100, |buf| {
                remaining.read(&mut buf[..1])
            })
            .unwrap(),
            body
        );
        let mut short = &body[..5];
        assert_eq!(
            read_body(Some(body.len()), 100, |buf| short.read(buf))
                .unwrap_err()
                .0,
            400
        );
        assert_eq!(
            read_body::<()>(Some(101), 100, |_| panic!()).unwrap_err().0,
            413
        );
        let mut remaining = body.as_slice();
        assert_eq!(
            read_body(None, 5, |buf| remaining.read(buf)).unwrap_err().0,
            413
        );
    }
}
