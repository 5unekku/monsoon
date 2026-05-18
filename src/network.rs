use crate::config::Config;
use anyhow::{Context, Result};
use rustls::ServerConfig;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// load or self-generate a tls cert+key pair for the network listener.
/// when paths are blank, generates a fresh self-signed cert in the config
/// dir on first start and updates the in-memory config (caller persists).
pub fn ensure_tls_material(config: &mut Config) -> Result<Arc<ServerConfig>> {
    let cert_path = if (config.network_cert_path.is_empty()) {
        let default_dir = Config::config_dir_for_tls()?;
        std::fs::create_dir_all(&default_dir).context("create tls dir")?;
        let path = default_dir.join("network.crt");
        config.network_cert_path = path.to_string_lossy().to_string();
        path
    } else {
        PathBuf::from(&config.network_cert_path)
    };
    let key_path = if (config.network_key_path.is_empty()) {
        let default_dir = Config::config_dir_for_tls()?;
        let path = default_dir.join("network.key");
        config.network_key_path = path.to_string_lossy().to_string();
        path
    } else {
        PathBuf::from(&config.network_key_path)
    };

    if (!cert_path.exists() || !key_path.exists()) {
        generate_self_signed(&cert_path, &key_path)?;
        tracing::info!(
            cert = %cert_path.display(),
            "generated self-signed tls cert for network listener"
        );
    }

    let cert_chain = load_certs(&cert_path)?;
    let private_key = load_private_key(&key_path)?;

    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .context("build tls server config")?;
    Ok(Arc::new(server_config))
}

fn generate_self_signed(cert_path: &Path, key_path: &Path) -> Result<()> {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()])?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(rcgen::DnType::CommonName, "rustor-network");
    let key_pair = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    std::fs::write(cert_path, cert.pem()).context("write cert")?;
    std::fs::write(key_path, key_pair.serialize_pem()).context("write key")?;
    // restrict key permissions on unix so other users can't read it
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn load_certs(path: &Path) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let file = std::fs::File::open(path).context("open cert")?;
    let mut reader = std::io::BufReader::new(file);
    let certs: Vec<_> = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parse cert pem")?;
    if (certs.is_empty()) {
        anyhow::bail!("no certs in {}", path.display());
    }
    Ok(certs)
}

fn load_private_key(path: &Path) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path).context("open key")?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .context("parse key pem")?
        .ok_or_else(|| anyhow::anyhow!("no private key in {}", path.display()))
}

/// generate a random 256-bit token, hex-encoded, for auth.
pub fn generate_token() -> String {
    use std::time::SystemTime;
    // we don't want to pull in rand for a one-shot. mix several entropy
    // sources (pid, time, an os random read) into a hash. the bridge already
    // pulls in cxx so we have no easy stable hasher; fall back to /dev/urandom.
    let mut bytes = [0u8; 32];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    }
    // mix in time + pid in case /dev/urandom failed for some reason
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let mix = now ^ pid;
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte ^= (mix >> ((index % 16) * 8)) as u8;
    }
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

/// constant-time string compare for the auth token. defends against timing
/// attacks where an attacker could probe prefix matches.
pub fn token_eq(left: &str, right: &str) -> bool {
    if (left.len() != right.len()) { return false; }
    let mut accumulated: u8 = 0;
    for (left_byte, right_byte) in left.bytes().zip(right.bytes()) {
        accumulated |= left_byte ^ right_byte;
    }
    accumulated == 0
}

/// state for a single TCP+TLS connection. owned by the listener thread.
/// once authenticated, downstream code reads/writes via the inner streams.
pub struct AuthedConnection {
    pub stream: rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream>,
}

impl AuthedConnection {
    /// perform the tls handshake and read the AUTH line. returns Ok only when
    /// the token matches. on any failure the connection is closed.
    pub fn accept(
        tcp: std::net::TcpStream,
        server_config: Arc<ServerConfig>,
        expected_token: &str,
    ) -> Result<Self> {
        let connection = rustls::ServerConnection::new(server_config).context("server tls")?;
        let mut stream = rustls::StreamOwned::new(connection, tcp);
        // read the first line (AUTH <token>\n) before doing anything else
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        reader.read_line(&mut line).context("read auth")?;
        drop(reader);
        let trimmed = line.trim();
        let token = trimmed.strip_prefix("AUTH ").unwrap_or("");
        if (!token_eq(token, expected_token)) {
            // give nothing back — just drop the connection
            let _ = stream.write_all(b"{\"Err\":\"auth required\"}\n");
            anyhow::bail!("unauthenticated connection");
        }
        Ok(Self { stream })
    }
}

impl Config {
    /// dedicated dir for tls material (sibling of config.toml)
    pub fn config_dir_for_tls() -> Result<PathBuf> {
        let proj = directories::ProjectDirs::from("com", "rustor", "rustor")
            .context("locate project dirs")?;
        Ok(proj.config_dir().to_path_buf().join("tls"))
    }
}

/// bind the configured TCP listener, returning it ready for accept().
/// caller is expected to set non-blocking + integrate with the main loop.
pub fn bind(address: &str) -> Result<TcpListener> {
    let listener = TcpListener::bind(address)
        .with_context(|| format!("bind tcp {}", address))?;
    listener.set_nonblocking(true).context("set nonblocking")?;
    Ok(listener)
}
