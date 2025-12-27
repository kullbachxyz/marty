use std::fs;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const PBKDF2_ITERS: u32 = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedValue {
    pub salt: String,
    pub nonce: String,
    pub data: String,
}

pub fn write_encrypted(path: &Path, passphrase: &str, plaintext: &[u8]) -> std::io::Result<()> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce_bytes);

    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), &salt, PBKDF2_ITERS, &mut key);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("key size");
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "encrypt failed"))?;

    let mut out = Vec::with_capacity(SALT_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    fs::write(path, out)
}

pub fn read_encrypted(path: &Path, passphrase: &str) -> std::io::Result<Vec<u8>> {
    let data = fs::read(path)?;
    if data.len() < SALT_LEN + NONCE_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ciphertext too short",
        ));
    }
    let (salt, rest) = data.split_at(SALT_LEN);
    let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);
    decrypt_bytes(passphrase, salt, nonce_bytes, ciphertext)
}

pub fn encrypt_value(passphrase: &str, plaintext: &[u8]) -> std::io::Result<EncryptedValue> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce_bytes);
    let ciphertext = encrypt_bytes(passphrase, &salt, &nonce_bytes, plaintext)?;
    Ok(EncryptedValue {
        salt: BASE64_STANDARD.encode(salt),
        nonce: BASE64_STANDARD.encode(nonce_bytes),
        data: BASE64_STANDARD.encode(ciphertext),
    })
}

pub fn decrypt_value(passphrase: &str, value: &EncryptedValue) -> std::io::Result<Vec<u8>> {
    let salt = BASE64_STANDARD
        .decode(&value.salt)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid salt"))?;
    let nonce = BASE64_STANDARD
        .decode(&value.nonce)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid nonce"))?;
    let data = BASE64_STANDARD
        .decode(&value.data)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid data"))?;
    if salt.len() != SALT_LEN || nonce.len() != NONCE_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid encrypted value",
        ));
    }
    decrypt_bytes(passphrase, &salt, &nonce, &data)
}

fn encrypt_bytes(
    passphrase: &str,
    salt: &[u8],
    nonce_bytes: &[u8],
    plaintext: &[u8],
) -> std::io::Result<Vec<u8>> {
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, PBKDF2_ITERS, &mut key);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("key size");
    let nonce = Nonce::from_slice(nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "encrypt failed"))?;
    Ok(ciphertext)
}

fn decrypt_bytes(
    passphrase: &str,
    salt: &[u8],
    nonce_bytes: &[u8],
    ciphertext: &[u8],
) -> std::io::Result<Vec<u8>> {
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, PBKDF2_ITERS, &mut key);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("key size");
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "decrypt failed"))?;
    Ok(plaintext)
}

pub fn room_log_path(base: &Path, room_id: &str) -> PathBuf {
    base.join(room_id.replace(':', "_")).join("messages.jsonl.enc")
}

pub fn ensure_room_dir(base: &Path, room_id: &str) -> std::io::Result<PathBuf> {
    let dir = base.join(room_id.replace(':', "_"));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub timestamp: i64,
    pub sender: String,
    pub body: String,
    #[serde(default)]
    pub event_id: Option<String>,
}

pub fn append_message(
    base: &Path,
    passphrase: &str,
    room_id: &str,
    record: StoredMessage,
) -> std::io::Result<()> {
    let _ = ensure_room_dir(base, room_id)?;
    let path = room_log_path(base, room_id);
    let mut records = if path.exists() {
        let raw = read_encrypted(&path, passphrase)?;
        serde_json::from_slice::<Vec<StoredMessage>>(&raw).unwrap_or_default()
    } else {
        Vec::new()
    };
    if let Some(event_id) = record.event_id.as_deref() {
        if records.iter().any(|msg| msg.event_id.as_deref() == Some(event_id)) {
            return Ok(());
        }
    }
    records.push(record);
    let data = serde_json::to_vec(&records)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    write_encrypted(&path, passphrase, &data)
}

pub fn load_all_messages(
    base: &Path,
    passphrase: &str,
) -> std::io::Result<Vec<(String, Vec<StoredMessage>)>> {
    let mut out = Vec::new();
    if !base.exists() {
        return Ok(out);
    }
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let room_key = entry.file_name().to_string_lossy().to_string();
        let path = entry.path().join("messages.jsonl.enc");
        if !path.exists() {
            continue;
        }
        let raw = read_encrypted(&path, passphrase)?;
        let records = serde_json::from_slice::<Vec<StoredMessage>>(&raw)
            .unwrap_or_default();
        out.push((room_key, records));
    }
    Ok(out)
}

pub fn latest_room_timestamp(
    base: &Path,
    room_id: &str,
    passphrase: &str,
) -> std::io::Result<Option<i64>> {
    let path = room_log_path(base, room_id);
    if !path.exists() {
        return Ok(None);
    }
    let raw = read_encrypted(&path, passphrase)?;
    let records = serde_json::from_slice::<Vec<StoredMessage>>(&raw).unwrap_or_default();
    let max_ts = records.iter().map(|msg| msg.timestamp).max();
    Ok(max_ts)
}
