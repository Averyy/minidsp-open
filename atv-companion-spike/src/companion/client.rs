use std::collections::{BTreeMap, VecDeque};

use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use sha2_11::Sha512 as SrpSha512;
use sha2_11::digest::Digest as _;
use srp::ClientG3072;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{Duration, timeout},
};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use crate::model::CredentialRecord;

use super::{
    crypto::{decrypt_pairing, decrypt_session, encrypt_pairing, encrypt_session, hkdf_expand},
    frame::{
        FrameType, MESSAGE_EVENT, MESSAGE_REQUEST, MESSAGE_RESPONSE, header_bytes, parse_header,
    },
    opack::{Value as OpackValue, decode as decode_opack, encode as encode_opack},
    tlv8::{Tag as TlvTag, decode as decode_tlv8, encode as encode_tlv8},
};

const IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_FRAME_PAYLOAD_LEN: usize = 2 * 1024 * 1024;
const CONTROLLER_SIGN_SALT: &str = "Pair-Setup-Controller-Sign-Salt";
const CONTROLLER_SIGN_INFO: &str = "Pair-Setup-Controller-Sign-Info";
const ACCESSORY_SIGN_SALT: &str = "Pair-Setup-Accessory-Sign-Salt";
const ACCESSORY_SIGN_INFO: &str = "Pair-Setup-Accessory-Sign-Info";
const PAIR_ENCRYPT_SALT: &str = "Pair-Setup-Encrypt-Salt";
const PAIR_ENCRYPT_INFO: &str = "Pair-Setup-Encrypt-Info";
const VERIFY_ENCRYPT_SALT: &str = "Pair-Verify-Encrypt-Salt";
const VERIFY_ENCRYPT_INFO: &str = "Pair-Verify-Encrypt-Info";
const CLIENT_MAIN_INFO: &str = "ClientEncrypt-main";
const SERVER_MAIN_INFO: &str = "ServerEncrypt-main";
const PAIR_SETUP_USERNAME: &[u8] = b"Pair-Setup";
const CLIENT_SOFTWARE_VERSION: &str = "170.18";
const SRP_3072_PRIME_HEX: &str = concat!(
    "ffffffffffffffffc90fdaa22168c234c4c6628b80dc1cd129024e088a67cc74020bbea63b139b22",
    "514a08798e3404ddef9519b3cd3a431b302b0a6df25f14374fe1356d6d51c245e485b576625e7ec6",
    "f44c42e9a637ed6b0bff5cb6f406b7edee386bfb5a899fa5ae9f24117c4b1fe649286651ece45b3d",
    "c2007cb8a163bf0598da48361c55d39a69163fa8fd24cf5f83655d23dca3ad961c62f356208552bb",
    "9ed529077096966d670c354e4abc9804f1746c08ca18217c32905e462e36ce3be39e772c180e8603",
    "9b2783a2ec07a28fb5c55df06f4c52c9de2bcbf6955817183995497cea956ae515d2261898fa05101",
    "5728e5a8aaac42dad33170d04507a33a85521abdf1cba64ecfb850458dbef0a8aea71575d060c7db",
    "3970f85a6e1e4c7abf5ae8cdb0933d71e8c94e04a25619dcee3d2261ad2ee6bf12ffa06d98a0864d",
    "87602733ec86a64521f2b18177b200cbbe117577a615d6c770988c0bad946e208e24fa074e5ab314",
    "3db5bfce0fd108e4b82d120a93ad2caffffffffffffffff"
);

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub controller_name: String,
}

impl ClientConfig {
    pub fn new(controller_name: String) -> Self {
        Self { controller_name }
    }
}

#[derive(Debug, Clone)]
pub struct CredentialMaterial {
    pub client_id: Vec<u8>,
    pub client_seed: [u8; 32],
    pub atv_id: Vec<u8>,
    pub atv_ltpk: [u8; 32],
}

impl TryFrom<&CredentialRecord> for CredentialMaterial {
    type Error = anyhow::Error;

    fn try_from(value: &CredentialRecord) -> Result<Self> {
        Ok(Self {
            client_id: hex::decode(&value.client_id_hex).context("invalid client_id_hex")?,
            client_seed: decode_fixed_32(&value.client_seed_hex)
                .context("invalid client_seed_hex")?,
            atv_id: hex::decode(&value.atv_id_hex).context("invalid atv_id_hex")?,
            atv_ltpk: decode_fixed_32(&value.atv_ltpk_hex).context("invalid atv_ltpk_hex")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairResult {
    pub client_id: Vec<u8>,
    pub client_seed: [u8; 32],
    pub client_ltpk: [u8; 32],
    pub atv_id: Vec<u8>,
    pub atv_ltpk: [u8; 32],
}

impl PairResult {
    pub fn into_record(self, device_id: &str) -> CredentialRecord {
        CredentialRecord {
            device_id: device_id.to_owned(),
            client_id_hex: hex::encode(&self.client_id),
            client_seed_hex: hex::encode(self.client_seed),
            client_ltpk_hex: hex::encode(self.client_ltpk),
            atv_id_hex: hex::encode(&self.atv_id),
            atv_ltpk_hex: hex::encode(self.atv_ltpk),
            paired_at_ms: crate::model::now_millis(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PairingChallenge {
    salt: Vec<u8>,
    server_pub: Vec<u8>,
}

#[derive(Debug)]
pub struct CompanionClient {
    stream: TcpStream,
    config: ClientConfig,
    next_xid: i64,
    write_key: Option<[u8; 32]>,
    read_key: Option<[u8; 32]>,
    write_counter: u64,
    read_counter: u64,
    session_id: Option<u64>,
    pending_events: VecDeque<OpackValue>,
}

impl CompanionClient {
    pub async fn connect(host: &str, port: u16, config: ClientConfig) -> Result<Self> {
        let stream = timeout(IO_TIMEOUT, TcpStream::connect((host, port)))
            .await
            .with_context(|| format!("timed out connecting to {host}:{port}"))?
            .with_context(|| format!("failed to connect to {host}:{port}"))?;

        Ok(Self {
            stream,
            config,
            next_xid: 1,
            write_key: None,
            read_key: None,
            write_counter: 0,
            read_counter: 0,
            session_id: None,
            pending_events: VecDeque::new(),
        })
    }

    pub async fn start_pairing(&mut self) -> Result<PairingChallenge> {
        let m1 = OpackValue::Dict(BTreeMap::from([
            (
                "_pd".to_owned(),
                OpackValue::Bytes(encode_tlv8(vec![
                    (TlvTag::Method as u8, vec![0x00]),
                    (TlvTag::SeqNo as u8, vec![0x01]),
                ])),
            ),
            ("_pwTy".to_owned(), OpackValue::Int(1)),
        ]));
        let response = self
            .exchange_plain(FrameType::PairSetupStart, m1)
            .await
            .context("pair-setup M1/M2 exchange failed")?;
        let tlv = extract_pairing_tlv(&response).context("pair-setup M2 payload rejected")?;
        let salt = tlv
            .get(&(TlvTag::Salt as u8))
            .cloned()
            .ok_or_else(|| anyhow!("pair-setup M2 missing salt"))?;
        let server_pub = tlv
            .get(&(TlvTag::PublicKey as u8))
            .cloned()
            .ok_or_else(|| anyhow!("pair-setup M2 missing server public key"))?;

        Ok(PairingChallenge { salt, server_pub })
    }

    pub async fn finish_pairing(
        &mut self,
        challenge: PairingChallenge,
        pin: String,
        display_name: Option<String>,
    ) -> Result<PairResult> {
        let PairingChallenge { salt, server_pub } = challenge;

        let mut client_seed = [0_u8; 32];
        OsRng.fill_bytes(&mut client_seed);
        let signing_key = SigningKey::from_bytes(&client_seed);
        let client_ltpk = signing_key.verifying_key().to_bytes();
        let pairing_id = uuid::Uuid::new_v4().to_string().into_bytes();

        let srp = ClientG3072::<SrpSha512>::new();
        let verifier = srp
            .process_reply(
                &client_seed,
                PAIR_SETUP_USERNAME,
                pin.as_bytes(),
                &salt,
                &server_pub,
            )
            .context("failed to compute SRP proof")?;
        let client_pub = srp.compute_public_ephemeral(&client_seed);
        let session_key = sha2_11::Sha512::digest(verifier.key()).to_vec();
        let client_proof = compute_hap_pair_setup_m1(
            PAIR_SETUP_USERNAME,
            &salt,
            &client_pub,
            &server_pub,
            &session_key,
        );

        let m3 = OpackValue::Dict(BTreeMap::from([
            (
                "_pd".to_owned(),
                OpackValue::Bytes(encode_tlv8(vec![
                    (TlvTag::SeqNo as u8, vec![0x03]),
                    (TlvTag::PublicKey as u8, client_pub.clone()),
                    (TlvTag::Proof as u8, client_proof.clone()),
                ])),
            ),
            ("_pwTy".to_owned(), OpackValue::Int(1)),
        ]));
        let response = self
            .exchange_plain(FrameType::PairSetupNext, m3)
            .await
            .context("pair-setup M3/M4 exchange failed")?;
        let tlv = extract_pairing_tlv(&response).context("pair-setup M4 payload rejected")?;
        let server_proof = tlv
            .get(&(TlvTag::Proof as u8))
            .cloned()
            .ok_or_else(|| anyhow!("pair-setup M4 missing proof"))?;
        let expected_server_proof =
            compute_hap_pair_setup_m2(&client_pub, &client_proof, &session_key);
        if server_proof != expected_server_proof {
            bail!("pair-setup server proof verification failed");
        }

        let controller_sign =
            hkdf_expand(&session_key, CONTROLLER_SIGN_SALT, CONTROLLER_SIGN_INFO)?;
        let pair_encrypt = hkdf_expand(&session_key, PAIR_ENCRYPT_SALT, PAIR_ENCRYPT_INFO)?;

        let mut device_info = controller_sign.to_vec();
        device_info.extend_from_slice(&pairing_id);
        device_info.extend_from_slice(&client_ltpk);
        let signature = signing_key.sign(&device_info).to_bytes().to_vec();

        let mut inner = vec![
            (TlvTag::Identifier as u8, pairing_id.clone()),
            (TlvTag::PublicKey as u8, client_ltpk.to_vec()),
            (TlvTag::Signature as u8, signature),
        ];
        if let Some(name) = display_name {
            let name_payload = encode_opack(&OpackValue::Dict(BTreeMap::from([(
                "name".to_owned(),
                OpackValue::String(name),
            )])));
            inner.push((TlvTag::Name as u8, name_payload));
        }

        let encrypted_identity = encrypt_pairing(&encode_tlv8(inner), &pair_encrypt, b"PS-Msg05")?;
        let m5 = OpackValue::Dict(BTreeMap::from([
            (
                "_pd".to_owned(),
                OpackValue::Bytes(encode_tlv8(vec![
                    (TlvTag::SeqNo as u8, vec![0x05]),
                    (TlvTag::EncryptedData as u8, encrypted_identity),
                ])),
            ),
            ("_pwTy".to_owned(), OpackValue::Int(1)),
        ]));
        let response = self
            .exchange_plain(FrameType::PairSetupNext, m5)
            .await
            .context("pair-setup M5/M6 exchange failed")?;
        let tlv = extract_pairing_tlv(&response).context("pair-setup M6 payload rejected")?;
        let encrypted = tlv
            .get(&(TlvTag::EncryptedData as u8))
            .cloned()
            .ok_or_else(|| anyhow!("pair-setup M6 missing encrypted data"))?;
        let decrypted = decrypt_pairing(&encrypted, &pair_encrypt, b"PS-Msg06")?;
        let accessory = decode_tlv8(&decrypted);
        let atv_id = accessory
            .get(&(TlvTag::Identifier as u8))
            .cloned()
            .ok_or_else(|| anyhow!("pair-setup M6 missing accessory identifier"))?;
        let atv_ltpk = decode_fixed_32_vec(
            accessory
                .get(&(TlvTag::PublicKey as u8))
                .cloned()
                .ok_or_else(|| anyhow!("pair-setup M6 missing accessory ltpk"))?,
        )?;
        let accessory_signature = accessory
            .get(&(TlvTag::Signature as u8))
            .cloned()
            .ok_or_else(|| anyhow!("pair-setup M6 missing accessory signature"))?;

        let accessory_sign = hkdf_expand(&session_key, ACCESSORY_SIGN_SALT, ACCESSORY_SIGN_INFO)?;
        let mut accessory_info = accessory_sign.to_vec();
        accessory_info.extend_from_slice(&atv_id);
        accessory_info.extend_from_slice(&atv_ltpk);
        let verifying_key = VerifyingKey::from_bytes(&atv_ltpk)?;
        let signature = Signature::from_slice(&accessory_signature)
            .context("invalid accessory signature bytes")?;
        verifying_key
            .verify(&accessory_info, &signature)
            .context("pair-setup accessory signature verification failed")?;

        Ok(PairResult {
            client_id: pairing_id,
            client_seed,
            client_ltpk,
            atv_id,
            atv_ltpk,
        })
    }

    pub async fn verify(&mut self, creds: CredentialMaterial) -> Result<()> {
        let mut secret_bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut secret_bytes);
        let local_secret = StaticSecret::from(secret_bytes);
        let local_public = X25519PublicKey::from(&local_secret);

        let m1 = OpackValue::Dict(BTreeMap::from([
            (
                "_pd".to_owned(),
                OpackValue::Bytes(encode_tlv8(vec![
                    (TlvTag::SeqNo as u8, vec![0x01]),
                    (TlvTag::PublicKey as u8, local_public.as_bytes().to_vec()),
                ])),
            ),
            ("_auTy".to_owned(), OpackValue::Int(4)),
        ]));
        let response = self.exchange_plain(FrameType::PairVerifyStart, m1).await?;
        let tlv = extract_pairing_tlv(&response).context("pair-verify M2 payload rejected")?;
        let server_pub = tlv
            .get(&(TlvTag::PublicKey as u8))
            .cloned()
            .ok_or_else(|| anyhow!("pair-verify M2 missing server public key"))?;
        let encrypted = tlv
            .get(&(TlvTag::EncryptedData as u8))
            .cloned()
            .ok_or_else(|| anyhow!("pair-verify M2 missing encrypted data"))?;

        let server_public = X25519PublicKey::from(
            decode_fixed_32_vec(server_pub.clone()).context("invalid pair-verify server pubkey")?,
        );
        let shared = local_secret.diffie_hellman(&server_public);
        let verify_key = hkdf_expand(shared.as_bytes(), VERIFY_ENCRYPT_SALT, VERIFY_ENCRYPT_INFO)?;
        let decrypted = decrypt_pairing(&encrypted, &verify_key, b"PV-Msg02")?;
        let payload = decode_tlv8(&decrypted);
        let atv_id = payload
            .get(&(TlvTag::Identifier as u8))
            .cloned()
            .ok_or_else(|| anyhow!("pair-verify M2 missing accessory identifier"))?;
        if atv_id != creds.atv_id {
            bail!("pair-verify accessory identifier mismatch");
        }
        let signature_bytes = payload
            .get(&(TlvTag::Signature as u8))
            .cloned()
            .ok_or_else(|| anyhow!("pair-verify M2 missing signature"))?;
        let mut server_sig_material = Vec::new();
        server_sig_material.extend_from_slice(&server_pub);
        server_sig_material.extend_from_slice(&atv_id);
        server_sig_material.extend_from_slice(local_public.as_bytes());
        let atv_ltpk = VerifyingKey::from_bytes(&creds.atv_ltpk)?;
        let signature =
            Signature::from_slice(&signature_bytes).context("invalid pair-verify signature")?;
        atv_ltpk
            .verify(&server_sig_material, &signature)
            .context("pair-verify server signature failed")?;

        let signing_key = SigningKey::from_bytes(&creds.client_seed);
        let mut client_sig_material = Vec::new();
        client_sig_material.extend_from_slice(local_public.as_bytes());
        client_sig_material.extend_from_slice(&creds.client_id);
        client_sig_material.extend_from_slice(&server_pub);
        let client_signature = signing_key.sign(&client_sig_material).to_bytes().to_vec();
        let inner = encode_tlv8(vec![
            (TlvTag::Identifier as u8, creds.client_id.clone()),
            (TlvTag::Signature as u8, client_signature),
        ]);
        let encrypted = encrypt_pairing(&inner, &verify_key, b"PV-Msg03")?;
        let m3 = OpackValue::Dict(BTreeMap::from([(
            "_pd".to_owned(),
            OpackValue::Bytes(encode_tlv8(vec![
                (TlvTag::SeqNo as u8, vec![0x03]),
                (TlvTag::EncryptedData as u8, encrypted),
            ])),
        )]));
        let _ = self.exchange_plain(FrameType::PairVerifyNext, m3).await?;

        self.write_key = Some(hkdf_expand(shared.as_bytes(), "", CLIENT_MAIN_INFO)?);
        self.read_key = Some(hkdf_expand(shared.as_bytes(), "", SERVER_MAIN_INFO)?);
        self.write_counter = 0;
        self.read_counter = 0;
        Ok(())
    }

    pub async fn perform_initial_handshake(
        &mut self,
        client_pairing_id: Option<&[u8]>,
    ) -> Result<()> {
        let controller_id = self.controller_identifier();
        let public_id = self.controller_public_id();
        let ids_id = client_pairing_id
            .map(|value| String::from_utf8_lossy(value).into_owned())
            .unwrap_or_else(|| controller_id.clone());

        let _ = self
            .exchange_encrypted(opack_dict([
                ("_i", OpackValue::String("_systemInfo".to_owned())),
                ("_t", OpackValue::Int(MESSAGE_REQUEST)),
                (
                    "_c",
                    opack_dict([
                        ("_bf", OpackValue::Int(0)),
                        ("_cf", OpackValue::Int(512)),
                        ("_clFl", OpackValue::Int(128)),
                        ("_i", OpackValue::String(controller_id.clone())),
                        ("_idsID", OpackValue::String(ids_id)),
                        ("_pubID", OpackValue::String(public_id)),
                        ("_sf", OpackValue::Int(256)),
                        (
                            "_sv",
                            OpackValue::String(CLIENT_SOFTWARE_VERSION.to_owned()),
                        ),
                        ("model", OpackValue::String("RustCompanion,1".to_owned())),
                        (
                            "name",
                            OpackValue::String(self.config.controller_name.clone()),
                        ),
                        ("_dC", OpackValue::String("1".to_owned())),
                    ]),
                ),
            ]))
            .await?;

        let _ = self
            .exchange_encrypted(opack_dict([
                ("_i", OpackValue::String("_touchStart".to_owned())),
                ("_t", OpackValue::Int(MESSAGE_REQUEST)),
                (
                    "_c",
                    opack_dict([
                        ("_height", OpackValue::Float(1000.0)),
                        ("_tFl", OpackValue::Int(0)),
                        ("_width", OpackValue::Float(1000.0)),
                    ]),
                ),
            ]))
            .await?;

        let local_sid = {
            let mut bytes = [0_u8; 4];
            OsRng.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        };
        let session = self
            .exchange_encrypted(opack_dict([
                ("_i", OpackValue::String("_sessionStart".to_owned())),
                ("_t", OpackValue::Int(MESSAGE_REQUEST)),
                (
                    "_c",
                    opack_dict([
                        (
                            "_srvT",
                            OpackValue::String("com.apple.tvremoteservices".to_owned()),
                        ),
                        ("_sid", OpackValue::Int(local_sid as i64)),
                    ]),
                ),
            ]))
            .await?;
        let remote_sid = session
            .dict_get("_c")
            .and_then(|value| value.dict_get("_sid"))
            .and_then(OpackValue::int)
            .ok_or_else(|| anyhow!("session start response missing _sid"))?;
        self.session_id = Some(((remote_sid as u64) << 32) | local_sid as u64);

        let _ = self
            .exchange_encrypted(opack_dict([
                ("_i", OpackValue::String("_tiStart".to_owned())),
                ("_t", OpackValue::Int(MESSAGE_REQUEST)),
                ("_c", opack_dict([("_tiV", OpackValue::Int(1))])),
            ]))
            .await?;

        self.send_event(opack_dict([
            ("_i", OpackValue::String("_interest".to_owned())),
            ("_t", OpackValue::Int(MESSAGE_EVENT)),
            (
                "_c",
                opack_dict([(
                    "_regEvents",
                    OpackValue::Array(vec![
                        OpackValue::String("_iMC".to_owned()),
                        OpackValue::String("SystemStatus".to_owned()),
                        OpackValue::String("TVSystemStatus".to_owned()),
                        OpackValue::String("NowPlayingInfo".to_owned()),
                    ]),
                )]),
            ),
        ]))
        .await?;

        Ok(())
    }

    pub async fn fetch_now_playing(&mut self) -> Result<OpackValue> {
        self.exchange_encrypted(opack_dict([
            (
                "_i",
                OpackValue::String("FetchCurrentNowPlayingInfoEvent".to_owned()),
            ),
            ("_t", OpackValue::Int(MESSAGE_REQUEST)),
            ("_c", OpackValue::Dict(BTreeMap::new())),
        ]))
        .await
    }

    pub async fn fetch_attention_state(&mut self) -> Result<OpackValue> {
        self.exchange_encrypted(opack_dict([
            ("_i", OpackValue::String("FetchAttentionState".to_owned())),
            ("_t", OpackValue::Int(MESSAGE_REQUEST)),
            ("_c", OpackValue::Dict(BTreeMap::new())),
        ]))
        .await
    }

    pub async fn fetch_up_next(&mut self) -> Result<OpackValue> {
        self.exchange_encrypted(opack_dict([
            ("_i", OpackValue::String("FetchUpNextInfoEvent".to_owned())),
            ("_t", OpackValue::Int(MESSAGE_REQUEST)),
            ("_c", OpackValue::Dict(BTreeMap::new())),
        ]))
        .await
    }

    pub async fn collect_events(
        &mut self,
        initial_wait: Duration,
        idle_grace: Duration,
    ) -> Result<Vec<OpackValue>> {
        let mut events = Vec::new();
        while let Some(event) = self.pending_events.pop_front() {
            events.push(event);
        }

        let started = tokio::time::Instant::now();
        let mut saw_event = !events.is_empty();
        loop {
            let elapsed = started.elapsed();
            if elapsed >= initial_wait {
                break;
            }

            let remaining = initial_wait - elapsed;
            let wait_for = if saw_event {
                idle_grace.min(remaining)
            } else {
                remaining
            };

            match self.read_message(wait_for).await? {
                Some(message) if is_event_message(&message) => {
                    saw_event = true;
                    events.push(message);
                }
                Some(_) => {}
                None => break,
            }
        }

        Ok(events)
    }

    async fn send_event(&mut self, mut payload: OpackValue) -> Result<()> {
        if let OpackValue::Dict(entries) = &mut payload {
            entries.insert("_x".to_owned(), OpackValue::Int(self.take_xid()));
        }
        self.write_frame(FrameType::OpackEncrypted, payload).await
    }

    async fn exchange_plain(
        &mut self,
        frame_type: FrameType,
        mut payload: OpackValue,
    ) -> Result<OpackValue> {
        if let OpackValue::Dict(entries) = &mut payload {
            entries
                .entry("_x".to_owned())
                .or_insert_with(|| OpackValue::Int(self.take_xid()));
        }
        self.write_frame(frame_type, payload).await?;
        let (_, response) = self.read_frame(false).await?;
        Ok(response)
    }

    async fn exchange_encrypted(&mut self, mut payload: OpackValue) -> Result<OpackValue> {
        let xid = self.take_xid();
        if let OpackValue::Dict(entries) = &mut payload {
            entries.insert("_x".to_owned(), OpackValue::Int(xid));
        }
        self.write_frame(FrameType::OpackEncrypted, payload).await?;

        loop {
            let (_, response) = self.read_frame(true).await?;
            if is_event_message(&response) {
                self.pending_events.push_back(response);
                continue;
            }
            if response
                .dict_get("_t")
                .and_then(OpackValue::int)
                .is_some_and(|value| value == MESSAGE_RESPONSE)
                && response
                    .dict_get("_x")
                    .and_then(OpackValue::int)
                    .is_some_and(|value| value == xid)
            {
                return Ok(response);
            }
        }
    }

    async fn write_frame(&mut self, frame_type: FrameType, payload: OpackValue) -> Result<()> {
        let plaintext = encode_opack(&payload);
        let body = if matches!(frame_type, FrameType::OpackEncrypted) {
            let key = self
                .write_key
                .ok_or_else(|| anyhow!("encrypted write attempted before pair-verify"))?;
            let header = header_bytes(frame_type, plaintext.len() + 16);
            let ciphertext = encrypt_session(&plaintext, &key, self.write_counter, &header)?;
            self.write_counter += 1;
            ciphertext
        } else {
            plaintext
        };

        let header = header_bytes(frame_type, body.len());
        timeout(IO_TIMEOUT, self.stream.write_all(&header))
            .await
            .context("timed out writing frame header")?
            .context("failed to write frame header")?;
        timeout(IO_TIMEOUT, self.stream.write_all(&body))
            .await
            .context("timed out writing frame body")?
            .context("failed to write frame body")?;
        Ok(())
    }

    async fn read_message(&mut self, wait_for: Duration) -> Result<Option<OpackValue>> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(Some(event));
        }

        match timeout(wait_for, self.read_frame(true)).await {
            Ok(Ok((_, response))) => Ok(Some(response)),
            Ok(Err(err)) => Err(err),
            Err(_) => Ok(None),
        }
    }

    async fn read_frame(&mut self, expect_encrypted: bool) -> Result<(FrameType, OpackValue)> {
        let mut header = [0_u8; 4];
        timeout(IO_TIMEOUT, self.stream.read_exact(&mut header))
            .await
            .context("timed out reading frame header")?
            .context("failed to read frame header")?;
        let (frame_type, payload_len) = parse_header(header)?;
        if payload_len > MAX_FRAME_PAYLOAD_LEN {
            bail!(
                "refusing to allocate {} bytes for companion frame (max {})",
                payload_len,
                MAX_FRAME_PAYLOAD_LEN
            );
        }
        let mut payload = vec![0_u8; payload_len];
        timeout(IO_TIMEOUT, self.stream.read_exact(&mut payload))
            .await
            .context("timed out reading frame body")?
            .context("failed to read frame body")?;

        let plaintext = if expect_encrypted && matches!(frame_type, FrameType::OpackEncrypted) {
            let key = self
                .read_key
                .ok_or_else(|| anyhow!("encrypted read attempted before pair-verify"))?;
            let decrypted = decrypt_session(&payload, &key, self.read_counter, &header)?;
            self.read_counter += 1;
            decrypted
        } else {
            payload
        };
        let decoded = decode_opack(&plaintext).context("failed to decode OPACK payload")?;
        Ok((frame_type, decoded))
    }

    fn take_xid(&mut self) -> i64 {
        let xid = self.next_xid;
        self.next_xid += 1;
        xid
    }

    fn controller_identifier(&self) -> String {
        let mut bytes = [0_u8; 6];
        let digest = sha2::Sha512::digest(self.config.controller_name.as_bytes());
        bytes.copy_from_slice(&digest[..6]);
        hex::encode(bytes)
    }

    fn controller_public_id(&self) -> String {
        let mut bytes = [0_u8; 6];
        let digest =
            sha2::Sha512::digest(format!("{}-pub", self.config.controller_name).as_bytes());
        bytes.copy_from_slice(&digest[..6]);
        let hex = hex::encode(bytes);
        [
            &hex[0..2],
            &hex[2..4],
            &hex[4..6],
            &hex[6..8],
            &hex[8..10],
            &hex[10..12],
        ]
        .join(":")
        .to_uppercase()
    }
}

fn extract_pairing_tlv(response: &OpackValue) -> Result<BTreeMap<u8, Vec<u8>>> {
    let OpackValue::Dict(entries) = response else {
        bail!("pairing response was not a dictionary");
    };
    let payload = entries
        .get("_pd")
        .and_then(OpackValue::bytes)
        .ok_or_else(|| anyhow!("pairing response missing _pd"))?;
    let tlv = decode_tlv8(payload);
    if let Some(error) = tlv.get(&(TlvTag::Error as u8)) {
        bail!("pairing protocol returned error code {:?}", error);
    }
    Ok(tlv)
}

fn compute_hap_pair_setup_m1(
    username: &[u8],
    salt: &[u8],
    client_pub: &[u8],
    server_pub: &[u8],
    session_key: &[u8],
) -> Vec<u8> {
    let prime = hex::decode(SRP_3072_PRIME_HEX).expect("valid SRP prime");
    let h_n = sha2_11::Sha512::digest(&prime);
    let h_g = sha2_11::Sha512::digest([0x05_u8]);
    let xor = h_n
        .iter()
        .zip(h_g.iter())
        .map(|(&lhs, &rhs)| lhs ^ rhs)
        .collect::<Vec<_>>();

    let mut digest = sha2_11::Sha512::new();
    digest.update(xor);
    digest.update(sha2_11::Sha512::digest(username));
    digest.update(salt);
    digest.update(client_pub);
    digest.update(server_pub);
    digest.update(session_key);
    digest.finalize().to_vec()
}

fn compute_hap_pair_setup_m2(
    client_pub: &[u8],
    client_proof: &[u8],
    session_key: &[u8],
) -> Vec<u8> {
    let mut digest = sha2_11::Sha512::new();
    digest.update(client_pub);
    digest.update(client_proof);
    digest.update(session_key);
    digest.finalize().to_vec()
}

fn opack_dict<const N: usize>(pairs: [(&str, OpackValue); N]) -> OpackValue {
    let mut entries = BTreeMap::new();
    for (key, value) in pairs {
        entries.insert(key.to_owned(), value);
    }
    OpackValue::Dict(entries)
}

fn is_event_message(value: &OpackValue) -> bool {
    value
        .dict_get("_t")
        .and_then(OpackValue::int)
        .is_some_and(|message_type| message_type == MESSAGE_EVENT)
}

fn decode_fixed_32(hex_str: &str) -> Result<[u8; 32]> {
    decode_fixed_32_vec(hex::decode(hex_str).context("invalid hex string")?)
}

fn decode_fixed_32_vec(bytes: Vec<u8>) -> Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| anyhow!("expected exactly 32 bytes"))
}
