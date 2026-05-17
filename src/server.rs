use crate::bridge::ffi;
use crate::config::Config;
use crate::ipc::{
    FileInfo, PeerInfo, Request, Response, StatsInfo, TorrentDetail, TorrentInfo, TrackerInfo,
};
use crate::session::{Session, TorrentHandle};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

struct ManagedTorrent {
    handle: TorrentHandle,
    magnet_uri: Option<String>,
    torrent_path: Option<String>,
    save_path: String,
    info_hash: String,
}

/// persisted record of a known torrent so the daemon can reload it on restart
#[derive(Serialize, Deserialize)]
struct TorrentRecord {
    info_hash: String,
    magnet_uri: Option<String>,
    torrent_path: Option<String>,
    save_path: String,
}

pub struct App {
    session: Session,
    torrents: Vec<ManagedTorrent>,
    config: Config,
}

impl App {
    pub fn new(config: Config) -> Result<Self> {
        let session = Session::new(&config)?;
        Ok(Self { session, torrents: Vec::new(), config })
    }

    /// load saved torrent list and resume each one with its fastresume data
    pub fn load_torrents(&mut self) -> Result<()> {
        let path = Config::torrent_list_path()?;
        if (!path.exists() || !self.config.auto_resume) { return Ok(()); }

        let content = std::fs::read_to_string(&path).context("read torrent list")?;
        let records: Vec<TorrentRecord> = serde_json::from_str(&content).unwrap_or_default();

        for record in records {
            let resume_data = Config::resume_path(&record.info_hash)
                .ok()
                .and_then(|resume_path| std::fs::read(resume_path).ok());

            let result = match (&record.magnet_uri, &record.torrent_path) {
                (Some(uri), _) => self.session.add_torrent_magnet(uri, &record.save_path, resume_data),
                (_, Some(file_path)) => self.session.add_torrent_file(file_path, &record.save_path, resume_data),
                _ => continue,
            };

            match result {
                Ok(handle) => {
                    let info_hash = handle.info_hash();
                    self.torrents.push(ManagedTorrent {
                        handle,
                        magnet_uri: record.magnet_uri,
                        torrent_path: record.torrent_path,
                        save_path: record.save_path,
                        info_hash,
                    });
                }
                Err(error) => tracing::warn!("failed to resume {}: {}", record.info_hash, error),
            }
        }
        Ok(())
    }

    fn persist_torrent_list(&self) {
        let records: Vec<TorrentRecord> = self.torrents.iter().map(|torrent| TorrentRecord {
            info_hash: torrent.info_hash.clone(),
            magnet_uri: torrent.magnet_uri.clone(),
            torrent_path: torrent.torrent_path.clone(),
            save_path: torrent.save_path.clone(),
        }).collect();
        if let Ok(list_path) = Config::torrent_list_path() {
            if let Ok(json) = serde_json::to_string(&records) {
                let _ = std::fs::write(list_path, json);
            }
        }
    }

    fn add_magnet(&mut self, uri: &str, save_path: Option<&str>) -> Result<String> {
        let save_path = save_path.unwrap_or(&self.config.default_save_path);
        let handle = self.session.add_torrent_magnet(uri, save_path, None)?;
        let info_hash = handle.info_hash();
        self.torrents.push(ManagedTorrent {
            handle,
            magnet_uri: Some(uri.to_string()),
            torrent_path: None,
            save_path: save_path.to_string(),
            info_hash: info_hash.clone(),
        });
        self.persist_torrent_list();
        Ok(info_hash)
    }

    fn add_file(&mut self, file_path: &str, save_path: Option<&str>) -> Result<String> {
        if (!std::path::Path::new(file_path).exists()) {
            return Err(anyhow::anyhow!("file not found: {}", file_path));
        }
        let save_path = save_path.unwrap_or(&self.config.default_save_path);
        let handle = self.session.add_torrent_file(file_path, save_path, None)?;
        let info_hash = handle.info_hash();
        self.torrents.push(ManagedTorrent {
            handle,
            magnet_uri: None,
            torrent_path: Some(file_path.to_string()),
            save_path: save_path.to_string(),
            info_hash: info_hash.clone(),
        });
        self.persist_torrent_list();
        Ok(info_hash)
    }

    fn remove(&mut self, index: usize, delete_files: bool) -> Result<()> {
        if (index >= self.torrents.len()) {
            return Err(anyhow::anyhow!("invalid index: {}", index));
        }
        let torrent = self.torrents.remove(index);
        self.session.remove_torrent(&torrent.handle, delete_files);
        if let Ok(resume_path) = Config::resume_path(&torrent.info_hash) {
            let _ = std::fs::remove_file(resume_path);
        }
        self.persist_torrent_list();
        Ok(())
    }

    fn apply_config_change(&mut self, key: &str, value: &str) -> Result<()> {
        // libtorrent's apply_settings does not re-bind listen interfaces;
        // those values only take effect on the next daemon start. surface that.
        let restart_required = matches!(key, "listen_address" | "listen_port");
        match key {
            "listen_address" => self.config.listen_address = value.to_string(),
            "listen_port" => self.config.listen_port = value.parse()?,
            "max_uploads" => self.config.max_uploads = value.parse()?,
            "max_connections" => self.config.max_connections = value.parse()?,
            "download_rate_limit" | "dl_limit" => self.config.download_rate_limit = value.parse()?,
            "upload_rate_limit" | "ul_limit" => self.config.upload_rate_limit = value.parse()?,
            "default_save_path" => self.config.default_save_path = value.to_string(),
            "enable_dht" | "dht" => self.config.enable_dht = parse_bool(value),
            "enable_lsd" | "lsd" => self.config.enable_lsd = parse_bool(value),
            "enable_upnp" | "upnp" => self.config.enable_upnp = parse_bool(value),
            "enable_natpmp" | "natpmp" => self.config.enable_natpmp = parse_bool(value),
            "anonymous_mode" => self.config.anonymous_mode = parse_bool(value),
            "encryption_mode" => {
                if (!matches!(value, "enabled" | "forced" | "disabled")) {
                    return Err(anyhow::anyhow!("encryption_mode must be: enabled | forced | disabled"));
                }
                self.config.encryption_mode = value.to_string();
            }
            "seed_ratio_limit" | "ratio_limit" => self.config.seed_ratio_limit = value.parse()?,
            "seed_time_limit" | "time_limit" => self.config.seed_time_limit = value.parse()?,
            "ssrf_mitigation" => self.config.ssrf_mitigation = parse_bool(value),
            "validate_https_tracker_certificate" | "validate_https" => {
                self.config.validate_https_tracker_certificate = parse_bool(value);
            }
            "enable_incoming_utp" | "incoming_utp" => self.config.enable_incoming_utp = parse_bool(value),
            "enable_outgoing_utp" | "outgoing_utp" => self.config.enable_outgoing_utp = parse_bool(value),
            "announce_to_all_trackers" => self.config.announce_to_all_trackers = parse_bool(value),
            "announce_to_all_tiers" => self.config.announce_to_all_tiers = parse_bool(value),
            "max_active_downloads" | "active_downloads" => self.config.max_active_downloads = value.parse()?,
            "max_active_uploads" | "active_uploads" => self.config.max_active_uploads = value.parse()?,
            "max_active_torrents" | "active_limit" => self.config.max_active_torrents = value.parse()?,
            // accepted but takes effect on next daemon start — edit config.toml
            // directly if you need to change this without restarting
            "auto_resume" => {
                self.config.auto_resume = parse_bool(value);
                self.config.save()?;
                return Err(anyhow::anyhow!(
                    "auto_resume saved but only applies on daemon restart"
                ));
            }
            _ => return Err(anyhow::anyhow!("unknown config key: {}", key)),
        }
        self.session.apply_settings(&self.config);
        self.config.save()?;
        if (restart_required) {
            return Err(anyhow::anyhow!(
                "{} saved but takes effect only on daemon restart", key
            ));
        }
        Ok(())
    }

    pub fn process_alerts(&mut self) {
        for alert in self.session.pop_alerts() {
            // surface rename outcomes at info/warn so users can see them in the daemon log
            match alert.alert_type.as_str() {
                "file_renamed_alert" => {
                    tracing::info!("rename: {}", alert.message);
                    // a rename changes the on-disk path libtorrent cares about for resume;
                    // persist immediately so a crash doesn't lose the new mapping
                    self.save_resume_data();
                    continue;
                }
                "file_rename_failed_alert" => {
                    tracing::warn!("rename failed: {}", alert.message);
                    continue;
                }
                "storage_moved_alert" => {
                    tracing::info!("move: {}", alert.message);
                    self.save_resume_data();
                    continue;
                }
                "storage_moved_failed_alert" => {
                    tracing::warn!("move failed: {}", alert.message);
                    continue;
                }
                _ => {}
            }
            match alert.category.as_str() {
                "error" => tracing::error!(alert_type = %alert.alert_type, "{}", alert.message),
                "status" => tracing::info!(alert_type = %alert.alert_type, "{}", alert.message),
                _ => tracing::debug!(category = %alert.category, "{}", alert.message),
            }
        }
    }

    fn move_storage(&mut self, index: usize, new_save_path: &str) -> Result<()> {
        let trimmed = new_save_path.trim();
        if (trimmed.is_empty()) {
            return Err(anyhow::anyhow!("save path cannot be empty"));
        }
        let path = std::path::Path::new(trimmed);
        if (!path.is_absolute()) {
            return Err(anyhow::anyhow!("save path must be absolute, not relative"));
        }
        // create the target directory up-front so libtorrent doesn't fail silently
        std::fs::create_dir_all(path)
            .map_err(|error| anyhow::anyhow!("create target directory: {}", error))?;

        let torrent = self.torrents.get_mut(index)
            .ok_or_else(|| anyhow::anyhow!("invalid index: {}", index))?;
        torrent.handle.move_storage(trimmed);
        torrent.save_path = trimmed.to_string();
        // outcome arrives via storage_moved_alert; persist the new path now so
        // a daemon restart before completion still points at the right place
        self.persist_torrent_list();
        tracing::info!(index, new_save_path = trimmed, "submitted move_storage");
        Ok(())
    }

    fn set_file_priority(&self, index: usize, file_index: usize, priority: u8) -> Result<()> {
        if (priority > 7) {
            return Err(anyhow::anyhow!("priority must be 0..=7"));
        }
        let torrent = self.torrents.get(index)
            .ok_or_else(|| anyhow::anyhow!("invalid index: {}", index))?;
        let files = torrent.handle.files();
        if (file_index >= files.len()) {
            return Err(anyhow::anyhow!("invalid file index: {}", file_index));
        }
        torrent.handle.set_file_priority(file_index as i32, priority as i32);
        Ok(())
    }

    fn rename_file(&self, index: usize, file_index: usize, new_name: &str) -> Result<()> {
        let torrent = self.torrents.get(index)
            .ok_or_else(|| anyhow::anyhow!("invalid index: {}", index))?;

        validate_rename_name(new_name)?;

        let files = torrent.handle.files();
        if (file_index >= files.len()) {
            return Err(anyhow::anyhow!("invalid file index: {}", file_index));
        }

        check_rename_collision(&files, file_index, new_name)?;

        torrent.handle.rename_file(file_index as i32, new_name);
        tracing::info!(torrent = %torrent.info_hash, file_index, new_name, "submitted rename");
        Ok(())
    }

    /// rewrite every file whose path starts with `old_prefix` so that the prefix
    /// is replaced by `new_prefix`. validation is atomic: if any target path
    /// would be invalid or collide, none of the renames are submitted.
    fn rename_folder(
        &self,
        index: usize,
        old_prefix: &str,
        new_prefix: &str,
    ) -> Result<crate::ipc::Response> {
        use crate::ipc::Response;

        let torrent = self.torrents.get(index)
            .ok_or_else(|| anyhow::anyhow!("invalid index: {}", index))?;

        validate_rename_name(new_prefix)?;

        let trimmed_old = old_prefix.trim_end_matches('/');
        let trimmed_new = new_prefix.trim_end_matches('/');
        if (trimmed_old.is_empty() || trimmed_new.is_empty()) {
            return Err(anyhow::anyhow!("prefix cannot be empty"));
        }

        let files = torrent.handle.files();
        let mut plan: Vec<(usize, String)> = Vec::new();
        let mut rejected: Vec<(usize, String)> = Vec::new();

        // collect matches and compute target paths
        for (file_index, file) in files.iter().enumerate() {
            let path = &file.path;
            let suffix = if (path == trimmed_old) {
                Some(String::new())
            } else {
                path.strip_prefix(&format!("{}/", trimmed_old)).map(str::to_string)
            };

            let Some(suffix) = suffix else { continue; };

            let new_path = if (suffix.is_empty()) {
                trimmed_new.to_string()
            } else {
                format!("{}/{}", trimmed_new, suffix)
            };

            if let Err(error) = validate_rename_name(&new_path) {
                rejected.push((file_index, error.to_string()));
                continue;
            }
            plan.push((file_index, new_path));
        }

        if (plan.is_empty() && rejected.is_empty()) {
            return Err(anyhow::anyhow!("no files matched prefix: {}", old_prefix));
        }

        // collision check against files that aren't being renamed in this batch
        let renaming_indices: std::collections::HashSet<usize> =
            plan.iter().map(|(file_index, _)| *file_index).collect();
        let static_files: Vec<&str> = files.iter().enumerate()
            .filter(|(file_index, _)| !renaming_indices.contains(file_index))
            .map(|(_, file)| file.path.as_str())
            .collect();

        // check both intra-batch and against-the-rest collisions
        let mut planned_targets: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut filtered_plan: Vec<(usize, String)> = Vec::new();
        for (file_index, new_path) in plan {
            if (static_files.contains(&new_path.as_str())) {
                rejected.push((file_index, format!("would collide with existing file: {}", new_path)));
                continue;
            }
            if (!planned_targets.insert(new_path.clone())) {
                rejected.push((file_index, format!("two files would rename to same target: {}", new_path)));
                continue;
            }
            filtered_plan.push((file_index, new_path));
        }

        // atomic semantics: if anything was rejected, don't submit any renames
        if (!rejected.is_empty()) {
            return Ok(Response::RenameResult { renamed: Vec::new(), rejected });
        }

        let mut renamed: Vec<usize> = Vec::new();
        for (file_index, new_path) in filtered_plan {
            torrent.handle.rename_file(file_index as i32, &new_path);
            tracing::info!(
                torrent = %torrent.info_hash, file_index, new_name = %new_path,
                "submitted rename (folder)"
            );
            renamed.push(file_index);
        }
        Ok(Response::RenameResult { renamed, rejected })
    }

    pub fn save_resume_data(&self) {
        for torrent in &self.torrents {
            if (!torrent.handle.is_valid() || !torrent.handle.status().has_metadata) { continue; }
            let data = torrent.handle.resume_data();
            if (data.is_empty()) { continue; }
            if let Ok(resume_path) = Config::resume_path(&torrent.info_hash) {
                if let Err(error) = std::fs::write(&resume_path, &data) {
                    tracing::warn!("failed to save resume for {}: {}", torrent.info_hash, error);
                }
            }
        }
    }

    pub fn handle_request(&mut self, request: Request) -> Response {
        match request {
            Request::List => {
                let list = self.torrents.iter().enumerate()
                    .map(|(index, torrent)| status_to_info(index, &torrent.handle))
                    .collect();
                Response::TorrentList(list)
            }
            Request::Info { index } => {
                match self.torrents.get(index) {
                    None => Response::Err(format!("invalid index: {}", index)),
                    Some(torrent) => {
                        let info = status_to_info(index, &torrent.handle);
                        let peers = torrent.handle.peers().into_iter().map(bridge_peer_to_ipc).collect();
                        let progresses = torrent.handle.file_progress();
                        let priorities = torrent.handle.file_priorities();
                        let files = torrent.handle.files().into_iter().enumerate()
                            .map(|(file_index, file)| FileInfo {
                                index: file_index,
                                path: file.path.clone(),
                                size: file.size,
                                progress: progresses.get(file_index).copied().unwrap_or(0.0),
                                priority: priorities.get(file_index).copied().unwrap_or(4) as u8,
                            })
                            .collect();
                        let trackers = torrent.handle.trackers().into_iter().map(|tracker| TrackerInfo {
                            url: tracker.url,
                            tier: tracker.tier,
                            verified: tracker.verified,
                            updating: tracker.updating,
                            fails: tracker.fails,
                            message: tracker.message,
                        }).collect();
                        Response::TorrentDetail(Box::new(TorrentDetail {
                            info, peers, files, trackers,
                        }))
                    }
                }
            }
            Request::Add { uri, save_path } => {
                let result = if (uri.starts_with("magnet:")) {
                    self.add_magnet(&uri, save_path.as_deref())
                } else {
                    self.add_file(&uri, save_path.as_deref())
                };
                match result {
                    Ok(hash) => Response::Added { id: hash },
                    Err(error) => Response::Err(error.to_string()),
                }
            }
            Request::Remove { index, delete_files } => match self.remove(index, delete_files) {
                Ok(_) => Response::Ok,
                Err(error) => Response::Err(error.to_string()),
            },
            Request::Pause { index } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => { torrent.handle.pause(); Response::Ok }
            },
            Request::Resume { index } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => { torrent.handle.resume(); Response::Ok }
            },
            Request::Recheck { index } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => { torrent.handle.force_recheck(); Response::Ok }
            },
            Request::Stats => {
                let session_stats = self.session.stats();
                let active = self.torrents.iter().filter(|t| !t.handle.status().is_paused).count() as i32;
                Response::Stats(StatsInfo {
                    num_torrents: self.torrents.len() as i32,
                    active_torrents: active,
                    paused_torrents: self.torrents.len() as i32 - active,
                    download_rate: session_stats.download_rate,
                    upload_rate: session_stats.upload_rate,
                    total_download: session_stats.total_download,
                    total_upload: session_stats.total_upload,
                    total_dht_nodes: session_stats.total_dht_nodes,
                    num_peers: session_stats.num_peers,
                })
            }
            Request::GetConfig => {
                Response::Config(toml::to_string_pretty(&self.config).unwrap_or_default())
            }
            Request::SetConfig { key, value } => match self.apply_config_change(&key, &value) {
                Ok(_) => Response::Ok,
                Err(error) => Response::Err(error.to_string()),
            },
            Request::SaveConfig => match self.config.save() {
                Ok(_) => Response::Ok,
                Err(error) => Response::Err(error.to_string()),
            },
            Request::RenameFile { index, file_index, new_name } => {
                match self.rename_file(index, file_index, &new_name) {
                    Ok(_) => Response::Ok,
                    Err(error) => Response::Err(error.to_string()),
                }
            }
            Request::RenameFolder { index, old_prefix, new_prefix } => {
                match self.rename_folder(index, &old_prefix, &new_prefix) {
                    Ok(response) => response,
                    Err(error) => Response::Err(error.to_string()),
                }
            }
            Request::Move { index, new_save_path } => match self.move_storage(index, &new_save_path) {
                Ok(_) => Response::Ok,
                Err(error) => Response::Err(error.to_string()),
            },
            Request::Reannounce { index } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => { torrent.handle.force_reannounce(); Response::Ok }
            },
            Request::SetFilePriority { index, file_index, priority } => {
                match self.set_file_priority(index, file_index, priority) {
                    Ok(_) => Response::Ok,
                    Err(error) => Response::Err(error.to_string()),
                }
            }
            Request::Magnet { index } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => Response::Magnet(torrent.handle.magnet_uri()),
            },
            Request::SetSequential { index, enabled } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => { torrent.handle.set_sequential(enabled); Response::Ok }
            },
            // caller checks for this before calling handle_request
            Request::Shutdown => Response::Ok,
        }
    }
}

fn status_to_info(index: usize, handle: &TorrentHandle) -> TorrentInfo {
    let status = handle.status();
    TorrentInfo {
        index,
        info_hash: status.info_hash,
        name: status.name,
        state: status.state,
        progress: status.progress,
        download_rate: status.download_rate,
        upload_rate: status.upload_rate,
        connected_peers: status.connected_peers,
        total_peers: status.total_peers,
        connected_seeds: status.connected_seeds,
        total_seeds: status.total_seeds,
        total_wanted: status.total_wanted,
        total_done: status.total_done,
        total_download: status.total_download,
        total_upload: status.total_upload,
        num_pieces: status.num_pieces,
        num_completed_pieces: status.num_completed_pieces,
        added_time: status.added_time,
        completed_time: status.completed_time,
        save_path: status.save_path,
        is_paused: status.is_paused,
        is_finished: status.is_finished,
        is_seeding: status.is_seeding,
        has_metadata: status.has_metadata,
        error: status.error,
    }
}

fn bridge_peer_to_ipc(peer: ffi::PeerInfo) -> PeerInfo {
    PeerInfo {
        ip: peer.ip,
        port: peer.port,
        download_rate: peer.download_rate,
        upload_rate: peer.upload_rate,
        client: peer.client,
        progress: peer.progress,
        flags: peer.flags,
    }
}

fn parse_bool(value: &str) -> bool {
    matches!(value, "true" | "1" | "yes")
}

/// reject obviously dangerous rename targets before submitting to libtorrent.
/// libtorrent itself will overwrite or escape the save_path silently otherwise.
fn validate_rename_name(new_name: &str) -> Result<()> {
    if (new_name.is_empty()) {
        return Err(anyhow::anyhow!("name cannot be empty"));
    }
    let path = std::path::Path::new(new_name);
    if (path.is_absolute()) {
        return Err(anyhow::anyhow!("name must be relative, not absolute"));
    }
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => return Err(anyhow::anyhow!("name cannot contain '..'")),
            Component::Prefix(_) | Component::RootDir => {
                return Err(anyhow::anyhow!("name cannot contain root or drive prefix"));
            }
            _ => {}
        }
    }
    if (new_name.contains('\0')) {
        return Err(anyhow::anyhow!("name cannot contain null bytes"));
    }
    Ok(())
}

fn check_rename_collision(
    files: &[ffi::TorrentFile],
    file_index: usize,
    new_name: &str,
) -> Result<()> {
    for (other_index, file) in files.iter().enumerate() {
        if (other_index == file_index) { continue; }
        if (file.path == new_name) {
            return Err(anyhow::anyhow!(
                "would collide with existing file at index {}: {}",
                other_index, new_name
            ));
        }
    }
    Ok(())
}

fn handle_connection(app: &mut App, stream: UnixStream) -> Result<bool> {
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).context("read request")?;

    if (line.trim().is_empty()) { return Ok(false); }

    // tell the client about parse errors instead of dropping the connection silently;
    // otherwise the client blocks on its 30s read timeout for malformed input.
    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(request) => request,
        Err(error) => {
            let response = Response::Err(format!("bad request: {}", error));
            let json = serde_json::to_string(&response).context("serialize response")?;
            let mut writer = &stream;
            writer.write_all(json.as_bytes())?;
            writer.write_all(b"\n")?;
            return Ok(false);
        }
    };
    let shutdown = matches!(request, Request::Shutdown);
    let response = app.handle_request(request);

    let json = serde_json::to_string(&response).context("serialize response")?;
    let mut writer = &stream;
    writer.write_all(json.as_bytes())?;
    writer.write_all(b"\n")?;

    Ok(shutdown)
}

/// run the daemon in the foreground — blocks until SIGTERM/SIGINT or a Shutdown request
pub fn run(quiet: bool) -> Result<()> {
    if (!quiet) {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive(tracing::Level::INFO.into()),
            )
            .with_target(false)
            .init();
    }

    let config = Config::load().context("load config")?;
    let socket_path = Config::socket_path().context("socket path")?;

    // remove any stale socket from a previous crash
    let _ = std::fs::remove_file(&socket_path);

    let mut app = App::new(config).context("create session")?;
    if let Err(error) = app.load_torrents() {
        tracing::warn!("could not restore saved torrents: {}", error);
    }

    let listener = UnixListener::bind(&socket_path).context("bind socket")?;
    listener.set_nonblocking(true).context("set nonblocking")?;

    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&shutdown))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown))?;

    tracing::info!(
        socket = %socket_path.display(),
        libtorrent = %crate::session::libtorrent_version(),
        "daemon started"
    );

    let mut last_alert_check = Instant::now();
    let mut last_resume_save = Instant::now();
    const RESUME_SAVE_INTERVAL: Duration = Duration::from_secs(5 * 60);

    loop {
        if (shutdown.load(Ordering::Relaxed)) { break; }

        if (last_alert_check.elapsed() >= Duration::from_millis(500)) {
            app.process_alerts();
            last_alert_check = Instant::now();
        }

        // periodic resume snapshot — a SIGKILL or power loss between graceful
        // shutdowns shouldn't cost more than ~5min of progress per torrent
        if (last_resume_save.elapsed() >= RESUME_SAVE_INTERVAL) {
            app.save_resume_data();
            last_resume_save = Instant::now();
        }

        match listener.accept() {
            Ok((stream, _)) => {
                match handle_connection(&mut app, stream) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(error) => tracing::warn!("ipc error: {}", error),
                }
            }
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error.into()),
        }
    }

    tracing::info!("shutting down, saving resume data");
    app.save_resume_data();
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}
