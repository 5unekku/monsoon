use crate::bridge::ffi::{
    self, AlertInfo, PeerInfo, SessionSettings, SessionStats, TorrentFile, TorrentStatus,
    TorrentTracker,
};
use anyhow::{Context, Result};

// alert category bits — must match libtorrent/alert.hpp. these are bit indices
// (1 << N), not arbitrary masks. previous values were off-by-one across the
// board, which silently subscribed us to peer/connect (firehose) and never
// delivered status_notification at all.
const ALERT_ERROR: i32 = 1 << 0;
const ALERT_PEER: i32 = 1 << 1;
const ALERT_PORT_MAPPING: i32 = 1 << 2;
const ALERT_STORAGE: i32 = 1 << 3;
const ALERT_TRACKER: i32 = 1 << 4;
const ALERT_CONNECT: i32 = 1 << 5;
const ALERT_STATUS: i32 = 1 << 6;
const ALERT_PROGRESS: i32 = 1 << 7;

pub struct Session {
    inner: cxx::UniquePtr<ffi::session>,
}

impl Session {
    pub fn new(config: &crate::config::Config) -> Result<Self> {
        // listen_interfaces takes precedence when non-empty: each entry is
        // either an interface name (resolved via getifaddrs) or an ip. when
        // empty we fall back to the legacy listen_address single value.
        let listen = if (!config.listen_interfaces.is_empty()) {
            let ips = crate::sources::resolve_listen_ips(&config.listen_interfaces);
            ips.iter()
                .map(|ip| format!("{}:{}", ip, config.listen_port))
                .collect::<Vec<_>>()
                .join(",")
        } else {
            format!("{}:{}", config.listen_address, config.listen_port)
        };
        // subscribe only to what we actually consume. progress / peer / connect
        // are firehose categories (block_downloading, peer_connect, etc.) and
        // would overflow the alert queue between our 500ms drains — we don't
        // use them since rates and peer counts come from polled status().
        let alert_mask = ALERT_ERROR | ALERT_STATUS | ALERT_STORAGE
            | ALERT_TRACKER | ALERT_PORT_MAPPING;
        let _ = (ALERT_PEER, ALERT_CONNECT, ALERT_PROGRESS); // reserved for future use
        let settings = config_to_settings(config);
        let inner = ffi::bridge_create_session(
            listen,
            alert_mask,
            format!("monsoon/{}", crate::VERSION),
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

    /// trigger an async post_session_stats. the result arrives via
    /// session_stats_alert and updates the bridge-side snapshot that
    /// `stats()` reads from.
    pub fn post_stats(&mut self) {
        ffi::bridge_session_post_stats(self.inner.pin_mut());
    }

    /// drain the bridge's pending resume-data slot. each (info_hash, bytes)
    /// pair was produced by a successful save_resume_data_alert.
    pub fn take_pending_resume_data(&self) -> Vec<crate::bridge::ffi::PendingResume> {
        ffi::bridge_take_pending_resume_data()
    }

    /// load an ip filter from a path on disk. returns the number of rules
    /// loaded, or -1 if the file couldn't be opened. an empty file (0 rules)
    /// is treated as "do not install a filter."
    pub fn load_ip_filter(&mut self, path: &str) -> i32 {
        ffi::bridge_session_load_ip_filter(self.inner.pin_mut(), path)
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

    /// submit an async save_resume_data. the bencoded blob arrives later
    /// via `Session::take_pending_resume_data()`.
    pub fn submit_save_resume_data(&self) {
        ffi::bridge_torrent_save_resume_data_async(&self.inner);
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

    pub fn set_download_limit(&self, limit: i32) {
        ffi::bridge_torrent_set_download_limit(&self.inner, limit);
    }

    pub fn set_upload_limit(&self, limit: i32) {
        ffi::bridge_torrent_set_upload_limit(&self.inner, limit);
    }

    pub fn download_limit(&self) -> i32 {
        ffi::bridge_torrent_download_limit(&self.inner)
    }

    pub fn upload_limit(&self) -> i32 {
        ffi::bridge_torrent_upload_limit(&self.inner)
    }

    pub fn add_tracker(&self, url: &str, tier: i32) {
        ffi::bridge_torrent_add_tracker(&self.inner, url, tier);
    }

    pub fn remove_tracker(&self, url: &str) {
        ffi::bridge_torrent_remove_tracker(&self.inner, url);
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
    // -1 in our config means "unlimited" — libtorrent treats large positive
    // ints as effectively unlimited; mapping to i32::MAX would risk overflow
    // when libtorrent adds the value to a counter, so use a safely large cap.
    let max_connections = if (config.max_connections == -1) { 65535 } else { config.max_connections };
    let max_uploads = if (config.max_uploads == -1) { 65535 } else { config.max_uploads };
    SessionSettings {
        max_uploads,
        max_connections,
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
