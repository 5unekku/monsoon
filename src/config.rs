use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// legacy: single ip-or-name. preserved for migration. when
    /// `listen_interfaces` is non-empty, this field is ignored.
    pub listen_address: String,
    /// list of interface names (e.g. "tun0", "wlan0") OR ip addresses.
    /// each entry is resolved to its current ip at session start via
    /// getifaddrs and the union is comma-joined for libtorrent. when
    /// empty, the daemon falls back to `listen_address`.
    #[serde(default)]
    pub listen_interfaces: Vec<String>,
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

    // ─── proxy / anonymity network ────────────────────────────────────────
    /// proxy type: "none", "socks4", "socks5", "socks5_pw", "http",
    /// "http_pw", "i2p". when set to anything but "none", the daemon will
    /// probe the proxy at startup and refuse to start if unreachable.
    #[serde(default = "default_proxy_type")]
    pub proxy_type: String,
    #[serde(default)]
    pub proxy_host: String,
    #[serde(default)]
    pub proxy_port: u16,
    #[serde(default)]
    pub proxy_username: String,
    #[serde(default)]
    pub proxy_password: String,
    /// proxy peer-to-peer connections (not just tracker traffic)
    #[serde(default = "default_true")]
    pub proxy_peer_connections: bool,
    /// proxy tracker (announce/scrape) connections
    #[serde(default = "default_true")]
    pub proxy_tracker_connections: bool,

    // ─── ip filter ─────────────────────────────────────────────────────────
    /// local path to an ip filter (PeerGuardian P2P format or CIDR lines).
    /// loaded on every daemon start.
    #[serde(default)]
    pub ip_filter_path: String,
    /// optional URL fetched at startup (and refreshed every refresh interval)
    /// and stored at ip_filter_path. unsafe TLS cert validation is never done.
    #[serde(default)]
    pub ip_filter_url: String,
    #[serde(default = "default_ip_filter_refresh_hours")]
    pub ip_filter_refresh_hours: u64,

    // ─── automation ───────────────────────────────────────────────────────
    /// directories scanned every ~5s for new .torrent files. matches are
    /// auto-added and the file is renamed `.loaded` to prevent re-adding.
    #[serde(default)]
    pub watch_directories: Vec<String>,
    /// optional command run when any torrent finishes. invoked with these
    /// env vars: MONSOON_TORRENT_NAME, MONSOON_TORRENT_HASH, MONSOON_SAVE_PATH,
    /// MONSOON_TOTAL_SIZE, MONSOON_CATEGORY (when set).
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
    /// ordered list of column keys visible in the torrent list. empty means
    /// "use built-in defaults". valid keys live in tui::columns.
    #[serde(default)]
    pub tui_columns: Vec<String>,
    /// per-column width overrides for the torrent list, written by the
    /// mouse-driven drag-to-resize. format is "column_key=cells". columns
    /// without an entry use their built-in default width.
    #[serde(default)]
    pub tui_column_widths: std::collections::BTreeMap<String, u16>,
    /// when true, the tui renders nerd font glyphs (`󰇚` etc.) instead of
    /// ascii state labels. off by default so users without a nerd-font
    /// terminal see legible text instead of tofu.
    #[serde(default)]
    pub tui_nerd_font: bool,

    // ─── networked daemon (TLS-only) ──────────────────────────────────────
    /// optional TCP listen address for remote control (e.g. "0.0.0.0:6890"
    /// or "127.0.0.1:6890"). when set, the daemon also accepts TLS-wrapped
    /// json-line connections. plaintext is never accepted, even on localhost.
    /// the unix socket remains active for local clients.
    #[serde(default)]
    pub network_listen_address: String,
    /// shared-secret token clients must send (`AUTH <token>\n`) before any
    /// other command. generated on first start when `network_listen_address`
    /// is set and persisted here for future reads.
    #[serde(default)]
    pub network_auth_token: String,
    /// path to the TLS cert (PEM). a self-signed cert is generated on first
    /// start when this is empty.
    #[serde(default)]
    pub network_cert_path: String,
    /// path to the TLS private key (PEM).
    #[serde(default)]
    pub network_key_path: String,
}

fn default_completion_script_timeout() -> u64 { 60 }
fn default_tui_sidebar_width() -> u16 { 22 }
fn default_tui_detail_split_percent() -> u16 { 40 }
fn default_proxy_type() -> String { "none".to_string() }
fn default_true() -> bool { true }
fn default_ip_filter_refresh_hours() -> u64 { 24 }

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_address: "0.0.0.0".to_string(),
            listen_interfaces: Vec::new(),
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
            proxy_type: default_proxy_type(),
            proxy_host: String::new(),
            proxy_port: 0,
            proxy_username: String::new(),
            proxy_password: String::new(),
            proxy_peer_connections: true,
            proxy_tracker_connections: true,
            ip_filter_path: String::new(),
            ip_filter_url: String::new(),
            ip_filter_refresh_hours: default_ip_filter_refresh_hours(),
            watch_directories: Vec::new(),
            completion_script: None,
            completion_script_timeout_seconds: default_completion_script_timeout(),
            tui_show_sidebar: false,
            tui_show_detail: false,
            tui_sidebar_width: default_tui_sidebar_width(),
            tui_detail_split_percent: default_tui_detail_split_percent(),
            tui_columns: Vec::new(),
            tui_column_widths: std::collections::BTreeMap::new(),
            tui_nerd_font: false,
            network_listen_address: String::new(),
            network_auth_token: String::new(),
            network_cert_path: String::new(),
            network_key_path: String::new(),
        }
    }
}

impl Config {
    /// load, sanitize, and immediately rewrite the config so every key is
    /// always present + valid. on first run this materialises a complete
    /// config.toml from `Config::default`. on subsequent runs it heals
    /// out-of-range numerics + unknown enums and ensures no field is
    /// missing.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        let mut config: Self = if (path.exists()) {
            let content = std::fs::read_to_string(&path).context("read config")?;
            // serde's #[serde(default)] on the struct fills in any missing keys
            toml::from_str(&content).context("parse config")?
        } else {
            Self::default()
        };
        config.sanitize();
        // always rewrite so the on-disk file is canonical (no missing keys,
        // no invalid numerics). costs one fs::write per daemon start.
        let _ = config.save();
        Ok(config)
    }

    /// clamp every numeric field to a valid range and validate enum-shaped
    /// strings against their accepted values. negative values other than -1
    /// reset to the field's default. -1 is preserved only on fields where
    /// "infinite" makes sense.
    pub fn sanitize(&mut self) {
        let default = Self::default();
        // fields where -1 OR 0 mean "unlimited" (libtorrent accepts both)
        if (self.max_connections < -1) { self.max_connections = default.max_connections; }
        if (self.max_uploads < -1) { self.max_uploads = default.max_uploads; }
        // fields where 0 = unlimited, negatives reset to default
        if (self.download_rate_limit < 0) { self.download_rate_limit = default.download_rate_limit; }
        if (self.upload_rate_limit < 0) { self.upload_rate_limit = default.upload_rate_limit; }
        // max_active_*: -1 = unlimited (libtorrent's sentinel), 0 = none allowed
        // (queue everything; nothing starts), 1+ = literal cap. only values < -1
        // are invalid and reset to default.
        if (self.max_active_downloads < -1) { self.max_active_downloads = default.max_active_downloads; }
        if (self.max_active_uploads < -1) { self.max_active_uploads = default.max_active_uploads; }
        if (self.max_active_torrents < -1) { self.max_active_torrents = default.max_active_torrents; }
        if (self.seed_ratio_limit < 0.0) { self.seed_ratio_limit = default.seed_ratio_limit; }
        if (self.seed_time_limit < 0) { self.seed_time_limit = default.seed_time_limit; }

        // enum-shaped strings
        if (!matches!(self.encryption_mode.as_str(), "enabled" | "forced" | "disabled")) {
            self.encryption_mode = default.encryption_mode.clone();
        }
        if (!matches!(
            self.proxy_type.as_str(),
            "none" | "socks4" | "socks5" | "socks5_pw" | "http" | "http_pw" | "i2p"
        )) {
            self.proxy_type = default.proxy_type.clone();
        }

        // tui sanity
        if (self.tui_sidebar_width < 8) { self.tui_sidebar_width = default.tui_sidebar_width; }
        if (self.tui_detail_split_percent < 10 || self.tui_detail_split_percent > 90) {
            self.tui_detail_split_percent = default.tui_detail_split_percent;
        }

        // listen_address must be non-empty (libtorrent bails otherwise)
        if (self.listen_address.trim().is_empty()) {
            self.listen_address = default.listen_address.clone();
        }
        if (self.default_save_path.trim().is_empty()) {
            self.default_save_path = default.default_save_path.clone();
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
        directories::ProjectDirs::from("com", "monsoon", "monsoon")
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
                return Ok(path.join("monsoon.sock"));
            }
        }
        Ok(Self::data_dir()?.join("monsoon.sock"))
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

    /// rules.toml — auto-tagging rules evaluated on torrent add
    pub fn tag_rules_path() -> Result<PathBuf> {
        Ok(Self::proj_dirs()?.config_dir().join("rules.toml"))
    }

    /// feeds.toml — rss/atom feed subscriptions
    pub fn feeds_path() -> Result<PathBuf> {
        Ok(Self::proj_dirs()?.config_dir().join("feeds.toml"))
    }

    /// rss_seen.json — set of guids/links already added, prevents re-adding on restart
    pub fn rss_seen_path() -> Result<PathBuf> {
        Ok(Self::data_dir()?.join("rss_seen.json"))
    }

    /// pidfile location for daemons launched via `monsoon daemon --detach`.
    /// lives alongside the socket so `monsoon status` / `monsoon kill` find it
    /// without consulting the daemon itself.
    pub fn pid_path() -> Result<PathBuf> {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            let path = PathBuf::from(runtime_dir);
            if (path.exists()) {
                return Ok(path.join("monsoon.pid"));
            }
        }
        Ok(Self::data_dir()?.join("monsoon.pid"))
    }

    /// where stdout/stderr go when the daemon is detached
    pub fn log_path() -> Result<PathBuf> {
        Ok(Self::proj_dirs()?.data_dir().join("daemon.log"))
    }
}
