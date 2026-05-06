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
    pub struct SessionStats {
        pub total_download: i64,
        pub total_upload: i64,
        pub download_rate: i64,
        pub upload_rate: i64,
        pub num_torrents: i32,
        pub active_torrents: i32,
        pub paused_torrents: i32,
        pub total_dht_nodes: i64,
        pub num_peers: i32,
        pub dht_running: bool,
        pub lsd_running: bool,
        pub upnp_running: bool,
        pub natpmp_running: bool,
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
            resume_data: String,
        ) -> Result<UniquePtr<torrent_handle>>;

        pub fn bridge_add_torrent_file(
            ses: Pin<&mut session>,
            torrent_path: &str,
            save_path: &str,
            sequential_download: bool,
            max_connections: i32,
            max_uploads: i32,
            resume_data: String,
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

        // Resume data
        pub fn bridge_get_resume_data(hdl: &torrent_handle) -> String;

        // Utility
        pub fn bridge_get_libtorrent_version() -> String;
        pub fn bridge_info_hash_to_string(hdl: &torrent_handle) -> String;
        pub fn bridge_torrent_is_valid(hdl: &torrent_handle) -> bool;

        // per-file priority — reserved for future cli exposure
        #[allow(dead_code)]
        pub fn bridge_set_file_priority(hdl: &torrent_handle, file_index: i32, priority: i32);
        #[allow(dead_code)]
        pub fn bridge_get_file_priorities(hdl: &torrent_handle) -> Vec<i32>;
    }
}
