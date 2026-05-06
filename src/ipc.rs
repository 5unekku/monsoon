use serde::{Deserialize, Serialize};

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
}

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
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    List,
    Info { index: usize },
    Add { uri: String, save_path: Option<String> },
    Remove { index: usize, delete_files: bool },
    Pause { index: usize },
    Resume { index: usize },
    Recheck { index: usize },
    Stats,
    GetConfig,
    SetConfig { key: String, value: String },
    SaveConfig,
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    TorrentList(Vec<TorrentInfo>),
    TorrentDetail(Box<TorrentDetail>),
    Added { id: String },
    Stats(StatsInfo),
    Config(String),
    Ok,
    Err(String),
}
