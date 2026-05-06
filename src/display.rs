use crate::ipc::{FileInfo, PeerInfo, Response, StatsInfo, TorrentDetail, TorrentInfo};
use chrono::DateTime;

pub fn format_bytes(bytes: i64) -> String {
    bytesize::to_string(bytes as u64, true)
}

pub fn format_rate(bytes_per_sec: i64) -> String {
    if (bytes_per_sec == 0) { return "0 B/s".to_string(); }
    format!("{}/s", bytesize::to_string(bytes_per_sec as u64, true))
}

pub fn format_timestamp(timestamp: i64) -> String {
    if (timestamp == 0) { return "N/A".to_string(); }
    DateTime::from_timestamp(timestamp, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "N/A".to_string())
}

pub fn format_progress_bar(progress: f64, width: usize) -> String {
    let filled = ((progress * width as f64).round() as usize).min(width);
    let bar = "█".repeat(filled) + &"░".repeat(width - filled);
    format!("{:.1}% [{}]", progress * 100.0, bar)
}

pub fn print_response(response: Response) {
    match response {
        Response::TorrentList(list) => print_torrent_list(&list),
        Response::TorrentDetail(detail) => print_torrent_detail(&detail),
        Response::Added { id } => println!("added: {}", id),
        Response::Stats(stats) => print_stats(&stats),
        Response::Config(toml) => print!("{}", toml),
        Response::Ok => {}
        Response::Err(message) => eprintln!("error: {}", message),
    }
}

fn print_torrent_list(list: &[TorrentInfo]) {
    if (list.is_empty()) {
        println!("no torrents");
        return;
    }
    println!("{:<4} {:<32} {:<4} {:>7} {:>12} {:>12} {:<14}",
        "#", "name", "st", "prog", "down", "up", "peers");
    println!("{}", "─".repeat(90));
    for torrent in list {
        let name = truncate(&torrent.name.chars().collect::<String>(), 31);
        let state = format_state(&torrent.state);
        let progress = format!("{:.1}%", torrent.progress * 100.0);
        let down = format_rate(torrent.download_rate);
        let up = format_rate(torrent.upload_rate);
        let peers = format!("{}/{}", torrent.connected_peers, torrent.total_peers);
        let seeds = format!("S:{}", torrent.connected_seeds);
        println!("{:<4} {:<32} {:<4} {:>7} {:>12} {:>12} {} {}",
            torrent.index, name, state, progress, down, up, peers, seeds);
    }
}

fn print_torrent_detail(detail: &TorrentDetail) {
    let info = &detail.info;
    println!("name:          {}", info.name);
    println!("hash:          {}", info.info_hash);
    println!("state:         {}", info.state);
    println!("save path:     {}", info.save_path);
    println!("progress:      {}", format_progress_bar(info.progress, 40));
    println!("downloaded:    {} ({})", format_bytes(info.total_download), format_rate(info.download_rate));
    println!("uploaded:      {} ({})", format_bytes(info.total_upload), format_rate(info.upload_rate));
    println!("peers:         {} connected / {} total", info.connected_peers, info.total_peers);
    println!("seeds:         {} connected / {} total", info.connected_seeds, info.total_seeds);
    println!("pieces:        {}/{}", info.num_completed_pieces, info.num_pieces);
    println!("added:         {}", format_timestamp(info.added_time));
    if (info.completed_time > 0) {
        println!("completed:     {}", format_timestamp(info.completed_time));
    }
    if (!info.error.is_empty() && info.error != "No error") {
        println!("error:         {}", info.error);
    }

    if (!detail.files.is_empty()) {
        println!("\nfiles:");
        print_files(&detail.files);
    }
    if (!detail.peers.is_empty()) {
        println!("\npeers:");
        print_peers(&detail.peers);
    }
}

fn print_stats(stats: &StatsInfo) {
    println!("torrents:     {} active / {} paused / {} total",
        stats.active_torrents, stats.paused_torrents, stats.num_torrents);
    println!("download:     {}", format_rate(stats.download_rate));
    println!("upload:       {}", format_rate(stats.upload_rate));
    println!("total down:   {}", format_bytes(stats.total_download));
    println!("total up:     {}", format_bytes(stats.total_upload));
    println!("dht nodes:    {}", stats.total_dht_nodes);
    println!("peers:        {}", stats.num_peers);
}

fn print_peers(peers: &[PeerInfo]) {
    println!("{:<24} {:>12} {:>12} {:<22} {:>6}",
        "ip:port", "down", "up", "client", "prog");
    println!("{}", "─".repeat(80));
    for peer in peers {
        println!("{:<24} {:>12} {:>12} {:<22} {:>5.1}%",
            truncate(&format!("{}:{}", peer.ip, peer.port), 23),
            format_rate(peer.download_rate),
            format_rate(peer.upload_rate),
            truncate(&peer.client, 21),
            peer.progress * 100.0,
        );
    }
}

fn print_files(files: &[FileInfo]) {
    println!("{:<4} {:<62} {:>12}", "#", "path", "size");
    println!("{}", "─".repeat(80));
    for file in files {
        println!("{:<4} {:<62} {:>12}",
            file.index,
            truncate(&file.path, 61),
            format_bytes(file.size));
    }
}

fn format_state(state: &str) -> String {
    match state {
        "downloading" => "DL".to_string(),
        "seeding" => "SE".to_string(),
        "finished" => "FN".to_string(),
        "downloading_metadata" => "MD".to_string(),
        "checking_files" => "CK".to_string(),
        "checking_resume_data" => "CR".to_string(),
        "allocating" => "AL".to_string(),
        other => other.chars().take(2).collect(),
    }
}

fn truncate(string: &str, max_chars: usize) -> String {
    let chars: Vec<char> = string.chars().collect();
    if (chars.len() <= max_chars) {
        string.to_string()
    } else {
        format!("{}…", chars[..max_chars.saturating_sub(1)].iter().collect::<String>())
    }
}
