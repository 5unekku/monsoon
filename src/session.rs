use crate::bridge::ffi::{
    self, AlertInfo, PeerInfo, SessionSettings, SessionStats, TorrentFile, TorrentStatus,
    TorrentTracker,
};
use anyhow::{Context, Result};

const ALERT_STATUS: i32 = 0x1;
const ALERT_ERROR: i32 = 0x2;
const ALERT_PORT_MAPPING: i32 = 0x8;
const ALERT_STORAGE: i32 = 0x10;
const ALERT_TRACKER: i32 = 0x20;
const ALERT_PROGRESS: i32 = 0x80;

pub struct Session {
    inner: cxx::UniquePtr<ffi::session>,
}

impl Session {
    pub fn new(config: &crate::config::Config) -> Result<Self> {
        let listen = format!("{}:{}", config.listen_address, config.listen_port);
        let alert_mask = ALERT_STATUS | ALERT_ERROR | ALERT_TRACKER
            | ALERT_STORAGE | ALERT_PROGRESS | ALERT_PORT_MAPPING;
        let settings = config_to_settings(config);
        let inner = ffi::bridge_create_session(
            listen,
            alert_mask,
            format!("rustor/{}", crate::VERSION),
            &settings,
        );
        Ok(Self { inner })
    }

    pub fn add_torrent_magnet(
        &mut self,
        magnet_uri: &str,
        save_path: &str,
        resume_data: Option<Vec<u8>>,
    ) -> Result<TorrentHandle> {
        let inner = ffi::bridge_add_torrent_magnet(
            self.inner.pin_mut(),
            magnet_uri,
            save_path,
            false,
            -1,
            -1,
            resume_data.as_deref().unwrap_or(&[]),
        )
        .context("add magnet")?;
        Ok(TorrentHandle { inner })
    }

    pub fn add_torrent_file(
        &mut self,
        torrent_path: &str,
        save_path: &str,
        resume_data: Option<Vec<u8>>,
    ) -> Result<TorrentHandle> {
        let inner = ffi::bridge_add_torrent_file(
            self.inner.pin_mut(),
            torrent_path,
            save_path,
            false,
            -1,
            -1,
            resume_data.as_deref().unwrap_or(&[]),
        )
        .context("add torrent file")?;
        Ok(TorrentHandle { inner })
    }

    pub fn remove_torrent(&mut self, handle: &TorrentHandle, remove_files: bool) {
        ffi::bridge_remove_torrent(self.inner.pin_mut(), &handle.inner, remove_files);
    }

    pub fn apply_settings(&mut self, config: &crate::config::Config) {
        let settings = config_to_settings(config);
        ffi::bridge_session_apply_settings(self.inner.pin_mut(), &settings);
    }

    pub fn pop_alerts(&mut self) -> Vec<AlertInfo> {
        ffi::bridge_pop_alerts(self.inner.pin_mut())
    }

    pub fn stats(&self) -> SessionStats {
        ffi::bridge_get_session_stats(&self.inner)
    }

    /// load an ip filter from a path on disk. returns the number of rules
    /// loaded, or -1 if the file couldn't be opened. an empty file (0 rules)
    /// is treated as "do not install a filter."
    pub fn load_ip_filter(&mut self, path: &str) -> i32 {
        ffi::bridge_session_load_ip_filter(self.inner.pin_mut(), path)
    }

    pub fn clear_ip_filter(&mut self) {
        ffi::bridge_session_clear_ip_filter(self.inner.pin_mut());
    }
}

pub struct TorrentHandle {
    inner: cxx::UniquePtr<ffi::torrent_handle>,
}

impl TorrentHandle {
    pub fn is_valid(&self) -> bool { ffi::bridge_torrent_is_valid(&self.inner) }
    pub fn status(&self) -> TorrentStatus { ffi::bridge_get_torrent_status(&self.inner) }
    pub fn files(&self) -> Vec<TorrentFile> { ffi::bridge_get_torrent_files(&self.inner) }
    pub fn peers(&self) -> Vec<PeerInfo> { ffi::bridge_get_torrent_peers(&self.inner) }
    pub fn pause(&self) { ffi::bridge_torrent_pause(&self.inner); }
    pub fn resume(&self) { ffi::bridge_torrent_resume(&self.inner); }
    pub fn force_recheck(&self) { ffi::bridge_torrent_force_recheck(&self.inner); }
    pub fn info_hash(&self) -> String { ffi::bridge_info_hash_to_string(&self.inner) }

    pub fn resume_data(&self) -> Vec<u8> {
        ffi::bridge_get_resume_data(&self.inner)
    }

    pub fn set_file_priority(&self, file_index: i32, priority: i32) {
        ffi::bridge_set_file_priority(&self.inner, file_index, priority);
    }

    pub fn file_priorities(&self) -> Vec<i32> {
        ffi::bridge_get_file_priorities(&self.inner)
    }

    pub fn file_progress(&self) -> Vec<f32> {
        ffi::bridge_get_file_progress(&self.inner)
    }

    pub fn trackers(&self) -> Vec<TorrentTracker> {
        ffi::bridge_get_torrent_trackers(&self.inner)
    }

    /// submit an async rename. validate the name first via [`validate_rename_name`]
    /// in the server layer — this call itself does not check anything.
    pub fn rename_file(&self, file_index: i32, new_name: &str) {
        ffi::bridge_torrent_rename_file(&self.inner, file_index, new_name);
    }

    pub fn force_reannounce(&self) {
        ffi::bridge_torrent_force_reannounce(&self.inner);
    }

    /// submit an async move-storage. validate the path before calling.
    pub fn move_storage(&self, new_save_path: &str) {
        ffi::bridge_torrent_move_storage(&self.inner, new_save_path);
    }

    pub fn magnet_uri(&self) -> String {
        ffi::bridge_make_magnet_uri(&self.inner)
    }

    pub fn set_sequential(&self, enabled: bool) {
        ffi::bridge_torrent_set_sequential(&self.inner, enabled);
    }

    pub fn use_interface(&self, interface: &str) {
        ffi::bridge_torrent_use_interface(&self.inner, interface);
    }
}

pub fn libtorrent_version() -> String {
    ffi::bridge_get_libtorrent_version()
}

fn config_to_settings(config: &crate::config::Config) -> SessionSettings {
    let encryption_policy = match config.encryption_mode.as_str() {
        "forced" => 0,
        "disabled" => 2,
        _ => 1, // "enabled" = prefer
    };
    SessionSettings {
        max_uploads: config.max_uploads,
        max_connections: config.max_connections,
        download_rate_limit: config.download_rate_limit * 1024,
        upload_rate_limit: config.upload_rate_limit * 1024,
        enable_dht: config.enable_dht,
        enable_lsd: config.enable_lsd,
        enable_upnp: config.enable_upnp,
        enable_natpmp: config.enable_natpmp,
        anonymous_mode: config.anonymous_mode,
        encryption_out_policy: encryption_policy,
        encryption_in_policy: encryption_policy,
        enable_incoming_utp: config.enable_incoming_utp,
        enable_outgoing_utp: config.enable_outgoing_utp,
        announce_to_all_trackers: config.announce_to_all_trackers,
        announce_to_all_tiers: config.announce_to_all_tiers,
        ssrf_mitigation: config.ssrf_mitigation,
        validate_https_trackers: config.validate_https_tracker_certificate,
        max_active_downloads: config.max_active_downloads,
        max_active_uploads: config.max_active_uploads,
        max_active_torrents: config.max_active_torrents,
        seed_ratio_limit: config.seed_ratio_limit,
        seed_time_limit: config.seed_time_limit,
        proxy_type: proxy_type_int(&config.proxy_type),
        proxy_hostname: config.proxy_host.clone(),
        proxy_port: config.proxy_port as i32,
        proxy_username: config.proxy_username.clone(),
        proxy_password: config.proxy_password.clone(),
        proxy_peer_connections: config.proxy_peer_connections,
        proxy_tracker_connections: config.proxy_tracker_connections,
    }
}

/// map our config string to libtorrent's proxy_type integer. unknown values
/// fall back to "none" — better safe than leaking over a misconfigured proxy.
fn proxy_type_int(name: &str) -> i32 {
    match name {
        "socks4" => 1,
        "socks5" => 2,
        "socks5_pw" => 3,
        "http" => 4,
        "http_pw" => 5,
        "i2p" => 6,
        _ => 0,
    }
}
