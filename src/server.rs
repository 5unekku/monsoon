use crate::bridge::ffi;
use crate::categories::{Categories, Category, TagRules};
use crate::config::Config;
use crate::network;
use crate::ipc::{
    CategoryInfo, FileInfo, PeerInfo, Request, Response, StatsInfo, TorrentDetail, TorrentInfo,
    TrackerInfo,
};
use crate::session::{Session, TorrentHandle};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
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
    tags: BTreeSet<String>,
    category: Option<String>,
    /// per-torrent network interface override (e.g. "tun0"). None = use session default.
    interface_override: Option<String>,
    /// custom display name set by the user. overrides the libtorrent name in responses.
    display_name: Option<String>,
    /// set when the torrent first transitions to "finished" so
    /// completion_script does not fire twice
    was_finished: bool,
    /// resolved content layout still to apply once the torrent is verified.
    /// None = nothing pending (already laid out, or IfMultiple no-op).
    pending_layout: Option<crate::ipc::ContentLayout>,
}

/// persisted record of a known torrent so the daemon can reload it on restart
#[derive(Serialize, Deserialize)]
struct TorrentRecord {
    info_hash: String,
    magnet_uri: Option<String>,
    torrent_path: Option<String>,
    save_path: String,
    #[serde(default)]
    tags: BTreeSet<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    interface_override: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    pending_layout: Option<crate::ipc::ContentLayout>,
}

pub struct App {
    session: Session,
    torrents: Vec<ManagedTorrent>,
    config: Config,
    categories: Categories,
    tag_rules: TagRules,
    rss_feeds: crate::rss::RssFeeds,
    rss_seen: crate::rss::RssSeen,
    /// last successful poll time per feed url
    rss_last_polled: std::collections::HashMap<String, Instant>,
    /// info_hashes of torrents we've already acted on for seed limits, so we
    /// don't repeatedly pause/remove on every poll tick
    seed_limit_acted: std::collections::HashSet<String>,
    /// sender half of the watch-dir scan channel (cloned into background threads)
    watch_dir_tx: std::sync::mpsc::SyncSender<std::path::PathBuf>,
    /// receiver half — drained on the main loop to add discovered .torrent files
    watch_dir_rx: std::sync::mpsc::Receiver<std::path::PathBuf>,
    /// true while a background scan thread is running; cleared when channel drains
    watch_dir_busy: bool,
    /// tracks paths already sent so a slow thread doesn't re-queue them
    watch_dir_seen: std::collections::HashSet<std::path::PathBuf>,
}

impl App {
    pub fn new(config: Config) -> Result<Self> {
        let session = Session::new(&config)?;
        let categories = Categories::load().unwrap_or_else(|error| {
            tracing::warn!("failed to load categories.toml ({}); using empty set", error);
            Categories::default()
        });
        let tag_rules = TagRules::load().unwrap_or_else(|error| {
            tracing::warn!("failed to load rules.toml ({}); using empty set", error);
            TagRules::default()
        });
        let rss_feeds = crate::rss::RssFeeds::load().unwrap_or_else(|error| {
            tracing::warn!("failed to load feeds.toml ({}); using empty set", error);
            crate::rss::RssFeeds::default()
        });
        let rss_seen = crate::rss::RssSeen::load().unwrap_or_else(|error| {
            tracing::warn!("failed to load rss_seen.json ({}); starting fresh", error);
            crate::rss::RssSeen::default()
        });
        // initialise last-polled to (now - interval) so each feed fires on
        // the first rss check tick rather than waiting a full interval cold
        let mut rss_last_polled = std::collections::HashMap::new();
        for feed in &rss_feeds.feeds {
            let ago = Instant::now()
                .checked_sub(Duration::from_secs(feed.poll_interval_minutes * 60))
                .unwrap_or_else(Instant::now);
            rss_last_polled.insert(feed.url.clone(), ago);
        }
        let (watch_dir_tx, watch_dir_rx) = std::sync::mpsc::sync_channel(64);
        Ok(Self {
            session,
            torrents: Vec::new(),
            config,
            categories,
            tag_rules,
            rss_feeds,
            rss_seen,
            rss_last_polled,
            seed_limit_acted: std::collections::HashSet::new(),
            watch_dir_tx,
            watch_dir_rx,
            watch_dir_busy: false,
            watch_dir_seen: std::collections::HashSet::new(),
        })
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
                    if let Some(interface) = record.interface_override.as_deref() {
                        handle.use_interface(interface);
                    }
                    let info_hash = handle.info_hash();
                    self.torrents.push(ManagedTorrent {
                        handle,
                        magnet_uri: record.magnet_uri,
                        torrent_path: record.torrent_path,
                        save_path: record.save_path,
                        info_hash,
                        tags: record.tags,
                        category: record.category,
                        interface_override: record.interface_override,
                        display_name: record.display_name,
                        was_finished: false,
                        pending_layout: record.pending_layout,
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
            tags: torrent.tags.clone(),
            category: torrent.category.clone(),
            interface_override: torrent.interface_override.clone(),
            display_name: torrent.display_name.clone(),
            pending_layout: torrent.pending_layout,
        }).collect();
        if let Ok(list_path) = Config::torrent_list_path() {
            if let Ok(json) = serde_json::to_string(&records) {
                let _ = std::fs::write(list_path, json);
            }
        }
    }

    /// resolve save path + auto-tags for a new torrent based on an explicit
    /// override, an explicit category, or the default category-less behaviour.
    fn resolve_add_target(
        &self,
        save_path: Option<&str>,
        category: Option<&str>,
    ) -> (String, BTreeSet<String>) {
        let mut tags = BTreeSet::new();
        let resolved_path = if let Some(path) = save_path {
            path.to_string()
        } else if let Some(name) = category {
            match self.categories.get(name) {
                Some(definition) => {
                    for tag in &definition.add_tags {
                        tags.insert(tag.clone());
                    }
                    definition.save_path.clone()
                }
                None => self.config.default_save_path.clone(),
            }
        } else {
            self.config.default_save_path.clone()
        };
        (resolved_path, tags)
    }

    fn add_magnet(
        &mut self,
        uri: &str,
        save_path: Option<&str>,
        category: Option<&str>,
        start_paused: bool,
        content_layout: crate::ipc::ContentLayout,
    ) -> Result<String> {
        let default_layout = self.config.default_content_layout.clone();
        let (save_path, mut tags) = self.resolve_add_target(save_path, category);
        let handle = self.session.add_torrent_magnet(uri, &save_path, None)?;
        if start_paused { handle.pause(); }
        let info_hash = handle.info_hash();
        // evaluate auto-tag rules against whatever we know now (name from
        // status, no trackers yet for magnets). retagging happens later on
        // metadata-received via `monsoon retag`.
        let status = handle.status();
        let trackers = handle.trackers().into_iter().map(|tracker| tracker.url).collect::<Vec<_>>();
        let auto_tags = self.tag_rules.evaluate(&status.name, status.total_wanted, &trackers);
        tags.extend(auto_tags);
        let resolved = content_layout.resolve(&default_layout);
        let pending_layout = if (matches!(resolved, crate::ipc::ContentLayout::IfMultiple)) { None } else { Some(resolved) };
        self.torrents.push(ManagedTorrent {
            handle,
            magnet_uri: Some(uri.to_string()),
            torrent_path: None,
            save_path,
            info_hash: info_hash.clone(),
            tags,
            category: category.map(str::to_string),
            interface_override: None,
            display_name: None,
            was_finished: false,
            pending_layout,
        });
        self.persist_torrent_list();
        Ok(info_hash)
    }

    fn add_file(
        &mut self,
        file_path: &str,
        save_path: Option<&str>,
        category: Option<&str>,
        start_paused: bool,
        content_layout: crate::ipc::ContentLayout,
    ) -> Result<String> {
        if (!std::path::Path::new(file_path).exists()) {
            return Err(anyhow::anyhow!("file not found: {}", file_path));
        }
        let default_layout = self.config.default_content_layout.clone();
        let (save_path, mut tags) = self.resolve_add_target(save_path, category);
        let handle = self.session.add_torrent_file(file_path, &save_path, None)?;
        if start_paused {
            handle.pause();
            // .torrent files have metadata immediately — reset any priority-0 files to normal
            // so the user can cherry-pick without needing to un-skip everything first.
            let priorities = handle.file_priorities();
            for (i, priority) in priorities.iter().enumerate() {
                if *priority == 0 { handle.set_file_priority(i as i32, 4); }
            }
        }
        let info_hash = handle.info_hash();
        let status = handle.status();
        let trackers = handle.trackers().into_iter().map(|tracker| tracker.url).collect::<Vec<_>>();
        let auto_tags = self.tag_rules.evaluate(&status.name, status.total_wanted, &trackers);
        tags.extend(auto_tags);
        let resolved = content_layout.resolve(&default_layout);
        let pending_layout = if (matches!(resolved, crate::ipc::ContentLayout::IfMultiple)) { None } else { Some(resolved) };
        self.torrents.push(ManagedTorrent {
            handle,
            magnet_uri: None,
            torrent_path: Some(file_path.to_string()),
            save_path,
            info_hash: info_hash.clone(),
            tags,
            category: category.map(str::to_string),
            interface_override: None,
            display_name: None,
            was_finished: false,
            pending_layout,
        });
        self.persist_torrent_list();
        Ok(info_hash)
    }

    /// re-evaluate auto-tag rules against every torrent's current state.
    /// useful after a magnet has fetched its metadata or after editing
    /// rules.toml. existing tags are preserved (rules can only add).
    fn retag_all(&mut self) -> usize {
        // reload rules from disk first so editing the file doesn't need a daemon restart
        if let Ok(rules) = TagRules::load() {
            self.tag_rules = rules;
        }
        let mut updated = 0;
        for torrent in self.torrents.iter_mut() {
            let status = torrent.handle.status();
            let trackers = torrent.handle.trackers().into_iter().map(|tracker| tracker.url).collect::<Vec<_>>();
            let auto_tags = self.tag_rules.evaluate(&status.name, status.total_wanted, &trackers);
            let before = torrent.tags.len();
            torrent.tags.extend(auto_tags);
            if (torrent.tags.len() > before) { updated += 1; }
        }
        if (updated > 0) { self.persist_torrent_list(); }
        updated
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
        // these settings only take effect after a daemon restart
        let restart_required = matches!(
            key,
            "listen_address" | "listen_port"
            | "proxy_type" | "proxy_host" | "proxy_port"
            | "proxy_username" | "proxy_password"
            | "proxy_peer_connections" | "proxy_tracker_connections"
        );
        match key {
            "listen_address" => self.config.listen_address = value.to_string(),
            "listen_port" => self.config.listen_port = value.parse()?,
            "max_uploads" => self.config.max_uploads = value.parse()?,
            "max_connections" => self.config.max_connections = value.parse()?,
            "download_rate_limit" | "dl_limit" => self.config.download_rate_limit = value.parse()?,
            "upload_rate_limit" | "ul_limit" => self.config.upload_rate_limit = value.parse()?,
            "default_save_path" => self.config.default_save_path = value.to_string(),
            "default_content_layout" => {
                if (!matches!(value, "always" | "never" | "if_multiple")) {
                    return Err(anyhow::anyhow!("default_content_layout must be: always | never | if_multiple"));
                }
                self.config.default_content_layout = value.to_string();
            }
            "rename_merge_same" => {
                if (!matches!(value, "always" | "ask")) {
                    return Err(anyhow::anyhow!("rename_merge_same must be: always | ask"));
                }
                self.config.rename_merge_same = value.to_string();
            }
            "rename_merge_unrelated" => {
                if (!matches!(value, "always" | "ask")) {
                    return Err(anyhow::anyhow!("rename_merge_unrelated must be: always | ask"));
                }
                self.config.rename_merge_unrelated = value.to_string();
            }
            "rename_untracked_files" => {
                if (!matches!(value, "always_move" | "always_leave" | "ask")) {
                    return Err(anyhow::anyhow!("rename_untracked_files must be: always_move | always_leave | ask"));
                }
                self.config.rename_untracked_files = value.to_string();
            }
            "watch_directories" => {
                self.config.watch_directories = value.lines()
                    .map(|line| line.trim().to_string())
                    .filter(|line| !line.is_empty())
                    .collect();
            }
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
            "seed_ratio_action" => {
                if (!matches!(value, "pause" | "remove")) {
                    return Err(anyhow::anyhow!("seed_ratio_action must be: pause | remove"));
                }
                self.config.seed_ratio_action = value.to_string();
            }
            "ssrf_mitigation" => self.config.ssrf_mitigation = parse_bool(value),
            "validate_https_tracker_certificate" | "validate_https" => {
                self.config.validate_https_tracker_certificate = parse_bool(value);
            }
            "enable_incoming_utp" | "incoming_utp" => self.config.enable_incoming_utp = parse_bool(value),
            "enable_outgoing_utp" | "outgoing_utp" => self.config.enable_outgoing_utp = parse_bool(value),
            "announce_to_all_trackers" => self.config.announce_to_all_trackers = parse_bool(value),
            "announce_to_all_tiers" => self.config.announce_to_all_tiers = parse_bool(value),
            "proxy_type" => {
                if (!matches!(value, "none" | "socks4" | "socks5" | "socks5_pw" | "http" | "http_pw" | "i2p")) {
                    return Err(anyhow::anyhow!("proxy_type must be: none | socks4 | socks5 | socks5_pw | http | http_pw | i2p"));
                }
                self.config.proxy_type = value.to_string();
            }
            "proxy_host" => self.config.proxy_host = value.to_string(),
            "proxy_port" => self.config.proxy_port = value.parse()?,
            "proxy_username" => self.config.proxy_username = value.to_string(),
            "proxy_password" => self.config.proxy_password = value.to_string(),
            "proxy_peer_connections" => self.config.proxy_peer_connections = parse_bool(value),
            "proxy_tracker_connections" => self.config.proxy_tracker_connections = parse_bool(value),
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
            "notifications_enabled" => self.config.notifications_enabled = parse_bool(value),
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

    /// detect torrents that have just transitioned to finished and fire the
    /// completion_script (if configured). polled from the main loop alongside
    /// alert processing so it picks up state changes from any source.
    pub fn process_completion_hooks(&mut self) {
        let script_path = match &self.config.completion_script {
            Some(path) if !path.trim().is_empty() => path.clone(),
            _ => {
                // even without a script we still update was_finished so toggling
                // the script on later doesn't fire for already-finished torrents
                for torrent in self.torrents.iter_mut() {
                    if (torrent.handle.status().is_finished) {
                        torrent.was_finished = true;
                    }
                }
                return;
            }
        };
        let timeout = Duration::from_secs(self.config.completion_script_timeout_seconds);
        // collect first to avoid holding &mut self across spawn
        let mut firings: Vec<(usize, String, String, String, i64, Option<String>)> = Vec::new();
        for (index, torrent) in self.torrents.iter_mut().enumerate() {
            let status = torrent.handle.status();
            if (status.is_finished && !torrent.was_finished) {
                torrent.was_finished = true;
                torrent.handle.submit_save_resume_data();
                firings.push((
                    index,
                    status.name,
                    torrent.info_hash.clone(),
                    torrent.save_path.clone(),
                    status.total_wanted,
                    torrent.category.clone(),
                ));
            } else if (!status.is_finished) {
                torrent.was_finished = false;
            }
        }
        for (index, name, hash, save_path, size, category) in firings {
            spawn_completion_script(&script_path, timeout, &name, &hash, &save_path, size, category.as_deref());
            if (self.config.notifications_enabled) {
                spawn_notification(&name, size as u64);
            }
            tracing::info!(index, name, "fired completion script");
        }
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

    /// apply any pending content layout once a torrent is verified (metadata
    /// present and past the initial check). issues rename_file calls and clears
    /// the latch so the resulting re-check doesn't re-trigger.
    fn apply_pending_layouts(&mut self) {
        let mut changed = false;
        for torrent in self.torrents.iter_mut() {
            let Some(layout) = torrent.pending_layout else { continue; };
            let status = torrent.handle.status();
            if (!status.has_metadata) { continue; }
            if (matches!(status.state.as_str(), "downloading_metadata" | "checking_files" | "checking_resume_data")) {
                continue;
            }
            let files: Vec<String> = torrent.handle.files().iter().map(|file| file.path.clone()).collect();
            if (files.is_empty()) { continue; }
            let name = torrent.display_name.clone().unwrap_or(status.name);
            let renames = crate::layout::compute_content_layout_renames(&files, &name, layout);
            for (file_index, new_path) in &renames {
                torrent.handle.rename_file(*file_index as i32, new_path);
                tracing::info!(torrent = %torrent.info_hash, file_index, new_path, "content layout rename");
            }
            torrent.pending_layout = None;
            changed = true;
        }
        if (changed) { self.persist_torrent_list(); }
    }

    /// check each configured rss feed against its poll interval and add any
    /// new matching items. returns the total number of torrents submitted.
    pub fn poll_rss_feeds(&mut self) {
        let now = Instant::now();
        let feeds = self.rss_feeds.feeds.clone();
        for feed in &feeds {
            let interval = Duration::from_secs(feed.poll_interval_minutes * 60);
            let due = self.rss_last_polled.get(&feed.url)
                .map(|last| last.elapsed() >= interval)
                .unwrap_or(true);
            if (!due) { continue; }
            self.rss_last_polled.insert(feed.url.clone(), now);

            let items = match crate::rss::poll_feed(feed, &self.rss_seen) {
                Ok(items) => items,
                Err(error) => {
                    tracing::warn!(url = %feed.url, "rss poll failed: {}", error);
                    continue;
                }
            };

            let mut added = 0usize;
            for (key, uri) in items {
                let result = match crate::sources::resolve(&uri) {
                    Ok(crate::sources::Source::Magnet(magnet)) => {
                        self.add_magnet(&magnet, feed.save_path.as_deref(), feed.category.as_deref(), feed.start_paused, crate::ipc::ContentLayout::Default)
                    }
                    Ok(crate::sources::Source::File(path)) => {
                        self.add_file(&path.to_string_lossy(), feed.save_path.as_deref(), feed.category.as_deref(), feed.start_paused, crate::ipc::ContentLayout::Default)
                    }
                    Err(error) => Err(error),
                };
                match result {
                    Ok(_) => {
                        self.rss_seen.insert(key);
                        added += 1;
                    }
                    Err(error) => tracing::warn!(url = %feed.url, uri, "rss: add failed: {}", error),
                }
            }
            if (added > 0) {
                tracing::info!(url = %feed.url, added, "rss: added new items");
                let _ = self.rss_seen.save();
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

        let torrent = self.torrents.get(index)
            .ok_or_else(|| anyhow::anyhow!("invalid index: {}", index))?;
        let current_save = torrent.save_path.clone();

        // create the target directory up-front so libtorrent doesn't fail
        // silently, and so we can canonicalize it for the symlink check below.
        std::fs::create_dir_all(path)
            .map_err(|error| anyhow::anyhow!("create target directory: {}", error))?;

        // detect when source and destination resolve to the same real path
        // (e.g. a symlink pointing back at the current location). submitting
        // move_storage in that case would tell libtorrent to copy files over
        // themselves, which is at best a no-op and at worst corruption.
        let current_canon = std::fs::canonicalize(&current_save).ok();
        let new_canon = std::fs::canonicalize(path).ok();
        if let (Some(current), Some(new)) = (current_canon, new_canon) {
            if (current == new) {
                tracing::info!(index, path = trimmed, "move_storage: destination resolves to the same path as current; skipping");
                return Ok(());
            }
        }

        let torrent = self.torrents.get_mut(index).unwrap();
        torrent.handle.move_storage(trimmed);
        torrent.save_path = trimmed.to_string();
        // outcome arrives via storage_moved_alert; persist now so a daemon
        // restart before completion still points at the right location
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

        // the target prefix itself must not be an existing file path — that
        // would make a name simultaneously a file and a directory prefix.
        // merging INTO an existing folder (where the prefix is already used
        // as a dir by other files) is explicitly allowed.
        if (static_files.contains(&trimmed_new)) {
            return Err(anyhow::anyhow!(
                "\"{}\" is already a file path — cannot use it as a folder",
                trimmed_new
            ));
        }

        // check both intra-batch and against-the-rest collisions. merging is
        // allowed: coexisting files in the same destination folder are fine;
        // only file-vs-file exact path collisions are rejected.
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

    /// load an ip filter (PeerGuardian or CIDR) from disk if configured.
    /// non-fatal — startup proceeds without filtering if the file is missing
    /// or malformed (the bridge logs the count). when ip_filter_url is set,
    /// we try to refresh the local file from the URL first using the system
    /// `curl` (avoids pulling in a tls http client just for blocklists).
    pub fn install_ip_filter(&mut self) {
        let path = self.config.ip_filter_path.clone();
        if (path.trim().is_empty()) { return; }
        // try to refresh from URL when configured. silent failure preserves
        // the previous on-disk copy so a transient outage doesn't drop the filter.
        let url = self.config.ip_filter_url.clone();
        if (!url.trim().is_empty()) {
            if let Err(error) = refresh_ip_filter(&url, &path) {
                tracing::warn!(url = %url, "ip filter refresh failed: {}", error);
            } else {
                tracing::info!(url = %url, path = %path, "ip filter refreshed");
            }
        }
        if (!std::path::Path::new(&path).exists()) {
            tracing::warn!(path = %path, "ip filter file missing");
            return;
        }
        let count = self.session.load_ip_filter(&path);
        if (count < 0) {
            tracing::warn!(path = %path, "ip filter could not be parsed");
        } else {
            tracing::info!(path = %path, rules = count, "ip filter loaded");
        }
    }

    /// drain the watch-dir channel and add any newly discovered .torrent files.
    /// when the channel empties the background thread is considered done and
    /// watch_dir_busy is cleared so a new scan can be scheduled.
    pub fn drain_watch_dir_rx(&mut self) {
        loop {
            match self.watch_dir_rx.try_recv() {
                Ok(entry_path) => {
                    if (!self.watch_dir_seen.contains(&entry_path)) {
                        self.watch_dir_seen.insert(entry_path.clone());
                        let path_string = entry_path.to_string_lossy().to_string();
                        match self.add_file(&path_string, None, None, false, crate::ipc::ContentLayout::Default) {
                            Ok(hash) => {
                                tracing::info!(file = %path_string, hash, "watch: added");
                                let loaded = entry_path.with_extension("loaded.torrent");
                                if let Err(error) = std::fs::rename(&entry_path, &loaded) {
                                    tracing::warn!(
                                        "could not rename {} after add: {}",
                                        entry_path.display(), error
                                    );
                                }
                            }
                            Err(error) => tracing::warn!("watch: add {}: {}", path_string, error),
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.watch_dir_busy = false;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.watch_dir_busy = false;
                    break;
                }
            }
        }
    }

    /// check all seeding torrents against ratio/time limits and pause or
    /// remove them when a limit is hit. each torrent is only acted on once
    /// (tracked in `seed_limit_acted`) — manual resume clears the entry.
    pub fn check_seed_limits(&mut self) {
        let ratio_limit = self.config.seed_ratio_limit;
        let time_limit = self.config.seed_time_limit;
        if (ratio_limit <= 0.0 && time_limit <= 0) { return; }

        let action = self.config.seed_ratio_action.clone();
        // collect indices to act on to avoid holding borrows across the action calls
        let mut to_act: Vec<(usize, String)> = Vec::new();

        for (index, torrent) in self.torrents.iter().enumerate() {
            let status = torrent.handle.status();
            if (!status.is_seeding || status.is_paused) { continue; }
            if (self.seed_limit_acted.contains(&torrent.info_hash)) { continue; }

            let ratio_hit = ratio_limit > 0.0 && status.ratio >= ratio_limit;
            let time_hit = time_limit > 0 && status.seeding_time >= (time_limit as i64) * 60;
            if (ratio_hit || time_hit) {
                to_act.push((index, torrent.info_hash.clone()));
            }
        }

        for (index, info_hash) in to_act {
            self.seed_limit_acted.insert(info_hash.clone());
            if (action == "remove") {
                tracing::info!(index, hash = %info_hash, "seed limit hit — removing");
                let _ = self.remove(index, false);
            } else {
                tracing::info!(index, hash = %info_hash, "seed limit hit — pausing");
                if let Some(torrent) = self.torrents.get(index) {
                    torrent.handle.pause();
                    torrent.handle.submit_save_resume_data();
                }
            }
        }
    }

    /// spawn a background thread to scan watch directories and send .torrent
    /// paths over watch_dir_tx. only called when !watch_dir_busy.
    /// silent on directories that don't exist — operators may symlink them in later.
    pub fn spawn_watch_dir_scan(&mut self) {
        let directories = self.config.watch_directories.clone();
        let tx = self.watch_dir_tx.clone();
        self.watch_dir_busy = true;
        std::thread::spawn(move || {
            for directory in directories {
                let path = std::path::PathBuf::from(&directory);
                if (!path.is_dir()) { continue; }
                let entries = match std::fs::read_dir(&path) {
                    Ok(entries) => entries,
                    Err(error) => {
                        tracing::warn!("watch dir {}: {}", directory, error);
                        continue;
                    }
                };
                for entry in entries.flatten() {
                    let entry_path = entry.path();
                    if (!entry_path.is_file()) { continue; }
                    let extension = entry_path.extension().and_then(|os| os.to_str());
                    if (extension != Some("torrent")) { continue; }
                    // skip files already marked as loaded
                    if let Some(name) = entry_path.file_name().and_then(|os| os.to_str()) {
                        if (name.contains(".loaded.")) { continue; }
                    }
                    // best-effort send; if the channel is full, skip this file
                    // — it'll be picked up on the next scan cycle
                    let _ = tx.try_send(entry_path);
                }
            }
        });
    }

    /// submit an async save_resume_data for every torrent that has metadata.
    /// the resulting blobs arrive via `save_resume_data_alert` and are written
    /// to disk by `drain_pending_resume_data`.
    pub fn save_resume_data(&self) {
        for torrent in &self.torrents {
            if (!torrent.handle.is_valid() || !torrent.handle.status().has_metadata) { continue; }
            torrent.handle.submit_save_resume_data();
        }
    }

    /// trigger the async session-stats alert. one of these per poll cycle
    /// keeps the bridge's snapshot fresh.
    pub fn post_session_stats(&mut self) {
        self.session.post_stats();
    }

    /// drain bencoded resume blobs the bridge collected from
    /// save_resume_data_alert callbacks and write each to its resume_path.
    pub fn drain_pending_resume_data(&self) {
        for pending in self.session.take_pending_resume_data() {
            if (pending.bytes.is_empty()) { continue; }
            if let Ok(resume_path) = Config::resume_path(&pending.info_hash) {
                if let Err(error) = std::fs::write(&resume_path, &pending.bytes) {
                    tracing::warn!(
                        "failed to write resume for {}: {}",
                        pending.info_hash, error
                    );
                }
            }
        }
    }

    pub fn handle_request(&mut self, request: Request) -> Response {
        match request {
            Request::List => {
                let list = self.torrents.iter().enumerate()
                    .map(|(index, torrent)| status_to_info(index, torrent))
                    .collect();
                Response::TorrentList(list)
            }
            Request::Info { index } => {
                match self.torrents.get(index) {
                    None => Response::Err(format!("invalid index: {}", index)),
                    Some(torrent) => {
                        let info = status_to_info(index, torrent);
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
            Request::Add { uri, save_path, category, start_paused, content_layout } => {
                // delegate scheme + path resolution to the sources module so
                // http/https/ftp/sftp urls and ~ expansion work uniformly.
                match crate::sources::resolve(&uri) {
                    Ok(crate::sources::Source::Magnet(magnet)) => {
                        match self.add_magnet(&magnet, save_path.as_deref(), category.as_deref(), start_paused, content_layout) {
                            Ok(hash) => Response::Added { id: hash },
                            Err(error) => Response::Err(error.to_string()),
                        }
                    }
                    Ok(crate::sources::Source::File(path)) => {
                        match self.add_file(&path.to_string_lossy(), save_path.as_deref(), category.as_deref(), start_paused, content_layout) {
                            Ok(hash) => Response::Added { id: hash },
                            Err(error) => Response::Err(error.to_string()),
                        }
                    }
                    Err(error) => Response::Err(error.to_string()),
                }
            }
            Request::Remove { index, delete_files } => match self.remove(index, delete_files) {
                Ok(_) => Response::Ok,
                Err(error) => Response::Err(error.to_string()),
            },
            Request::Pause { index } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => { torrent.handle.pause(); torrent.handle.submit_save_resume_data(); Response::Ok }
            },
            Request::Resume { index } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => {
                    // clear so check_seed_limits doesn't immediately re-pause
                    self.seed_limit_acted.remove(&torrent.info_hash.clone());
                    torrent.handle.resume();
                    torrent.handle.submit_save_resume_data();
                    Response::Ok
                }
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
            Request::RenameFolder { index, old_prefix, new_prefix, decisions: _ } => {
                match self.rename_folder(index, &old_prefix, &new_prefix) {
                    Ok(response) => response,
                    Err(error) => Response::Err(error.to_string()),
                }
            }
            Request::Move { index, new_save_path, decisions: _ } => match self.move_storage(index, &new_save_path) {
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
            Request::SetFilePrioritiesBatch { index, priorities } => {
                match self.torrents.get(index) {
                    None => Response::Err(format!("invalid index: {}", index)),
                    Some(torrent) => {
                        for (file_index, priority) in &priorities {
                            if *priority > 7 {
                                return Response::Err(format!("priority must be 0..=7, got {}", priority));
                            }
                            torrent.handle.set_file_priority(*file_index as i32, *priority as i32);
                        }
                        Response::Ok
                    }
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
            Request::SetFirstLastPriority { index, enabled } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => { torrent.handle.set_first_last_prio(enabled); Response::Ok }
            },
            Request::SetTags { index, tags } => match self.torrents.get_mut(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => {
                    torrent.tags = tags;
                    self.persist_torrent_list();
                    Response::Ok
                }
            },
            Request::SetCategory { index, name } => {
                if let Some(category_name) = &name {
                    if (self.categories.get(category_name).is_none()) {
                        return Response::Err(format!("unknown category: {}", category_name));
                    }
                }
                match self.torrents.get_mut(index) {
                    None => Response::Err(format!("invalid index: {}", index)),
                    Some(torrent) => {
                        torrent.category = name;
                        self.persist_torrent_list();
                        Response::Ok
                    }
                }
            }
            Request::ListCategories => {
                let entries: Vec<CategoryInfo> = self.categories.entries.iter().map(|(name, definition)| {
                    let torrent_count = self.torrents.iter()
                        .filter(|torrent| torrent.category.as_deref() == Some(name.as_str()))
                        .count();
                    CategoryInfo {
                        name: name.clone(),
                        save_path: definition.save_path.clone(),
                        add_tags: definition.add_tags.clone(),
                        torrent_count,
                    }
                }).collect();
                Response::Categories(entries)
            }
            Request::SetCategoryDefinition { name, save_path, add_tags } => {
                if (name.trim().is_empty()) {
                    return Response::Err("category name cannot be empty".to_string());
                }
                self.categories.entries.insert(name, Category { save_path, add_tags });
                match self.categories.save() {
                    Ok(_) => Response::Ok,
                    Err(error) => Response::Err(error.to_string()),
                }
            }
            Request::RetagAll => {
                let count = self.retag_all();
                Response::Config(format!("retagged {} torrent(s)\n", count))
            }
            Request::RenameTorrent { index, new_name } => {
                match self.torrents.get_mut(index) {
                    None => Response::Err(format!("invalid index: {}", index)),
                    Some(torrent) => {
                        torrent.display_name = if new_name.is_empty() { None } else { Some(new_name) };
                        self.persist_torrent_list();
                        Response::Ok
                    }
                }
            }
            Request::SetTorrentInterface { index, interface } => {
                match self.torrents.get_mut(index) {
                    None => Response::Err(format!("invalid index: {}", index)),
                    Some(torrent) => {
                        // empty string clears the override at the libtorrent level
                        let applied = interface.clone().unwrap_or_default();
                        torrent.handle.use_interface(&applied);
                        torrent.interface_override = interface;
                        self.persist_torrent_list();
                        Response::Ok
                    }
                }
            }
            Request::RemoveCategory { name } => {
                if (self.categories.entries.remove(&name).is_some()) {
                    // null the category on any torrent that was in it
                    for torrent in self.torrents.iter_mut() {
                        if (torrent.category.as_deref() == Some(name.as_str())) {
                            torrent.category = None;
                        }
                    }
                    self.persist_torrent_list();
                    match self.categories.save() {
                        Ok(_) => Response::Ok,
                        Err(error) => Response::Err(error.to_string()),
                    }
                } else {
                    Response::Err(format!("unknown category: {}", name))
                }
            }
            Request::ListFeeds => {
                let feeds = self.rss_feeds.feeds.iter().enumerate().map(|(index, feed)| {
                    crate::ipc::FeedInfo {
                        index,
                        url: feed.url.clone(),
                        filter: feed.filter.clone(),
                        category: feed.category.clone(),
                        save_path: feed.save_path.clone(),
                        poll_interval_minutes: feed.poll_interval_minutes,
                        start_paused: feed.start_paused,
                    }
                }).collect();
                Response::Feeds(feeds)
            }
            Request::AddFeed { url, filter, category, save_path, poll_interval_minutes, start_paused } => {
                let feed = crate::rss::RssFeed {
                    url: url.clone(),
                    filter,
                    category,
                    save_path,
                    poll_interval_minutes,
                    start_paused,
                };
                // replace if url already exists, otherwise append
                if let Some(existing) = self.rss_feeds.feeds.iter_mut().find(|f| f.url == url) {
                    *existing = feed;
                } else {
                    // schedule an immediate poll for new feeds
                    let ago = Instant::now()
                        .checked_sub(Duration::from_secs(poll_interval_minutes * 60))
                        .unwrap_or_else(Instant::now);
                    self.rss_last_polled.insert(url, ago);
                    self.rss_feeds.feeds.push(feed);
                }
                match self.rss_feeds.save() {
                    Ok(_) => Response::Ok,
                    Err(error) => Response::Err(error.to_string()),
                }
            }
            Request::RemoveFeed { index } => {
                if (index >= self.rss_feeds.feeds.len()) {
                    return Response::Err(format!("invalid feed index: {}", index));
                }
                let removed = self.rss_feeds.feeds.remove(index);
                self.rss_last_polled.remove(&removed.url);
                match self.rss_feeds.save() {
                    Ok(_) => Response::Ok,
                    Err(error) => Response::Err(error.to_string()),
                }
            }
            Request::PollFeeds => {
                // reset all timers so every feed fires on the next check tick
                for feed in &self.rss_feeds.feeds {
                    let ago = Instant::now()
                        .checked_sub(Duration::from_secs(feed.poll_interval_minutes * 60))
                        .unwrap_or_else(Instant::now);
                    self.rss_last_polled.insert(feed.url.clone(), ago);
                }
                self.poll_rss_feeds();
                Response::Ok
            }
            Request::SetTorrentRateLimit { index, download, upload } => {
                match self.torrents.get(index) {
                    None => Response::Err(format!("invalid index: {}", index)),
                    Some(torrent) => {
                        torrent.handle.set_download_limit(download);
                        torrent.handle.set_upload_limit(upload);
                        Response::Ok
                    }
                }
            }
            Request::AddTracker { index, url, tier } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => { torrent.handle.add_tracker(&url, tier); Response::Ok }
            },
            Request::RemoveTracker { index, url } => match self.torrents.get(index) {
                None => Response::Err(format!("invalid index: {}", index)),
                Some(torrent) => { torrent.handle.remove_tracker(&url); Response::Ok }
            },
            // caller checks for this before calling handle_request
            Request::Shutdown => Response::Ok,
        }
    }
}

fn status_to_info(index: usize, torrent: &ManagedTorrent) -> TorrentInfo {
    let status = torrent.handle.status();
    TorrentInfo {
        index,
        info_hash: status.info_hash,
        name: torrent.display_name.clone().unwrap_or(status.name),
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
        tags: torrent.tags.clone(),
        category: torrent.category.clone(),
        download_limit: torrent.handle.download_limit(),
        upload_limit: torrent.handle.upload_limit(),
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

/// true if any file that isn't part of this rename already lives under
/// `new_prefix` — i.e. the rename merges into an existing (same-torrent) folder.
fn folder_merge_same(static_files: &[String], new_prefix: &str) -> bool {
    let dir_prefix = format!("{}/", new_prefix);
    static_files.iter().any(|path| path.starts_with(&dir_prefix))
}

/// physically-present files under `dir` whose path relative to `save_root`
/// isn't in `tracked` (the torrent's file paths). recurses. `save_root` is the
/// torrent save path so on-disk paths map back to torrent-relative paths.
fn scan_unrelated_in_dir(
    dir: &std::path::Path,
    tracked: &std::collections::HashSet<String>,
    save_root: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out; };
    for entry in entries.flatten() {
        let path = entry.path();
        if (path.is_dir()) {
            out.extend(scan_unrelated_in_dir(&path, tracked, save_root));
        } else if let Ok(relative) = path.strip_prefix(save_root) {
            let key = relative.to_string_lossy().replace('\\', "/");
            if (!tracked.contains(&key)) {
                out.push(path);
            }
        }
    }
    out
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

/// fire a user-configured completion script with env vars describing the
/// completed torrent. fully detached — we do not block the daemon waiting
/// for it. a separate watchdog thread kills the process if it overruns.
struct NetworkState {
    listener: std::net::TcpListener,
    tls_config: std::sync::Arc<rustls::ServerConfig>,
    token: String,
}

/// build the network listener when configured. side effect: may generate a
/// self-signed cert and a random auth token, persisting both to config.toml
/// on first start so a follow-up `monsoon` invocation can read them back.
fn setup_network_listener(config: &mut Config) -> Result<Option<NetworkState>> {
    if (config.network_listen_address.trim().is_empty()) {
        return Ok(None);
    }
    let tls_config = network::ensure_tls_material(config).context("tls material")?;
    if (config.network_auth_token.is_empty()) {
        config.network_auth_token = network::generate_token();
        tracing::info!("generated network auth token (see config.toml or MONSOON_TOKEN)");
    }
    // persist any generated cert paths + token so the next start finds them
    let _ = config.save();
    let listener = network::bind(&config.network_listen_address)?;
    tracing::info!(
        listen = %config.network_listen_address,
        "network listener active (TLS only)"
    );
    Ok(Some(NetworkState {
        listener,
        tls_config,
        token: config.network_auth_token.clone(),
    }))
}

fn handle_network_connection(app: &mut App, mut authed: network::AuthedConnection) -> Result<()> {
    use std::io::{BufRead, BufReader, Write};
    // read one request per connection. clients re-connect for each command
    // (same as the unix socket path) so per-conn state stays tiny.
    let mut reader = BufReader::new(&mut authed.stream);
    let mut line = String::new();
    reader.read_line(&mut line).context("read network request")?;
    drop(reader);
    if (line.trim().is_empty()) { return Ok(()); }
    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(request) => request,
        Err(error) => {
            let response = Response::Err(format!("bad request: {}", error));
            let json = serde_json::to_string(&response).context("serialize response")?;
            authed.stream.write_all(json.as_bytes())?;
            authed.stream.write_all(b"\n")?;
            return Ok(());
        }
    };
    // Shutdown over the network is disallowed — kill the daemon from local
    if (matches!(request, Request::Shutdown)) {
        let response = Response::Err("Shutdown is disallowed over the network".to_string());
        let json = serde_json::to_string(&response)?;
        authed.stream.write_all(json.as_bytes())?;
        authed.stream.write_all(b"\n")?;
        return Ok(());
    }
    let response = app.handle_request(request);
    let json = serde_json::to_string(&response)?;
    authed.stream.write_all(json.as_bytes())?;
    authed.stream.write_all(b"\n")?;
    Ok(())
}

/// fetch an ip-filter blocklist into `target`. downloads to a `.partial` temp
/// file first and atomically renames on success so a partial fetch can't
/// corrupt the live filter.
fn refresh_ip_filter(url: &str, target: &str) -> Result<()> {
    let temp_path = format!("{}.partial", target);
    let temp = std::path::Path::new(&temp_path);
    if let Some(parent) = std::path::Path::new(target).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    crate::sources::fetch_url(temp, url)
        .inspect_err(|_| { let _ = std::fs::remove_file(temp); })?;
    std::fs::rename(temp, target).context("swap ip filter into place")?;
    Ok(())
}

/// open a tcp connection to the proxy host:port. used as a hard-fail probe
/// before starting the libtorrent session — if the proxy is unreachable, the
/// daemon refuses to start rather than fall through to a direct connection.
fn probe_proxy(host: &str, port: u16) -> Result<()> {
    use std::net::ToSocketAddrs;
    if (host.trim().is_empty() || port == 0) {
        anyhow::bail!("proxy_type is set but proxy_host / proxy_port are not");
    }
    let address = format!("{}:{}", host, port);
    let resolved = address.to_socket_addrs()
        .map_err(|error| anyhow::anyhow!("resolve {}: {}", address, error))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no addresses for {}", address))?;
    std::net::TcpStream::connect_timeout(&resolved, Duration::from_secs(5))
        .map_err(|error| anyhow::anyhow!("connect {}: {}", address, error))?;
    tracing::info!(proxy = %address, "proxy reachable");
    Ok(())
}

fn spawn_notification(name: &str, size: u64) {
    let body = format!("{} ({})", name, bytesize::ByteSize(size));
    let _ = std::process::Command::new("notify-send")
        .args(["Monsoon — download complete", &body, "--app-name=monsoon", "--urgency=normal", "--icon=network-transmit-receive"])
        .spawn();
}

fn spawn_completion_script(
    script: &str,
    timeout: Duration,
    name: &str,
    hash: &str,
    save_path: &str,
    total_size: i64,
    category: Option<&str>,
) {
    use std::process::{Command, Stdio};
    let mut command = Command::new(script);
    command
        .env("MONSOON_TORRENT_NAME", name)
        .env("MONSOON_TORRENT_HASH", hash)
        .env("MONSOON_SAVE_PATH", save_path)
        .env("MONSOON_TOTAL_SIZE", total_size.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(category_name) = category {
        command.env("MONSOON_CATEGORY", category_name);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!("completion script spawn failed: {}", error);
            return;
        }
    };
    // detach a watchdog so a runaway script can't pin a daemon thread
    std::thread::spawn(move || {
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if (!status.success()) {
                        tracing::warn!("completion script exited with {}", status);
                    }
                    return;
                }
                Ok(None) => {
                    if (start.elapsed() >= timeout) {
                        let _ = child.kill();
                        tracing::warn!("completion script killed after {:?} timeout", timeout);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(error) => {
                    tracing::warn!("completion script wait: {}", error);
                    return;
                }
            }
        }
    });
}

fn check_rename_collision(
    files: &[ffi::TorrentFile],
    file_index: usize,
    new_name: &str,
) -> Result<()> {
    let dir_prefix = format!("{}/", new_name);
    for (other_index, file) in files.iter().enumerate() {
        if (other_index == file_index) { continue; }
        if (file.path == new_name) {
            return Err(anyhow::anyhow!(
                "would collide with existing file at index {}: {}",
                other_index, new_name
            ));
        }
        // renaming a file to a name that is already a directory prefix of
        // another file would make that name simultaneously a file and a dir
        if (file.path.starts_with(&dir_prefix)) {
            return Err(anyhow::anyhow!(
                "\"{}\" is used as a directory by file {}: {}",
                new_name, other_index, file.path
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
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

/// fork and exec the daemon as a detached background process. parent returns
/// immediately. child writes its pid to Config::pid_path() and redirects
/// stdout/stderr to Config::log_path().
///
/// fork-vs-double-fork: we trust the child to detach itself (setsid + close
/// stdin) via the `process::Command` plumbing below. this works because the
/// child reopens its own session and is reparented to init if its parent dies.
pub fn run_detached(_quiet: bool) -> Result<()> {
    let binary = std::env::current_exe().context("locate current binary")?;
    let log_path = Config::log_path()?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).context("create log dir")?;
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("open daemon log")?;
    let log_file_err = log_file.try_clone().context("clone log fd")?;

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new(&binary);
        command
            .arg("daemon")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_file_err))
            .process_group(0);
        command.spawn().context("spawn detached daemon")?;
    }
    #[cfg(not(unix))]
    {
        let _ = log_file;
        let _ = log_file_err;
        std::process::Command::new(&binary).arg("daemon").spawn().context("spawn detached daemon")?;
    }
    println!("daemon spawned in background; log: {}", log_path.display());
    println!("status: monsoon status   stop: monsoon stop (or kill)");
    Ok(())
}

/// write our own pid to the pidfile and return the path for later removal.
/// honours a stale pidfile (process not alive) by overwriting it.
fn write_pidfile() -> Result<std::path::PathBuf> {
    let path = Config::pid_path()?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if (path.exists()) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(existing_pid) = text.trim().parse::<i32>() {
                // probe with signal 0 — non-zero means the process is gone
                let alive = crate::process::is_alive(existing_pid);
                if (alive) {
                    anyhow::bail!(
                        "another daemon (pid {}) is already running. \
                         use `monsoon stop` first, or remove {} if it's stale",
                        existing_pid, path.display()
                    );
                }
                tracing::warn!("removing stale pidfile (pid {} not alive)", existing_pid);
            }
        }
    }
    std::fs::write(&path, std::process::id().to_string()).context("write pidfile")?;
    Ok(path)
}


/// if a daemon is already running, return its pid. used to decide whether to
/// attach (interactive TTY) or refuse (service/pipe context) on `monsoon daemon`.
fn existing_daemon_pid() -> Option<i32> {
    let path = Config::pid_path().ok()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let pid = text.trim().parse::<i32>().ok()?;
    if (crate::process::is_alive(pid)) { Some(pid) } else { None }
}

/// follow the running daemon's log via `tail -f`. ctrl-c just detaches the
/// view; the daemon keeps running. used when `monsoon daemon` is invoked from
/// an interactive shell while another daemon is already up.
fn attach_to_daemon_log(pid: i32) -> Result<()> {
    let log_path = Config::log_path()?;
    println!("daemon already running (pid {}); attaching to log", pid);
    println!("log: {}", log_path.display());
    println!("(ctrl-c detaches; daemon keeps running. use `monsoon kill` to stop it)");
    if (!log_path.exists()) {
        // daemon may have been started in the foreground (no log file). just
        // wait for the user to ctrl-c since there's nothing to tail.
        println!("(no log file — daemon is running in the foreground elsewhere)");
        let shutdown = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown))?;
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&shutdown))?;
        while (!shutdown.load(Ordering::Relaxed)) {
            std::thread::sleep(Duration::from_millis(200));
        }
        return Ok(());
    }
    let status = std::process::Command::new("tail")
        .arg("-n").arg("50")
        .arg("-f")
        .arg(&log_path)
        .status()
        .context("spawn tail -f")?;
    let _ = status;
    Ok(())
}

/// run the daemon in the foreground — blocks until SIGTERM/SIGINT or a Shutdown request
pub fn run(quiet: bool) -> Result<()> {
    // if another daemon is already up and we're on an interactive terminal,
    // attach to its log rather than failing. service/pipe contexts (systemd,
    // shell scripts) fall through to the normal pidfile error so accidental
    // double-starts are still caught.
    if let Some(pid) = existing_daemon_pid() {
        use std::io::IsTerminal;
        if (std::io::stdout().is_terminal()) {
            return attach_to_daemon_log(pid);
        }
    }

    if (!quiet) {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive(tracing::Level::INFO.into()),
            )
            .with_target(false)
            .init();
    }

    let mut config = Config::load().context("load config")?;

    // hard-fail safety checks BEFORE binding the ipc socket so a misconfigured
    // proxy or ip filter takes the daemon down before it can leak traffic
    if (config.proxy_type != "none") {
        probe_proxy(&config.proxy_host, config.proxy_port)
            .context("proxy unreachable — refusing to start to prevent leaks")?;
    }
    // encryption hard-fail: when 'forced' is set, ssrf_mitigation must also be
    // on (otherwise tracker redirects can bypass it) and the daemon should
    // refuse to start with manifestly leaky options.
    if (config.encryption_mode == "forced" && !config.ssrf_mitigation) {
        anyhow::bail!(
            "encryption_mode = forced requires ssrf_mitigation = true \
             (otherwise tracker redirects to plaintext peers can bypass it)"
        );
    }

    #[cfg(unix)]
    let socket_path = {
        let p = Config::socket_path().context("socket path")?;
        let _ = std::fs::remove_file(&p);
        p
    };

    let mut app = App::new(config.clone()).context("create session")?;
    app.install_ip_filter();

    #[cfg(unix)]
    let listener = {
        let l = UnixListener::bind(&socket_path).context("bind socket")?;
        l.set_nonblocking(true).context("set nonblocking")?;
        l
    };

    // optional TLS-only TCP listener for remote control. when configured,
    // we ensure cert+token exist, persist them, and start accepting.
    let network_state = setup_network_listener(&mut config)?;

    let pidfile_path = write_pidfile().context("write pidfile")?;

    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&shutdown))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown))?;

    // socket is up and pidfile is written — tell systemd we're ready now so
    // startup doesn't block on torrent loading. resume data loads below.
    #[cfg(unix)]
    notify_systemd_ready();

    if let Err(error) = app.load_torrents() {
        tracing::warn!("could not restore saved torrents: {}", error);
    }

    #[cfg(unix)]
    tracing::info!(
        socket = %socket_path.display(),
        pidfile = %pidfile_path.display(),
        libtorrent = %crate::session::libtorrent_version(),
        "daemon started"
    );
    #[cfg(not(unix))]
    tracing::info!(
        pidfile = %pidfile_path.display(),
        libtorrent = %crate::session::libtorrent_version(),
        "daemon started (tcp-only; local socket not available on this platform)"
    );

    let mut last_alert_check = Instant::now();
    let mut last_resume_save = Instant::now();
    let mut last_watch_scan = Instant::now() - Duration::from_secs(60);
    // initialise to now so the first periodic refresh fires one full interval
    // after startup (startup already called install_ip_filter once)
    let mut last_ip_filter_refresh = Instant::now();
    let mut last_rss_check = Instant::now();
    const RESUME_SAVE_INTERVAL: Duration = Duration::from_secs(5 * 60);
    const WATCH_SCAN_INTERVAL: Duration = Duration::from_secs(5);
    const RSS_CHECK_INTERVAL: Duration = Duration::from_secs(60);

    loop {
        if (shutdown.load(Ordering::Relaxed)) { break; }

        if (last_alert_check.elapsed() >= Duration::from_millis(500)) {
            // post_session_stats triggers the next stats alert. process_alerts
            // then picks it up and updates the bridge's snapshot. drain pending
            // resume data on the same cadence — they arrive as alerts.
            app.post_session_stats();
            app.process_alerts();
            app.apply_pending_layouts();
            app.process_completion_hooks();
            app.drain_pending_resume_data();
            last_alert_check = Instant::now();
        }

        // drain whatever the background scan thread has sent, then schedule a
        // new scan if enough time has elapsed and the previous one is done
        app.drain_watch_dir_rx();
        if (!app.watch_dir_busy && last_watch_scan.elapsed() >= WATCH_SCAN_INTERVAL) {
            app.spawn_watch_dir_scan();
            app.check_seed_limits();
            last_watch_scan = Instant::now();
        }

        // periodic resume snapshot — a SIGKILL or power loss between graceful
        // shutdowns shouldn't cost more than ~5min of progress per torrent
        if (last_resume_save.elapsed() >= RESUME_SAVE_INTERVAL) {
            app.save_resume_data();
            last_resume_save = Instant::now();
        }

        if (last_rss_check.elapsed() >= RSS_CHECK_INTERVAL) {
            app.poll_rss_feeds();
            last_rss_check = Instant::now();
        }

        // periodic ip filter refresh — only when a URL is configured
        if (!app.config.ip_filter_url.trim().is_empty()) {
            let refresh_interval = Duration::from_secs(app.config.ip_filter_refresh_hours * 3600);
            if (last_ip_filter_refresh.elapsed() >= refresh_interval) {
                app.install_ip_filter();
                last_ip_filter_refresh = Instant::now();
            }
        }

        #[cfg(unix)]
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
        // on non-unix there is no local socket; throttle the poll loop
        #[cfg(not(unix))]
        std::thread::sleep(Duration::from_millis(50));

        // network listener — accept one connection per loop tick, with full
        // tls handshake + AUTH gate happening on this thread. heavy clients
        // should connect via the unix socket; the network path is for
        // occasional remote control and is intentionally not threaded.
        if let Some(network) = &network_state {
            match network.listener.accept() {
                Ok((tcp_stream, peer_addr)) => {
                    match network::AuthedConnection::accept(
                        tcp_stream,
                        Arc::clone(&network.tls_config),
                        &network.token,
                    ) {
                        Ok(authed) => {
                            tracing::info!(peer = %peer_addr, "network: authed connection");
                            if let Err(error) = handle_network_connection(&mut app, authed) {
                                tracing::warn!("network ipc error: {}", error);
                            }
                        }
                        Err(error) => tracing::warn!(peer = %peer_addr, "network rejected: {}", error),
                    }
                }
                Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => tracing::warn!("network accept: {}", error),
            }
        }
    }

    tracing::info!("shutting down, saving resume data");
    app.save_resume_data();
    // give libtorrent ~1s to deliver save_resume_data_alerts then drain
    for _ in 0..10 {
        app.process_alerts();
        app.drain_pending_resume_data();
        std::thread::sleep(Duration::from_millis(100));
    }
    #[cfg(unix)]
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&pidfile_path);
    Ok(())
}

/// notify systemd that the daemon is ready, when launched under Type=notify.
/// pure no-op on systems without NOTIFY_SOCKET set.
#[cfg(unix)]
fn notify_systemd_ready() {
    let socket_path = match std::env::var("NOTIFY_SOCKET") {
        Ok(value) if !value.is_empty() => value,
        _ => return,
    };
    use std::os::unix::net::UnixDatagram;
    let datagram = match UnixDatagram::unbound() {
        Ok(datagram) => datagram,
        Err(_) => return,
    };
    let target = if let Some(stripped) = socket_path.strip_prefix('@') {
        // abstract namespace — currently rare for sd_notify but supported by systemd
        format!("\0{}", stripped)
    } else {
        socket_path
    };
    let _ = datagram.send_to(b"READY=1\n", &target);
}

#[cfg(test)]
mod rename_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn merge_same_detects_existing_torrent_file_under_prefix() {
        let static_files = vec!["Dest/already.txt".to_string(), "Other/x.txt".to_string()];
        assert!(folder_merge_same(&static_files, "Dest"));
        assert!(!folder_merge_same(&static_files, "Fresh"));
    }

    #[test]
    fn scan_unrelated_lists_only_untracked_files() {
        let dir = std::env::temp_dir().join(format!("monsoon_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("tracked.txt"), b"x").unwrap();
        std::fs::write(dir.join("stray.txt"), b"y").unwrap();
        std::fs::write(dir.join("sub/stray2.txt"), b"z").unwrap();

        let mut tracked = HashSet::new();
        tracked.insert("tracked.txt".to_string());

        let mut found: Vec<String> = scan_unrelated_in_dir(&dir, &tracked, &dir)
            .into_iter()
            .map(|path| path.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        found.sort();
        assert_eq!(found, vec!["stray.txt".to_string(), "sub/stray2.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
