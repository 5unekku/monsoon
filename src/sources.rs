//! resolve user-supplied torrent sources (magnet uris, URLs, paths) to
//! something the daemon can hand to libtorrent. supports:
//! - magnet:?xt=…                           (passed through)
//! - http://…, https://…                    (fetched via libcurl)
//! - ftp://…, sftp://…                      (fetched via libcurl)
//! - /absolute/path/to/x.torrent            (linux/macos)
//! - C:\path\to\x.torrent                   (windows, case-insensitive)
//! - ~/foo.torrent or ~user/foo.torrent     (expanded to home dir)
//!
//! also exposes network-interface enumeration so listen_interfaces in
//! config.toml can be specified by interface name (e.g. "tun0") instead
//! of by raw ip.

use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;

/// enumerate available network interfaces. cross-platform: uses
/// getifaddrs() on unix and GetAdaptersAddresses on windows via the
/// if-addrs crate. returned tuples are (interface_name, ip_string).
pub fn enumerate_interfaces() -> Vec<(String, String)> {
    if_addrs::get_if_addrs()
        .map(|interfaces| {
            interfaces.into_iter()
                .map(|interface| (interface.name.clone(), interface.addr.ip().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// resolve a list of interface-name-or-ip entries into ip strings ready
/// to be joined for libtorrent's `listen_interfaces` setting. unknown
/// names are dropped with a log line. when the resolution would be empty
/// (no entries, or no matches) we fall back to `["0.0.0.0"]`.
pub fn resolve_listen_ips(entries: &[String]) -> Vec<String> {
    if (entries.is_empty()) { return vec!["0.0.0.0".to_string()]; }
    let interfaces = enumerate_interfaces();
    let mut resolved: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.trim();
        if (entry.is_empty()) { continue; }
        if (entry.parse::<std::net::IpAddr>().is_ok()) {
            resolved.push(entry.to_string());
            continue;
        }
        let matches: Vec<&String> = interfaces.iter()
            .filter(|(name, _)| name == entry)
            .map(|(_, ip)| ip)
            .collect();
        if (matches.is_empty()) {
            tracing::warn!(entry, "interface not found; skipping");
            continue;
        }
        for ip in matches { resolved.push(ip.clone()); }
    }
    if (resolved.is_empty()) {
        vec!["0.0.0.0".to_string()]
    } else {
        resolved
    }
}

pub enum Source {
    /// magnet uri — handed directly to libtorrent
    Magnet(String),
    /// path to a local .torrent file on disk
    File(PathBuf),
}

/// what one input line will be treated as. shared by the tui's live
/// indicator, the tui's submit-time expansion, and the daemon's resolve(),
/// so all three always agree.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceKind {
    /// magnet uri, passed through untouched
    Magnet,
    /// http/https/ftp/sftp, fetched daemon-side via libcurl
    Url,
    /// tilde-expanded, normalised local path (may or may not exist)
    LocalPath(PathBuf),
    /// tilde-expanded pattern containing glob metacharacters
    LocalGlob(String),
}

/// classify one input line the same way resolve() will treat it. returns
/// None for empty (after trim) input. the literal-path-exists check runs
/// before glob detection so bracket-laden real filenames ("[Group] Show")
/// are never reinterpreted as character classes.
pub fn classify(input: &str) -> Option<SourceKind> {
    let trimmed = input.trim();
    if (trimmed.is_empty()) { return None; }
    if (trimmed.starts_with("magnet:")) { return Some(SourceKind::Magnet); }
    if (is_url(trimmed)) { return Some(SourceKind::Url); }
    // tilde expansion failure (no resolvable home dir) falls back to the raw
    // input; the path simply won't exist and surfaces as not-found
    let expanded = expand_tilde(trimmed).unwrap_or_else(|_| trimmed.to_string());
    let path = normalise_path(&expanded);
    if (path.exists()) { return Some(SourceKind::LocalPath(path)); }
    if (expanded.chars().any(|character| matches!(character, '*' | '?' | '['))) {
        return Some(SourceKind::LocalGlob(expanded));
    }
    Some(SourceKind::LocalPath(path))
}

/// expand a glob pattern against the local filesystem. matches come back in
/// the glob crate's deterministic per-directory alphabetical order.
/// unreadable entries yielded by the walk are skipped; they would fail at
/// add time anyway. directory matches are not filtered out on purpose: the
/// daemon rejects them with a real error the results overlay can show.
pub fn expand_glob(pattern: &str) -> Result<Vec<PathBuf>, String> {
    let paths = glob::glob(pattern)
        .map_err(|error| format!("bad glob pattern: {}", error))?;
    Ok(paths.filter_map(|entry| entry.ok()).collect())
}

/// classify and resolve one user input string into a Source. on http/ftp
/// the file is downloaded to a temp path; the caller owns cleanup. glob
/// patterns are the client's job to expand and are rejected here.
pub fn resolve(input: &str) -> Result<Source> {
    match classify(input) {
        None => anyhow::bail!("empty source"),
        Some(SourceKind::Magnet) => Ok(Source::Magnet(input.trim().to_string())),
        Some(SourceKind::Url) => {
            let temp = std::env::temp_dir().join(format!(
                "monsoon-fetch-{}.torrent",
                std::process::id()
            ));
            fetch_url(&temp, input.trim())
                .inspect_err(|_| { let _ = std::fs::remove_file(&temp); })?;
            Ok(Source::File(temp))
        }
        Some(SourceKind::LocalPath(path)) => {
            if (!path.exists()) {
                anyhow::bail!("file not found: {}", path.display());
            }
            Ok(Source::File(path))
        }
        Some(SourceKind::LocalGlob(_)) => {
            anyhow::bail!("glob patterns must be expanded by the client")
        }
    }
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
        let suffix = rest.trim_start_matches(['/', '\\']);
        return Ok(home.join(suffix).to_string_lossy().to_string());
    }
    // ~someuser/foo — best-effort: ask getpwnam on unix via /etc/passwd lookup.
    // skip for windows / when /etc/passwd is unreadable; surface the path as-is
    // so the user knows what we tried.
    #[cfg(unix)]
    {
        if let Some(slash) = rest.find(['/', '\\']) {
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

/// fetch a url into memory and return the body as a String. follows redirects,
/// enforces a 60s timeout. intended for small text payloads (RSS feeds, etc.).
pub fn fetch_to_string(url: &str) -> Result<String> {
    use curl::easy::Easy;
    use std::time::Duration;
    let mut body: Vec<u8> = Vec::new();
    let mut easy = Easy::new();
    easy.url(url).map_err(|e| anyhow::anyhow!("curl url: {}", e))?;
    easy.follow_location(true).map_err(|e| anyhow::anyhow!("{}", e))?;
    easy.max_redirections(10).map_err(|e| anyhow::anyhow!("{}", e))?;
    easy.connect_timeout(Duration::from_secs(30)).map_err(|e| anyhow::anyhow!("{}", e))?;
    easy.timeout(Duration::from_secs(60)).map_err(|e| anyhow::anyhow!("{}", e))?;
    easy.fail_on_error(true).map_err(|e| anyhow::anyhow!("{}", e))?;
    {
        let mut transfer = easy.transfer();
        transfer.write_function(|data| { body.extend_from_slice(data); Ok(data.len()) })
            .map_err(|e| anyhow::anyhow!("curl write: {}", e))?;
        transfer.perform().map_err(|e| anyhow::anyhow!("curl: {}", e))?;
    }
    String::from_utf8(body).map_err(|_| anyhow::anyhow!("response is not valid utf-8"))
}

/// download a url to a local file via libcurl. follows redirects, enforces a
/// 120s timeout, and fails with a structured error on non-2xx responses.
/// supports http, https, ftp, and sftp (same protocols curl supports).
pub fn fetch_url(dest: &std::path::Path, url: &str) -> Result<()> {
    use curl::easy::Easy;
    use std::time::Duration;
    let file = std::fs::File::create(dest)
        .map_err(|error| anyhow::anyhow!("create temp file: {}", error))?;
    let mut file = std::io::BufWriter::new(file);
    let mut easy = Easy::new();
    easy.url(url).map_err(|error| anyhow::anyhow!("curl url: {}", error))?;
    easy.follow_location(true).map_err(|error| anyhow::anyhow!("curl follow_location: {}", error))?;
    easy.max_redirections(10).map_err(|error| anyhow::anyhow!("curl max_redirections: {}", error))?;
    easy.connect_timeout(Duration::from_secs(30)).map_err(|error| anyhow::anyhow!("curl connect_timeout: {}", error))?;
    easy.timeout(Duration::from_secs(120)).map_err(|error| anyhow::anyhow!("curl timeout: {}", error))?;
    easy.fail_on_error(true).map_err(|error| anyhow::anyhow!("curl fail_on_error: {}", error))?;
    {
        let mut transfer = easy.transfer();
        transfer.write_function(|data| {
            file.write_all(data)
                .map(|_| data.len())
                .map_err(|_| curl::easy::WriteError::Pause)
        }).map_err(|error| anyhow::anyhow!("curl write_function: {}", error))?;
        transfer.perform().map_err(|error| anyhow::anyhow!("curl: {}", error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{classify, expand_glob, SourceKind};
    use std::path::PathBuf;

    /// tiny scoped tempdir, removed on drop. avoids a tempfile dev-dependency
    /// for what a ten-line Drop impl covers.
    pub struct TestDir(pub PathBuf);

    impl TestDir {
        pub fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("monsoon-sources-test-{}-{}", label, std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).unwrap(); }
            std::fs::write(&path, b"x").unwrap();
            path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn empty_and_whitespace_classify_as_none() {
        assert_eq!(classify(""), None);
        assert_eq!(classify("   "), None);
    }

    #[test]
    fn magnet_uris_classify_as_magnet() {
        assert_eq!(classify("magnet:?xt=urn:btih:abcdef"), Some(SourceKind::Magnet));
    }

    #[test]
    fn each_url_scheme_classifies_as_url_case_insensitively() {
        for input in [
            "http://example.com/a.torrent",
            "https://example.com/a.torrent",
            "ftp://example.com/a.torrent",
            "sftp://example.com/a.torrent",
            "HTTPS://EXAMPLE.COM/A.TORRENT",
        ] {
            assert_eq!(classify(input), Some(SourceKind::Url), "input: {}", input);
        }
    }

    #[test]
    fn tilde_paths_expand_to_home() {
        match classify("~/monsoon-classify-test.torrent") {
            Some(SourceKind::LocalPath(path)) => {
                assert!(
                    !path.to_string_lossy().starts_with('~'),
                    "tilde not expanded: {}",
                    path.display()
                );
            }
            other => panic!("expected LocalPath, got {:?}", other),
        }
    }

    #[test]
    fn windows_drive_paths_classify_as_local_path_with_folded_drive_letter() {
        match classify(r"c:\downloads\a.torrent") {
            Some(SourceKind::LocalPath(path)) => {
                assert!(path.to_string_lossy().starts_with('C'));
            }
            other => panic!("expected LocalPath, got {:?}", other),
        }
    }

    #[test]
    fn existing_literal_path_with_brackets_beats_glob_interpretation() {
        let dir = TestDir::new("brackets");
        let real = dir.file("[Group] Show.torrent");
        assert_eq!(
            classify(real.to_str().unwrap()),
            Some(SourceKind::LocalPath(real.clone()))
        );
    }

    #[test]
    fn nonexistent_path_with_metacharacters_classifies_as_glob() {
        let pattern = "/monsoon-does-not-exist/*.torrent";
        assert_eq!(classify(pattern), Some(SourceKind::LocalGlob(pattern.to_string())));
    }

    #[test]
    fn nonexistent_plain_path_stays_local_path() {
        let path = "/monsoon-does-not-exist/a.torrent";
        assert_eq!(classify(path), Some(SourceKind::LocalPath(PathBuf::from(path))));
    }

    #[test]
    fn expand_glob_returns_alphabetical_matches() {
        let dir = TestDir::new("expand-order");
        dir.file("b.torrent");
        dir.file("a.torrent");
        dir.file("notes.txt");
        let pattern = format!("{}/*.torrent", dir.0.display());
        let matches = expand_glob(&pattern).unwrap();
        assert_eq!(matches, vec![dir.0.join("a.torrent"), dir.0.join("b.torrent")]);
    }

    #[test]
    fn expand_glob_zero_matches_is_ok_and_empty() {
        let dir = TestDir::new("expand-empty");
        dir.file("a.torrent");
        let pattern = format!("{}/*.nope", dir.0.display());
        assert_eq!(expand_glob(&pattern).unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn expand_glob_recursive_double_star_descends() {
        let dir = TestDir::new("expand-recursive");
        dir.file("sub/inner/c.torrent");
        let pattern = format!("{}/**/c.torrent", dir.0.display());
        let matches = expand_glob(&pattern).unwrap();
        assert!(matches.contains(&dir.0.join("sub/inner/c.torrent")));
    }

    #[test]
    fn expand_glob_bad_pattern_is_a_clear_error() {
        // unclosed character class is a pattern error, not a silent empty
        assert!(expand_glob("/tmp/[").is_err());
    }
}
