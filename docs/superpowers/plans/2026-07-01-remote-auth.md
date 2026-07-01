# Remote Fetch Auth (http/ftp/sftp credentials) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let remote torrent fetches (`http://`, `https://`, `ftp://`, `sftp://`) authenticate.
A fetch that fails on credentials becomes a typed `AuthRequiredError` in `src/sources.rs`, which
the daemon's Add handler turns into a new `Response::AuthRequired`; the TUI answers with a
username/password overlay (password masked at the draw site) and resends the same `Add` with
one-shot `TransferCredentials`. Persistent auth is `~/.netrc` only (new `use_netrc` config key,
default on). A cached probe reports whether the system libcurl speaks sftp at all.

**Architecture:** Mirrors the landed two-phase `RenameConfirmation` round trip: first `Add` fails
soft with a structured response, the resend carries the answers, the daemon stores nothing.
`sources.rs` gains a `FetchAuth` input applied to every curl handle plus a pure, unit-tested
classification function (`auth_failure_hint`) deciding when a failure means "credentials would
help" (http 401; curl code 67 for ftp/sftp). The TUI's single `Request::Add` call site
(`dispatch_add_options`) is restructured into a pausable `AddDispatch` so a batch can stop at the
entry that needs credentials and resume after the prompt.

**Verified deviation from the spec:** the spec's section D calls for an `ssh_private_key_path`
config key applied via `easy.ssh_private_key(path)`. Verified against the vendored crate source
(`curl 0.4.49`, and upstream master fetched 2026-07-01): the safe `curl` crate wraps **neither**
`CURLOPT_SSH_PRIVATE_KEYFILE` **nor** `CURLOPT_SSH_KNOWNHOSTS`. The spec's own non-goals rule
("if the safe crate doesn't wrap an option we want, the option is dropped from v1, not
hand-rolled through unsafe") therefore drops both. v1 sftp key auth rides libcurl's native
defaults (ssh-agent, then the default `~/.ssh` key locations libssh2 tries on its own); custom
key paths and known-hosts verification wait for an upstream wrapper or a reviewed `curl-sys`
exception. Everything else in the spec is implemented as written.

**Tech Stack:** Rust 2021, `curl` 0.4.49 against the system libcurl, `serde`/`serde_json` (IPC),
`ratatui`/`crossterm` (TUI), `toml` (config). No new dependencies. The cxx/libtorrent bridge is
untouched, so incremental builds stay in pure-Rust territory (still minutes on first build; let
them run).

**Spec:** `docs/superpowers/specs/2026-07-01-remote-auth-design.md`

---

## Sequencing

Tasks run strictly in order 1 → 7 (single worktree, no parallelization; this sub-project itself
runs after the other universal-input sub-projects, sequentially).

| Task | Files touched |
|---|---|
| 1 (config key) | `src/config.rs`, `src/server.rs` (one match arm), `src/tui.rs` (settings schema) |
| 2 (ipc types) | `src/ipc.rs`, `src/display.rs`, `src/main.rs`, `src/tui.rs` (one construction site), `src/server.rs` (one pattern) |
| 3 (fetch plumbing) | `src/sources.rs`, `src/server.rs` (callers), `src/rss.rs` |
| 4 (AuthRequired response) | `src/server.rs` |
| 5 (pausable dispatch) | `src/tui.rs` |
| 6 (credentials overlay) | `src/tui.rs` |
| 7 (verification) | none (build/test/manual only) |

`src/tui.rs` is touched by tasks 1, 2, 5, 6 and `src/server.rs` by tasks 1, 2, 3, 4; running in
order avoids all conflicts. `src/ipc.rs` changes land once, in task 2. Line anchors below refer
to the tree as of commit `34b4e2d`; later tasks shift earlier anchors by a handful of lines.

---

## Task 1: `use_netrc` config key

The only persistent credential path besides ssh defaults. Spec section D (minus the dropped
`ssh_private_key_path`, see header).

**Files:**
- Modify: `src/config.rs` (field, default, test)
- Modify: `src/server.rs:437` (`apply_config_change` arm)
- Modify: `src/tui.rs:464` (settings schema entry), `src/tui.rs:689` (`config_value_string` arm)

- [ ] **Step 1: Write the failing test**

`src/config.rs` has no tests module. Append one at the end of the file (after line 412):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_netrc_defaults_true_and_survives_a_missing_key() {
        // old config.toml files have no use_netrc line; serde default fills it
        let config: Config = toml::from_str("").expect("empty config parses");
        assert!(config.use_netrc);
        assert!(Config::default().use_netrc);
    }
}
```

Run: `cargo test config::`
Expected: FAIL (compile error: no field `use_netrc` on `Config`).

- [ ] **Step 2: Add the field**

In `src/config.rs`, after the `proxy_tracker_connections` field (line 97, end of the proxy
group), insert:

```rust
    // ─── remote fetch auth ────────────────────────────────────────────────
    /// authenticate http/ftp/sftp torrent fetches from ~/.netrc entries
    /// (curl NetRc::Optional). inert when the file does not exist; explicit
    /// per-transfer credentials always win over a netrc entry.
    #[serde(default = "default_true")]
    pub use_netrc: bool,
```

(`default_true` already exists at `src/config.rs:180`.)

In `impl Default for Config`, after `proxy_tracker_connections: true,` (line 229), insert:

```rust
            use_netrc: true,
```

- [ ] **Step 3: Accept the key in `apply_config_change`**

In `src/server.rs`, after the `"proxy_tracker_connections"` arm (line 437), insert:

```rust
            "use_netrc" => self.config.use_netrc = parse_bool(value),
```

Do not add it to the `restart_required` `matches!` list at `src/server.rs:354`: the flag is read
per fetch, so it applies immediately.

- [ ] **Step 4: Surface it in the settings overlay**

In `src/tui.rs`, `SETTING_FIELDS`, after the `proxy_tracker_connections` entry (its closing `},`
is line 464, just before the `// ── connection` comment), insert:

```rust
    SettingField {
        section: "security & anonymity",
        key: "use_netrc",
        label: "use ~/.netrc for fetches",
        description: "authenticate http/ftp/sftp torrent fetches from ~/.netrc entries. inert when the file does not exist; turn off if you consider a netrc file itself a liability.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
```

In `config_value_string` (`src/tui.rs:657`), after the `"proxy_tracker_connections"` arm
(line 689), insert:

```rust
        "use_netrc" => config.use_netrc.to_string(),
```

- [ ] **Step 5: Run the test**

Run: `cargo test config::`
Expected: PASS (first compile after the bridge build takes a while; let it run).

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/server.rs src/tui.rs
git commit -m "config: use_netrc key for netrc-backed fetch auth"
```

---

## Task 2: IPC types, CLI guidance, construction-site updates

Spec section B's data model: `TransferCredentials` (with the redacting `Debug`), the
`credentials` field on `Request::Add`, `Response::AuthRequired`, and the CLI's non-interactive
handling. Adding a field to a struct variant breaks every construction/destructuring site, so
they are all updated here with inert values; behavior changes come in tasks 4 and 6.

**Files:**
- Modify: `src/ipc.rs:106` (Add variant), `src/ipc.rs:282` (Response), `src/ipc.rs:308` (tests)
- Modify: `src/display.rs:29` (`print_response` arm)
- Modify: `src/main.rs:442` (CLI Add construction)
- Modify: `src/tui.rs:2571` (TUI Add construction)
- Modify: `src/server.rs:1201` (Add pattern, field ignored for now)

- [ ] **Step 1: Write the failing tests**

Append to the existing `tests` module in `src/ipc.rs` (after line 328):

```rust
    #[test]
    fn add_request_without_credentials_still_deserializes() {
        // json an old client would send: no credentials key at all
        let json = r#"{"Add":{"uri":"magnet:?xt=urn:btih:aaa","save_path":null,"category":null,"start_paused":false}}"#;
        let request: Request = serde_json::from_str(json).expect("old add json parses");
        match request {
            Request::Add { credentials, .. } => assert!(credentials.is_none()),
            other => panic!("expected Add, got {:?}", other),
        }
    }

    #[test]
    fn auth_required_response_round_trips() {
        let response = Response::AuthRequired {
            url: "http://tracker.example/file.torrent".to_string(),
            scheme: "http".to_string(),
            hint: "http 401 unauthorized".to_string(),
        };
        let json = serde_json::to_string(&response).expect("serialize");
        let parsed: Response = serde_json::from_str(&json).expect("parse");
        match parsed {
            Response::AuthRequired { url, scheme, hint } => {
                assert_eq!(url, "http://tracker.example/file.torrent");
                assert_eq!(scheme, "http");
                assert_eq!(hint, "http 401 unauthorized");
            }
            other => panic!("expected AuthRequired, got {:?}", other),
        }
    }

    #[test]
    fn transfer_credentials_debug_never_contains_the_password() {
        let credentials = TransferCredentials {
            username: "alice".to_string(),
            password: "hunter2".to_string(),
        };
        let debug_output = format!("{:?}", credentials);
        assert!(!debug_output.contains("hunter2"));
        assert!(debug_output.contains("alice"));
    }
```

Run: `cargo test ipc::`
Expected: FAIL (compile error: no `TransferCredentials`, no `credentials` field, no
`AuthRequired` variant).

- [ ] **Step 2: Add the types**

In `src/ipc.rs`, directly above `pub enum Request` (line 102), insert:

```rust
/// one-shot credentials for a single fetch. never persisted anywhere:
/// the daemon applies them to one curl handle and drops them.
#[derive(Clone, Serialize, Deserialize)]
pub struct TransferCredentials {
    pub username: String,
    pub password: String,
}

// manual impl instead of derive: Request derives Debug and requests can be
// logged, so the password must never reach a log line. the ipc test pins
// this; keep the manual impl through any future field cleanup.
impl std::fmt::Debug for TransferCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("TransferCredentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}
```

Change the `Add` variant (line 106) from:

```rust
    Add { uri: String, save_path: Option<String>, category: Option<String>, start_paused: bool, #[serde(default)] content_layout: ContentLayout },
```

to:

```rust
    Add { uri: String, save_path: Option<String>, category: Option<String>, start_paused: bool, #[serde(default)] content_layout: ContentLayout, #[serde(default)] credentials: Option<TransferCredentials> },
```

In `pub enum Response`, after the `RenameConfirmation` variant (line 297), insert:

```rust
    /// the fetch behind an Add needs credentials. resend the identical Add
    /// with `credentials` filled in. `url` echoes the original input uri;
    /// `scheme` is the literal lowercase url scheme; `hint` is human text
    /// derived from the curl error.
    AuthRequired { url: String, scheme: String, hint: String },
```

- [ ] **Step 3: Update every construction/destructuring site**

`src/main.rs:442`, change:

```rust
        Commands::Add { uri, save_path, category } => Request::Add { uri, save_path, category, start_paused: false, content_layout: crate::ipc::ContentLayout::Default },
```

to:

```rust
        Commands::Add { uri, save_path, category } => Request::Add { uri, save_path, category, start_paused: false, content_layout: crate::ipc::ContentLayout::Default, credentials: None },
```

`src/tui.rs:2571` (inside `dispatch_add_options`), change:

```rust
        let added_id = match client::send(Request::Add {
            uri: uri.clone(),
            save_path,
            category: None,
            start_paused: true,
            content_layout: options.content_layout,
        }) {
```

to:

```rust
        let added_id = match client::send(Request::Add {
            uri: uri.clone(),
            save_path,
            category: None,
            start_paused: true,
            content_layout: options.content_layout,
            credentials: None,
        }) {
```

`src/server.rs:1201`, change the pattern (the field is used in task 3; ignore it for now):

```rust
            Request::Add { uri, save_path, category, start_paused, content_layout } => {
```

to:

```rust
            Request::Add { uri, save_path, category, start_paused, content_layout, credentials: _ } => {
```

- [ ] **Step 4: CLI guidance arm in `print_response`**

`src/display.rs` matches `Response` exhaustively, so the build fails until this arm exists. In
`print_response` (line 29), after the `Response::RenameConfirmation` arm (line 49), insert:

```rust
        // cli add is non-interactive by design; netrc is the scripted answer
        Response::AuthRequired { url, hint, .. } => eprintln!(
            "authentication required for {} ({}); add an entry to ~/.netrc or use the tui",
            url, hint
        ),
```

- [ ] **Step 5: Run the tests**

Run: `cargo test ipc::`
Expected: PASS, including the redaction pin. `cargo build` also succeeds (all Response matches
outside display.rs use catch-all arms; the compiler confirms).

- [ ] **Step 6: Commit**

```bash
git add src/ipc.rs src/display.rs src/main.rs src/tui.rs src/server.rs
git commit -m "ipc: transfer credentials, AuthRequired response, cli netrc guidance"
```

---

## Task 3: fetch auth plumbing in `sources.rs`

Spec sections A and E: `FetchAuth` applied to every curl handle, the typed `AuthRequiredError`,
the classification rules, the sftp probe, and all caller updates (daemon paths pass
config-derived auth; the Add handler passes request credentials through but still maps errors to
`Response::Err`; task 4 adds the downcast).

**Files:**
- Modify: `src/sources.rs` (`resolve:72`, `fetch_to_string:180`, `fetch_url:203`, new items, tests)
- Modify: `src/server.rs:592`, `src/server.rs:582` (rss poll), `src/server.rs:994` (`install_ip_filter`), `src/server.rs:1204` (Add handler), `src/server.rs:1742` (`refresh_ip_filter`)
- Modify: `src/rss.rs:78` (`poll_feed` signature)

- [ ] **Step 1: Write the failing tests**

`src/sources.rs` has no tests module. Append at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_401_maps_to_auth_required() {
        // 22 = CURLE_HTTP_RETURNED_ERROR (fail_on_error fired)
        assert_eq!(auth_failure_hint("http", 22, 401).as_deref(), Some("http 401 unauthorized"));
        assert!(auth_failure_hint("https", 22, 401).is_some());
        // fail_on_error is documented to let 401 slip through as a "success"
        // once http auth is engaged, so curl code 0 + status 401 must also
        // classify as auth-required (this is the wrong-password retry path)
        assert!(auth_failure_hint("https", 0, 401).is_some());
    }

    #[test]
    fn login_denied_maps_to_auth_required_for_ftp_and_sftp() {
        // 67 = CURLE_LOGIN_DENIED
        assert_eq!(auth_failure_hint("ftp", 67, 0).as_deref(), Some("login denied"));
        assert!(auth_failure_hint("sftp", 67, 0).is_some());
    }

    #[test]
    fn other_failures_stay_plain_errors() {
        assert!(auth_failure_hint("http", 22, 403).is_none());  // forbidden: credentials will not help
        assert!(auth_failure_hint("http", 22, 407).is_none());  // proxy auth is out of scope
        assert!(auth_failure_hint("http", 28, 0).is_none());    // timeout
        assert!(auth_failure_hint("sftp", 1, 0).is_none());     // unsupported protocol
        assert!(auth_failure_hint("ftp", 22, 401).is_none());   // http rules never apply to ftp
    }

    #[test]
    fn auth_required_error_survives_the_anyhow_downcast() {
        let error: anyhow::Error = AuthRequiredError {
            scheme: "http".to_string(),
            hint: "http 401 unauthorized".to_string(),
        }.into();
        let recovered = error.downcast_ref::<AuthRequiredError>().expect("downcast back");
        assert_eq!(recovered.scheme, "http");
        assert_eq!(recovered.hint, "http 401 unauthorized");
    }

    #[test]
    fn fetch_auth_from_config_maps_the_netrc_flag() {
        let mut config = crate::config::Config::default();
        config.use_netrc = false;
        assert!(!FetchAuth::from_config(&config).use_netrc);
        config.use_netrc = true;
        let auth = FetchAuth::from_config(&config);
        assert!(auth.use_netrc);
        assert!(auth.credentials.is_none());
    }

    #[test]
    fn url_scheme_is_the_lowercased_prefix() {
        assert_eq!(url_scheme("HTTPS://Example.com/x.torrent"), "https");
        assert_eq!(url_scheme("sftp://host/x"), "sftp");
        assert_eq!(url_scheme("no-scheme-here"), "no-scheme-here");
    }
}
```

Run: `cargo test sources::`
Expected: FAIL (compile error: none of `auth_failure_hint`, `AuthRequiredError`, `FetchAuth`,
`url_scheme` exist yet).

- [ ] **Step 2: Add the new types and helpers**

In `src/sources.rs`, after the `Source` enum (line 68) and before `resolve`, insert:

```rust
/// everything a fetch needs to authenticate. built by the caller from
/// config plus any per-transfer credentials from the ipc request.
pub struct FetchAuth {
    /// one-shot username/password from the ipc request. None on every
    /// non-interactive path (rss, watch dirs, ip filter, cli).
    pub credentials: Option<crate::ipc::TransferCredentials>,
    /// map ~/.netrc entries onto fetches (curl NetRc::Optional). explicit
    /// credentials above always win, which is curl's documented Optional
    /// behavior.
    pub use_netrc: bool,
}

impl FetchAuth {
    /// config-only auth: what every non-interactive fetch path uses.
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self { credentials: None, use_netrc: config.use_netrc }
    }
}

/// fetch failed because the server wants credentials. the add handler
/// downcasts to this to emit Response::AuthRequired instead of a plain Err.
#[derive(Debug)]
pub struct AuthRequiredError {
    /// literal url scheme, lowercase: "http", "https", "ftp", "sftp"
    pub scheme: String,
    /// human text, e.g. "http 401 unauthorized", "login denied"
    pub hint: String,
}

impl std::fmt::Display for AuthRequiredError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "authentication required ({})", self.hint)
    }
}

impl std::error::Error for AuthRequiredError {}

/// true when the system libcurl was built with sftp support (libssh2).
/// probed once per process; the daemon logs the result at startup so a
/// missing backend is visible before anyone tries an sftp url.
pub fn sftp_supported() -> bool {
    static SFTP_SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SFTP_SUPPORTED.get_or_init(|| {
        curl::Version::get().protocols().any(|protocol| protocol == "sftp")
    })
}

/// literal lowercase scheme of a url ("http", "ftp", ...). inputs without
/// "://" come back whole; callers only pass strings that passed is_url.
fn url_scheme(url: &str) -> String {
    url.split("://").next().unwrap_or("").to_ascii_lowercase()
}

/// decide whether a failed transfer should surface as "server wants
/// credentials". curl_code is the raw CURLE_* value (0 when the transfer
/// nominally succeeded), http_status is response_code() (0 when unknown).
fn auth_failure_hint(scheme: &str, curl_code: u32, http_status: u32) -> Option<String> {
    match scheme {
        // 401 is the only http status where credentials plausibly help
        // (403/407/5xx stay plain errors). checked on the error path AND
        // after a "successful" perform, because fail_on_error is documented
        // to let 401 slip through once http auth is engaged.
        "http" | "https" if (http_status == 401) => Some("http 401 unauthorized".to_string()),
        // 67 = CURLE_LOGIN_DENIED, kept as a literal so we don't import
        // curl-sys for one constant. covers ftp login rejections and
        // libssh2 auth failures alike.
        "ftp" | "sftp" if (curl_code == 67) => Some("login denied".to_string()),
        _ => None,
    }
}

/// apply credentials and the netrc policy to a curl handle.
fn apply_auth(easy: &mut curl::easy::Easy, scheme: &str, auth: &FetchAuth) -> Result<()> {
    use curl::easy::{Auth, NetRc};
    let netrc_mode = if (auth.use_netrc) { NetRc::Optional } else { NetRc::Ignored };
    easy.netrc(netrc_mode).map_err(|error| anyhow::anyhow!("curl netrc: {}", error))?;
    if let Some(credentials) = &auth.credentials {
        easy.username(&credentials.username)
            .map_err(|error| anyhow::anyhow!("curl username: {}", error))?;
        easy.password(&credentials.password)
            .map_err(|error| anyhow::anyhow!("curl password: {}", error))?;
        // basic + digest so curl negotiates whichever the server offers;
        // never volunteered without credentials. ftp uses username/password
        // as the login; sftp maps them to ssh password and
        // keyboard-interactive auth natively (no ui takeover needed).
        if (scheme == "http" || scheme == "https") {
            easy.http_auth(Auth::new().basic(true).digest(true))
                .map_err(|error| anyhow::anyhow!("curl http_auth: {}", error))?;
        }
    }
    Ok(())
}

/// shared post-perform classification for both fetch functions. takes the
/// handle mutably because Easy::response_code does.
fn finish_fetch(
    easy: &mut curl::easy::Easy,
    scheme: &str,
    perform_result: std::result::Result<(), curl::Error>,
) -> Result<()> {
    let http_status = easy.response_code().unwrap_or(0);
    match perform_result {
        Err(error) => {
            if let Some(hint) = auth_failure_hint(scheme, error.code() as u32, http_status) {
                return Err(AuthRequiredError { scheme: scheme.to_string(), hint }.into());
            }
            Err(anyhow::anyhow!("curl: {}", error))
        }
        Ok(()) => {
            if let Some(hint) = auth_failure_hint(scheme, 0, http_status) {
                return Err(AuthRequiredError { scheme: scheme.to_string(), hint }.into());
            }
            Ok(())
        }
    }
}
```

- [ ] **Step 3: Rework `fetch_url` and `fetch_to_string`**

Replace `fetch_to_string` (`src/sources.rs:180`) in full:

```rust
/// fetch a url into memory and return the body as a String. follows redirects,
/// enforces a 60s timeout. intended for small text payloads (RSS feeds, etc.).
/// auth failures surface as AuthRequiredError so callers log a clear message.
pub fn fetch_to_string(url: &str, auth: &FetchAuth) -> Result<String> {
    use curl::easy::Easy;
    use std::time::Duration;
    let mut body: Vec<u8> = Vec::new();
    let scheme = url_scheme(url);
    let mut easy = Easy::new();
    easy.url(url).map_err(|e| anyhow::anyhow!("curl url: {}", e))?;
    easy.follow_location(true).map_err(|e| anyhow::anyhow!("{}", e))?;
    easy.max_redirections(10).map_err(|e| anyhow::anyhow!("{}", e))?;
    easy.connect_timeout(Duration::from_secs(30)).map_err(|e| anyhow::anyhow!("{}", e))?;
    easy.timeout(Duration::from_secs(60)).map_err(|e| anyhow::anyhow!("{}", e))?;
    easy.fail_on_error(true).map_err(|e| anyhow::anyhow!("{}", e))?;
    apply_auth(&mut easy, &scheme, auth)?;
    let perform_result = {
        let mut transfer = easy.transfer();
        transfer.write_function(|data| { body.extend_from_slice(data); Ok(data.len()) })
            .map_err(|e| anyhow::anyhow!("curl write: {}", e))?;
        transfer.perform()
    };
    finish_fetch(&mut easy, &scheme, perform_result)?;
    String::from_utf8(body).map_err(|_| anyhow::anyhow!("response is not valid utf-8"))
}
```

Replace `fetch_url` (`src/sources.rs:203`) in full:

```rust
/// download a url to a local file via libcurl. follows redirects, enforces a
/// 120s timeout, and fails with a structured error on non-2xx responses.
/// supports http, https, ftp, and sftp (same protocols curl supports).
/// auth failures surface as AuthRequiredError so interactive callers can
/// prompt for credentials.
pub fn fetch_url(dest: &std::path::Path, url: &str, auth: &FetchAuth) -> Result<()> {
    use curl::easy::Easy;
    use std::time::Duration;
    let file = std::fs::File::create(dest)
        .map_err(|error| anyhow::anyhow!("create temp file: {}", error))?;
    let mut file = std::io::BufWriter::new(file);
    let scheme = url_scheme(url);
    let mut easy = Easy::new();
    easy.url(url).map_err(|error| anyhow::anyhow!("curl url: {}", error))?;
    easy.follow_location(true).map_err(|error| anyhow::anyhow!("curl follow_location: {}", error))?;
    easy.max_redirections(10).map_err(|error| anyhow::anyhow!("curl max_redirections: {}", error))?;
    easy.connect_timeout(Duration::from_secs(30)).map_err(|error| anyhow::anyhow!("curl connect_timeout: {}", error))?;
    easy.timeout(Duration::from_secs(120)).map_err(|error| anyhow::anyhow!("curl timeout: {}", error))?;
    easy.fail_on_error(true).map_err(|error| anyhow::anyhow!("curl fail_on_error: {}", error))?;
    apply_auth(&mut easy, &scheme, auth)?;
    let perform_result = {
        let mut transfer = easy.transfer();
        transfer.write_function(|data| {
            file.write_all(data)
                .map(|_| data.len())
                .map_err(|_| curl::easy::WriteError::Pause)
        }).map_err(|error| anyhow::anyhow!("curl write_function: {}", error))?;
        transfer.perform()
    };
    finish_fetch(&mut easy, &scheme, perform_result)
}
```

Note the structural change in both: `transfer.perform()`'s result is carried out of the transfer
scope instead of `?`-ed inside it, because `response_code()` needs the `easy` borrow back.
URL-embedded credentials (`ftp://user:pass@host/`) keep working untouched: curl parses them
itself and they simply pre-empt the auth-required error.

- [ ] **Step 4: Thread `FetchAuth` through `resolve` and gate sftp**

Change `resolve` (`src/sources.rs:72`). The signature gains the auth parameter and the url
branch gains the probe check; the magnet and local-path branches are untouched:

```rust
/// classify and resolve one user input string into a Source. on http/ftp
/// the file is downloaded to a temp path; the caller owns cleanup.
pub fn resolve(input: &str, auth: &FetchAuth) -> Result<Source> {
    let trimmed = input.trim();
    if (trimmed.is_empty()) {
        anyhow::bail!("empty source");
    }

    // magnet uris are the simplest case
    if (trimmed.starts_with("magnet:")) {
        return Ok(Source::Magnet(trimmed.to_string()));
    }

    // network protocols: fetch via libcurl (http/https/ftp/sftp).
    if (is_url(trimmed)) {
        // sftp needs the libssh2-backed protocol in the system libcurl.
        // a missing backend is one clear error, never an auth prompt.
        if (trimmed.to_ascii_lowercase().starts_with("sftp://") && !sftp_supported()) {
            anyhow::bail!("sftp support requires libcurl built with libssh2; this system's libcurl does not provide it");
        }
        let temp = std::env::temp_dir().join(format!(
            "monsoon-fetch-{}.torrent",
            std::process::id()
        ));
        fetch_url(&temp, trimmed, auth)
            .inspect_err(|_| { let _ = std::fs::remove_file(&temp); })?;
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
```

- [ ] **Step 5: Update all four caller groups**

**Add handler** (`src/server.rs`, pattern already says `credentials: _` from task 2). Change the
pattern to bind the field and build the auth up front; error handling stays `Response::Err`
until task 4:

```rust
            Request::Add { uri, save_path, category, start_paused, content_layout, credentials } => {
                // delegate scheme + path resolution to the sources module so
                // http/https/ftp/sftp urls and ~ expansion work uniformly.
                // credentials are used for this one transfer and dropped.
                let fetch_auth = crate::sources::FetchAuth {
                    credentials,
                    use_netrc: self.config.use_netrc,
                };
                match crate::sources::resolve(&uri, &fetch_auth) {
                    Ok(crate::sources::Source::Magnet(magnet)) => {
                        match self.add_magnet(&magnet, save_path.as_deref(), category.as_deref(), start_paused, content_layout) {
                            Ok(hash) => Response::Added { id: hash },
                            Err(error) => Response::Err(error.to_string()),
                        }
                    }
                    Ok(crate::sources::Source::File(path)) => {
                        match self.add_file(&path.to_string_lossy(), save_path.as_deref(), category.as_deref(), start_paused, content_layout) {
                            Ok(hash) => Response::Added { id: hash },
                            Err(error) => Response::Err(error.to_string()),
                        }
                    }
                    Err(error) => Response::Err(error.to_string()),
                }
            }
```

**rss** (`src/server.rs:571`, `poll_rss_feeds`). Before the `for feed in &feeds` loop
(line 574), insert:

```rust
        let fetch_auth = crate::sources::FetchAuth::from_config(&self.config);
```

Change the `poll_feed` call (line 582) to pass it through:

```rust
            let items = match crate::rss::poll_feed(feed, &self.rss_seen, &fetch_auth) {
```

Change the resolve call (line 592):

```rust
                let result = match crate::sources::resolve(&uri, &fetch_auth) {
```

And in `src/rss.rs`, change `poll_feed` (line 78) to accept and forward it:

```rust
pub fn poll_feed(feed: &RssFeed, seen: &RssSeen, fetch_auth: &crate::sources::FetchAuth) -> Result<Vec<(String, String)>> {
    let xml = crate::sources::fetch_to_string(&feed.url, fetch_auth)
        .with_context(|| format!("fetch feed {}", feed.url))?;
```

(rest of the function unchanged.)

**ip filter** (`src/server.rs`). `refresh_ip_filter` (line 1742) gains the parameter:

```rust
fn refresh_ip_filter(url: &str, target: &str, fetch_auth: &crate::sources::FetchAuth) -> Result<()> {
    let temp_path = format!("{}.partial", target);
    let temp = std::path::Path::new(&temp_path);
    if let Some(parent) = std::path::Path::new(target).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    crate::sources::fetch_url(temp, url, fetch_auth)
        .inspect_err(|_| { let _ = std::fs::remove_file(temp); })?;
    std::fs::rename(temp, target).context("swap ip filter into place")?;
    Ok(())
}
```

and its one call site inside `install_ip_filter` (line 1001) becomes:

```rust
        if (!url.trim().is_empty()) {
            let fetch_auth = crate::sources::FetchAuth::from_config(&self.config);
            if let Err(error) = refresh_ip_filter(&url, &path, &fetch_auth) {
```

**CLI:** verified there is no client-side `resolve` call (grep: `sources::resolve` appears only
in `src/server.rs`), so nothing to update; the CLI's `Add` rides the daemon path above.

- [ ] **Step 6: Run the tests and build**

Run: `cargo test sources::`
Expected: PASS (all six new tests).
Run: `cargo build`
Expected: clean; the compiler has now verified every `resolve`/`fetch_url`/`fetch_to_string`
caller carries a `FetchAuth`.

- [ ] **Step 7: Commit**

```bash
git add src/sources.rs src/server.rs src/rss.rs
git commit -m "sources: fetch auth plumbing, auth-required classification, sftp probe"
```

---

## Task 4: daemon answers `Add` with `AuthRequired`; startup probe log

Spec section B (daemon side) and the logging half of section E. Non-interactive surfaces need
no changes: rss/watch/ip-filter never send credentials and log the downcast-formatted
`AuthRequiredError` Display text through their existing `tracing::warn!` paths.

**Files:**
- Modify: `src/server.rs` (Add handler error arm; startup log near line 2094)

- [ ] **Step 1: Downcast in the Add handler**

In the `Request::Add` arm rewritten in task 3, replace the final error arm:

```rust
                    Err(error) => Response::Err(error.to_string()),
```

with:

```rust
                    // "needs credentials" is a soft failure the tui can answer;
                    // everything else stays a plain error, exactly as before.
                    Err(error) => match error.downcast_ref::<crate::sources::AuthRequiredError>() {
                        Some(auth_error) => Response::AuthRequired {
                            url: uri.clone(),
                            scheme: auth_error.scheme.clone(),
                            hint: auth_error.hint.clone(),
                        },
                        None => Response::Err(error.to_string()),
                    },
```

A resend that fails auth again produces another `AuthRequired` with a fresh hint through this
same arm, so wrong passwords loop back to the prompt naturally; there is no retry cap because
Esc cancels client-side. On success the fetched file proceeds through the normal add path
unchanged (organize step, rename confirmations, `FinalizeAdd`).

- [ ] **Step 2: Log the sftp probe once at startup**

In `src/server.rs`, `run()`, directly after the two `daemon started` `tracing::info!` blocks
(after line 2094), insert:

```rust
    // one info line so a missing libssh2 backend is visible before anyone
    // tries an sftp url. the probe result is cached for the process lifetime.
    tracing::info!(supported = crate::sources::sftp_supported(), "libcurl sftp support probed");
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: clean. (The `AuthRequired` daemon behavior is exercised manually in task 7; it needs a
real 401 server.)

- [ ] **Step 4: Commit**

```bash
git add src/server.rs
git commit -m "server: answer add fetch auth failures with AuthRequired, log sftp probe"
```

---

## Task 5: restructure TUI add dispatch into a pausable `AddDispatch`

Pure refactor, no behavior change. `dispatch_add_options` (`src/tui.rs:2558`) is the TUI's only
`Request::Add` call site; task 6 needs to stop it mid-batch and resume later, so its loop state
(position, success/failure accumulators, organize bookkeeping) moves into a struct.

This refines the spec's `entry + remaining: AddOptionsForm` stash shape: parking the whole
dispatch keeps the already-accumulated success/failure counts, which the split shape would lose
from the final summary line ("added N ok, M failed"). Behavior is otherwise exactly the spec's.

**Files:**
- Modify: `src/tui.rs:2558` (`dispatch_add_options` replaced by three functions and a struct)

- [ ] **Step 1: Replace `dispatch_add_options` in full**

Replace the entire function (`src/tui.rs:2558` through 2613) with:

```rust
/// in-flight add-batch dispatch state. normally consumed in one pass by
/// run_add_dispatch; parked inside the auth prompt when an entry needs
/// credentials, so the batch can resume where it stopped.
struct AddDispatch {
    entries: Vec<String>,
    options: Vec<AddOptions>,
    /// index of the next entry to send
    next: usize,
    succeeded: usize,
    failures: Vec<String>,
    organize_indices: Vec<usize>,
    organize_entries: Vec<String>,
    organize_resume: Vec<bool>,
}

fn dispatch_add_options(form: AddOptionsForm, state: &mut AppState) {
    run_add_dispatch(AddDispatch {
        entries: form.entries,
        options: form.options,
        next: 0,
        succeeded: 0,
        failures: Vec::new(),
        organize_indices: Vec::new(),
        organize_entries: Vec::new(),
        organize_resume: Vec::new(),
    }, state);
}

fn run_add_dispatch(mut dispatch: AddDispatch, state: &mut AppState) {
    while (dispatch.next < dispatch.entries.len()) {
        let entry_index = dispatch.next;
        let uri = dispatch.entries[entry_index].clone();
        let options = dispatch.options[entry_index].clone();
        let save_path = if (options.save_path.trim().is_empty()) { None } else { Some(options.save_path.clone()) };
        // always add paused: the organize step must run before any data
        // downloads, regardless of the user's requested start/pause option.
        // `options.start` is remembered below and applied once the step
        // concludes for this entry.
        let response = client::send(Request::Add {
            uri: uri.clone(),
            save_path,
            category: None,
            start_paused: true,
            content_layout: options.content_layout,
            credentials: None,
        });
        match response {
            Ok(Response::Added { .. }) => {
                record_added_entry(&mut dispatch, &uri, &options);
                dispatch.next += 1;
            }
            Ok(Response::Err(message)) => {
                dispatch.failures.push(format!("{}: {}", uri, message));
                dispatch.next += 1;
            }
            Ok(_) => {
                dispatch.failures.push(format!("{}: unexpected response", uri));
                dispatch.next += 1;
            }
            Err(error) => {
                dispatch.failures.push(format!("{}: {}", uri, error));
                dispatch.next += 1;
            }
        }
    }
    finish_add_dispatch(dispatch, state);
}

/// post-add bookkeeping shared by first-pass adds and auth retries:
/// sequential/first-last tweaks plus organize-step queueing.
fn record_added_entry(dispatch: &mut AddDispatch, uri: &str, options: &AddOptions) {
    dispatch.succeeded += 1;
    let new_index = match client::send(Request::List) {
        Ok(Response::TorrentList(list)) => list.len().saturating_sub(1),
        _ => return,
    };
    if (options.sequential) {
        let _ = client::send(Request::SetSequential { index: new_index, enabled: true });
    }
    if (options.first_last) {
        let _ = client::send(Request::SetFirstLastPriority { index: new_index, enabled: true });
    }
    dispatch.organize_indices.push(new_index);
    dispatch.organize_entries.push(uri.to_string());
    dispatch.organize_resume.push(options.start);
}

fn finish_add_dispatch(dispatch: AddDispatch, state: &mut AppState) {
    if (dispatch.failures.is_empty()) {
        state.error = Some(format!("added {} torrent(s)", dispatch.succeeded));
    } else if (dispatch.succeeded == 0) {
        state.error = Some(format!("all sources failed: {}", dispatch.failures.join("; ")));
    } else {
        state.error = Some(format!(
            "added {} ok, {} failed: {}",
            dispatch.succeeded, dispatch.failures.len(), dispatch.failures.join("; ")
        ));
    }
    state.last_poll = Instant::now() - POLL_INTERVAL;
    if (!dispatch.organize_indices.is_empty()) {
        state.priority_step = Some(Box::new(PriorityStep::new(
            dispatch.organize_entries, dispatch.organize_indices, dispatch.organize_resume,
        )));
    }
}
```

- [ ] **Step 2: Verify it compiles and behaves identically**

Run: `cargo build`
Expected: clean, no new warnings (`AddDispatch` is fully used; `AddOptions` already derives
`Clone` at `src/tui.rs:1017`).

Manual smoke test: `cargo run -- tui`, press `a`, add one magnet and one garbage string in the
multi-line prompt, confirm the options form, and check the summary line still reads
`added 1 ok, 1 failed: ...` and the organize step opens for the magnet.

- [ ] **Step 3: Commit**

```bash
git add src/tui.rs
git commit -m "tui: restructure add dispatch into a pausable AddDispatch"
```

---

## Task 6: TUI credentials overlay

Spec section C. The overlay opens only in reaction to a daemon `AuthRequired` (so the sftp
gating from section E falls out for free), sits at the same routing rank as `rename_confirm`
(the two can never be active simultaneously because auth happens before the torrent exists), and
masks the password purely at the draw site: `TextField` is untouched, keeping exactly one
text-editing implementation.

**Files:**
- Modify: `src/tui.rs:1135` (AppState field), `src/tui.rs:1265` (init)
- Modify: `src/tui.rs:1530` (key-routing ladder)
- Modify: `src/tui.rs` `run_add_dispatch` (new `AuthRequired` arm, from task 5's shape)
- Modify: `src/tui.rs:3587` (`draw` overlay calls, both branches)
- Add: `AuthPrompt` struct, `handle_auth_prompt_key`, `submit_auth_prompt`,
  `auth_prompt_field`, `draw_auth_prompt`, `auth_field_line`,
  `render_masked_field_with_cursor` (place next to their rename_confirm counterparts)

- [ ] **Step 1: State**

In `src/tui.rs`, after the `rename_confirm` field on `AppState` (line 1135), insert:

```rust
    /// credentials overlay opened when the daemon answers an Add with
    /// Response::AuthRequired. boxed: it parks the whole in-flight batch.
    auth_prompt: Option<Box<AuthPrompt>>,
```

In `AppState::new()`, after `rename_confirm: None,` (line 1265), insert:

```rust
            auth_prompt: None,
```

Next to `RenameConfirm` (after `enum RenameConfirmKind`, line 986), insert:

```rust
/// credentials overlay for a fetch that answered AuthRequired. holds the
/// paused AddDispatch so the batch resumes after this entry is retried,
/// failed, or skipped. both fields are ordinary TextFields with the full
/// landed editing behavior; masking happens at the draw site only.
struct AuthPrompt {
    url: String,
    scheme: String,
    hint: String,
    /// true after a failed retry: draw adds "authentication failed, try again"
    retry_notice: bool,
    username: TextField,
    password: TextField,
    focus_password: bool,
    dispatch: AddDispatch,
}
```

- [ ] **Step 2: Pause the dispatch on `AuthRequired`**

In `run_add_dispatch` (task 5), insert a new arm directly after the `Ok(Response::Added ...)`
arm and before `Ok(Response::Err ...)`:

```rust
            Ok(Response::AuthRequired { url, scheme, hint }) => {
                // stop the batch here; entries already dispatched keep their
                // results. esc/enter on the prompt resumes from this entry.
                state.auth_prompt = Some(Box::new(AuthPrompt {
                    url,
                    scheme,
                    hint,
                    retry_notice: false,
                    username: TextField::new(String::new()),
                    password: TextField::new(String::new()),
                    focus_password: false,
                    dispatch,
                }));
                return;
            }
```

- [ ] **Step 3: Key handling**

Add next to `handle_rename_confirm_key` (`src/tui.rs:4171`):

```rust
fn handle_auth_prompt_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Esc, _) => {
            // skip this entry and keep going with the rest of the batch
            let Some(prompt) = state.auth_prompt.take() else { return false; };
            let AuthPrompt { mut dispatch, .. } = *prompt;
            if let Some(uri) = dispatch.entries.get(dispatch.next) {
                dispatch.failures.push(format!("{}: authentication cancelled", uri));
            }
            dispatch.next += 1;
            run_add_dispatch(dispatch, state);
        }
        (KeyCode::Tab, _) | (KeyCode::Up, _) | (KeyCode::Down, _) => {
            if let Some(prompt) = state.auth_prompt.as_mut() {
                prompt.focus_password = !prompt.focus_password;
            }
        }
        (KeyCode::Enter, _) => submit_auth_prompt(state),
        (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
            let Ok(mut clipboard) = arboard::Clipboard::new() else { return false; };
            let Ok(text) = clipboard.get_text() else { return false; };
            if let Some(field) = auth_prompt_field(state) { field.paste(&text); }
        }
        (KeyCode::Left, KeyModifiers::CONTROL) | (KeyCode::Left, KeyModifiers::ALT) => {
            if let Some(field) = auth_prompt_field(state) { field.move_word_left(); }
        }
        (KeyCode::Right, KeyModifiers::CONTROL) | (KeyCode::Right, KeyModifiers::ALT) => {
            if let Some(field) = auth_prompt_field(state) { field.move_word_right(); }
        }
        (KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Backspace, KeyModifiers::ALT) => {
            if let Some(field) = auth_prompt_field(state) { field.delete_word_backward(); }
        }
        (KeyCode::Delete, KeyModifiers::CONTROL) | (KeyCode::Delete, KeyModifiers::ALT) => {
            if let Some(field) = auth_prompt_field(state) { field.delete_word_forward(); }
        }
        (KeyCode::Left, _) => { if let Some(field) = auth_prompt_field(state) { field.move_left(); } }
        (KeyCode::Right, _) => { if let Some(field) = auth_prompt_field(state) { field.move_right(); } }
        (KeyCode::Home, _) => { if let Some(field) = auth_prompt_field(state) { field.move_home(); } }
        (KeyCode::End, _) => { if let Some(field) = auth_prompt_field(state) { field.move_end(); } }
        (KeyCode::Delete, _) => { if let Some(field) = auth_prompt_field(state) { field.delete_forward(); } }
        (KeyCode::Backspace, _) => { if let Some(field) = auth_prompt_field(state) { field.backspace(); } }
        (KeyCode::Char(character), modifiers)
            if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
        {
            if let Some(field) = auth_prompt_field(state) { field.insert_char(character); }
        }
        _ => {}
    }
    false
}

fn auth_prompt_field(state: &mut AppState) -> Option<&mut TextField> {
    let prompt = state.auth_prompt.as_mut()?;
    if (prompt.focus_password) { Some(&mut prompt.password) } else { Some(&mut prompt.username) }
}

fn submit_auth_prompt(state: &mut AppState) {
    let Some(prompt) = state.auth_prompt.take() else { return; };
    let AuthPrompt { username, password, mut dispatch, .. } = *prompt;
    let entry_index = dispatch.next;
    let Some(uri) = dispatch.entries.get(entry_index).cloned() else {
        // defensive: position ran past the batch; just close it out
        finish_add_dispatch(dispatch, state);
        return;
    };
    let options = dispatch.options[entry_index].clone();
    let save_path = if (options.save_path.trim().is_empty()) { None } else { Some(options.save_path.clone()) };
    // identical Add to the first attempt, credentials filled in. used for
    // this one transfer server-side and dropped.
    let response = client::send(Request::Add {
        uri: uri.clone(),
        save_path,
        category: None,
        start_paused: true,
        content_layout: options.content_layout,
        credentials: Some(crate::ipc::TransferCredentials {
            username: username.buffer().to_string(),
            password: password.buffer().to_string(),
        }),
    });
    match response {
        Ok(Response::Added { .. }) => {
            record_added_entry(&mut dispatch, &uri, &options);
            dispatch.next += 1;
            run_add_dispatch(dispatch, state);
        }
        Ok(Response::AuthRequired { url, scheme, hint }) => {
            // wrong credentials: keep the username, clear the password,
            // show the fresh hint. no retry cap; esc remains the way out.
            state.auth_prompt = Some(Box::new(AuthPrompt {
                url,
                scheme,
                hint,
                retry_notice: true,
                username,
                password: TextField::new(String::new()),
                focus_password: true,
                dispatch,
            }));
        }
        Ok(Response::Err(message)) => {
            dispatch.failures.push(format!("{}: {}", uri, message));
            dispatch.next += 1;
            run_add_dispatch(dispatch, state);
        }
        Ok(_) => {
            dispatch.failures.push(format!("{}: unexpected response", uri));
            dispatch.next += 1;
            run_add_dispatch(dispatch, state);
        }
        Err(error) => {
            dispatch.failures.push(format!("{}: {}", uri, error));
            dispatch.next += 1;
            run_add_dispatch(dispatch, state);
        }
    }
}
```

- [ ] **Step 4: Routing**

In the input ladder (`src/tui.rs:1530`), insert the auth branch directly after the
`rename_confirm` branch, so it reads:

```rust
                    let exit = if (state.rename_confirm.is_some()) {
                        handle_rename_confirm_key(key.code, &mut state);
                        false
                    } else if (state.auth_prompt.is_some()) {
                        // same rank as rename_confirm; the two can never be
                        // active at once (auth happens before the torrent exists)
                        handle_auth_prompt_key(key.code, key.modifiers, &mut state)
                    } else if (state.prompt.is_some()) {
```

- [ ] **Step 5: Drawing**

Add next to `render_field_with_cursor` (`src/tui.rs:4004`):

```rust
/// like render_field_with_cursor but every buffer char renders as '*'.
/// masking lives here at the draw site; TextField itself is untouched.
fn render_masked_field_with_cursor(field: &TextField) -> Vec<Span<'static>> {
    let length = field.buffer().chars().count();
    let cursor = field.cursor().min(length);
    let before = "*".repeat(cursor);
    let at = if (cursor == length) { " ".to_string() } else { "*".to_string() };
    let after = if (cursor >= length) { String::new() } else { "*".repeat(length - cursor - 1) };
    vec![
        Span::raw(before),
        Span::styled(at, Style::default().fg(Color::Black).bg(Color::Yellow)),
        Span::raw(after),
    ]
}
```

Add next to `draw_rename_confirm` (`src/tui.rs:4094`):

```rust
fn draw_auth_prompt(frame: &mut ratatui::Frame, state: &AppState) {
    let Some(prompt) = &state.auth_prompt else { return; };
    let area = frame.area();
    let height = 9u16.min(area.height.saturating_sub(2));
    let width = (area.width * 70 / 100).clamp(50, area.width.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect { x, y, width, height };

    frame.render_widget(ratatui::widgets::Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" authentication required ");
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let layout = Layout::vertical([
        Constraint::Length(1), // url + scheme
        Constraint::Length(1), // hint (+ retry notice)
        Constraint::Length(1), // gap
        Constraint::Length(1), // username
        Constraint::Length(1), // password
        Constraint::Length(1), // gap
        Constraint::Length(1), // key hint
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{} ({})", prompt.url, prompt.scheme),
            Style::default().fg(Color::DarkGray),
        ))),
        layout[0],
    );
    let hint_text = if (prompt.retry_notice) {
        format!("{}; authentication failed, try again", prompt.hint)
    } else {
        prompt.hint.clone()
    };
    frame.render_widget(
        Paragraph::new(hint_text).style(Style::default().fg(Color::Red)),
        layout[1],
    );

    frame.render_widget(
        Paragraph::new(auth_field_line("username: ", &prompt.username, !prompt.focus_password, false)),
        layout[3],
    );
    frame.render_widget(
        Paragraph::new(auth_field_line("password: ", &prompt.password, prompt.focus_password, true)),
        layout[4],
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" tab ", Style::default().fg(Color::Yellow)),
            Span::raw("switch field  "),
            Span::styled("enter ", Style::default().fg(Color::Yellow)),
            Span::raw("submit  "),
            Span::styled("esc ", Style::default().fg(Color::Yellow)),
            Span::raw("skip entry"),
        ])).style(Style::default().fg(Color::Gray)),
        layout[6],
    );
}

/// one labeled credential row. the focused field gets the block cursor;
/// masked fields render '*' per char whether focused or not.
fn auth_field_line(label: &'static str, field: &TextField, focused: bool, masked: bool) -> Line<'static> {
    let marker = if (focused) { "› " } else { "  " };
    let mut spans = vec![
        Span::styled(marker, Style::default().fg(Color::Yellow)),
        Span::raw(label),
    ];
    if (focused && masked) {
        spans.extend(render_masked_field_with_cursor(field));
    } else if (focused) {
        spans.extend(render_field_with_cursor(field));
    } else if (masked) {
        spans.push(Span::raw("*".repeat(field.buffer().chars().count())));
    } else {
        spans.push(Span::raw(field.buffer().to_string()));
    }
    Line::from(spans)
}
```

In `draw` (`src/tui.rs:3587`), add the overlay to both branches, each time directly after the
`draw_rename_confirm` call (lines 3594-3596 and 3609-3611):

```rust
        if (state.auth_prompt.is_some()) {
            draw_auth_prompt(frame, state);
        }
```

- [ ] **Step 6: Verify it compiles**

Run: `cargo build && cargo clippy`
Expected: clean build; no new clippy warnings.

Quick manual sanity (full auth flow is task 7): `cargo run -- tui`, add a normal magnet, confirm
nothing about the add flow changed.

- [ ] **Step 7: Commit**

```bash
git add src/tui.rs
git commit -m "tui: credentials overlay for AuthRequired adds"
```

---

## Task 7: full verification pass

**Files:** none (verification only; commit only if fixes were needed).

- [ ] **Step 1: Unit tests and lints**

```bash
cargo test
cargo clippy
```

Expected: every test passes (textfield, ipc, config, sources, layout, all pre-existing suites);
clippy is clean.

- [ ] **Step 2: Manual, http 401**

Save as `/tmp/auth_server.py` and run `python3 /tmp/auth_server.py` from a directory containing
any small `test.torrent`:

```python
# 401 until basic creds alice/hunter2 are presented
import base64, http.server

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        expected = "Basic " + base64.b64encode(b"alice:hunter2").decode()
        if self.headers.get("Authorization") != expected:
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="test"')
            self.end_headers()
            return
        super().do_GET()

http.server.HTTPServer(("127.0.0.1", 8018), Handler).serve_forever()
```

In the TUI (`cargo run -- tui`), press `a`, enter `http://127.0.0.1:8018/test.torrent` plus one
magnet as a second line, confirm the options form. Verify:

- [ ] the credentials overlay opens with the url, `http`, and "http 401 unauthorized"
- [ ] cursor movement, word ops (ctrl+arrows, ctrl+backspace), and ctrl+v paste work in both
      fields; the password renders as `*` per char with a visible block cursor
- [ ] wrong password: overlay stays open, username kept, password cleared, hint gains
      "authentication failed, try again"
- [ ] correct credentials (`alice` / `hunter2`): entry proceeds into the normal organize/confirm
      flow, then the magnet (second entry) dispatches too
- [ ] esc instead: summary reports the entry as "authentication cancelled" and the magnet still
      dispatches
- [ ] CLI: `cargo run -- add http://127.0.0.1:8018/test.torrent` prints the netrc guidance line

- [ ] **Step 3: Manual, netrc**

Write `~/.netrc` (mode 600) with `machine 127.0.0.1 login alice password hunter2`. Adding the
same url now authenticates with no prompt. Set `use_netrc = false` in the settings overlay
(security & anonymity tab) and confirm the prompt comes back. Restore your previous `~/.netrc`
state afterward.

- [ ] **Step 4: Manual, sftp (requires a local sshd and libcurl with libssh2)**

- [ ] daemon log shows `libcurl sftp support probed supported=true` at startup (check
      `journalctl` or the daemon's stderr)
- [ ] `sftp://user@127.0.0.1/path/test.torrent` with password auth: prompt opens (hint
      "login denied" path), correct password fetches
- [ ] with an ssh-agent key loaded (or a default `~/.ssh/id_rsa`), the same url fetches with no
      prompt (libcurl's native agent/default-key auth; no key-path config in v1, see header)
- [ ] on a libcurl without libssh2 (or by temporarily inverting the probe), an sftp add fails
      with the "requires libcurl built with libssh2" message and never prompts

- [ ] **Step 5: Commit any stragglers**

Only if steps above forced fixes:

```bash
git add -A src/
git commit -m "fix issues found in remote auth verification pass"
```
