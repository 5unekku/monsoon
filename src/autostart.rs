#![allow(dead_code)]

use anyhow::Result;

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InitSystem {
    Systemd,
    Runit,
    Dinit,
    Xdg,
}

#[cfg(unix)]
impl std::fmt::Display for InitSystem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitSystem::Systemd => write!(formatter, "systemd"),
            InitSystem::Runit => write!(formatter, "runit"),
            InitSystem::Dinit => write!(formatter, "dinit"),
            InitSystem::Xdg => write!(formatter, "xdg"),
        }
    }
}

#[cfg(unix)]
fn parse_init(text: &str) -> Option<InitSystem> {
    match text.trim() {
        "systemd" => Some(InitSystem::Systemd),
        "runit" => Some(InitSystem::Runit),
        "dinit" => Some(InitSystem::Dinit),
        "xdg" => Some(InitSystem::Xdg),
        _ => None,
    }
}

#[cfg(unix)]
fn cache_path() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new()
        .map(|base| base.config_dir().join("monsoon").join("init_system"))
}

/// detects the init system once and caches the result to disk across runs
#[cfg(unix)]
pub fn init_system() -> InitSystem {
    use std::sync::OnceLock;
    static INIT: OnceLock<InitSystem> = OnceLock::new();
    *INIT.get_or_init(|| {
        if let Some(cached) = cache_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| parse_init(&text))
        {
            return cached;
        }
        let init = probe();
        if let Some(path) = cache_path() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(path, init.to_string()).ok();
        }
        init
    })
}

#[cfg(unix)]
fn probe() -> InitSystem {
    let pid1 = std::fs::read_to_string("/proc/1/comm")
        .map(|text| text.trim().to_lowercase())
        .unwrap_or_default();
    match pid1.as_str() {
        "systemd" => probe_systemd(),
        "dinit" => probe_dinit(),
        "runit" => probe_runit(),
        _ => InitSystem::Xdg,
    }
}

#[cfg(unix)]
fn probe_systemd() -> InitSystem {
    let running = std::process::Command::new("systemctl")
        .args(["--user", "show", "--no-pager"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if running { InitSystem::Systemd } else { InitSystem::Xdg }
}

#[cfg(unix)]
fn probe_dinit() -> InitSystem {
    // socket check before spawning a process
    let has_socket = std::env::var("XDG_RUNTIME_DIR")
        .map(|runtime| std::path::Path::new(&runtime).join("dinit").exists())
        .unwrap_or(false);
    if has_socket {
        return InitSystem::Dinit;
    }
    let running = std::process::Command::new("dinitctl")
        .arg("list")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if running { InitSystem::Dinit } else { InitSystem::Xdg }
}

#[cfg(unix)]
fn probe_runit() -> InitSystem {
    let service_dir_exists = directories::BaseDirs::new()
        .map(|base| base.config_dir().join("service").is_dir())
        .unwrap_or(false);
    if !service_dir_exists {
        return InitSystem::Xdg;
    }
    if runsvdir_running_as_self() { InitSystem::Runit } else { InitSystem::Xdg }
}

/// reads effective UID from /proc/self/status to avoid a libc dependency
#[cfg(unix)]
fn self_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("Uid:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|uid| uid.parse().ok())
        })
        .unwrap_or(0)
}

#[cfg(unix)]
fn runsvdir_running_as_self() -> bool {
    let uid = self_uid();
    let Ok(entries) = std::fs::read_dir("/proc") else { return false };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        if !file_name.to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let proc_path = entry.path();
        let Ok(cmdline) = std::fs::read(proc_path.join("cmdline")) else { continue };
        let executable = cmdline.split(|&byte: &u8| byte == 0).next().unwrap_or(&[]);
        if !executable.windows(9).any(|window| window == b"runsvdir") {
            continue;
        }
        let proc_uid: u32 = std::fs::read_to_string(proc_path.join("status"))
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|line| line.starts_with("Uid:"))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|uid| uid.parse().ok())
            })
            .unwrap_or(u32::MAX);
        if proc_uid == uid {
            return true;
        }
    }
    false
}

#[cfg(unix)]
pub fn is_enabled() -> bool {
    let Some(base) = directories::BaseDirs::new() else { return false };
    match init_system() {
        InitSystem::Systemd => base
            .config_dir()
            .join("systemd/user/default.target.wants/monsoon.service")
            .exists(),
        InitSystem::Runit => base.config_dir().join("service/monsoon").exists(),
        InitSystem::Dinit => base.config_dir().join("dinit.d/boot.d/monsoon").exists(),
        InitSystem::Xdg => base.config_dir().join("autostart/monsoon.desktop").exists(),
    }
}

#[cfg(unix)]
pub fn enable() -> Result<()> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let exe = std::env::current_exe()?;
    match init_system() {
        InitSystem::Systemd => {
            let service_dir = base.config_dir().join("systemd/user");
            std::fs::create_dir_all(&service_dir)?;
            std::fs::write(
                service_dir.join("monsoon.service"),
                format!(
                    "[Unit]\nDescription=monsoon daemon\nAfter=network.target\n\n\
                     [Service]\nExecStart={} daemon --quiet\nRestart=on-failure\n\n\
                     [Install]\nWantedBy=default.target\n",
                    exe.display()
                ),
            )?;
            std::process::Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status()?;
            std::process::Command::new("systemctl")
                .args(["--user", "enable", "--now", "monsoon"])
                .status()?;
        }
        InitSystem::Runit => {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let sv_dir = base.data_local_dir().join("sv/monsoon");
            std::fs::create_dir_all(&sv_dir)?;
            let run_script = sv_dir.join("run");
            std::fs::write(
                &run_script,
                format!("#!/bin/sh\nexec {} daemon --quiet 2>&1\n", exe.display()),
            )?;
            std::fs::set_permissions(&run_script, std::fs::Permissions::from_mode(0o755))?;
            let link = base.config_dir().join("service/monsoon");
            if !link.exists() {
                symlink(&sv_dir, &link)?;
            }
        }
        InitSystem::Dinit => {
            let dinit_dir = base.config_dir().join("dinit.d");
            std::fs::create_dir_all(&dinit_dir)?;
            std::fs::write(
                dinit_dir.join("monsoon"),
                format!(
                    "type = process\ncommand = {} daemon --quiet\nrestart = true\nlogfile = /dev/null\n",
                    exe.display()
                ),
            )?;
            std::process::Command::new("dinitctl")
                .args(["enable", "monsoon"])
                .status()?;
            std::process::Command::new("dinitctl")
                .args(["start", "monsoon"])
                .status()?;
        }
        InitSystem::Xdg => {
            let autostart_dir = base.config_dir().join("autostart");
            std::fs::create_dir_all(&autostart_dir)?;
            std::fs::write(
                autostart_dir.join("monsoon.desktop"),
                format!(
                    "[Desktop Entry]\nType=Application\nName=monsoon\n\
                     Exec={} daemon --quiet\nHidden=false\nNoDisplay=false\n\
                     X-GNOME-Autostart-enabled=true\n",
                    exe.display()
                ),
            )?;
        }
    }
    Ok(())
}

#[cfg(unix)]
pub fn disable() -> Result<()> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    match init_system() {
        InitSystem::Systemd => {
            std::process::Command::new("systemctl")
                .args(["--user", "disable", "--now", "monsoon"])
                .status()?;
            let service_file = base.config_dir().join("systemd/user/monsoon.service");
            if service_file.exists() {
                std::fs::remove_file(&service_file)?;
            }
            std::process::Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status()?;
        }
        InitSystem::Runit => {
            let link = base.config_dir().join("service/monsoon");
            if link.exists() {
                std::fs::remove_file(&link)?;
            }
            let sv_dir = base.data_local_dir().join("sv/monsoon");
            if sv_dir.exists() {
                std::fs::remove_dir_all(sv_dir)?;
            }
        }
        InitSystem::Dinit => {
            // stop before disable; ignore error if already stopped
            std::process::Command::new("dinitctl")
                .args(["stop", "monsoon"])
                .status()
                .ok();
            std::process::Command::new("dinitctl")
                .args(["disable", "monsoon"])
                .status()?;
            let service_file = base.config_dir().join("dinit.d/monsoon");
            if service_file.exists() {
                std::fs::remove_file(service_file)?;
            }
        }
        InitSystem::Xdg => {
            let desktop_file = base.config_dir().join("autostart/monsoon.desktop");
            if desktop_file.exists() {
                std::fs::remove_file(desktop_file)?;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn is_enabled() -> bool {
    use winreg::RegKey;
    use winreg::enums::*;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
        .and_then(|run| run.get_value::<String, _>("monsoon"))
        .is_ok()
}

#[cfg(windows)]
pub fn enable() -> Result<()> {
    use winreg::RegKey;
    use winreg::enums::*;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu.open_subkey_with_flags(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        KEY_SET_VALUE,
    )?;
    let exe = std::env::current_exe()?;
    run.set_value("monsoon", &format!("\"{}\" daemon --quiet", exe.display()))?;
    Ok(())
}

#[cfg(windows)]
pub fn disable() -> Result<()> {
    use winreg::RegKey;
    use winreg::enums::*;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu.open_subkey_with_flags(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        KEY_SET_VALUE,
    )?;
    let _ = run.delete_value("monsoon");
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn is_enabled() -> bool { false }

#[cfg(not(any(unix, windows)))]
pub fn enable() -> Result<()> { Ok(()) }

#[cfg(not(any(unix, windows)))]
pub fn disable() -> Result<()> { Ok(()) }
