#[cxx::bridge(namespace = "rustbridge")]
pub mod ffi {
    // ─── Shared Structs ─────────────────────────────────────────────────────

    #[derive(Debug, Clone)]
    pub struct TorrentStatus {
        pub name: String,
        pub info_hash: String,
        pub state: String,
        pub save_path: String,
        pub progress: f64,
        pub total_download: i64,
        pub total_upload: i64,
        pub total_done: i64,
        pub total_wanted: i64,
        pub download_rate: i64,
        pub upload_rate: i64,
        pub total_peers: i64,
        pub connected_peers: i64,
        pub total_seeds: i64,
        pub connected_seeds: i64,
        pub num_pieces: i32,
        pub num_completed_pieces: i32,
        pub error: String,
        pub is_paused: bool,
        pub is_finished: bool,
        pub is_seeding: bool,
        pub has_metadata: bool,
        pub added_time: i64,
        pub completed_time: i64,
        pub list_peers: i32,
        pub list_seeds: i32,
        /// upload / max(1, total_done) ratio
        pub ratio: f64,
        /// seconds spent seeding
        pub seeding_time: i64,
    }

    #[derive(Debug, Clone)]
    pub struct AlertInfo {
        pub timestamp: i64,
        pub category: String,
        pub message: String,
        pub alert_type: String,
    }

    #[derive(Debug, Clone)]
    pub struct PeerInfo {
        pub ip: String,
        pub port: i32,
        pub download_rate: i64,
        pub upload_rate: i64,
        pub client: String,
        pub progress: f32,
        pub flags: String,
    }

    #[derive(Debug, Clone)]
    pub struct TorrentFile {
        pub path: String,
        pub size: i64,
        pub offset: i64,
    }

    #[derive(Debug, Clone)]
    pub struct TorrentTracker {
        pub url: String,
        pub tier: i32,
        pub verified: bool,
        pub updating: bool,
        pub fails: i32,
        /// libtorrent message for the last failure (empty when ok)
        pub message: String,
    }

    /// one bencoded resume blob stashed by the bridge after a
    /// save_resume_data_alert fired. Rust side writes it to disk.
    #[derive(Debug, Clone)]
    pub struct PendingResume {
        pub info_hash: String,
        pub bytes: Vec<u8>,
    }

    /// session-wide counters from libtorrent. counts of torrents are computed
    /// in the server layer from `App::torrents`, not from libtorrent, so the
    /// per-state torrent counts and the dht/lsd/upnp/natpmp running flags are
    /// deliberately omitted here.
    #[derive(Debug, Clone)]
    pub struct SessionStats {
        pub total_download: i64,
        pub total_upload: i64,
        pub download_rate: i64,
        pub upload_rate: i64,
        pub total_dht_nodes: i64,
        pub num_peers: i32,
    }

    /// all configurable session settings in one flat struct, passed to create and apply
    #[derive(Debug, Clone)]
    pub struct SessionSettings {
        pub max_uploads: i32,
        pub max_connections: i32,
        /// bytes/sec (0 = unlimited)
        pub download_rate_limit: i32,
        /// bytes/sec (0 = unlimited)
        pub upload_rate_limit: i32,
        pub enable_dht: bool,
        pub enable_lsd: bool,
        pub enable_upnp: bool,
        pub enable_natpmp: bool,
        pub anonymous_mode: bool,
        /// 0=forced, 1=enabled (prefer enc), 2=disabled
        pub encryption_out_policy: i32,
        pub encryption_in_policy: i32,
        pub enable_incoming_utp: bool,
        pub enable_outgoing_utp: bool,
        pub announce_to_all_trackers: bool,
        pub announce_to_all_tiers: bool,
        pub ssrf_mitigation: bool,
        pub validate_https_trackers: bool,
        pub max_active_downloads: i32,
        pub max_active_uploads: i32,
        pub max_active_torrents: i32,
        /// stop seeding above this ratio. 0.0 = unlimited.
        pub seed_ratio_limit: f64,
        /// stop seeding after this many minutes. 0 = unlimited.
        pub seed_time_limit: i32,

        // ─── proxy ──────────────────────────────────────────────────────
        /// libtorrent proxy_type: 0=none, 1=socks4, 2=socks5, 3=socks5_pw,
        /// 4=http, 5=http_pw, 6=i2p_proxy
        pub proxy_type: i32,
        pub proxy_hostname: String,
        pub proxy_port: i32,
        pub proxy_username: String,
        pub proxy_password: String,
        pub proxy_peer_connections: bool,
        pub proxy_tracker_connections: bool,
    }

    // ─── Opaque C++ Types ──────────────────────────────────────────────────

    unsafe extern "C++" {
        include!("bridge.h");

        type session;
        type torrent_handle;

        // Session management
        pub fn bridge_create_session(
            listen_interfaces: String,
            alert_mask: i32,
            user_agent: String,
            settings: &SessionSettings,
        ) -> UniquePtr<session>;

        pub fn bridge_session_apply_settings(
            ses: Pin<&mut session>,
            settings: &SessionSettings,
        );

        // Torrent management
        pub fn bridge_add_torrent_magnet(
            ses: Pin<&mut session>,
            magnet_uri: &str,
            save_path: &str,
            sequential_download: bool,
            max_connections: i32,
            max_uploads: i32,
            resume_data: &[u8],
        ) -> Result<UniquePtr<torrent_handle>>;

        pub fn bridge_add_torrent_file(
            ses: Pin<&mut session>,
            torrent_path: &str,
            save_path: &str,
            sequential_download: bool,
            max_connections: i32,
            max_uploads: i32,
            resume_data: &[u8],
        ) -> Result<UniquePtr<torrent_handle>>;

        pub fn bridge_remove_torrent(
            ses: Pin<&mut session>,
            hdl: &torrent_handle,
            remove_files: bool,
        );

        pub fn bridge_torrent_force_recheck(hdl: &torrent_handle);
        pub fn bridge_torrent_pause(hdl: &torrent_handle);
        pub fn bridge_torrent_resume(hdl: &torrent_handle);

        // Status
        pub fn bridge_get_torrent_status(hdl: &torrent_handle) -> TorrentStatus;

        // Files
        pub fn bridge_get_torrent_files(hdl: &torrent_handle) -> Vec<TorrentFile>;

        // Peers
        pub fn bridge_get_torrent_peers(hdl: &torrent_handle) -> Vec<PeerInfo>;

        // Alerts
        pub fn bridge_pop_alerts(ses: Pin<&mut session>) -> Vec<AlertInfo>;

        // Session stats
        pub fn bridge_get_session_stats(ses: &session) -> SessionStats;

        // Utility
        pub fn bridge_get_libtorrent_version() -> String;
        pub fn bridge_info_hash_to_string(hdl: &torrent_handle) -> String;
        pub fn bridge_torrent_is_valid(hdl: &torrent_handle) -> bool;

        // per-file priority — reserved for future cli exposure
        #[allow(dead_code)]
        pub fn bridge_set_file_priority(hdl: &torrent_handle, file_index: i32, priority: i32);
        #[allow(dead_code)]
        pub fn bridge_get_file_priorities(hdl: &torrent_handle) -> Vec<i32>;

        // rename a single file inside a torrent. async — outcome arrives through alerts.
        pub fn bridge_torrent_rename_file(hdl: &torrent_handle, file_index: i32, new_name: &str);

        // force an immediate tracker announce, bypassing the regular interval.
        pub fn bridge_torrent_force_reannounce(hdl: &torrent_handle);

        // submit an async move-storage. emits storage_moved_alert / storage_moved_failed_alert.
        pub fn bridge_torrent_move_storage(hdl: &torrent_handle, new_save_path: &str);

        // per-torrent tracker list (one row per tier/url)
        pub fn bridge_get_torrent_trackers(hdl: &torrent_handle) -> Vec<TorrentTracker>;

        // add a tracker url at the given tier
        pub fn bridge_torrent_add_tracker(hdl: &torrent_handle, url: &str, tier: i32);

        // remove a tracker by url
        pub fn bridge_torrent_remove_tracker(hdl: &torrent_handle, url: &str);

        // per-file completion fraction (0.0..=1.0), order matches bridge_get_torrent_files
        pub fn bridge_get_file_progress(hdl: &torrent_handle) -> Vec<f32>;

        // shareable magnet URI for an active torrent (empty when invalid)
        pub fn bridge_make_magnet_uri(hdl: &torrent_handle) -> String;

        // toggle the sequential_download flag at runtime
        pub fn bridge_torrent_set_sequential(hdl: &torrent_handle, enabled: bool);

        /// bind this torrent's outgoing connections to a specific interface
        /// (e.g. "tun0" or an ip). pass empty to clear and use the session default.
        pub fn bridge_torrent_use_interface(hdl: &torrent_handle, interface: &str);

        /// load an ip filter file in PeerGuardian/eMule "name:start-end" format
        /// (CIDR lines are also accepted). returns the number of rules loaded,
        /// or -1 on parse error.
        pub fn bridge_session_load_ip_filter(ses: Pin<&mut session>, path: &str) -> i32;


        // per-torrent rate limits. -1 = inherit global limit, 0 = unlimited.
        pub fn bridge_torrent_set_download_limit(hdl: &torrent_handle, limit: i32);
        pub fn bridge_torrent_set_upload_limit(hdl: &torrent_handle, limit: i32);
        pub fn bridge_torrent_download_limit(hdl: &torrent_handle) -> i32;
        pub fn bridge_torrent_upload_limit(hdl: &torrent_handle) -> i32;

        // ─── async session stats migration ─────────────────────────────────
        // post_session_stats triggers a session_stats_alert which the bridge
        // accumulates internally. fetch the latest snapshot via the existing
        // bridge_get_session_stats (which now reads from the accumulator
        // instead of calling the deprecated ses.status()).
        pub fn bridge_session_post_stats(ses: Pin<&mut session>);

        // ─── async resume data migration ───────────────────────────────────
        // submit an async save_resume_data() for this torrent. the result
        // arrives via save_resume_data_alert and is stashed in the bridge.
        pub fn bridge_torrent_save_resume_data_async(hdl: &torrent_handle);

        // drain the bridge's pending resume-data slot. returns a vector of
        // (info_hash, bencoded_bytes) pairs. an empty result is normal —
        // alerts may not have arrived yet for very-recently-submitted saves.
        pub fn bridge_take_pending_resume_data() -> Vec<PendingResume>;
    }

    extern "Rust" {
        fn string_from_lossy(bytes: &[u8]) -> String;
    }
}

fn string_from_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

