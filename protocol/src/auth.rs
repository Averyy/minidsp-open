//! Bounded, platform-independent primitives for optional board authentication.

use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const ITERATIONS: u32 = 10_000;
pub const MAX_PASSWORD_BYTES: usize = 128;
pub const IDLE_SECONDS: u64 = 30 * 60;
pub const ABSOLUTE_SECONDS: u64 = 12 * 60 * 60;
pub const BASIC_SECONDS: u64 = 5 * 60;
pub const RECORD_BYTES: usize = 54;

#[derive(Clone, Copy)]
pub struct PasswordRecord {
    iterations: u32,
    salt: [u8; 16],
    verifier: [u8; 32],
}

impl PasswordRecord {
    pub fn new(password: &str, salt: [u8; 16]) -> Option<Self> {
        if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
            return None;
        }
        let mut verifier = [0; 32];
        pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, ITERATIONS, &mut verifier);
        Some(Self {
            iterations: ITERATIONS,
            salt,
            verifier,
        })
    }

    pub fn verify(&self, password: &str) -> bool {
        if password.len() > MAX_PASSWORD_BYTES {
            return false;
        }
        let mut candidate = [0; 32];
        pbkdf2::pbkdf2_hmac::<Sha256>(
            password.as_bytes(),
            &self.salt,
            self.iterations,
            &mut candidate,
        );
        bool::from(candidate.ct_eq(&self.verifier))
    }

    /// Layout: record version 1, algorithm 1 (PBKDF2-HMAC-SHA256), iterations
    /// (u32 LE), 16-byte salt, 32-byte verifier.
    pub fn encode(&self) -> [u8; RECORD_BYTES] {
        let mut out = [0; RECORD_BYTES];
        out[0] = 1;
        out[1] = 1;
        out[2..6].copy_from_slice(&self.iterations.to_le_bytes());
        out[6..22].copy_from_slice(&self.salt);
        out[22..].copy_from_slice(&self.verifier);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != RECORD_BYTES || bytes[0..2] != [1, 1] {
            return None;
        }
        let iterations = u32::from_le_bytes(bytes[2..6].try_into().ok()?);
        if !(ITERATIONS..=1_000_000).contains(&iterations) {
            return None;
        }
        Some(Self {
            iterations,
            salt: bytes[6..22].try_into().ok()?,
            verifier: bytes[22..].try_into().ok()?,
        })
    }
}

pub fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub fn token(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn cookie(token: &str) -> String {
    format!("minidsp_session={token}; Path=/; HttpOnly; SameSite=Strict")
}

pub const CLEAR_COOKIE: &str = "minidsp_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0";
pub fn cookie_token(header: &str) -> Option<&str> {
    header
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| {
            (key == "minidsp_session"
                && value.len() == 64
                && value.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(value)
        })
}

pub fn basic_header(password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("admin:{password}")))
}

pub fn basic_password(header: &str) -> Option<String> {
    let (scheme, value) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Basic") || value.len() > 192 {
        return None;
    }
    let bytes = STANDARD.decode(value).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let password = text.strip_prefix("admin:")?;
    (password.len() <= MAX_PASSWORD_BYTES).then(|| password.to_owned())
}

pub fn basic_digest(secret: &[u8; 32], header: &str) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC key length");
    mac.update(header.as_bytes());
    mac.finalize().into_bytes().into()
}

/// Reject ambiguous browser paths, encoded separators, control bytes and redirects back to login.
pub fn next_path(path: &str) -> &str {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.starts_with("/login")
        || path.len() > 512
        || path
            .bytes()
            .any(|b| b <= 32 || b == 127 || b == b'\\' || b == b'%' || b == b'#')
    {
        "/"
    } else {
        path
    }
}

#[derive(Clone, Copy)]
struct Session {
    digest: [u8; 32],
    created: u64,
    used: u64,
}

#[derive(Default)]
pub struct Sessions {
    entries: [Option<Session>; 8],
}

impl Sessions {
    pub fn insert(&mut self, digest: [u8; 32], now: u64) {
        let index = self
            .entries
            .iter()
            .position(|s| s.is_none_or(|s| !alive(s, now)))
            .unwrap_or_else(|| {
                self.entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, s)| s.unwrap().created)
                    .unwrap()
                    .0
            });
        self.entries[index] = Some(Session {
            digest,
            created: now,
            used: now,
        });
    }

    pub fn valid(&mut self, digest: &[u8; 32], now: u64, touch: bool) -> bool {
        for entry in &mut self.entries {
            if let Some(s) = entry {
                if !alive(*s, now) {
                    *entry = None;
                    continue;
                }
                if bool::from(s.digest.ct_eq(digest)) {
                    if touch {
                        s.used = now;
                    }
                    return true;
                }
            }
        }
        false
    }

    pub fn revoke(&mut self, digest: &[u8; 32]) {
        for entry in &mut self.entries {
            if entry.is_some_and(|s| bool::from(s.digest.ct_eq(digest))) {
                *entry = None;
            }
        }
    }
}

fn alive(s: Session, now: u64) -> bool {
    now.saturating_sub(s.used) < IDLE_SECONDS && now.saturating_sub(s.created) < ABSOLUTE_SECONDS
}

#[derive(Clone, Copy, Default)]
struct Budget {
    failures: u8,
    until: u64,
    last: u64,
}

impl Budget {
    fn fail(&mut self, now: u64, allowance: u8) {
        if now.saturating_sub(self.last) >= 300 {
            self.failures = 0;
        }
        self.failures = self.failures.saturating_add(1);
        self.last = now;
        if self.failures > allowance {
            self.until = now + (1u64 << (self.failures - allowance - 1).min(6));
        }
    }
}

#[derive(Default)]
pub struct RateLimiter {
    global: Budget,
    ips: [Option<(u32, Budget)>; 8],
}

impl RateLimiter {
    pub fn retry_after(&self, ip: u32, now: u64) -> u64 {
        let local = self
            .ips
            .iter()
            .flatten()
            .find(|(key, _)| *key == ip)
            .map_or(0, |(_, b)| b.until);
        self.global.until.max(local).saturating_sub(now)
    }

    pub fn failure(&mut self, ip: u32, now: u64) {
        self.global.fail(now, 10);
        let index = self
            .ips
            .iter()
            .position(|s| s.is_some_and(|(key, _)| key == ip))
            .or_else(|| self.ips.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                self.ips
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, b)| b.unwrap().1.last)
                    .unwrap()
                    .0
            });
        let entry = self.ips[index].get_or_insert((ip, Budget::default()));
        if entry.0 != ip {
            *entry = (ip, Budget::default());
        }
        entry.1.fail(now, 3);
    }
}

#[derive(Default)]
pub struct BasicCache {
    entries: [Option<([u8; 32], u64)>; 8],
}

impl BasicCache {
    pub fn valid(&self, digest: &[u8; 32], now: u64) -> bool {
        self.entries
            .iter()
            .flatten()
            .any(|(key, until)| now < *until && bool::from(key.ct_eq(digest)))
    }

    pub fn insert(&mut self, digest: [u8; 32], now: u64) {
        let i = self
            .entries
            .iter()
            .position(|s| s.is_none_or(|(_, until)| now >= until))
            .unwrap_or_else(|| {
                self.entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, s)| s.unwrap().1)
                    .unwrap()
                    .0
            });
        self.entries[i] = Some((digest, now + BASIC_SECONDS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_record_roundtrip_and_validation() {
        let record = PasswordRecord::new("pass:word", [7; 16]).unwrap();
        let decoded = PasswordRecord::decode(&record.encode()).unwrap();
        assert!(decoded.verify("pass:word"));
        assert!(!decoded.verify("wrong"));
        assert_ne!(
            record.encode(),
            PasswordRecord::new("pass:word", [8; 16]).unwrap().encode()
        );
        let mut bad = record.encode();
        bad[0] = 2;
        assert!(PasswordRecord::decode(&bad).is_none());
        bad = record.encode();
        bad[2..6].copy_from_slice(&0u32.to_le_bytes());
        assert!(PasswordRecord::decode(&bad).is_none());
        assert!(PasswordRecord::decode(&bad[..53]).is_none());
        assert!(PasswordRecord::new("", [0; 16]).is_none());
    }

    #[test]
    fn pbkdf2_known_vector() {
        let mut record = [0; RECORD_BYTES];
        record[..2].copy_from_slice(&[1, 1]);
        record[2..6].copy_from_slice(&10_000u32.to_le_bytes());
        // Independently generated with Python hashlib.pbkdf2_hmac, 16 zero salt bytes.
        let expected = "7eaaa213eda0463c77446b666903b5035c84075c5f7cbace10aac54a2dd6674f";
        let actual = PasswordRecord::new("password", [0; 16]).unwrap();
        assert_eq!(token(&actual.verifier), expected);
    }

    #[test]
    fn session_expiry_eviction_revoke() {
        let mut sessions = Sessions::default();
        for i in 0..9 {
            sessions.insert([i; 32], u64::from(i));
        }
        assert!(!sessions.valid(&[0; 32], 9, false));
        assert!(sessions.valid(&[1; 32], 9, true));
        assert!(!sessions.valid(&[1; 32], 9 + IDLE_SECONDS, false));
        sessions.insert([10; 32], 0);
        for t in (100..ABSOLUTE_SECONDS).step_by(100) {
            assert!(sessions.valid(&[10; 32], t, true));
        }
        assert!(!sessions.valid(&[10; 32], ABSOLUTE_SECONDS, true));
        sessions.insert([11; 32], 1);
        sessions.revoke(&[11; 32]);
        assert!(!sessions.valid(&[11; 32], 2, false));
    }

    #[test]
    fn rate_limits_are_bounded_and_recover() {
        let mut rate = RateLimiter::default();
        for _ in 0..3 {
            rate.failure(1, 0);
        }
        assert_eq!(rate.retry_after(1, 0), 0);
        rate.failure(1, 0);
        assert_eq!(rate.retry_after(1, 0), 1);
        assert_eq!(rate.retry_after(2, 0), 0);
        for _ in 0..30 {
            rate.failure(1, 0);
        }
        assert_eq!(rate.retry_after(2, 0), 64);
        assert_eq!(rate.retry_after(1, 64), 0);
        for ip in 2..20 {
            rate.failure(ip, 300);
        }
        assert!(rate.retry_after(19, 300) <= 64);
        rate.failure(1, 1000);
        assert_eq!(rate.retry_after(1, 1000), 0);
    }

    #[test]
    fn headers_paths_cookies_and_cache() {
        assert_eq!(basic_password(&basic_header("a:b é")), Some("a:b é".into()));
        for bad in [
            "Basic !!!",
            "Basic dXNlcjpwYXNz",
            "Bearer abc",
            "Basic /w==",
        ] {
            assert!(basic_password(bad).is_none());
        }
        for bad in [
            "https://evil",
            "//evil",
            "/\\evil",
            "/%2fevil",
            "/ok\r\nX: y",
            "/login?next=/",
        ] {
            assert_eq!(next_path(bad), "/");
        }
        assert_eq!(next_path("/status?foo=bar"), "/status?foo=bar");
        let token = token(&[9; 32]);
        assert_eq!(
            cookie(&token),
            format!("minidsp_session={token}; Path=/; HttpOnly; SameSite=Strict")
        );
        assert_eq!(
            cookie_token(&format!("other=x; minidsp_session={token}")),
            Some(token.as_str())
        );
        let mut cache = BasicCache::default();
        let key = basic_digest(&[1; 32], "Basic abc");
        cache.insert(key, 0);
        assert!(cache.valid(&key, 299));
        assert!(!cache.valid(&key, 300));
        assert!(!cache.valid(&basic_digest(&[2; 32], "Basic abc"), 1));
    }
}
