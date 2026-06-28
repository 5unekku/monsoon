use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// snapshot of a single torrent's state, safe to send over ipc
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentInfo {
    pub index: usize,
    pub info_hash: String,
    pub name: String,
    pub state: String,
    pub progress: f64,
    pub download_rate: i64,
    pub upload_rate: i64,
    pub connected_peers: i64,
    pub total_peers: i64,
    pub connected_seeds: i64,
    pub total_seeds: i64,
    pub total_wanted: i64,
    pub total_done: i64,
    pub total_download: i64,
    pub total_upload: i64,
    pub num_pieces: i32,
    pub num_completed_pieces: i32,
    pub added_time: i64,
    pub completed_time: i64,
    pub save_path: String,
    pub is_paused: bool,
    pub is_finished: bool,
    pub is_seeding: bool,
    pub has_metadata: bool,
    pub error: String,
    #[serde(default)]
    pub tags: BTreeSet<String>,
    #[serde(default)]
    pub category: Option<String>,
    /// per-torrent download limit in bytes/sec. -1 = inherit global, 0 = unlimited.
    #[serde(default = "default_minus_one")]
    pub download_limit: i32,
    /// per-torrent upload limit in bytes/sec. -1 = inherit global, 0 = unlimited.
    #[serde(default = "default_minus_one")]
    pub upload_limit: i32,
}

fn default_minus_one() -> i32 { -1 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub ip: String,
    pub port: i32,
    pub download_rate: i64,
    pub upload_rate: i64,
    pub client: String,
    pub progress: f32,
    pub flags: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub index: usize,
    pub path: String,
    pub size: i64,
    /// completion fraction 0.0..=1.0
    pub progress: f32,
    /// libtorrent priority 0..=7 (0 = don't download, 4 = normal, 7 = high)
    pub priority: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackerInfo {
    pub url: String,
    pub tier: i32,
    pub verified: bool,
    pub updating: bool,
    pub fails: i32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsInfo {
    pub num_torrents: i32,
    pub active_torrents: i32,
    pub paused_torrents: i32,
    pub download_rate: i64,
    pub upload_rate: i64,
    pub total_download: i64,
    pub total_upload: i64,
    pub total_dht_nodes: i64,
    pub num_peers: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentDetail {
    pub info: TorrentInfo,
    pub peers: Vec<PeerInfo>,
    pub files: Vec<FileInfo>,
    pub trackers: Vec<TrackerInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    List,
    Info { index: usize },
    Add { uri: String, save_path: Option<String>, category: Option<String>, start_paused: bool, #[serde(default)] content_layout: ContentLayout },
    Remove { index: usize, delete_files: bool },
    Pause { index: usize },
    Resume { index: usize },
    Recheck { index: usize },
    Stats,
    GetConfig,
    SetConfig { key: String, value: String },
    SaveConfig,
    /// rename a single file inside a torrent. `new_name` is the new path
    /// relative to the torrent's save_path; may contain subdirectory components.
    RenameFile { index: usize, file_index: usize, new_name: String },
    /// rename a folder by rewriting the prefix of every file path that starts
    /// with `old_prefix`. validation is atomic — if any file in the set would
    /// fail validation, none are renamed.
    RenameFolder { index: usize, old_prefix: String, new_prefix: String },
    /// move the entire torrent's save directory. async — libtorrent emits
    /// storage_moved_alert / storage_moved_failed_alert.
    Move { index: usize, new_save_path: String },
    /// force tracker announce immediately (bypass the regular interval)
    Reannounce { index: usize },
    /// set the download priority for a single file. 0 = skip, 1..=7 = normal..high.
    SetFilePriority { index: usize, file_index: usize, priority: u8 },
    /// set priorities for many files in one roundtrip. each tuple is (file_index, priority).
    SetFilePrioritiesBatch { index: usize, priorities: Vec<(usize, u8)> },
    /// build a shareable magnet URI for the active torrent
    Magnet { index: usize },
    /// toggle the sequential-download flag (front-to-back piece order)
    SetSequential { index: usize, enabled: bool },
    /// boost the first and last pieces of every file to highest priority
    SetFirstLastPriority { index: usize, enabled: bool },
    /// replace the tag set on a torrent
    SetTags { index: usize, tags: BTreeSet<String> },
    /// set or clear the category on a torrent. `None` clears it.
    SetCategory { index: usize, name: Option<String> },
    /// list all configured categories with their save paths
    ListCategories,
    /// define or update a category (writes to categories.toml)
    SetCategoryDefinition { name: String, save_path: String, add_tags: Vec<String> },
    /// remove a category. torrents previously in it keep their save_path.
    RemoveCategory { name: String },
    /// set a custom display name for a torrent. stored server-side and used
    /// in place of the libtorrent name in all responses. empty string clears it.
    RenameTorrent { index: usize, new_name: String },
    /// pin a torrent's outgoing connections to a specific network interface.
    /// `None` clears the per-torrent override.
    SetTorrentInterface { index: usize, interface: Option<String> },
    /// re-evaluate rules.toml against every torrent and apply add_tags.
    /// returns the number of torrents whose tag set grew.
    RetagAll,
    /// list configured rss/atom feed subscriptions
    ListFeeds,
    /// add or replace a feed subscription
    AddFeed {
        url: String,
        filter: String,
        category: Option<String>,
        save_path: Option<String>,
        poll_interval_minutes: u64,
        start_paused: bool,
    },
    /// remove a feed by index (from ListFeeds order)
    RemoveFeed { index: usize },
    /// force an immediate poll of all feeds regardless of their intervals
    PollFeeds,
    /// set per-torrent rate limits in bytes/sec. -1 = inherit global, 0 = unlimited.
    SetTorrentRateLimit { index: usize, download: i32, upload: i32 },
    /// add a tracker url to a torrent at the given tier
    AddTracker { index: usize, url: String, tier: i32 },
    /// remove a tracker url from a torrent
    RemoveTracker { index: usize, url: String },
    Shutdown,
}

/// whether to wrap a torrent's content in a folder named after the torrent.
/// `Default` resolves to the `default_content_layout` config setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentLayout {
    Default,
    Always,
    Never,
    IfMultiple,
}

impl Default for ContentLayout {
    fn default() -> Self { ContentLayout::Default }
}

impl ContentLayout {
    pub fn label(self) -> &'static str {
        match self {
            ContentLayout::Default => "default",
            ContentLayout::Always => "always",
            ContentLayout::Never => "never",
            ContentLayout::IfMultiple => "if multiple files",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            ContentLayout::Default => ContentLayout::Always,
            ContentLayout::Always => ContentLayout::Never,
            ContentLayout::Never => ContentLayout::IfMultiple,
            ContentLayout::IfMultiple => ContentLayout::Default,
        }
    }

    /// turn `Default` into a concrete layout using the config string;
    /// any unrecognised setting falls back to the natural `IfMultiple`.
    pub fn resolve(self, default_setting: &str) -> ContentLayout {
        match self {
            ContentLayout::Default => match default_setting {
                "always" => ContentLayout::Always,
                "never" => ContentLayout::Never,
                _ => ContentLayout::IfMultiple,
            },
            other => other,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryInfo {
    pub name: String,
    pub save_path: String,
    pub add_tags: Vec<String>,
    pub torrent_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedInfo {
    pub index: usize,
    pub url: String,
    pub filter: String,
    pub category: Option<String>,
    pub save_path: Option<String>,
    pub poll_interval_minutes: u64,
    pub start_paused: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    TorrentList(Vec<TorrentInfo>),
    TorrentDetail(Box<TorrentDetail>),
    Added { id: String },
    Stats(StatsInfo),
    Config(String),
    /// rename outcome. `renamed` are file indices that passed validation and
    /// were submitted to libtorrent; the final filesystem outcome arrives via
    /// alerts and is logged by the daemon. `rejected` are paths that failed
    /// pre-flight validation, paired with the human-readable reason.
    RenameResult { renamed: Vec<usize>, rejected: Vec<(usize, String)> },
    /// magnet URI for a torrent (empty string when invalid or not yet ready)
    Magnet(String),
    /// list of categories
    Categories(Vec<CategoryInfo>),
    /// list of rss feed subscriptions
    Feeds(Vec<FeedInfo>),
    Ok,
    Err(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_layout_cycle_is_four_way() {
        let order = [ContentLayout::Default, ContentLayout::Always, ContentLayout::Never, ContentLayout::IfMultiple];
        for (index, layout) in order.iter().enumerate() {
            assert_eq!(layout.cycle(), order[(index + 1) % order.len()]);
        }
    }

    #[test]
    fn content_layout_resolve_maps_default_to_setting() {
        assert_eq!(ContentLayout::Default.resolve("always"), ContentLayout::Always);
        assert_eq!(ContentLayout::Default.resolve("never"), ContentLayout::Never);
        assert_eq!(ContentLayout::Default.resolve("if_multiple"), ContentLayout::IfMultiple);
        assert_eq!(ContentLayout::Default.resolve("garbage"), ContentLayout::IfMultiple);
        // non-default passes through untouched
        assert_eq!(ContentLayout::Never.resolve("always"), ContentLayout::Never);
    }
}
