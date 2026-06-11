// project style uses parens in if/while conditions by convention
#![allow(unused_parens)]

mod bridge;
mod categories;
mod client;
mod config;
mod display;
mod ipc;
mod network;
mod server;
mod session;
mod sources;
mod tui;

mod autostart;
mod process;
mod rss;
use anyhow::Result;
use clap::{Parser, Subcommand};
use ipc::Request;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "monsoon",
    version = VERSION,
    about = "torrent client — runs a background daemon, attach with any subcommand"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// connect to a remote daemon over TCP+TLS instead of the local unix socket.
    /// example: --server example.com:6890 --token <hex>
    #[arg(long, global = true)]
    server: Option<String>,
    /// auth token for --server. if omitted, MONSOON_TOKEN env or
    /// ~/.config/monsoon/token is consulted.
    #[arg(long, global = true)]
    token: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// launch the interactive terminal interface (default when no subcommand)
    Tui,

    /// run the daemon in the foreground (for systemd or manual use)
    Daemon {
        /// suppress all log output (used when auto-spawned by cli commands)
        #[arg(long)]
        quiet: bool,
        /// fork into the background, write a pidfile to $XDG_RUNTIME_DIR/monsoon,
        /// and redirect stdout/stderr to $XDG_STATE_HOME/monsoon/daemon.log.
        /// `monsoon stop` / `monsoon kill` still control it from the same binary.
        #[arg(long)]
        detach: bool,
    },

    /// list available network interfaces and their addresses. useful when
    /// configuring listen_interfaces in config.toml.
    Interfaces,

    /// show whether the daemon is running, its pid, socket path, and version
    Status,

    /// send SIGTERM to the running daemon (cleaner than `monsoon stop` if the
    /// ipc socket is wedged)
    Kill,

    /// list all torrents
    #[command(alias = "ls")]
    List,

    /// show detailed info for a torrent
    Info {
        /// index from `monsoon list`
        index: usize,
    },

    /// add a torrent from a magnet URI or .torrent file path
    Add {
        /// magnet URI or path to .torrent file
        uri: String,
        /// override the download directory for this torrent
        #[arg(short, long)]
        save_path: Option<String>,
        /// assign to a configured category (inherits the category's save_path
        /// when --save-path is not given)
        #[arg(short, long)]
        category: Option<String>,
    },

    /// remove a torrent
    Remove {
        /// index from `monsoon list`
        index: usize,
        /// also delete downloaded files from disk
        #[arg(short = 'd', long)]
        delete_files: bool,
    },

    /// pause a torrent
    Pause {
        /// index from `monsoon list`
        index: usize,
    },

    /// resume a paused torrent
    Resume {
        /// index from `monsoon list`
        index: usize,
    },

    /// force a piece hash recheck
    Recheck {
        /// index from `monsoon list`
        index: usize,
    },

    /// show session-wide statistics
    Stats,

    /// show current daemon configuration
    Config,

    /// change a configuration value (takes effect immediately, persists to disk)
    Set {
        key: String,
        value: String,
    },

    /// rename a single file inside a torrent
    Rename {
        /// torrent index from `monsoon list`
        index: usize,
        /// file index from `monsoon info <index>`
        file_index: usize,
        /// new path, relative to the torrent's save_path (subdirs allowed)
        new_name: String,
    },

    /// rename a folder inside a torrent by rewriting every file path prefix
    #[command(name = "rename-folder")]
    RenameFolder {
        /// torrent index from `monsoon list`
        index: usize,
        /// current folder path prefix (e.g. "Show.Name.S01")
        old_prefix: String,
        /// new folder path prefix
        new_prefix: String,
    },

    /// move a torrent's save directory (libtorrent moves the files on disk)
    Move {
        /// torrent index from `monsoon list`
        index: usize,
        /// absolute path to the new save directory
        new_save_path: String,
    },

    /// force a tracker re-announce immediately
    Reannounce {
        /// torrent index from `monsoon list`
        index: usize,
    },

    /// set the download priority for a single file (0 = skip, 4 = normal, 7 = high)
    Priority {
        /// torrent index from `monsoon list`
        index: usize,
        /// file index from `monsoon info <index>`
        file_index: usize,
        /// priority 0..=7
        priority: u8,
    },

    /// print a shareable magnet URI for the torrent
    Magnet {
        /// torrent index from `monsoon list`
        index: usize,
    },

    /// toggle sequential download (front-to-back piece order, good for streaming)
    Sequential {
        /// torrent index from `monsoon list`
        index: usize,
        /// "on" or "off"
        enabled: String,
    },

    /// replace the tag set on a torrent (space-separated). pass no tags to clear.
    Tags {
        /// torrent index from `monsoon list`
        index: usize,
        /// tag names (any number)
        tags: Vec<String>,
    },

    /// re-evaluate ~/.config/monsoon/rules.toml against every torrent and
    /// apply matching add_tags. useful after editing rules or after a magnet
    /// has fetched its metadata.
    Retag,

    /// manage categories (named save-path + auto-tag presets)
    Category {
        #[command(subcommand)]
        action: CategoryAction,
    },

    /// pin a torrent's outgoing connections to a specific network interface
    /// (e.g. tun0 for a vpn). pass empty to clear.
    Bind {
        /// torrent index from `monsoon list`
        index: usize,
        /// interface name or ip. omit to clear the override.
        interface: Option<String>,
    },

    /// manage rss/atom feed subscriptions
    Feed {
        #[command(subcommand)]
        action: FeedAction,
    },

    /// set per-torrent rate limits (bytes/s, 0=unlimited, -1=global)
    Limit {
        /// torrent index from `monsoon list`
        index: usize,
        /// download limit in bytes/sec (0 = unlimited, -1 = inherit global)
        #[arg(long)]
        download: Option<i32>,
        /// upload limit in bytes/sec (0 = unlimited, -1 = inherit global)
        #[arg(long)]
        upload: Option<i32>,
    },

    /// stop the daemon
    Stop,

    /// manage the systemd user service
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Subcommand)]
enum CategoryAction {
    /// list all configured categories with their save paths
    List,
    /// define or update a category (writes ~/.config/monsoon/categories.toml)
    Set {
        name: String,
        /// save directory torrents in this category default to
        save_path: String,
        /// tags auto-applied to torrents in this category (optional, repeatable)
        #[arg(short, long)]
        tag: Vec<String>,
    },
    /// remove a category. torrents previously in it keep their save_path
    /// but lose their category label.
    Remove { name: String },
    /// assign or clear a torrent's category. omit name to clear.
    Assign {
        index: usize,
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum FeedAction {
    /// list all configured feed subscriptions
    List,
    /// add or update a feed subscription
    Add {
        /// rss or atom feed url
        url: String,
        /// regex filter applied to item titles (case-sensitive unless (?i) prefix used)
        #[arg(short, long, default_value = "")]
        filter: String,
        /// assign matched torrents to this category
        #[arg(short, long)]
        category: Option<String>,
        /// save matched torrents here instead of the category/default path
        #[arg(short = 'p', long)]
        save_path: Option<String>,
        /// how often to poll in minutes
        #[arg(short, long, default_value_t = 30)]
        interval: u64,
        /// add matched torrents in paused state
        #[arg(long)]
        paused: bool,
    },
    /// remove a feed by index (from `monsoon feed list`)
    Remove { index: usize },
    /// force an immediate poll of all feeds right now
    Poll,
}

#[derive(Subcommand)]
enum ServiceAction {
    /// install the systemd user service unit file
    Install,
    /// remove the systemd user service unit file
    Uninstall,
    /// show service status via systemctl
    Status,
    /// enable daemon autostart on login
    Enable,
    /// disable daemon autostart on login
    Disable,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Tui) => tui::run(),
        Some(Commands::Daemon { quiet, detach }) => {
            if (detach) {
                server::run_detached(quiet)
            } else {
                server::run(quiet)
            }
        }
        Some(Commands::Status) => print_daemon_status(),
        Some(Commands::Kill) => send_sigterm_to_daemon(),
        Some(Commands::Interfaces) => print_interfaces(),
        Some(Commands::Service { action }) => run_service_command(action),
        Some(command) => {
            let request = command_to_request(command);
            let response = match cli.server {
                Some(server) => {
                    let token = resolve_token(cli.token)?;
                    client::send_network(&server, &token, request)?
                }
                None => client::send(request)?,
            };
            display::print_response(response);
            Ok(())
        }
    }
}

fn print_interfaces() -> Result<()> {
    let mut entries = sources::enumerate_interfaces();
    entries.sort();
    if (entries.is_empty()) {
        println!("(no interfaces detected)");
        return Ok(());
    }
    println!("{:<16} address", "interface");
    println!("{}", "─".repeat(40));
    for (name, address) in entries {
        println!("{:<16} {}", name, address);
    }
    Ok(())
}

fn resolve_token(explicit: Option<String>) -> Result<String> {
    if let Some(token) = explicit {
        return Ok(token);
    }
    if let Ok(token) = std::env::var("MONSOON_TOKEN") {
        if (!token.is_empty()) { return Ok(token); }
    }
    let proj = directories::ProjectDirs::from("com", "monsoon", "monsoon")
        .ok_or_else(|| anyhow::anyhow!("locate project dirs"))?;
    let token_path = proj.config_dir().join("token");
    if (token_path.exists()) {
        let token = std::fs::read_to_string(&token_path)?;
        return Ok(token.trim().to_string());
    }
    anyhow::bail!("no token: pass --token, set MONSOON_TOKEN, or write {}", token_path.display())
}

fn print_daemon_status() -> Result<()> {
    let pid_path = config::Config::pid_path()?;
    let socket_path = config::Config::socket_path()?;
    if (!pid_path.exists()) {
        println!("daemon: not running");
        println!("socket: {}", socket_path.display());
        return Ok(());
    }
    let pid_text = std::fs::read_to_string(&pid_path)?;
    let pid: i32 = pid_text.trim().parse().unwrap_or(0);
    // kill(pid, 0) probes whether the process exists without affecting it
    let alive = libc_kill(pid, 0) == 0;
    if (!alive) {
        println!("daemon: stale pidfile (pid {} not running)", pid);
        let _ = std::fs::remove_file(&pid_path);
    } else {
        println!("daemon: running (pid {})", pid);
        println!("socket: {}", socket_path.display());
        println!("pidfile: {}", pid_path.display());
    }
    Ok(())
}

fn send_sigterm_to_daemon() -> Result<()> {
    let pid_path = config::Config::pid_path()?;
    if (!pid_path.exists()) {
        anyhow::bail!("daemon: not running (no pidfile)");
    }
    let pid: i32 = std::fs::read_to_string(&pid_path)?.trim().parse()
        .map_err(|error| anyhow::anyhow!("bad pidfile: {}", error))?;
    if (libc_kill(pid, 15) != 0) {
        anyhow::bail!("kill {}: {}", pid, std::io::Error::last_os_error());
    }
    // wait for the process to exit so callers (service scripts) don't race
    // against write_pidfile() seeing it still alive
    for _ in 0..300 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if (libc_kill(pid, 0) != 0) { return Ok(()); }
    }
    anyhow::bail!("daemon (pid {}) did not exit within 30s", pid)
}

// signal 0 is a no-op probe (does the pid exist?); 15 is SIGTERM.
// kept off the `libc` crate — one extern fn doesn't justify a dep.
#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

#[cfg(unix)]
fn libc_kill(pid: i32, signal: i32) -> i32 {
    unsafe { kill(pid, signal) }
}

#[cfg(not(unix))]
fn libc_kill(_pid: i32, _signal: i32) -> i32 { -1 }

fn command_to_request(command: Commands) -> Request {
    match command {
        Commands::List => Request::List,
        Commands::Info { index } => Request::Info { index },
        Commands::Add { uri, save_path, category } => Request::Add { uri, save_path, category, start_paused: false },
        Commands::Remove { index, delete_files } => Request::Remove { index, delete_files },
        Commands::Pause { index } => Request::Pause { index },
        Commands::Resume { index } => Request::Resume { index },
        Commands::Recheck { index } => Request::Recheck { index },
        Commands::Stats => Request::Stats,
        Commands::Config => Request::GetConfig,
        Commands::Set { key, value } => Request::SetConfig { key, value },
        Commands::Rename { index, file_index, new_name } => {
            Request::RenameFile { index, file_index, new_name }
        }
        Commands::RenameFolder { index, old_prefix, new_prefix } => {
            Request::RenameFolder { index, old_prefix, new_prefix }
        }
        Commands::Move { index, new_save_path } => Request::Move { index, new_save_path },
        Commands::Reannounce { index } => Request::Reannounce { index },
        Commands::Priority { index, file_index, priority } => {
            Request::SetFilePriority { index, file_index, priority }
        }
        Commands::Magnet { index } => Request::Magnet { index },
        Commands::Sequential { index, enabled } => Request::SetSequential {
            index,
            enabled: matches!(enabled.as_str(), "on" | "true" | "1" | "yes"),
        },
        Commands::Tags { index, tags } => Request::SetTags {
            index,
            tags: tags.into_iter().collect(),
        },
        Commands::Retag => Request::RetagAll,
        Commands::Bind { index, interface } => Request::SetTorrentInterface { index, interface },
        Commands::Category { action } => match action {
            CategoryAction::List => Request::ListCategories,
            CategoryAction::Set { name, save_path, tag } => Request::SetCategoryDefinition {
                name, save_path, add_tags: tag,
            },
            CategoryAction::Remove { name } => Request::RemoveCategory { name },
            CategoryAction::Assign { index, name } => Request::SetCategory { index, name },
        },
        Commands::Feed { action } => match action {
            FeedAction::List => Request::ListFeeds,
            FeedAction::Add { url, filter, category, save_path, interval, paused } => {
                Request::AddFeed {
                    url, filter, category, save_path,
                    poll_interval_minutes: interval,
                    start_paused: paused,
                }
            }
            FeedAction::Remove { index } => Request::RemoveFeed { index },
            FeedAction::Poll => Request::PollFeeds,
        },
        Commands::Limit { index, download, upload } => Request::SetTorrentRateLimit {
            index,
            download: download.unwrap_or(-1),
            upload: upload.unwrap_or(-1),
        },
        Commands::Stop => Request::Shutdown,
        Commands::Daemon { .. }
        | Commands::Service { .. }
        | Commands::Tui
        | Commands::Status
        | Commands::Kill
        | Commands::Interfaces => unreachable!("handled in main"),
    }
}

fn run_service_command(_action: ServiceAction) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("systemd service management is only supported on Linux");
    }
    #[cfg(target_os = "linux")]
    {
        match _action {
            ServiceAction::Install => install_service(),
            ServiceAction::Uninstall => uninstall_service(),
            // systemctl status returns 3 when the unit is inactive; that's not an
            // error from the user's perspective. forward the output as-is.
            ServiceAction::Status => run_systemctl_status(&["--user", "status", "monsoon"]),
            ServiceAction::Enable => run_systemctl(&["--user", "enable", "monsoon"]),
            ServiceAction::Disable => run_systemctl(&["--user", "disable", "monsoon"]),
        }
    }
}

#[cfg(target_os = "linux")]
fn install_service() -> Result<()> {
    use anyhow::Context;


    let binary = std::env::current_exe().context("locate binary path")?;
    let unit_dir = dirs_for_systemd()?;
    std::fs::create_dir_all(&unit_dir).context("create systemd user unit dir")?;
    let unit_path = unit_dir.join("monsoon.service");

    let unit_content = format!(
        "[Unit]\n\
         Description=monsoon torrent daemon\n\
         After=network.target\n\
         \n\
         [Service]\n\
         Type=notify\n\
         NotifyAccess=main\n\
         ExecStart={binary} daemon\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        binary = binary.display(),
    );

    std::fs::write(&unit_path, &unit_content).context("write unit file")?;
    println!("installed: {}", unit_path.display());

    run_systemctl(&["--user", "daemon-reload"])?;
    println!("run `monsoon service enable` to start on login");
    println!("run `monsoon service status` to check status");
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_service() -> Result<()> {
    use anyhow::Context;
    let unit_path = dirs_for_systemd()?.join("monsoon.service");
    let _ = run_systemctl(&["--user", "disable", "--now", "monsoon"]);
    std::fs::remove_file(&unit_path).context("remove unit file")?;
    run_systemctl(&["--user", "daemon-reload"])?;
    println!("service removed");
    Ok(())
}

#[cfg(target_os = "linux")]
fn dirs_for_systemd() -> anyhow::Result<std::path::PathBuf> {
    let base_dirs = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    Ok(base_dirs.config_dir().join("systemd").join("user"))
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> anyhow::Result<()> {
    let status = std::process::Command::new("systemctl")
        .args(args)
        .status()
        .map_err(|error| anyhow::anyhow!("systemctl: {}", error))?;
    if (!status.success()) {
        anyhow::bail!("systemctl exited with {}", status);
    }
    Ok(())
}

/// systemctl status convention: exit 0 = active, 3 = inactive, 4 = no such unit.
/// none of these are errors for our caller — we just want the output shown.
#[cfg(target_os = "linux")]
fn run_systemctl_status(args: &[&str]) -> anyhow::Result<()> {
    let _ = std::process::Command::new("systemctl")
        .args(args)
        .status()
        .map_err(|error| anyhow::anyhow!("systemctl: {}", error))?;
    Ok(())
}
