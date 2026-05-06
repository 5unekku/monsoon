// project style uses parens in if/while conditions by convention
#![allow(unused_parens)]

mod bridge;
mod client;
mod config;
mod display;
mod ipc;
mod server;
mod session;

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
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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

    /// stop the daemon
    Stop,

    /// manage the systemd user service
    Service {
        #[command(subcommand)]
        action: ServiceAction,
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
        Commands::Daemon { quiet } => server::run(quiet),
        Commands::Service { action } => run_service_command(action),
        command => {
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
        Commands::Add { uri, save_path } => Request::Add { uri, save_path },
        Commands::Remove { index, delete_files } => Request::Remove { index, delete_files },
        Commands::Pause { index } => Request::Pause { index },
        Commands::Resume { index } => Request::Resume { index },
        Commands::Recheck { index } => Request::Recheck { index },
        Commands::Stats => Request::Stats,
        Commands::Config => Request::GetConfig,
        Commands::Set { key, value } => Request::SetConfig { key, value },
        Commands::Stop => Request::Shutdown,
        Commands::Daemon { .. } | Commands::Service { .. } => unreachable!(),
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
            ServiceAction::Status => run_systemctl(&["--user", "status", "rustor"]),
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
    let home = std::env::var("HOME").unwrap_or_default();
    Ok(std::path::PathBuf::from(home)
        .join(".config")
        .join("systemd")
        .join("user"))
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
