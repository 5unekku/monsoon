use crate::ipc::{
    CategoryInfo, FileInfo, PeerInfo, Response, StatsInfo, TorrentDetail, TorrentInfo, TrackerInfo,
};
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
        Response::RenameResult { renamed, rejected } => print_rename_result(&renamed, &rejected),
        Response::Magnet(uri) => {
            if (uri.is_empty()) {
                eprintln!("magnet: torrent invalid or metadata not yet downloaded");
            } else {
                println!("{}", uri);
            }
        }
        Response::Categories(entries) => print_categories(&entries),
        Response::Ok => {}
        Response::Err(message) => eprintln!("error: {}", message),
    }
}

fn print_rename_result(renamed: &[usize], rejected: &[(usize, String)]) {
    if (!rejected.is_empty()) {
        eprintln!("rename rejected — no files were renamed:");
        for (file_index, reason) in rejected {
            eprintln!("  file {}: {}", file_index, reason);
        }
        return;
    }
    if (renamed.is_empty()) {
        println!("nothing to rename");
        return;
    }
    println!("submitted rename for {} file(s):", renamed.len());
    for file_index in renamed {
        println!("  file {}", file_index);
    }
    println!("(libtorrent applies renames asynchronously; check daemon logs for final outcome)");
}

fn print_torrent_list(list: &[TorrentInfo]) {
    if (list.is_empty()) {
        println!("no torrents");
        return;
    }
    println!("{:<5} {:<32} {:<20} {:>8} {:>12} {:>12} {:<10} seeds",
        "index", "name", "status", "progress", "down", "up", "peers");
    println!("{}", "─".repeat(110));
    for torrent in list {
        let name = truncate(&torrent.name.chars().collect::<String>(), 31);
        let state = format_state(&torrent.state, torrent.is_paused);
        let progress = format!("{:.1}%", torrent.progress * 100.0);
        let down = format_rate(torrent.download_rate);
        let up = format_rate(torrent.upload_rate);
        let peers = format!("{}/{}", torrent.connected_peers, torrent.total_peers);
        let seeds = format!("{}/{}", torrent.connected_seeds, torrent.total_seeds);
        println!("{:<5} {:<32} {:<20} {:>8} {:>12} {:>12} {:<10} {}",
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
    if (!detail.trackers.is_empty()) {
        println!("\ntrackers:");
        print_trackers(&detail.trackers);
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
    println!("{:<24} {:>12} {:>12} {:<22} {:>8}",
        "address", "down", "up", "client", "progress");
    println!("{}", "─".repeat(80));
    for peer in peers {
        println!("{:<24} {:>12} {:>12} {:<22} {:>7.1}%",
            truncate(&format!("{}:{}", peer.ip, peer.port), 23),
            format_rate(peer.download_rate),
            format_rate(peer.upload_rate),
            truncate(&peer.client, 21),
            peer.progress * 100.0,
        );
    }
}

fn print_files(files: &[FileInfo]) {
    println!("{:<5} {:<48} {:>12} {:>8} {:>8}",
        "index", "path", "size", "progress", "priority");
    println!("{}", "─".repeat(85));
    for file in files {
        println!("{:<5} {:<48} {:>12} {:>7.1}% {:>8}",
            file.index,
            truncate(&file.path, 47),
            format_bytes(file.size),
            file.progress * 100.0,
            file.priority);
    }
}

fn print_trackers(trackers: &[TrackerInfo]) {
    println!("{:<4} {:<60} {:>5} {:>5}", "tier", "url", "fails", "state");
    println!("{}", "─".repeat(80));
    for tracker in trackers {
        let state = if (tracker.updating) {
            "upd"
        } else if (tracker.fails > 0) {
            "err"
        } else {
            "ok"
        };
        println!("{:<4} {:<60} {:>5} {:>5}",
            tracker.tier,
            truncate(&tracker.url, 59),
            tracker.fails,
            state);
        if (!tracker.message.is_empty()) {
            println!("     {}", tracker.message);
        }
    }
}

/// human-readable state name. natural english, no abbreviations.
fn format_state(state: &str, is_paused: bool) -> String {
    if (is_paused) { return "paused".to_string(); }
    match state {
        "downloading" => "downloading".to_string(),
        "seeding" => "seeding".to_string(),
        "finished" => "finished".to_string(),
        "downloading_metadata" => "downloading metadata".to_string(),
        "checking_files" => "checking files".to_string(),
        "checking_resume_data" => "checking resume".to_string(),
        "allocating" => "allocating".to_string(),
        other => other.replace('_', " "),
    }
}

fn print_categories(entries: &[CategoryInfo]) {
    if (entries.is_empty()) {
        println!("no categories configured (use `monsoon category set <name> <path>`)");
        return;
    }
    println!("{:<16} {:<48} {:>6} tags", "name", "save path", "count");
    println!("{}", "─".repeat(80));
    for entry in entries {
        println!("{:<16} {:<48} {:>6} {}",
            truncate(&entry.name, 15),
            truncate(&entry.save_path, 47),
            entry.torrent_count,
            entry.add_tags.join(","));
    }
}

/// like [`truncate`] but PADS the result to exactly `max_cells` if the
/// input is shorter. used by the tui torrent list header so column labels
/// fill the column slot rather than ending early at the cell boundary.
/// display width in cells of a string (CJK / emoji / combining mark aware).
pub fn display_width(string: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(string)
}

pub fn truncate_to_width(string: &str, max_cells: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let truncated = truncate(string, max_cells);
    let cells = UnicodeWidthStr::width(truncated.as_str());
    if (cells < max_cells) {
        let pad = max_cells - cells;
        format!("{}{}", truncated, " ".repeat(pad))
    } else {
        truncated
    }
}

/// truncate to a display-width budget (cells, not chars). CJK / emoji /
/// combining marks are accounted for via unicode-width so columns don't
/// shift when the data contains wide characters. an ellipsis (1 cell) is
/// appended when truncation actually happened.
fn truncate(string: &str, max_cells: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let total: usize = string.chars().map(|c| UnicodeWidthChar::width(c).unwrap_or(0)).sum();
    if (total <= max_cells) { return string.to_string(); }
    let budget = max_cells.saturating_sub(1); // reserve one cell for the ellipsis
    let mut accumulated = 0usize;
    let mut output = String::new();
    for character in string.chars() {
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if (accumulated + width > budget) { break; }
        output.push(character);
        accumulated += width;
    }
    output.push('…');
    output
}
