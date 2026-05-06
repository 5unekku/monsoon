use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub listen_address: String,
    pub listen_port: u16,
    /// upload slot limit (-1 = unlimited)
    pub max_uploads: i32,
    pub max_connections: i32,
    /// download cap in KiB/s (0 = unlimited)
    pub download_rate_limit: i32,
    /// upload cap in KiB/s (0 = unlimited)
    pub upload_rate_limit: i32,
    pub default_save_path: String,
    pub enable_dht: bool,
    pub enable_lsd: bool,
    pub enable_upnp: bool,
    pub enable_natpmp: bool,
    /// re-add saved torrents on daemon start
    pub auto_resume: bool,
    /// strip identifying info from peer/tracker connections
    pub anonymous_mode: bool,
    /// protocol encryption: "enabled" (prefer), "forced" (require), "disabled"
    pub encryption_mode: String,
    /// seed ratio limit (0.0 = unlimited)
    pub seed_ratio_limit: f64,
    /// seed time limit in minutes (0 = unlimited)
    pub seed_time_limit: i32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_address: "0.0.0.0".to_string(),
            listen_port: 6881,
            max_uploads: -1,
            max_connections: 200,
            download_rate_limit: 0,
            upload_rate_limit: 0,
            default_save_path: directories::BaseDirs::new()
                .map(|dirs| dirs.home_dir().join("Downloads"))
                .unwrap_or_else(|| PathBuf::from("."))
                .to_string_lossy()
                .to_string(),
            enable_dht: true,
            enable_lsd: true,
            enable_upnp: true,
            enable_natpmp: true,
            auto_resume: true,
            anonymous_mode: false,
            encryption_mode: "enabled".to_string(),
            seed_ratio_limit: 0.0,
            seed_time_limit: 0,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if (path.exists()) {
            let content = std::fs::read_to_string(&path).context("read config")?;
            toml::from_str(&content).context("parse config")
        } else {
            let config = Config::default();
            config.save()?;
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("create config dir")?;
        }
        let content = toml::to_string_pretty(self).context("serialize config")?;
        std::fs::write(&path, content).context("write config")
    }

    fn proj_dirs() -> Result<directories::ProjectDirs> {
        directories::ProjectDirs::from("com", "rustor", "rustor")
            .context("determine app directories")
    }

    fn data_dir() -> Result<PathBuf> {
        let dir = Self::proj_dirs()?.data_dir().to_path_buf();
        std::fs::create_dir_all(&dir).context("create data dir")?;
        Ok(dir)
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::proj_dirs()?.config_dir().join("config.toml"))
    }

    /// socket lives in XDG_RUNTIME_DIR when available (cleaned up on logout),
    /// otherwise falls back to the data dir
    pub fn socket_path() -> Result<PathBuf> {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            let path = PathBuf::from(runtime_dir);
            if (path.exists()) {
                return Ok(path.join("rustor.sock"));
            }
        }
        Ok(Self::data_dir()?.join("rustor.sock"))
    }

    pub fn resume_dir() -> Result<PathBuf> {
        let dir = Self::data_dir()?.join("resume");
        std::fs::create_dir_all(&dir).context("create resume dir")?;
        Ok(dir)
    }

    pub fn resume_path(info_hash: &str) -> Result<PathBuf> {
        Ok(Self::resume_dir()?.join(format!("{}.resume", info_hash)))
    }

    /// list of known torrents, persisted so the daemon can resume them on restart
    pub fn torrent_list_path() -> Result<PathBuf> {
        Ok(Self::data_dir()?.join("torrents.json"))
    }
}
