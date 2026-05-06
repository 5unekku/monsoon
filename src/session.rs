use crate::bridge::ffi::{self, AlertInfo, PeerInfo, SessionStats, TorrentFile, TorrentStatus};
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
        let inner = ffi::bridge_create_session(
            listen,
            alert_mask,
            config.max_uploads,
            config.max_connections,
            config.download_rate_limit * 1024,
            config.upload_rate_limit * 1024,
            format!("rustor/{}", crate::VERSION),
        );
        Ok(Self { inner })
    }

    pub fn add_torrent_magnet(
        &mut self,
        magnet_uri: &str,
        save_path: &str,
        resume_data: Option<Vec<u8>>,
    ) -> Result<TorrentHandle> {
        let resume_str = resume_data
            .map(|data| String::from_utf8_lossy(&data).to_string())
            .unwrap_or_default();
        let inner = ffi::bridge_add_torrent_magnet(
            self.inner.pin_mut(),
            magnet_uri,
            save_path,
            false,
            -1,
            -1,
            resume_str,
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
        let resume_str = resume_data
            .map(|data| String::from_utf8_lossy(&data).to_string())
            .unwrap_or_default();
        let inner = ffi::bridge_add_torrent_file(
            self.inner.pin_mut(),
            torrent_path,
            save_path,
            false,
            -1,
            -1,
            resume_str,
        )
        .context("add torrent file")?;
        Ok(TorrentHandle { inner })
    }

    pub fn remove_torrent(&mut self, handle: &TorrentHandle, remove_files: bool) {
        ffi::bridge_remove_torrent(self.inner.pin_mut(), &handle.inner, remove_files);
    }

    pub fn apply_settings(&mut self, config: &crate::config::Config) {
        ffi::bridge_session_apply_settings(
            self.inner.pin_mut(),
            config.max_uploads,
            config.max_connections,
            config.download_rate_limit * 1024,
            config.upload_rate_limit * 1024,
        );
    }

    pub fn pop_alerts(&mut self) -> Vec<AlertInfo> {
        ffi::bridge_pop_alerts(self.inner.pin_mut())
    }

    pub fn stats(&self) -> SessionStats {
        ffi::bridge_get_session_stats(&self.inner)
    }
}

pub struct TorrentHandle {
    inner: cxx::UniquePtr<ffi::torrent_handle>,
}

impl TorrentHandle {
    pub fn is_valid(&self) -> bool {
        ffi::bridge_torrent_is_valid(&self.inner)
    }

    pub fn status(&self) -> TorrentStatus {
        ffi::bridge_get_torrent_status(&self.inner)
    }

    pub fn files(&self) -> Vec<TorrentFile> {
        ffi::bridge_get_torrent_files(&self.inner)
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        ffi::bridge_get_torrent_peers(&self.inner)
    }

    pub fn pause(&self) {
        ffi::bridge_torrent_pause(&self.inner);
    }

    pub fn resume(&self) {
        ffi::bridge_torrent_resume(&self.inner);
    }

    pub fn force_recheck(&self) {
        ffi::bridge_torrent_force_recheck(&self.inner);
    }

    pub fn info_hash(&self) -> String {
        ffi::bridge_info_hash_to_string(&self.inner)
    }

    pub fn resume_data(&self) -> Vec<u8> {
        ffi::bridge_get_resume_data(&self.inner).into_bytes()
    }

    pub fn set_file_priority(&self, file_index: i32, priority: i32) {
        ffi::bridge_set_file_priority(&self.inner, file_index, priority);
    }

    pub fn file_priorities(&self) -> Vec<i32> {
        ffi::bridge_get_file_priorities(&self.inner)
    }
}

pub fn libtorrent_version() -> String {
    ffi::bridge_get_libtorrent_version()
}
