use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use matrix_sdk::matrix_auth::MatrixSession;
use serde::{Deserialize, Serialize};

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
