# Remote fetch auth (http/ftp/sftp credentials): design

**Date:** 2026-07-01
**Status:** approved for planning

## Overview

Sub-project #5 of the five-part universal-input effort (see the 2026-06-30 spec's
overview). Today `src/sources.rs` fetches `http://`, `https://`, `ftp://`, and `sftp://`
torrent sources via the `curl` crate with zero auth options: a URL behind basic/digest
auth, an FTP login, or an SSH password simply fails with a raw curl error. This spec adds
the auth phase.

Shape of the solution, reusing patterns that already exist in the codebase:

- **A.** `sources.rs` grows a `FetchAuth` input (per-transfer credentials plus the
  config-derived netrc and ssh-key settings) applied to every curl handle, and a typed
  auth-required error the daemon can recognize.
- **B.** A two-phase IPC flow, structurally identical to the landed
  `Response::RenameConfirmation` round trip: an `Add` whose fetch fails on auth returns
  `Response::AuthRequired { url, scheme, hint }`; the TUI collects credentials and resends
  the same `Add` with `credentials: Option<TransferCredentials>` filled in.
- **C.** A TUI credentials overlay (username + password `TextField`s, password masked at
  the draw site) slotted into the existing overlay precedence next to `rename_confirm`.
- **D.** The only persistent credential paths: `~/.netrc` (curl-native, http/ftp) and a
  `ssh_private_key_path` config key for sftp keyfile auth. Passwords are never written to
  config.
- **E.** A cached probe for sftp support in the system libcurl, so a missing libssh2
  backend produces one clear error instead of a cryptic curl code.

The user-facing requirement "SSH auth takes over the UI unless the default
username+password flow can be handled natively" resolves to: **no UI takeover at all in
v1**. libcurl with libssh2 handles ssh password and keyboard-interactive auth natively
from `username()`/`password()`, and key/agent auth needs no interaction, so the in-app
prompt covers the whole default flow. There is no external `ssh` subprocess and no
terminal handover.

### Non-goals

- No remember-password option, no keyring/credential store, no per-host saved logins.
  `~/.netrc` and `ssh_private_key_path` are the only persistence, both curl-native.
- No interactive auth for the CLI (`monsoon add`), rss feeds, watch directories, or the
  ip-filter refresh. Those paths are non-interactive by nature; netrc and the ssh key
  config cover them. On auth failure they surface/log a clear error and move on.
- No proxy-auth changes: `proxy_username`/`proxy_password` already exist in config and
  apply at the session level. http 407 stays a plain error.
- No parsing of `WWW-Authenticate` headers (realm display, scheme negotiation UI). curl
  negotiates basic-vs-digest internally; the hint string is derived from the curl error.
- No password zeroization (see Risks).
- No raw `curl-sys` setopt calls. If the safe crate doesn't wrap an option we want, the
  option is dropped from v1, not hand-rolled through unsafe (per the safe-rust constraint).

---

## A. Fetch auth plumbing in `sources.rs`

`fetch_url` and `fetch_to_string` (`src/sources.rs:203` / `:180`) currently build their
`Easy` handle from nothing but the URL. Both gain a `&FetchAuth` parameter, and
`resolve()` (`src/sources.rs:72`) passes one through:

```rust
/// everything a fetch needs to authenticate. built by the caller from
/// config plus any per-transfer credentials from the ipc request.
pub struct FetchAuth {
    pub credentials: Option<TransferCredentials>,  // from src/ipc.rs
    pub use_netrc: bool,
    pub ssh_private_key_path: String,
}
```

Applied to the handle, in full:

- `credentials` present: `easy.username(..)` + `easy.password(..)`, and for http/https
  additionally `easy.http_auth(Auth::new().basic(true).digest(true))` so curl negotiates
  whichever the server offers. For ftp the username/password are the login; for sftp
  libcurl maps them to ssh password and keyboard-interactive auth natively.
- `use_netrc` true: `easy.netrc(NetRc::Optional)`; false: `NetRc::Ignored`. Explicit
  credentials take precedence over a netrc entry, which is curl's documented behavior for
  `Optional`.
- scheme is sftp and `ssh_private_key_path` non-empty: `easy.ssh_private_key(path)`.
  `ssh_auth_types` is left at libcurl's default (any), so agent auth on unix, the keyfile,
  and password auth are all tried in libcurl's normal order. No public-key path option:
  libcurl derives it.
- sftp known-hosts: at impl time, check whether the `curl` crate wraps
  `CURLOPT_SSH_KNOWNHOSTS` (candidate name `ssh_known_hosts`). If it does, point it at
  `~/.ssh/known_hosts` when that file exists. If it does not, v1 ships without host-key
  verification for sftp fetches (recorded in Risks; the fallback of raw `curl-sys` setopt
  is explicitly out of scope).

URL-embedded credentials (`ftp://user:pass@host/`) keep working untouched; curl parses
them itself and they simply pre-empt the auth-required error.

**Callers updated** (all four, so the plumbing is done once in the shared functions):

| Call site | `credentials` | source of `use_netrc` / key path |
|---|---|---|
| `Request::Add` handler (`src/server.rs:1201`) | from the request | daemon config |
| `poll_rss_feeds` (`src/server.rs:571`) and `rss::poll_feed` (`src/rss.rs:79`) | `None` | daemon config |
| `refresh_ip_filter` (`src/server.rs:1742`) | `None` | daemon config |
| CLI-side resolve, if any path resolves client-side | `None` | client config defaults |

**Typed auth-required error.** `sources.rs` defines a marker error so the Add handler can
tell "needs credentials" apart from "network broke", while everything stays `anyhow`:

```rust
/// fetch failed because the server wants credentials. the add handler
/// downcasts to this to emit Response::AuthRequired instead of Err.
#[derive(Debug)]
pub struct AuthRequiredError {
    pub scheme: String,  // literal url scheme, lowercase: "http", "https", "ftp", "sftp"
    pub hint: String,    // human text, e.g. "http 401 unauthorized", "login denied"
}
```

(implements `Display` + `std::error::Error`; raised via `anyhow::bail!`/`Err(..into())`,
recovered via `error.downcast_ref::<AuthRequiredError>()` at the single call site that
cares.)

**Classification rules**, applied when `transfer.perform()` fails:

- http/https: `error.is_http_returned_error()` and `easy.response_code()` is 401. Any
  other status (403, 407, 5xx) stays a plain error; 401 is the only "credentials would
  help" signal worth reacting to.
- ftp/sftp: raw curl code 67, `CURLE_LOGIN_DENIED` (`error.code() == 67`, local constant
  with a comment; the crate exposes the raw code so no `curl-sys` import is needed).
  libssh2 auth failures and ftp login rejections both land here.

---

## B. Two-phase `AuthRequired` flow (daemon)

Mirrors the landed rename-confirmation shape (`Response::RenameConfirmation`,
`src/ipc.rs:297`): first call fails soft with a structured "I need more from you"
response, resend carries the answers.

1. TUI sends `Request::Add` as today, `credentials: None`.
2. The daemon's Add handler calls `sources::resolve` with a `FetchAuth` built from config
   plus the request credentials. On error it downcasts: `AuthRequiredError` becomes
   `Response::AuthRequired { url, scheme, hint }` (where `url` is the original input uri);
   anything else stays `Response::Err` exactly as today.
3. TUI prompts (section C) and resends the identical `Add` with
   `credentials: Some(TransferCredentials { username, password })`.
4. The daemon uses the credentials for that one transfer and drops them; nothing is stored
   server-side. A resend that fails auth again just produces another `AuthRequired`
   (fresh hint), so wrong passwords loop back to the prompt naturally; there is no retry
   cap because Esc cancels client-side.
5. On success the fetched file proceeds through the normal add path unchanged: organize
   step, rename confirmations, `FinalizeAdd`, all as landed.

IPC additions (`src/ipc.rs`):

```rust
/// one-shot credentials for a single fetch. never persisted anywhere.
#[derive(Clone, Serialize, Deserialize)]
pub struct TransferCredentials {
    pub username: String,
    pub password: String,
}
// manual Debug impl redacts the password: Request derives Debug and may be
// logged; the password must never reach a log line.

// Request::Add gains:
//   #[serde(default)] credentials: Option<TransferCredentials>,
// Response gains:
//   AuthRequired { url: String, scheme: String, hint: String },
```

`#[serde(default)]` keeps old clients compatible, same as the `decisions` fields.

Non-interactive surfaces handle the new response without prompting:

- CLI: `display::print_response` (`src/display.rs:31`) gains an arm printing
  `authentication required for <url> (<hint>); add an entry to ~/.netrc or use the tui`.
- rss / watch / ip-filter never send credentials, so they never see `AuthRequired` (they
  call `resolve`/`fetch_url` directly and just log the downcast-formatted error).

---

## C. TUI credentials prompt

New overlay state on `AppState`, modeled on `rename_confirm` (`src/tui.rs:1135`):

```rust
struct AuthPrompt {
    url: String,
    scheme: String,
    hint: String,
    username: TextField,          // CompletionSource::None
    password: TextField,          // CompletionSource::None
    focus_password: bool,
    entry: (String, AddOptions),  // the uri + options being retried
    remaining: AddOptionsForm,    // not-yet-dispatched tail of the batch
}
```

**Trigger.** `dispatch_add_options` (`src/tui.rs:2558`) is the TUI's only `Request::Add`
call site. When an entry's Add returns `Response::AuthRequired`, the loop stops: the
current entry plus the undispatched tail of the form are stashed into
`state.auth_prompt`, and the overlay opens. Entries already dispatched keep their results.

**Keys.** Tab / Up / Down switch fields; both fields are ordinary `TextField`s with the
full landed editing behavior (cursor, word ops, paste). Enter resends `Request::Add` for
`entry` with the typed credentials:

- `Response::Added`: close the prompt, apply the entry's post-add tweaks
  (sequential/first-last/organize bookkeeping), then continue dispatching `remaining` by
  re-entering `dispatch_add_options` with it.
- another `Response::AuthRequired`: keep the prompt open, keep the username, clear the
  password, show the new hint plus "authentication failed, try again".
- `Response::Err`: record it as a failure line for that entry (same formatting as today)
  and continue with `remaining`.

Esc skips the entry (recorded as a failure: "authentication cancelled") and continues
with `remaining`.

**Masking.** The password draw site renders `"*".repeat(field.buffer().chars().count())`
and places the cursor by char offset as usual; `TextField` itself is untouched, exactly
as decided in the requirements. No reveal toggle in v1.

**Precedence.** The key-routing chain at `src/tui.rs:1527` (rename_confirm, then prompt,
then priority step) gains `auth_prompt` at the same rank as `rename_confirm`; the two can
never be active simultaneously because auth happens before the torrent exists.

**sftp gating.** The prompt only ever opens in reaction to a daemon `AuthRequired`, and
the daemon never emits `AuthRequired` for sftp when the backend is missing (section E
errors out first), so "only offer the sftp prompt when sftp is supported" falls out with
no TUI-side probe.

---

## D. Persistent auth paths: config keys

Two new `Config` fields (`src/config.rs`), both wired into `apply_config_change`
(`src/server.rs:352`) and the settings overlay's `security & anonymity` section (the
existing `SettingField` list, next to the proxy credentials around `src/tui.rs:440`):

| Field | Type | Default | Meaning |
|---|---|---|---|
| `use_netrc` | bool | `true` | pass `NetRc::Optional` to curl so `~/.netrc` entries authenticate http/ftp fetches automatically |
| `ssh_private_key_path` | String | `""` | private key file for sftp fetches; empty means agent/default behavior only |

Both are read per fetch, so `restart_required: false`. `ssh_private_key_path` gets
`CompletionSource::Filesystem` in the settings overlay, same as `network_cert_path`.

`use_netrc` defaults on because a netrc file is opt-in by existing (no file, no effect),
it is the standard unix answer to "scripted credentials without plaintext in app config",
and it keeps the rss/watch/CLI paths usable against authenticated hosts with zero new
machinery. Users who consider `~/.netrc` itself a liability can switch it off.

Passwords typed into the prompt are never written to config, config.toml never gains a
password field, and there is no "remember" checkbox to accidentally tick.

---

## E. sftp support probe

The system libcurl only speaks sftp when built against libssh2. Probe once,
daemon-side:

- `sources.rs` holds a `static SFTP_SUPPORTED: OnceLock<bool>`, filled from
  `curl::Version::get().protocols()` containing `"sftp"`.
- the daemon logs the result once at startup (info line), so a missing backend is
  visible before anyone tries an sftp url.
- `resolve()` checks the flag before fetching any `sftp://` input and bails with:
  `sftp support requires libcurl built with libssh2; this system's libcurl does not
  provide it`. This is a plain `Response::Err`, never `AuthRequired`.

---

## Decisions

Every open choice, decided so implementation needs no further input. The
security-sensitive ones lean safe per the project's security priorities.

| # | Decision | Rationale |
|---|---|---|
| 1 | Two-phase `AuthRequired` round trip instead of daemon-side blocking prompt or pre-emptive credential entry | matches the landed `RenameConfirmation` pattern exactly; daemon and TUI are separate processes |
| 2 | No credential persistence in config, no remember option in v1 | plaintext passwords in config.toml is the one outcome this design must prevent; netrc and ssh keys already solve "don't ask me again" |
| 3 | `use_netrc` default `true` via `NetRc::Optional` | inert unless the user creates a netrc; standard tooling behavior; enables non-interactive paths |
| 4 | `ssh_private_key_path` config key, empty default; no public-key or auth-type knobs | keyfile auth is the portable path (windows has no standard agent socket); libcurl derives the rest |
| 5 | No UI takeover for ssh; native username/password through libcurl | libssh2 handles password and keyboard-interactive from `CURLOPT_PASSWORD`, covering the default flow the requirement prefers |
| 6 | sftp probed via `curl::Version::get().protocols()`, cached in a `OnceLock`, logged at startup | one clear error instead of curl code 1; probe is static per process |
| 7 | Auth detection: http 401 only, plus curl code 67 for ftp/sftp; local `67` constant | the only codes where credentials plausibly fix the failure; avoids importing `curl-sys` for one constant |
| 8 | `AuthRequiredError` marker type recovered by `downcast_ref` | keeps `anyhow` everywhere; exactly one call site needs to distinguish it |
| 9 | `http_auth(basic + digest)` set only when credentials are present | lets curl negotiate; never volunteers a basic header on an unauthenticated first attempt |
| 10 | `scheme` in `AuthRequired` is the literal lowercase url scheme string | display-only field; an enum adds ceremony for zero behavior |
| 11 | `hint` derived from the curl error, no `WWW-Authenticate` parsing | realm display is cosmetic; header capture is real added machinery |
| 12 | Wrong password loops the prompt with no retry cap; Esc cancels | user-paced; a cap invents a failure mode nobody asked for |
| 13 | CLI prints guidance on `AuthRequired`, no interactive prompt | CLI add is scriptable/non-interactive today; netrc is the scripted answer |
| 14 | `TransferCredentials` gets a manual `Debug` impl redacting the password | `Request` derives `Debug`; one impl guarantees no log line ever carries a password |
| 15 | sftp known-hosts only if the safe curl crate wraps it; otherwise dropped from v1 | safe-rust constraint outranks it; recorded in Risks with the upgrade path |
| 16 | Password masking at the draw site (`*` per char), `TextField` untouched | keeps exactly one text-editing implementation, per the universal-input goal |

---

## IPC / data-model summary

**Changed request (`src/ipc.rs`):** `Add` gains
`#[serde(default)] credentials: Option<TransferCredentials>`.

**New types:** `TransferCredentials { username, password }` (manual redacting `Debug`).

**New response:** `AuthRequired { url: String, scheme: String, hint: String }`.

**New config fields (`src/config.rs`):** `use_netrc: bool` (default true),
`ssh_private_key_path: String` (default empty); both in `apply_config_change` and the
settings overlay's security section.

**`sources.rs` API:** `resolve`, `fetch_url`, `fetch_to_string` gain a `&FetchAuth`
parameter; new `AuthRequiredError`; new sftp-support probe.

---

## Testing

- **Unit (Rust):**
  - classification: synthetic curl error codes / response codes map to
    `AuthRequiredError` for (http, 401) and (ftp/sftp, 67), and to plain errors for 403,
    407, timeouts, and code 1.
  - `AuthRequiredError` survives the `anyhow` downcast round trip.
  - ipc serde: old-client `Add` json without `credentials` still deserializes;
    `AuthRequired` round-trips; `format!("{:?}", transfer_credentials)` does not contain
    the password.
  - `FetchAuth` construction from config: netrc flag and key-path mapping.
- **Manual (TUI):** against local servers (a python http.server wrapper returning 401
  until basic creds match; system sshd for sftp password and keyfile):
  - http 401 opens the prompt; correct creds proceed into the normal organize/confirm
    flow; wrong creds re-prompt with username kept and password cleared; Esc skips and
    the rest of a multi-entry batch still dispatches.
  - password renders masked, cursor and word-ops work in both fields.
  - a matching `~/.netrc` entry authenticates with no prompt; `use_netrc = false`
    brings the prompt back.
  - sftp: password auth via prompt; keyfile auth via `ssh_private_key_path` with no
    prompt; forcing the probe false yields the libssh2 error message, not a prompt.
  - CLI add against the 401 server prints the netrc guidance.

## Risks / notes

- **Passwords live in ordinary `String`s** (TextField buffer, ipc struct, curl handle)
  and are not zeroized on drop; they also transit the IPC channel as plaintext json.
  Acceptable for the local unix socket (same-user, permission-guarded); for the network
  IPC mode the credentials ride the same token-authenticated channel as everything else,
  which `src/client.rs:97` already documents as "trusted transport only". Zeroization is
  a known v1 limitation; revisit with a `zeroize`-backed buffer if it ever matters.
- **No sftp host-key verification in v1** if the curl crate turns out not to wrap
  `CURLOPT_SSH_KNOWNHOSTS`: a mitm on the sftp fetch path would go undetected. Upgrade
  path: the wrapped option if it appears upstream, or a reviewed `curl-sys` exception.
  Torrent payload integrity is still protected downstream by infohash verification.
- **Redaction is one manual impl away from regressing:** any future field rename or
  derive cleanup on `TransferCredentials` must keep the manual `Debug`. The unit test
  above pins it.
- The retried fetch downloads the whole file again after a 401/login failure. Auth
  failures happen before payload transfer, so the duplicate cost is a handshake, not a
  download.
