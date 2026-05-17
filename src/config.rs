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

    // ─── security & privacy ──────────────────────────────────────────────
    /// strip identifying info from peer/tracker connections (disables PEX, DHT announce)
    pub anonymous_mode: bool,
    /// protocol encryption: "enabled" (prefer), "forced" (require), "disabled"
    pub encryption_mode: String,
    /// reject tracker responses that redirect to private/local addresses
    pub ssrf_mitigation: bool,
    /// verify TLS certificates for HTTPS trackers
    pub validate_https_tracker_certificate: bool,

    // ─── protocol & transport ─────────────────────────────────────────────
    pub enable_incoming_utp: bool,
    pub enable_outgoing_utp: bool,

    // ─── tracker behaviour ────────────────────────────────────────────────
    /// announce to every tracker in the list rather than stopping at first success
    pub announce_to_all_trackers: bool,
    /// announce to all tiers even when a tracker in an earlier tier succeeds
    pub announce_to_all_tiers: bool,

    // ─── active torrent limits ────────────────────────────────────────────
    pub max_active_downloads: i32,
    pub max_active_uploads: i32,
    pub max_active_torrents: i32,

    // ─── seeding goals (0 = unlimited) ───────────────────────────────────
    pub seed_ratio_limit: f64,
    /// stop seeding after this many minutes (0 = unlimited)
    pub seed_time_limit: i32,

    // ─── automation ───────────────────────────────────────────────────────
    /// directories scanned every ~5s for new .torrent files. matches are
    /// auto-added and the file is renamed `.loaded` to prevent re-adding.
    #[serde(default)]
    pub watch_directories: Vec<String>,
    /// optional command run when any torrent finishes. invoked with these
    /// env vars: RUSTOR_TORRENT_NAME, RUSTOR_TORRENT_HASH, RUSTOR_SAVE_PATH,
    /// RUSTOR_TOTAL_SIZE, RUSTOR_CATEGORY (when set).
    #[serde(default)]
    pub completion_script: Option<String>,
    /// kill the completion script if it runs longer than this
    #[serde(default = "default_completion_script_timeout")]
    pub completion_script_timeout_seconds: u64,

    // ─── tui defaults (applied by the tui on startup) ─────────────────────
    #[serde(default)]
    pub tui_show_sidebar: bool,
    #[serde(default)]
    pub tui_show_detail: bool,
    #[serde(default = "default_tui_sidebar_width")]
    pub tui_sidebar_width: u16,
    #[serde(default = "default_tui_detail_split_percent")]
    pub tui_detail_split_percent: u16,
}

fn default_completion_script_timeout() -> u64 { 60 }
fn default_tui_sidebar_width() -> u16 { 22 }
fn default_tui_detail_split_percent() -> u16 { 40 }

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
            ssrf_mitigation: true,
            validate_https_tracker_certificate: true,
            enable_incoming_utp: true,
            enable_outgoing_utp: true,
            announce_to_all_trackers: false,
            announce_to_all_tiers: true,
            max_active_downloads: 3,
            max_active_uploads: 5,
            max_active_torrents: 8,
            seed_ratio_limit: 0.0,
            seed_time_limit: 0,
            watch_directories: Vec::new(),
            completion_script: None,
            completion_script_timeout_seconds: default_completion_script_timeout(),
            tui_show_sidebar: false,
            tui_show_detail: false,
            tui_sidebar_width: default_tui_sidebar_width(),
            tui_detail_split_percent: default_tui_detail_split_percent(),
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

    /// categories.toml — definitions of named categories (save_path, etc.)
    pub fn categories_path() -> Result<PathBuf> {
        Ok(Self::proj_dirs()?.config_dir().join("categories.toml"))
    }
}
