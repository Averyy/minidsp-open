use anyhow::Result;
use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce, Tag,
    aead::{AeadInPlace, KeyInit},
};
use hkdf::Hkdf;
use sha2::Sha512;

pub fn hkdf_expand(secret: &[u8], salt: &str, info: &str) -> Result<[u8; 32]> {
    let hkdf = Hkdf::<Sha512>::new(Some(salt.as_bytes()), secret);
    let mut output = [0_u8; 32];
    hkdf.expand(info.as_bytes(), &mut output)
        .map_err(|_| anyhow::anyhow!("hkdf expand failed"))?;
    Ok(output)
}

pub fn encrypt_pairing(plaintext: &[u8], key: &[u8; 32], nonce_label: &[u8; 8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce_bytes = [0_u8; 12];
    nonce_bytes[4..].copy_from_slice(nonce_label);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let mut buffer = plaintext.to_vec();
    let tag = cipher
        .encrypt_in_place_detached(nonce, b"", &mut buffer)
        .map_err(|_| anyhow::anyhow!("pairing encrypt failed"))?;
    buffer.extend_from_slice(tag.as_slice());
    Ok(buffer)
}

pub fn decrypt_pairing(
    ciphertext_and_tag: &[u8],
    key: &[u8; 32],
    nonce_label: &[u8; 8],
) -> Result<Vec<u8>> {
    if ciphertext_and_tag.len() < 16 {
        anyhow::bail!("ciphertext too short");
    }

    let split_at = ciphertext_and_tag.len() - 16;
    let mut buffer = ciphertext_and_tag[..split_at].to_vec();
    let tag = Tag::from_slice(&ciphertext_and_tag[split_at..]);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));

    let mut nonce_bytes = [0_u8; 12];
    nonce_bytes[4..].copy_from_slice(nonce_label);
    let nonce = Nonce::from_slice(&nonce_bytes);

    cipher
        .decrypt_in_place_detached(nonce, b"", &mut buffer, tag)
        .map_err(|_| anyhow::anyhow!("pairing decrypt failed"))?;
    Ok(buffer)
}

pub fn encrypt_session(
    plaintext: &[u8],
    key: &[u8; 32],
    counter: u64,
    aad: &[u8],
) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce_bytes = [0_u8; 12];
    nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
    let nonce = Nonce::from_slice(&nonce_bytes);
    let mut buffer = plaintext.to_vec();
    let tag = cipher
        .encrypt_in_place_detached(nonce, aad, &mut buffer)
        .map_err(|_| anyhow::anyhow!("session encrypt failed"))?;
    buffer.extend_from_slice(tag.as_slice());
    Ok(buffer)
}

pub fn decrypt_session(
    ciphertext_and_tag: &[u8],
    key: &[u8; 32],
    counter: u64,
    aad: &[u8],
) -> Result<Vec<u8>> {
    if ciphertext_and_tag.len() < 16 {
        anyhow::bail!("ciphertext too short");
    }
    let split_at = ciphertext_and_tag.len() - 16;
    let mut buffer = ciphertext_and_tag[..split_at].to_vec();
    let tag = Tag::from_slice(&ciphertext_and_tag[split_at..]);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));

    let mut nonce_bytes = [0_u8; 12];
    nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
    let nonce = Nonce::from_slice(&nonce_bytes);

    cipher
        .decrypt_in_place_detached(nonce, aad, &mut buffer, tag)
        .map_err(|_| anyhow::anyhow!("session decrypt failed"))?;
    Ok(buffer)
}
