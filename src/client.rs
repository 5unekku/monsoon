use crate::config::Config;
use crate::ipc::{Request, Response};
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

const SPAWN_WAIT_MS: u64 = 50;
const SPAWN_MAX_RETRIES: u32 = 40; // 2 second total

/// send a request to the daemon, auto-spawning it silently if not running
pub fn send(request: Request) -> Result<Response> {
    let socket_path = Config::socket_path()?;

    // try to connect; if it fails, spawn the daemon quietly and retry
    if (!socket_path.exists()) {
        spawn_daemon_quiet()?;
        wait_for_socket(&socket_path)?;
    }

    let mut stream = UnixStream::connect(&socket_path).context(
        "connect to daemon — try: rustor daemon"
    )?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;

    let json = serde_json::to_string(&request).context("serialize request")?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).context("read response")?;

    serde_json::from_str(line.trim()).context("parse response")
}

fn spawn_daemon_quiet() -> Result<()> {
    let binary = std::env::current_exe().context("locate current binary")?;
    // new process group so the child survives terminal close
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        std::process::Command::new(&binary)
            .args(["daemon", "--quiet"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .context("spawn daemon")?;
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new(&binary)
            .args(["daemon", "--quiet"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("spawn daemon")?;
    }
    Ok(())
}

fn wait_for_socket(socket_path: &Path) -> Result<()> {
    for _ in 0..SPAWN_MAX_RETRIES {
        std::thread::sleep(Duration::from_millis(SPAWN_WAIT_MS));
        if (socket_path.exists()) { return Ok(()); }
    }
    Err(anyhow::anyhow!(
        "daemon did not start within {}ms",
        SPAWN_WAIT_MS * SPAWN_MAX_RETRIES as u64
    ))
}
