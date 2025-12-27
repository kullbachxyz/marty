use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use matrix_sdk::matrix_auth::MatrixSession;
use serde::{Deserialize, Serialize};

use crate::storage::{decrypt_value, encrypt_value, EncryptedValue};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub accounts: Vec<AccountConfig>,
    pub active: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AccountConfig {
    pub homeserver: String,
    pub username: String,
    pub user_id: Option<String>,
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_encrypted: Option<EncryptedValue>,
    #[serde(default, skip_serializing)]
    pub session: Option<MatrixSession>,
}

pub fn config_path() -> io::Result<PathBuf> {
    let base = home_dir()?;
    let dir = base.join(".config").join("marty");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("config"))
}

pub fn data_dir() -> io::Result<PathBuf> {
    let base = home_dir()?;
    let dir = base.join(".local").join("share").join("marty");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn crypto_dir() -> io::Result<PathBuf> {
    let dir = data_dir()?.join("crypto");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn messages_dir() -> io::Result<PathBuf> {
    let dir = data_dir()?.join("messages");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn home_dir() -> io::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "HOME not set"))
}

pub fn load_config(path: &Path) -> io::Result<AppConfig> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let raw = fs::read_to_string(path)?;
    let cfg = toml::from_str(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(cfg)
}

pub fn save_config(path: &Path, cfg: &AppConfig) -> io::Result<()> {
    let raw = toml::to_string_pretty(cfg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    fs::write(path, raw)
}

pub fn decrypt_sessions(cfg: &mut AppConfig, passphrase: &str) -> io::Result<()> {
    for account in &mut cfg.accounts {
        if account.session.is_some() {
            continue;
        }
        let Some(encrypted) = &account.session_encrypted else {
            continue;
        };
        let raw = decrypt_value(passphrase, encrypted).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to decrypt session: {}", e),
            )
        })?;
        let session = serde_json::from_slice(&raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        account.session = Some(session);
    }
    Ok(())
}

pub fn encrypt_account_session(account: &mut AccountConfig, passphrase: &str) -> io::Result<()> {
    let Some(session) = &account.session else {
        return Ok(());
    };
    let raw = serde_json::to_vec(session)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let encrypted = encrypt_value(passphrase, &raw)?;
    account.session_encrypted = Some(encrypted);
    Ok(())
}

pub fn encrypt_missing_sessions(cfg: &mut AppConfig, passphrase: &str) -> io::Result<bool> {
    let mut changed = false;
    for account in &mut cfg.accounts {
        if account.session.is_some() && account.session_encrypted.is_none() {
            encrypt_account_session(account, passphrase)?;
            changed = true;
        }
    }
    Ok(changed)
}
