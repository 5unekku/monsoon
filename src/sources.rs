//! resolve user-supplied torrent sources (magnet uris, URLs, paths) to
//! something the daemon can hand to libtorrent. supports:
//! - magnet:?xt=…                           (passed through)
//! - http://…, https://…                    (fetched via system curl)
//! - ftp://…, sftp://…                      (fetched via system curl)
//! - /absolute/path/to/x.torrent            (linux/macos)
//! - C:\path\to\x.torrent                   (windows, case-insensitive)
//! - ~/foo.torrent or ~user/foo.torrent     (expanded to home dir)

use anyhow::{Context, Result};
use std::path::PathBuf;

pub enum Source {
    /// magnet uri — handed directly to libtorrent
    Magnet(String),
    /// path to a local .torrent file on disk
    File(PathBuf),
}

/// classify and resolve one user input string into a Source. on http/ftp
/// the file is downloaded to a temp path; the caller owns cleanup.
pub fn resolve(input: &str) -> Result<Source> {
    let trimmed = input.trim();
    if (trimmed.is_empty()) {
        anyhow::bail!("empty source");
    }

    // magnet uris are the simplest case
    if (trimmed.starts_with("magnet:")) {
        return Ok(Source::Magnet(trimmed.to_string()));
    }

    // network protocols — shell out to curl which handles all four protocols
    // and TLS validation. fall back to a temp file in std::env::temp_dir().
    if (is_url(trimmed)) {
        let temp = std::env::temp_dir().join(format!(
            "rustor-fetch-{}.torrent",
            std::process::id()
        ));
        let status = std::process::Command::new("curl")
            .args(["-fsSL", "--max-time", "120", "-o"])
            .arg(&temp)
            .arg(trimmed)
            .status()
            .map_err(|error| anyhow::anyhow!("curl: {}", error))?;
        if (!status.success()) {
            let _ = std::fs::remove_file(&temp);
            anyhow::bail!("curl fetch failed (exit {})", status);
        }
        return Ok(Source::File(temp));
    }

    // otherwise treat as a local path. expand ~ first.
    let expanded = expand_tilde(trimmed)?;
    let path = normalise_path(&expanded);
    if (!path.exists()) {
        anyhow::bail!("file not found: {}", path.display());
    }
    Ok(Source::File(path))
}

fn is_url(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("sftp://")
}

/// expand a leading `~` or `~user` to the appropriate home directory.
/// linux: BaseDirs::home_dir(). windows: %USERPROFILE% (BaseDirs also
/// resolves this transparently). a bare `~/` becomes the current user's
/// home; `~someuser/` is left intact on platforms where we can't look up
/// other users without nss bindings.
fn expand_tilde(input: &str) -> Result<String> {
    if (!input.starts_with('~')) {
        return Ok(input.to_string());
    }
    // strip the tilde, find the next path separator
    let rest = &input[1..];
    if (rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        // ~/foo or ~\foo — current user's home
        let home = directories::BaseDirs::new()
            .ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?
            .home_dir()
            .to_path_buf();
        let suffix = rest.trim_start_matches(|c| c == '/' || c == '\\');
        return Ok(home.join(suffix).to_string_lossy().to_string());
    }
    // ~someuser/foo — best-effort: ask getpwnam on unix via /etc/passwd lookup.
    // skip for windows / when /etc/passwd is unreadable; surface the path as-is
    // so the user knows what we tried.
    #[cfg(unix)]
    {
        if let Some(slash) = rest.find(|c| c == '/' || c == '\\') {
            let username = &rest[..slash];
            let suffix = &rest[slash + 1..];
            if let Some(home) = lookup_user_home_via_passwd(username) {
                return Ok(PathBuf::from(home).join(suffix).to_string_lossy().to_string());
            }
        }
    }
    Ok(input.to_string())
}

#[cfg(unix)]
fn lookup_user_home_via_passwd(username: &str) -> Option<String> {
    // small embedded parser to avoid pulling in nix/getpwnam_r. acceptable
    // because /etc/passwd format is rock-stable and lines are tiny.
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if (fields.len() >= 6 && fields[0] == username) {
            return Some(fields[5].to_string());
        }
    }
    None
}

/// normalise a path string. on unix this is mostly a no-op; on windows we
/// uppercase the drive letter so `c:\foo` and `C:\foo` resolve identically.
fn normalise_path(input: &str) -> PathBuf {
    // simple windows drive-letter case fold. example: "c:\foo" → "C:\foo"
    if (input.len() >= 2) {
        let bytes = input.as_bytes();
        let first = bytes[0] as char;
        if (first.is_ascii_alphabetic() && bytes[1] == b':') {
            let mut owned = input.to_string();
            // safe because we already verified the first byte is ascii
            owned.replace_range(0..1, &first.to_ascii_uppercase().to_string());
            return PathBuf::from(owned);
        }
    }
    PathBuf::from(input)
}
