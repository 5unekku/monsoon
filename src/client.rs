use crate::config::Config;
use crate::ipc::{Request, Response};
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const SPAWN_WAIT_MS: u64 = 50;
const SPAWN_MAX_RETRIES: u32 = 40; // 2 second total

/// Check if the local daemon is already running and actively accepting connections
#[allow(dead_code)]
pub fn is_local_daemon_running() -> bool {
    if let Ok(socket_path) = Config::socket_path() {
        UnixStream::connect(&socket_path).is_ok()
    } else {
        false
    }
}

/// send a request to the daemon, auto-spawning it silently if not running
pub fn send(request: Request) -> Result<Response> {
    let socket_path = Config::socket_path()?;

    // try to connect; if it fails, spawn the daemon quietly and retry
    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => {
            if socket_path.exists() {
                // remove stale socket path from a previous crash before spawning
                let _ = std::fs::remove_file(&socket_path);
            }
            spawn_daemon_quiet()?;
            wait_for_socket(&socket_path)?;
            UnixStream::connect(&socket_path).context("connect to self-spawned daemon")?
        }
    };

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

/// connect to a remote daemon over TCP+TLS, authenticate, and send one request.
/// the server's self-signed cert is accepted without verification — this is
/// safe over an already-trusted transport (vpn, ssh tunnel) and would need
/// fingerprint pinning before being used over the open internet.
pub fn send_network(server: &str, token: &str, request: Request) -> Result<Response> {
    let server_name = server.split(':').next().unwrap_or("localhost");
    let host_name = rustls::pki_types::ServerName::try_from(server_name.to_string())
        .map_err(|error| anyhow::anyhow!("server name: {}", error))?;

    let tls_config = rustls_client_config_insecure();
    let mut connection = rustls::ClientConnection::new(Arc::new(tls_config), host_name)
        .context("client tls")?;
    let mut tcp = TcpStream::connect(server).with_context(|| format!("tcp connect {}", server))?;
    tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut stream = rustls::Stream::new(&mut connection, &mut tcp);

    // auth + request, both as separate \n-terminated lines
    stream.write_all(format!("AUTH {}\n", token).as_bytes())?;
    let json = serde_json::to_string(&request).context("serialize request")?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).context("read network response")?;
    serde_json::from_str(line.trim()).context("parse response")
}

/// rustls client config that trusts ANY server cert. acceptable for the
/// self-signed model when the transport (ssh tunnel / vpn) is already trusted.
/// FUTURE: pin the server cert by SPKI hash once we surface it through the
/// daemon's `monsoon status` so users can copy it explicitly.
fn rustls_client_config_insecure() -> rustls::ClientConfig {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};

    #[derive(Debug)]
    struct AcceptAll;
    impl ServerCertVerifier for AcceptAll {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> std::result::Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                SignatureScheme::RSA_PKCS1_SHA256,
                SignatureScheme::RSA_PKCS1_SHA384,
                SignatureScheme::RSA_PKCS1_SHA512,
                SignatureScheme::ECDSA_NISTP256_SHA256,
                SignatureScheme::ECDSA_NISTP384_SHA384,
                SignatureScheme::ED25519,
                SignatureScheme::RSA_PSS_SHA256,
                SignatureScheme::RSA_PSS_SHA384,
                SignatureScheme::RSA_PSS_SHA512,
            ]
        }
    }

    rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAll))
        .with_no_client_auth()
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
