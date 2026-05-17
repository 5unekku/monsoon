// project style uses parens in if/while conditions by convention
#![allow(unused_parens)]

mod bridge;
mod categories;
mod client;
mod config;
mod display;
mod ipc;
mod server;
mod session;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use ipc::Request;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "rustor",
    version = VERSION,
    about = "torrent client — runs a background daemon, attach with any subcommand"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
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
    },

    /// list all torrents
    #[command(alias = "ls")]
    List,

    /// show detailed info for a torrent
    Info {
        /// index from `rustor list`
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
        /// index from `rustor list`
        index: usize,
        /// also delete downloaded files from disk
        #[arg(short = 'd', long)]
        delete_files: bool,
    },

    /// pause a torrent
    Pause {
        /// index from `rustor list`
        index: usize,
    },

    /// resume a paused torrent
    Resume {
        /// index from `rustor list`
        index: usize,
    },

    /// force a piece hash recheck
    Recheck {
        /// index from `rustor list`
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
        /// torrent index from `rustor list`
        index: usize,
        /// file index from `rustor info <index>`
        file_index: usize,
        /// new path, relative to the torrent's save_path (subdirs allowed)
        new_name: String,
    },

    /// rename a folder inside a torrent by rewriting every file path prefix
    #[command(name = "rename-folder")]
    RenameFolder {
        /// torrent index from `rustor list`
        index: usize,
        /// current folder path prefix (e.g. "Show.Name.S01")
        old_prefix: String,
        /// new folder path prefix
        new_prefix: String,
    },

    /// move a torrent's save directory (libtorrent moves the files on disk)
    Move {
        /// torrent index from `rustor list`
        index: usize,
        /// absolute path to the new save directory
        new_save_path: String,
    },

    /// force a tracker re-announce immediately
    Reannounce {
        /// torrent index from `rustor list`
        index: usize,
    },

    /// set the download priority for a single file (0 = skip, 4 = normal, 7 = high)
    Priority {
        /// torrent index from `rustor list`
        index: usize,
        /// file index from `rustor info <index>`
        file_index: usize,
        /// priority 0..=7
        priority: u8,
    },

    /// print a shareable magnet URI for the torrent
    Magnet {
        /// torrent index from `rustor list`
        index: usize,
    },

    /// toggle sequential download (front-to-back piece order, good for streaming)
    Sequential {
        /// torrent index from `rustor list`
        index: usize,
        /// "on" or "off"
        enabled: String,
    },

    /// replace the tag set on a torrent (space-separated). pass no tags to clear.
    Tags {
        /// torrent index from `rustor list`
        index: usize,
        /// tag names (any number)
        tags: Vec<String>,
    },

    /// manage categories (named save-path + auto-tag presets)
    Category {
        #[command(subcommand)]
        action: CategoryAction,
    },

    /// pin a torrent's outgoing connections to a specific network interface
    /// (e.g. tun0 for a vpn). pass empty to clear.
    Bind {
        /// torrent index from `rustor list`
        index: usize,
        /// interface name or ip. omit to clear the override.
        interface: Option<String>,
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
    /// define or update a category (writes ~/.config/rustor/categories.toml)
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
        Some(Commands::Daemon { quiet }) => server::run(quiet),
        Some(Commands::Service { action }) => run_service_command(action),
        Some(command) => {
            let request = command_to_request(command);
            let response = client::send(request)?;
            display::print_response(response);
            Ok(())
        }
    }
}

fn command_to_request(command: Commands) -> Request {
    match command {
        Commands::List => Request::List,
        Commands::Info { index } => Request::Info { index },
        Commands::Add { uri, save_path, category } => Request::Add { uri, save_path, category },
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
        Commands::Bind { index, interface } => Request::SetTorrentInterface { index, interface },
        Commands::Category { action } => match action {
            CategoryAction::List => Request::ListCategories,
            CategoryAction::Set { name, save_path, tag } => Request::SetCategoryDefinition {
                name, save_path, add_tags: tag,
            },
            CategoryAction::Remove { name } => Request::RemoveCategory { name },
            CategoryAction::Assign { index, name } => Request::SetCategory { index, name },
        },
        Commands::Stop => Request::Shutdown,
        Commands::Daemon { .. } | Commands::Service { .. } | Commands::Tui => unreachable!(),
    }
}

fn run_service_command(action: ServiceAction) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("systemd service management is only supported on Linux");
    }
    #[cfg(target_os = "linux")]
    {
        match action {
            ServiceAction::Install => install_service(),
            ServiceAction::Uninstall => uninstall_service(),
            // systemctl status returns 3 when the unit is inactive; that's not an
            // error from the user's perspective. forward the output as-is.
            ServiceAction::Status => run_systemctl_status(&["--user", "status", "rustor"]),
            ServiceAction::Enable => run_systemctl(&["--user", "enable", "rustor"]),
            ServiceAction::Disable => run_systemctl(&["--user", "disable", "rustor"]),
        }
    }
}

#[cfg(target_os = "linux")]
fn install_service() -> Result<()> {
    use anyhow::Context;


    let binary = std::env::current_exe().context("locate binary path")?;
    let unit_dir = dirs_for_systemd()?;
    std::fs::create_dir_all(&unit_dir).context("create systemd user unit dir")?;
    let unit_path = unit_dir.join("rustor.service");

    let unit_content = format!(
        "[Unit]\n\
         Description=rustor torrent daemon\n\
         After=network.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
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
    println!("run `rustor service enable` to start on login");
    println!("run `rustor service status` to check status");
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_service() -> Result<()> {
    use anyhow::Context;
    let unit_path = dirs_for_systemd()?.join("rustor.service");
    let _ = run_systemctl(&["--user", "disable", "--now", "rustor"]);
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
