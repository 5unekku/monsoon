use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table,
    TableState, Tabs,
};
use std::io::{Stdout, stdout};
use std::time::{Duration, Instant};

use crate::client;
use crate::config::Config;
use crate::ipc::{
    ContentLayout, FeedInfo, PeerInfo as IpcPeerInfo, Request, Response, StatsInfo, TorrentDetail,
    TorrentInfo, TrackerInfo,
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const DETAIL_POLL_INTERVAL: Duration = Duration::from_millis(250);
const EVENT_TICK: Duration = Duration::from_millis(100);

pub fn run() -> Result<()> {
    let mut terminal = setup_terminal().context("set up terminal")?;
    let result = run_loop(&mut terminal);
    let _ = restore_terminal(&mut terminal);
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enable raw mode")?;
    let mut out = stdout();
    // EnableMouseCapture switches the terminal into SGR mouse mode (xterm
    // extension; supported by every modern terminal). releases on restore.
    execute!(out, EnterAlternateScreen, EnableMouseCapture)
        .context("enter alt screen + mouse")?;
    let backend = CrosstermBackend::new(out);
    Terminal::new(backend).context("create terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), DisableMouseCapture, LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    Ok(())
}

/// which pane currently consumes nav input. cycled by tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Sidebar,
    List,
    Detail,
}

/// columns selectable in the torrent list. only columns whose data the
/// daemon actually exposes today are listed here. the qBT picker has more
/// (popularity, ratio limit, last seen complete, reannounce in, etc.) —
/// add them as the bridge surfaces the underlying fields.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Column {
    Index,
    Name,
    State,
    Progress,
    Down,
    Up,
    Peers,
    Seeds,
    Size,
    Downloaded,
    Uploaded,
    AddedOn,
    CompletedOn,
    SavePath,
    Category,
    Tags,
    InfoHash,
}

impl Column {
    const ALL: [Column; 17] = [
        Column::Index, Column::Name, Column::State, Column::Progress,
        Column::Down, Column::Up, Column::Peers, Column::Seeds,
        Column::Size, Column::Downloaded, Column::Uploaded,
        Column::AddedOn, Column::CompletedOn, Column::SavePath,
        Column::Category, Column::Tags, Column::InfoHash,
    ];

    fn key(&self) -> &'static str {
        match self {
            Column::Index => "index",
            Column::Name => "name",
            Column::State => "state",
            Column::Progress => "progress",
            Column::Down => "down",
            Column::Up => "up",
            Column::Peers => "peers",
            Column::Seeds => "seeds",
            Column::Size => "size",
            Column::Downloaded => "downloaded",
            Column::Uploaded => "uploaded",
            Column::AddedOn => "added_on",
            Column::CompletedOn => "completed_on",
            Column::SavePath => "save_path",
            Column::Category => "category",
            Column::Tags => "tags",
            Column::InfoHash => "info_hash",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Column::Index => "index",
            Column::Name => "name",
            Column::State => "status",
            Column::Progress => "progress",
            Column::Down => "down",
            Column::Up => "up",
            Column::Peers => "peers",
            Column::Seeds => "seeds",
            Column::Size => "size",
            Column::Downloaded => "downloaded",
            Column::Uploaded => "uploaded",
            Column::AddedOn => "added on",
            Column::CompletedOn => "completed on",
            Column::SavePath => "save path",
            Column::Category => "category",
            Column::Tags => "tags",
            Column::InfoHash => "info hash",
        }
    }

    /// default width in cells (used when no per-column override is set in
    /// config). all columns use a concrete number so the manual layout +
    /// drag-resize math has predictable inputs.
    fn default_width_cells(&self) -> u16 {
        match self {
            // widths chosen so the full (non-abbreviated) label fits and the
            // typical value also fits without ellipsis. drag-resize lets the
            // user adjust per-column.
            Column::Index => 6,
            Column::Name => 28,
            Column::State => 12,       // longest is "downloading metadata" (20) — user can drag wider
            Column::Progress => 9,     // "progress" label + "100.0%" value
            Column::Down | Column::Up => 12,
            Column::Peers => 9,
            Column::Seeds => 9,
            Column::Size => 10,
            Column::Downloaded | Column::Uploaded => 12,
            Column::AddedOn | Column::CompletedOn => 19,
            Column::SavePath => 28,
            Column::Category => 12,
            Column::Tags => 14,
            Column::InfoHash => 40,
        }
    }

    fn render(&self, index: usize, torrent: &TorrentInfo, nerd_font: bool) -> String {
        match self {
            Column::Index => index.to_string(),
            Column::Name => torrent.name.clone(),
            Column::State => format_state_with(&torrent.state, torrent.is_paused, nerd_font),
            Column::Progress => format!("{:>5.1}%", torrent.progress * 100.0),
            Column::Down => crate::display::format_rate(torrent.download_rate),
            Column::Up => crate::display::format_rate(torrent.upload_rate),
            Column::Peers => format!("{}/{}", torrent.connected_peers, torrent.total_peers),
            Column::Seeds => format!("{}/{}", torrent.connected_seeds, torrent.total_seeds),
            Column::Size => crate::display::format_bytes(torrent.total_wanted),
            Column::Downloaded => crate::display::format_bytes(torrent.total_download),
            Column::Uploaded => crate::display::format_bytes(torrent.total_upload),
            Column::AddedOn => crate::display::format_timestamp(torrent.added_time),
            Column::CompletedOn => crate::display::format_timestamp(torrent.completed_time),
            Column::SavePath => torrent.save_path.clone(),
            Column::Category => torrent.category.clone().unwrap_or_default(),
            Column::Tags => torrent.tags.iter().cloned().collect::<Vec<_>>().join(","),
            Column::InfoHash => torrent.info_hash.clone(),
        }
    }

    fn from_key(key: &str) -> Option<Column> {
        Column::ALL.iter().copied().find(|column| column.key() == key)
    }
}

const DEFAULT_COLUMNS: &[Column] = &[
    Column::Index, Column::Name, Column::State, Column::Progress,
    Column::Down, Column::Up, Column::Peers,
];

/// minimum column width — even a fully-dragged-down column keeps this many
/// cells so its label remains at least partially legible.
const MIN_COLUMN_WIDTH: u16 = 3;

/// compute the on-screen widths for every visible column, totalling exactly
/// `available - (n - 1)` cells (one cell between each adjacent pair of
/// columns is reserved for the divider character). the LAST column absorbs
/// any leftover space so the right edge of the table always reaches the
/// right border of the pane.
fn compute_column_widths(
    visible: &[Column],
    overrides: &std::collections::BTreeMap<String, u16>,
    available: u16,
) -> Vec<u16> {
    let count = visible.len();
    if (count == 0) { return Vec::new(); }
    let separator_cells = (count.saturating_sub(1)) as u16;
    let usable = available.saturating_sub(separator_cells);
    let mut widths = vec![0u16; count];
    let mut consumed: u16 = 0;
    // all but the last column take their override-or-default
    let fixed_count = count.saturating_sub(1);
    for index in 0..fixed_count {
        let column = visible[index];
        let proposed = overrides.get(column.key()).copied()
            .unwrap_or_else(|| column.default_width_cells())
            .max(MIN_COLUMN_WIDTH);
        let remaining = usable.saturating_sub(consumed);
        // leave at least MIN_COLUMN_WIDTH for the last column
        let cap = remaining.saturating_sub(MIN_COLUMN_WIDTH);
        let actual = proposed.min(cap);
        widths[index] = actual;
        consumed += actual;
    }
    widths[count - 1] = usable.saturating_sub(consumed).max(MIN_COLUMN_WIDTH);
    widths
}

/// active drag-to-resize on a column header divider.
#[derive(Clone, Copy)]
struct ColumnDrag {
    /// index in `visible_columns` of the column whose right edge is being dragged.
    /// dragging this divider stretches column[index] and shrinks column[index+1]
    /// (the column to the left of the divider is the one being resized).
    column_index: usize,
    /// terminal column of the cursor when the drag started
    start_x: u16,
    /// width of the dragged column at drag start
    start_width: u16,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusFilter {
    All,
    Downloading,
    Seeding,
    Completed,
    Paused,
    Checking,
    Errored,
}

impl StatusFilter {
    const ALL: [StatusFilter; 7] = [
        StatusFilter::All,
        StatusFilter::Downloading,
        StatusFilter::Seeding,
        StatusFilter::Completed,
        StatusFilter::Paused,
        StatusFilter::Checking,
        StatusFilter::Errored,
    ];

    fn label(&self) -> &'static str {
        match self {
            StatusFilter::All => "all",
            StatusFilter::Downloading => "downloading",
            StatusFilter::Seeding => "seeding",
            StatusFilter::Completed => "completed",
            StatusFilter::Paused => "paused",
            StatusFilter::Checking => "checking",
            StatusFilter::Errored => "errored",
        }
    }

    fn matches(&self, torrent: &TorrentInfo) -> bool {
        let has_error = !torrent.error.is_empty() && torrent.error != "No error";
        match self {
            StatusFilter::All => true,
            StatusFilter::Downloading => !torrent.is_paused && torrent.state == "downloading",
            StatusFilter::Seeding => !torrent.is_paused && torrent.is_seeding,
            StatusFilter::Completed => torrent.is_finished && !torrent.is_paused,
            StatusFilter::Paused => torrent.is_paused,
            StatusFilter::Checking => {
                torrent.state == "checking_files" || torrent.state == "checking_resume_data"
            }
            StatusFilter::Errored => has_error,
        }
    }
}

/// settings field types — drives both the renderer and the input handler.
#[derive(Clone, Copy)]
enum FieldKind {
    Bool,
    Integer,
    /// integer with three meaningful states displayed differently:
    /// -1 = "∞ unlimited", 0 = "0 (none allowed)", 1+ = literal count.
    /// used for max_active_* (libtorrent accepts -1 directly) and the
    /// connection caps max_connections / max_uploads (where session.rs
    /// maps -1 → 65535 for libtorrent).
    IntegerUnlimited,
    Float,
    Text,
    /// dropdown of fixed string options; enter cycles to the next one
    Choice(&'static [&'static str]),
    /// network-interface dropdown. enter opens a picker populated from
    /// `enumerate_interfaces()` plus an "any" sentinel and a "specific ip"
    /// escape hatch that drops into the text editor.
    Interface,
}

struct SettingField {
    section: &'static str,
    /// must match a key accepted by `apply_config_change` in server.rs
    key: &'static str,
    label: &'static str,
    description: &'static str,
    kind: FieldKind,
    /// true if changing this value only takes effect on daemon restart
    restart_required: bool,
    /// true for Vec<String> fields (newline-joined) — renders as an inline
    /// list with add/remove/edit controls instead of a single text editor
    is_list: bool,
}

/// schema for the settings page. ordering matters: security & anonymity first,
/// then connection (interface binding, port forwarding), then everything else.
/// see plans/TODO-tui.md and the security-anonymity-priorities memory.
const SETTING_FIELDS: &[SettingField] = &[
    // ── security & anonymity (first — defaults bias toward safety) ──
    SettingField {
        section: "security & anonymity",
        key: "anonymous_mode",
        label: "anonymous mode",
        description: "fingerprint reduction: blanks the client name in peer-id, drops the http user-agent on tracker announces, disables LSD and UPnP/NAT-PMP, and suppresses optional protocol features that identify the client. independent of encryption.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "encryption_mode",
        label: "encryption mode",
        description: "protocol encryption between peers. 'forced' refuses plaintext peers entirely (recommended). independent of anonymous mode — does not affect tracker traffic or fingerprinting.",
        kind: FieldKind::Choice(&["enabled", "forced", "disabled"]),
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "ssrf_mitigation",
        label: "ssrf mitigation",
        description: "reject tracker responses that redirect to private/local addresses.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "validate_https_tracker_certificate",
        label: "validate https tracker cert",
        description: "verify TLS certificates for HTTPS trackers.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "announce_to_all_trackers",
        label: "announce to all trackers",
        description: "announce to every tracker rather than stopping at the first success.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "announce_to_all_tiers",
        label: "announce to all tiers",
        description: "announce to all tracker tiers even when an earlier tier succeeds.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "proxy_type",
        label: "proxy type",
        description: "route traffic through a proxy. hard-fail semantics: if the proxy is unreachable at startup the daemon refuses to start rather than leaking on the bare interface.",
        kind: FieldKind::Choice(&["none", "socks4", "socks5", "socks5_pw", "http", "http_pw", "i2p"]),
        restart_required: true,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "proxy_host",
        label: "proxy host",
        description: "hostname or ip of the proxy server.",
        kind: FieldKind::Text,
        restart_required: true,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "proxy_port",
        label: "proxy port",
        description: "port of the proxy server.",
        kind: FieldKind::Integer,
        restart_required: true,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "proxy_username",
        label: "proxy username",
        description: "username for socks5_pw / http_pw proxy types.",
        kind: FieldKind::Text,
        restart_required: true,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "proxy_password",
        label: "proxy password",
        description: "password for socks5_pw / http_pw proxy types.",
        kind: FieldKind::Text,
        restart_required: true,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "proxy_peer_connections",
        label: "proxy peer traffic",
        description: "route peer-to-peer connections through the proxy. enable this to prevent peer ip leaks.",
        kind: FieldKind::Bool,
        restart_required: true,
        is_list: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "proxy_tracker_connections",
        label: "proxy tracker traffic",
        description: "route tracker announces and scrapes through the proxy.",
        kind: FieldKind::Bool,
        restart_required: true,
        is_list: false,
    },

    // ── connection (interface binding is a vpn kill-switch) ──
    SettingField {
        section: "connection",
        key: "listen_address",
        label: "listen address (interface)",
        description: "bind to a specific NIC (e.g. wireguard's interface) to kill-switch traffic if the vpn drops. pick from available interfaces or choose 'specific ip' to enter a raw address. requires daemon restart.",
        kind: FieldKind::Interface,
        restart_required: true,
        is_list: false,
    },
    SettingField {
        section: "connection",
        key: "listen_port",
        label: "listen port",
        description: "incoming peer port. requires daemon restart to re-bind.",
        kind: FieldKind::Integer,
        restart_required: true,
        is_list: false,
    },
    SettingField {
        section: "connection",
        key: "enable_upnp",
        label: "upnp port forwarding",
        description: "automatic LAN router port forwarding via UPnP. opt-in.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "connection",
        key: "enable_natpmp",
        label: "nat-pmp port forwarding",
        description: "automatic LAN router port forwarding via NAT-PMP. opt-in.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "connection",
        key: "max_connections",
        label: "max connections",
        description: "global peer connection ceiling. -1 means unlimited; 0 means none allowed.",
        kind: FieldKind::IntegerUnlimited,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "connection",
        key: "max_uploads",
        label: "max upload slots",
        description: "global upload slot ceiling. -1 means unlimited; 0 means none allowed.",
        kind: FieldKind::IntegerUnlimited,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "connection",
        key: "download_rate_limit",
        label: "download cap (KiB/s)",
        description: "global download rate ceiling in KiB/s. 0 means unlimited.",
        kind: FieldKind::Integer,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "connection",
        key: "upload_rate_limit",
        label: "upload cap (KiB/s)",
        description: "global upload rate ceiling in KiB/s. 0 means unlimited.",
        kind: FieldKind::Integer,
        restart_required: false,
        is_list: false,
    },

    // ── bittorrent ──
    SettingField {
        section: "bittorrent",
        key: "enable_dht",
        label: "dht",
        description: "distributed hash table for trackerless discovery.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "bittorrent",
        key: "enable_lsd",
        label: "local service discovery",
        description: "find peers on the same LAN via multicast.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "bittorrent",
        key: "enable_incoming_utp",
        label: "incoming µTP",
        description: "accept incoming µTP (UDP) connections.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "bittorrent",
        key: "enable_outgoing_utp",
        label: "outgoing µTP",
        description: "open outgoing connections over µTP.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },

    // ── limits ──
    SettingField {
        section: "limits",
        key: "max_active_downloads",
        label: "max active downloads",
        description: "concurrent active downloads. -1 = unlimited; 0 = none allowed (queue but never start); 1+ = literal cap.",
        kind: FieldKind::IntegerUnlimited,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "limits",
        key: "max_active_uploads",
        label: "max active uploads",
        description: "concurrent active uploads/seeds. -1 = unlimited; 0 = none allowed; 1+ = literal cap.",
        kind: FieldKind::IntegerUnlimited,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "limits",
        key: "max_active_torrents",
        label: "max active torrents",
        description: "concurrent active torrents (downloads + uploads). -1 = unlimited; 0 = none allowed; 1+ = literal cap.",
        kind: FieldKind::IntegerUnlimited,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "limits",
        key: "seed_ratio_limit",
        label: "seed ratio limit",
        description: "stop seeding at this ratio. 0 means unlimited.",
        kind: FieldKind::Float,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "limits",
        key: "seed_time_limit",
        label: "seed time limit (minutes)",
        description: "stop seeding after this many minutes. 0 means unlimited.",
        kind: FieldKind::Integer,
        restart_required: false,
        is_list: false,
    },

    // ── paths ──
    SettingField {
        section: "paths",
        key: "default_save_path",
        label: "default save path",
        description: "where new torrents save their files by default.",
        kind: FieldKind::Text,
        restart_required: false,
        is_list: false,
    },
    SettingField {
        section: "paths",
        key: "watch_directories",
        label: "watch directories",
        description: "directories the daemon watches for .torrent files to auto-add. one path per entry.",
        kind: FieldKind::Text,
        restart_required: false,
        is_list: true,
    },

    // ── general ──
    SettingField {
        section: "general",
        key: "autostart",
        label: "start on login",
        description: "launch the monsoon daemon automatically when you log in. on linux, registers with the running init system (systemd, runit, or dinit) or falls back to an xdg .desktop entry. on windows, adds a registry run key.",
        kind: FieldKind::Bool,
        restart_required: false,
        is_list: false,
    },
];

fn config_value_string(config: &Config, key: &str) -> String {
    match key {
        "listen_address" => config.listen_address.clone(),
        "listen_port" => config.listen_port.to_string(),
        "max_connections" => config.max_connections.to_string(),
        "max_uploads" => config.max_uploads.to_string(),
        "download_rate_limit" => config.download_rate_limit.to_string(),
        "upload_rate_limit" => config.upload_rate_limit.to_string(),
        "default_save_path" => config.default_save_path.clone(),
        "enable_dht" => config.enable_dht.to_string(),
        "enable_lsd" => config.enable_lsd.to_string(),
        "enable_upnp" => config.enable_upnp.to_string(),
        "enable_natpmp" => config.enable_natpmp.to_string(),
        "anonymous_mode" => config.anonymous_mode.to_string(),
        "encryption_mode" => config.encryption_mode.clone(),
        "ssrf_mitigation" => config.ssrf_mitigation.to_string(),
        "validate_https_tracker_certificate" => config.validate_https_tracker_certificate.to_string(),
        "enable_incoming_utp" => config.enable_incoming_utp.to_string(),
        "enable_outgoing_utp" => config.enable_outgoing_utp.to_string(),
        "announce_to_all_trackers" => config.announce_to_all_trackers.to_string(),
        "announce_to_all_tiers" => config.announce_to_all_tiers.to_string(),
        "max_active_downloads" => config.max_active_downloads.to_string(),
        "max_active_uploads" => config.max_active_uploads.to_string(),
        "max_active_torrents" => config.max_active_torrents.to_string(),
        "seed_ratio_limit" => config.seed_ratio_limit.to_string(),
        "seed_time_limit" => config.seed_time_limit.to_string(),
        "proxy_type" => config.proxy_type.clone(),
        "proxy_host" => config.proxy_host.clone(),
        "proxy_port" => config.proxy_port.to_string(),
        "proxy_username" => config.proxy_username.clone(),
        "proxy_password" => config.proxy_password.clone(),
        "proxy_peer_connections" => config.proxy_peer_connections.to_string(),
        "proxy_tracker_connections" => config.proxy_tracker_connections.to_string(),
        "autostart" => crate::autostart::is_enabled().to_string(),
        _ => String::new(),
    }
}

struct SettingsState {
    config: Config,
    /// index into SETTING_FIELDS — always points at a field in the current tab
    selected: usize,
    /// index into the unique sections list (the "tabs")
    current_tab: usize,
    /// when Some, an inline editor for the selected field's value is active
    edit_buffer: Option<String>,
    /// last action outcome (success message or daemon error)
    status: Option<String>,
    /// scroll offset for the settings body (in terms of display lines)
    scroll: u16,
    /// when Some, an interface dropdown is open for the current field
    interface_picker: Option<InterfacePickerState>,
    /// current contents of the watch_directories list
    watch_dir_list: Vec<String>,
    /// which row in watch_dir_list is highlighted
    watch_dir_selected: usize,
    /// true when the user is editing a watch dir entry inline
    watch_dir_editing: bool,
    /// text buffer while watch_dir_editing is true
    watch_dir_buffer: String,
}

/// list of options shown by the network-interface dropdown. each entry's
/// `value` is what gets written to config (an empty string means "any";
/// the magic `__specific__` value triggers a switch to text-edit mode).
struct InterfacePickerState {
    items: Vec<(String, String)>,  // (display label, persisted value)
    selected: usize,
}

impl InterfacePickerState {
    fn build() -> Self {
        let mut items: Vec<(String, String)> = Vec::new();
        items.push(("any (all interfaces)".to_string(), String::new()));
        for (name, ip) in crate::sources::enumerate_interfaces() {
            items.push((format!("{}  ({})", name, ip), name));
        }
        items.push(("specific ip…".to_string(), "__specific__".to_string()));
        Self { items, selected: 0 }
    }
}

/// distinct sections in SETTING_FIELDS order. each section becomes one tab
/// in the settings overlay. computed once at startup; if you add a new
/// section to SETTING_FIELDS, append it here.
fn section_tabs() -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for field in SETTING_FIELDS {
        if (!seen.contains(&field.section)) { seen.push(field.section); }
    }
    seen
}

/// indices into SETTING_FIELDS that belong to the given section, in order.
fn section_field_indices(section: &str) -> Vec<usize> {
    SETTING_FIELDS.iter().enumerate()
        .filter(|(_, field)| field.section == section)
        .map(|(index, _)| index)
        .collect()
}

impl SettingsState {
    fn load() -> Result<Self> {
        let config = fetch_config()?;
        let watch_dir_list = config.watch_directories.clone();
        Ok(Self {
            config,
            selected: 0,
            current_tab: 0,
            edit_buffer: None,
            status: None,
            scroll: 0,
            interface_picker: None,
            watch_dir_list,
            watch_dir_selected: 0,
            watch_dir_editing: false,
            watch_dir_buffer: String::new(),
        })
    }

    /// indices of fields visible in the current tab. drives navigation +
    /// selection bounds.
    fn current_tab_indices(&self) -> Vec<usize> {
        let tabs = section_tabs();
        if let Some(section) = tabs.get(self.current_tab) {
            section_field_indices(section)
        } else {
            Vec::new()
        }
    }

    fn switch_tab(&mut self, delta: isize) {
        let tabs = section_tabs();
        if (tabs.is_empty()) { return; }
        let length = tabs.len() as isize;
        let next = (self.current_tab as isize + delta).rem_euclid(length) as usize;
        self.current_tab = next;
        // reset selection to the first field in the new tab
        if let Some(first) = self.current_tab_indices().first().copied() {
            self.selected = first;
        }
        self.scroll = 0;
        self.edit_buffer = None;
    }

    fn refresh_config(&mut self) {
        if let Ok(config) = fetch_config() {
            self.watch_dir_list = config.watch_directories.clone();
            self.watch_dir_selected = self.watch_dir_selected.min(
                self.watch_dir_list.len().saturating_sub(1)
            );
            self.config = config;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let indices = self.current_tab_indices();
        if (indices.is_empty()) { return; }
        let current_position = indices.iter().position(|index| *index == self.selected).unwrap_or(0);
        let length = indices.len() as isize;
        let next = (current_position as isize + delta).rem_euclid(length) as usize;
        self.selected = indices[next];
    }

    fn current_field(&self) -> &'static SettingField {
        &SETTING_FIELDS[self.selected]
    }
}

fn fetch_config() -> Result<Config> {
    match client::send(Request::GetConfig)? {
        Response::Config(toml_text) => {
            toml::from_str(&toml_text).context("parse daemon config response")
        }
        Response::Err(message) => Err(anyhow::anyhow!("daemon: {}", message)),
        _ => Err(anyhow::anyhow!("unexpected response to GetConfig")),
    }
}

fn submit_set(key: &str, value: &str) -> Result<()> {
    match client::send(Request::SetConfig { key: key.to_string(), value: value.to_string() })? {
        Response::Ok => Ok(()),
        Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
        _ => Err(anyhow::anyhow!("unexpected response to SetConfig")),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Trackers,
    Peers,
    Content,
}

impl DetailTab {
    const ALL: [DetailTab; 3] = [DetailTab::Trackers, DetailTab::Peers, DetailTab::Content];

    fn label(&self) -> &'static str {
        match self {
            DetailTab::Trackers => "trackers",
            DetailTab::Peers => "peers",
            DetailTab::Content => "content",
        }
    }
}

/// top-level UI mode. settings hijacks the screen and consumes all input.
/// boxed because SettingsState is ~570 bytes — without the indirection the
/// Main variant would carry that dead weight on every AppState.mode access.
enum Mode {
    Main,
    Settings(Box<SettingsState>),
    Feeds(Box<FeedsState>),
}

struct FeedsState {
    feeds: Vec<FeedInfo>,
    table_state: TableState,
    last_poll: Instant,
    /// transient status line shown after an action (poll, remove)
    status: Option<String>,
}

impl FeedsState {
    fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            feeds: Vec::new(),
            table_state,
            last_poll: Instant::now() - Duration::from_secs(10),
            status: None,
        }
    }

    fn selected(&self) -> Option<usize> { self.table_state.selected() }

    fn move_selection(&mut self, delta: isize) {
        move_table(&mut self.table_state, self.feeds.len(), delta);
    }
}

/// generic text-input capture. when `AppState::active_input` holds one of
/// these, the main key handler routes every printable keystroke into the
/// buffer instead of looking up a keybind. this is how inline text fields
/// (the `/` torrent filter today, and future rename-in-place / set-
/// download-priority-by-typing-a-number features) coexist with the
/// letter-heavy main-view keybinds without conflicting.
///
/// the `purpose` field is what `commit` reads after `enter` is pressed.
struct TextInput {
    purpose: InputPurpose,
    buffer: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputPurpose {
    /// case-insensitive substring filter over torrent names. submits as you
    /// type — enter just closes the input bar.
    ListFilter,
    /// fuzzy filter over file paths in the content tab. live-updates as you
    /// type; switches draw_content_tab to a flat filtered list.
    ContentFilter,
}

/// modal prompt overlay that floats on top of the main view. captures all input
/// until submitted (`enter`) or cancelled (`esc`).
struct Prompt {
    title: String,
    helper: String,
    /// one line per item. most prompts use a single line; the add-torrent
    /// prompt uses shift+enter to add additional lines for bulk submission.
    lines: Vec<String>,
    /// index of the line being edited
    cursor_line: usize,
    /// the request to send when the user submits. takes the buffer as argument
    /// and produces an ipc Request.
    action: PromptAction,
    /// torrent the action targets (so the prompt remembers it across redraws)
    torrent_index: usize,
    /// when true, shift+enter splits the line into two. on false (single-line
    /// prompts like rename/move) shift+enter is ignored.
    allow_multiline: bool,
}

impl Prompt {
    fn single_line_buffer(&self) -> String {
        self.lines.first().cloned().unwrap_or_default()
    }
}

/// what a Prompt's `enter` should do. names are intentionally bare (no
/// `Torrent` suffix) because that suffix would repeat on every variant.
#[derive(Clone)]
enum PromptAction {
    Rename,
    Move,
    Add,
    /// rename an individual file inside the active torrent
    RenameFile { file_index: usize },
    /// rename a folder inside the active torrent (recursive prefix rewrite).
    /// the backend already auto-merges into existing folders as long as no
    /// individual file paths collide, so no separate merge-confirm flow is
    /// needed for the common case.
    RenameFolder { old_prefix: String },
    /// add a new feed subscription. buffer = url; all other options are defaults.
    AddFeed,
    /// set per-torrent rate limits. line 0 = download, line 1 = upload.
    SetRateLimit,
    /// add a tracker url to the active torrent. buffer = url.
    AddTracker,
}

/// a single row in the sidebar flat list. headers are visual separators;
/// selecting one is a no-op. selectable items set the active filter.
#[derive(Clone, PartialEq)]
enum SidebarItem {
    StatusHeader,
    Status(StatusFilter),
    CategoryHeader,
    CategoryAll,
    CategoryUncategorized,
    Category(String),
    TagHeader,
    TagAll,
    Tag(String),
}

struct ConfirmDelete {
    torrent_index: usize,
    torrent_name: String,
    /// whether to also remove downloaded files from disk
    delete_files: bool,
}

/// per-torrent add-time options collected by the options form before
/// dispatch. mirrors qbittorrent's add-torrent dialog.
#[derive(Clone)]
struct AddOptions {
    start: bool,
    sequential: bool,
    first_last: bool,
    content_layout: ContentLayout,
    save_path: String,
}

impl Default for AddOptions {
    fn default() -> Self {
        Self {
            start: true,
            sequential: false,
            first_last: false,
            content_layout: ContentLayout::default(),
            save_path: String::new(),
        }
    }
}


/// modal form shown after the add-torrent prompt: walks each pending entry
/// and lets the user tune options before dispatch. on the last entry's
/// confirm we send every Add together.
struct AddOptionsForm {
    entries: Vec<String>,
    options: Vec<AddOptions>,
    /// index into `entries` currently being configured
    current: usize,
    /// index of the focused field (0..N matches the field-order in
    /// `draw_add_options_form`)
    field: usize,
    /// when Some, the save_path field is in inline-edit mode
    edit_buffer: Option<String>,
}

enum PriorityRenameTarget {
    Torrent,
    File { file_index: usize },
    Folder { old_prefix: String },
}

/// post-add file priority configuration step. appears after adding paused
/// torrents so the user can cherry-pick files before any data downloads.
/// one torrent at a time; tab/enter advances, esc skips.
struct PriorityStep {
    entries: Vec<String>,   // source URIs (for display), parallel to indices
    indices: Vec<usize>,    // torrent indices in the daemon's list
    current: usize,
    detail: Option<TorrentDetail>,
    /// lowercased file paths, rebuilt on each detail poll
    paths_lc: Vec<String>,
    files_state: TableState,
    filter: String,
    filter_lc: String,
    filter_matches: Vec<usize>,
    collapsed_folders: std::collections::BTreeSet<String>,
    last_poll: Instant,
    filter_active: bool,
    rename_buffer: Option<String>,
    rename_target: Option<PriorityRenameTarget>,
}

impl PriorityStep {
    fn new(entries: Vec<String>, indices: Vec<usize>) -> Self {
        let mut files_state = TableState::default();
        files_state.select(Some(0));
        Self {
            entries,
            indices,
            current: 0,
            detail: None,
            paths_lc: Vec::new(),
            files_state,
            filter: String::new(),
            filter_lc: String::new(),
            filter_matches: Vec::new(),
            collapsed_folders: std::collections::BTreeSet::new(),
            last_poll: Instant::now() - DETAIL_POLL_INTERVAL,
            filter_active: false,
            rename_buffer: None,
            rename_target: None,
        }
    }

    fn torrent_index(&self) -> Option<usize> {
        self.indices.get(self.current).copied()
    }

    fn row_count(&self) -> usize {
        if self.filter.is_empty() {
            self.detail.as_ref()
                .map(|detail| build_tree_rows(detail, &self.collapsed_folders).len())
                .unwrap_or(0)
        } else {
            self.filter_matches.len()
        }
    }

    fn current_rows(&self) -> Vec<TreeRow> {
        let Some(detail) = &self.detail else { return Vec::new(); };
        if self.filter.is_empty() {
            build_tree_rows(detail, &self.collapsed_folders)
        } else {
            filter_content_rows(detail, &self.filter_matches)
        }
    }

    fn rebuild_filter_matches(&mut self) {
        if self.filter.is_empty() {
            self.filter_matches.clear();
            return;
        }
        self.filter_matches = self.paths_lc.iter().enumerate()
            .filter(|(_, path_lc)| fuzzy_match_lc(path_lc, &self.filter_lc))
            .map(|(i, _)| i)
            .collect();
    }
}

struct AppState {
    mode: Mode,
    prompt: Option<Prompt>,
    /// add-options form opened after the multi-line add prompt is confirmed
    add_options: Option<AddOptionsForm>,
    /// post-add file priority step. opened when torrents are added paused.
    priority_step: Option<Box<PriorityStep>>,
    /// inline text field active in the main view. when Some, the main-view
    /// key handler is bypassed and every printable keystroke goes into the
    /// buffer. see [TextInput] for the rationale.
    active_input: Option<TextInput>,
    /// last filter substring that was applied to the torrent list, kept after
    /// the input bar closes so the user can re-open it and edit
    name_filter: String,
    /// fuzzy filter over file paths in the content tab. empty = tree view.
    content_filter: String,
    /// lowercase of content_filter, kept in sync. avoids re-lowercasing
    /// the needle on every match call.
    content_filter_lc: String,
    /// lowercased file paths parallel to detail.files. rebuilt once per
    /// detail poll so fuzzy matching never allocates per file per keystroke.
    detail_paths_lc: Vec<String>,
    /// indices into detail.files that match the current content_filter.
    /// rebuilt once per keypress (not per draw frame). empty when filter
    /// is empty (tree mode is used instead).
    content_filter_matches: Vec<usize>,
    torrents: Vec<TorrentInfo>,
    stats: Option<StatsInfo>,
    detail: Option<TorrentDetail>,
    table_state: TableState,
    sidebar_state: ListState,
    detail_files_state: TableState,
    detail_peers_state: TableState,
    detail_trackers_state: TableState,
    last_poll: Instant,
    last_detail_poll: Instant,
    error: Option<String>,
    daemon_unreachable: bool,
    show_sidebar: bool,
    show_detail: bool,
    focus: Pane,
    status_filter: StatusFilter,
    /// when Some, only torrents in this category are shown. None = no cat filter;
    /// inner None = "(uncategorized)" (torrents with category == None).
    category_filter: Option<Option<String>>,
    /// category names fetched from the daemon, sorted alphabetically
    sidebar_categories: Vec<String>,
    /// when Some, only torrents with this tag are shown
    tag_filter: Option<String>,
    detail_tab: DetailTab,
    // pane rectangles from the last draw — used by mouse handler to route
    // clicks. zero-sized when the pane is hidden.
    sidebar_rect: Rect,
    list_rect: Rect,
    detail_rect: Rect,
    detail_tab_bar_rect: Rect,
    /// timestamp of the most recent left-click; used to detect double-clicks
    last_click: Option<(Instant, u16, u16)>,
    /// columns shown in the torrent list, in display order
    visible_columns: Vec<Column>,
    /// per-column width overrides, loaded from config and updated by drag-resize
    column_width_overrides: std::collections::BTreeMap<String, u16>,
    /// boundary x-positions (right edge of each non-last column). recomputed
    /// every draw and used by the mouse handler for divider hit-testing.
    column_boundaries: Vec<u16>,
    /// header row's y position from the last draw; mouse uses this for hit-test
    header_y: u16,
    /// active drag-resize, set on mouse Down over a divider and cleared on Up
    column_drag: Option<ColumnDrag>,
    /// when Some, the column picker overlay is open (selection index)
    column_picker: Option<usize>,
    /// when true, the keybind help overlay is shown
    show_help: bool,
    /// when Some, a delete confirmation dialog is open
    confirm_delete: Option<ConfirmDelete>,
    /// folder paths that are currently collapsed in the content tab
    collapsed_folders: std::collections::BTreeSet<String>,
    /// terminal capabilities probed at startup. truecolor is recorded but
    /// not yet used; gates a future richer hsl palette.
    #[allow(dead_code)]
    truecolor: bool,
    nerd_font: bool,
    /// set to true whenever state changes; cleared after each draw. gates
    /// terminal.draw so idle ticks with no events skip the render pass.
    dirty: bool,
}

impl AppState {
    fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut sidebar_state = ListState::default();
        sidebar_state.select(Some(0));

        // load [tui] defaults from config.toml. failure here is non-fatal —
        // worst case the user sees built-in defaults until they fix the file.
        let (show_sidebar, show_detail, configured_columns, nerd_font, configured_widths) =
            Config::load()
                .map(|config| (
                    config.tui_show_sidebar,
                    config.tui_show_detail,
                    config.tui_columns,
                    config.tui_nerd_font,
                    config.tui_column_widths,
                ))
                .unwrap_or((false, false, Vec::new(), false, std::collections::BTreeMap::new()));

        // truecolor probe — most modern terminals export COLORTERM=truecolor.
        // we don't use the result much (ratatui maps to whatever the terminal
        // supports) but it gates a future richer palette.
        let truecolor = std::env::var("COLORTERM")
            .map(|value| value.contains("truecolor") || value.contains("24bit"))
            .unwrap_or(false);
        let visible_columns: Vec<Column> = if (configured_columns.is_empty()) {
            DEFAULT_COLUMNS.to_vec()
        } else {
            configured_columns.iter()
                .filter_map(|key| Column::from_key(key))
                .collect()
        };
        // fall back to defaults if everything in the config was unrecognised
        let visible_columns = if (visible_columns.is_empty()) {
            DEFAULT_COLUMNS.to_vec()
        } else {
            visible_columns
        };

        Self {
            mode: Mode::Main,
            prompt: None,
            add_options: None,
            priority_step: None,
            active_input: None,
            name_filter: String::new(),
            content_filter: String::new(),
            content_filter_lc: String::new(),
            detail_paths_lc: Vec::new(),
            content_filter_matches: Vec::new(),
            torrents: Vec::new(),
            stats: None,
            detail: None,
            table_state,
            sidebar_state,
            detail_files_state: TableState::default(),
            detail_peers_state: TableState::default(),
            detail_trackers_state: TableState::default(),
            last_poll: Instant::now() - POLL_INTERVAL,
            last_detail_poll: Instant::now() - DETAIL_POLL_INTERVAL,
            error: None,
            daemon_unreachable: false,
            show_sidebar,
            show_detail,
            focus: Pane::List,
            status_filter: StatusFilter::All,
            category_filter: None,
            sidebar_categories: Vec::new(),
            tag_filter: None,
            detail_tab: DetailTab::Content,
            sidebar_rect: Rect::default(),
            list_rect: Rect::default(),
            detail_rect: Rect::default(),
            detail_tab_bar_rect: Rect::default(),
            last_click: None,
            visible_columns,
            column_width_overrides: configured_widths,
            column_boundaries: Vec::new(),
            header_y: 0,
            column_drag: None,
            column_picker: None,
            show_help: false,
            confirm_delete: None,
            collapsed_folders: std::collections::BTreeSet::new(),
            truecolor,
            nerd_font,
            dirty: true,
        }
    }

    fn sidebar_items(&self) -> Vec<SidebarItem> {
        let mut items = Vec::new();
        items.push(SidebarItem::StatusHeader);
        for filter in StatusFilter::ALL.iter().copied() {
            items.push(SidebarItem::Status(filter));
        }
        items.push(SidebarItem::CategoryHeader);
        items.push(SidebarItem::CategoryAll);
        if (self.torrents.iter().any(|t| t.category.is_none())) {
            items.push(SidebarItem::CategoryUncategorized);
        }
        for name in &self.sidebar_categories {
            items.push(SidebarItem::Category(name.clone()));
        }
        // collect all tags present across all torrents
        let mut all_tags: Vec<String> = {
            let mut set = std::collections::BTreeSet::new();
            for torrent in &self.torrents {
                for tag in &torrent.tags {
                    set.insert(tag.clone());
                }
            }
            set.into_iter().collect()
        };
        if (!all_tags.is_empty()) {
            items.push(SidebarItem::TagHeader);
            items.push(SidebarItem::TagAll);
            for tag in all_tags.drain(..) {
                items.push(SidebarItem::Tag(tag));
            }
        }
        items
    }

    fn filtered_indices(&self) -> Vec<usize> {
        let name_needle = self.name_filter.to_lowercase();
        self.torrents.iter()
            .enumerate()
            .filter(|(_, torrent)| self.status_filter.matches(torrent))
            .filter(|(_, torrent)| match &self.category_filter {
                None => true,
                Some(None) => torrent.category.is_none(),
                Some(Some(name)) => torrent.category.as_deref() == Some(name.as_str()),
            })
            .filter(|(_, torrent)| match &self.tag_filter {
                None => true,
                Some(tag) => torrent.tags.contains(tag.as_str()),
            })
            .filter(|(_, torrent)| {
                name_needle.is_empty() || torrent.name.to_lowercase().contains(&name_needle)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_torrent_index(&self) -> Option<usize> {
        let visible = self.filtered_indices();
        let row = self.table_state.selected()?;
        visible.get(row).copied()
    }

    fn cycle_focus(&mut self) {
        let order = [Pane::Sidebar, Pane::List, Pane::Detail];
        let visible: Vec<Pane> = order.iter().copied().filter(|pane| match pane {
            Pane::Sidebar => self.show_sidebar,
            Pane::List => true,
            Pane::Detail => self.show_detail,
        }).collect();
        if (visible.is_empty()) { return; }
        let current_position = visible.iter().position(|pane| *pane == self.focus).unwrap_or(0);
        self.focus = visible[(current_position + 1) % visible.len()];
    }

    fn move_focused(&mut self, delta: isize) {
        match self.focus {
            Pane::List => {
                let length = self.filtered_indices().len();
                move_table(&mut self.table_state, length, delta);
            }
            Pane::Sidebar => {
                let count = self.sidebar_items().len();
                move_list(&mut self.sidebar_state, count, delta);
            }
            Pane::Detail => match self.detail_tab {
                DetailTab::Content => {
                    let count = if self.content_filter_matches.is_empty() && self.content_filter.is_empty() {
                        self.detail.as_ref()
                            .map(|detail| build_tree_rows(detail, &self.collapsed_folders).len())
                            .unwrap_or(0)
                    } else {
                        self.content_filter_matches.len()
                    };
                    move_table(&mut self.detail_files_state, count, delta);
                }
                DetailTab::Peers => {
                    let count = self.detail.as_ref().map(|detail| detail.peers.len()).unwrap_or(0);
                    move_table(&mut self.detail_peers_state, count, delta);
                }
                DetailTab::Trackers => {
                    let count = self.detail.as_ref().map(|detail| detail.trackers.len()).unwrap_or(0);
                    move_table(&mut self.detail_trackers_state, count, delta);
                }
            },
        }
    }

    fn apply_sidebar_selection(&mut self) {
        let Some(index) = self.sidebar_state.selected() else { return; };
        let items = self.sidebar_items();
        let Some(item) = items.get(index) else { return; };
        match item.clone() {
            SidebarItem::StatusHeader | SidebarItem::CategoryHeader => {}
            SidebarItem::Status(filter) => {
                if (filter != self.status_filter || self.category_filter.is_some()) {
                    self.status_filter = filter;
                    self.category_filter = None;
                    self.table_state.select(Some(0));
                }
            }
            SidebarItem::CategoryAll => {
                if (self.category_filter.is_some()) {
                    self.category_filter = None;
                    self.table_state.select(Some(0));
                }
            }
            SidebarItem::CategoryUncategorized => {
                if (self.category_filter != Some(None)) {
                    self.category_filter = Some(None);
                    self.table_state.select(Some(0));
                }
            }
            SidebarItem::Category(name) => {
                if (self.category_filter.as_ref().and_then(|c| c.as_deref()) != Some(name.as_str())) {
                    self.category_filter = Some(Some(name));
                    self.table_state.select(Some(0));
                }
            }
            SidebarItem::TagHeader => {}
            SidebarItem::TagAll => {
                if (self.tag_filter.is_some()) {
                    self.tag_filter = None;
                    self.table_state.select(Some(0));
                }
            }
            SidebarItem::Tag(tag) => {
                if (self.tag_filter.as_deref() != Some(tag.as_str())) {
                    self.tag_filter = Some(tag);
                    self.table_state.select(Some(0));
                }
            }
        }
    }

    fn cycle_detail_tab(&mut self, delta: isize) {
        let current = DetailTab::ALL.iter().position(|tab| *tab == self.detail_tab).unwrap_or(0)
            as isize;
        let length = DetailTab::ALL.len() as isize;
        let next = ((current + delta).rem_euclid(length)) as usize;
        self.detail_tab = DetailTab::ALL[next];
    }
}

fn move_table(state: &mut TableState, length: usize, delta: isize) {
    if (length == 0) { state.select(None); return; }
    let current = state.selected().unwrap_or(0) as isize;
    let next = (current + delta).clamp(0, length as isize - 1) as usize;
    state.select(Some(next));
}

fn move_list(state: &mut ListState, length: usize, delta: isize) {
    if (length == 0) { state.select(None); return; }
    let current = state.selected().unwrap_or(0) as isize;
    let next = (current + delta).clamp(0, length as isize - 1) as usize;
    state.select(Some(next));
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    let mut state = AppState::new();

    loop {
        // skip background polling while the settings overlay owns the screen —
        // the main pane is hidden so its data does not need to be fresh
        if (matches!(state.mode, Mode::Main)) {
            if (state.last_poll.elapsed() >= POLL_INTERVAL) {
                poll_daemon(&mut state);
                state.dirty = true;
            }
            if (state.show_detail && state.last_detail_poll.elapsed() >= DETAIL_POLL_INTERVAL) {
                poll_detail(&mut state);
                state.dirty = true;
            }
        }
        if (matches!(state.mode, Mode::Feeds(_))) {
            poll_feeds_page(&mut state);
            state.dirty = true;
        }
        if (state.priority_step.is_some()) {
            poll_priority_step(&mut state);
            state.dirty = true;
        }

        if (state.dirty) {
            terminal.draw(|frame| draw(frame, &mut state))?;
            state.dirty = false;
        }

        if (event::poll(EVENT_TICK)?) {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // input-routing ladder. each level captures input wholesale —
                    // letters bound in the main view don't reach handle_key while
                    // a higher level is active.
                    let exit = if (state.priority_step.is_some()) {
                        handle_priority_step_key(key.code, key.modifiers, &mut state)
                    } else if (state.show_help) {
                        // any key closes help overlay
                        if (key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL) {
                            true
                        } else {
                            state.show_help = false;
                            false
                        }
                    } else if (state.confirm_delete.is_some()) {
                        handle_delete_confirm_key(key.code, key.modifiers, &mut state)
                    } else if (state.column_picker.is_some()) {
                        handle_picker_key(key.code, key.modifiers, &mut state)
                    } else if (state.add_options.is_some()) {
                        handle_add_options_key(key.code, key.modifiers, &mut state)
                    } else if (state.prompt.is_some()) {
                        handle_prompt_key(key.code, key.modifiers, &mut state)
                    } else if (state.active_input.is_some()) {
                        handle_active_input_key(key.code, key.modifiers, &mut state)
                    } else if (matches!(state.mode, Mode::Settings(_))) {
                        handle_settings_key(key.code, key.modifiers, &mut state)
                    } else if (matches!(state.mode, Mode::Feeds(_))) {
                        handle_feeds_key(key.code, key.modifiers, &mut state)
                    } else {
                        handle_key(key.code, key.modifiers, &mut state)
                    };
                    if (exit) { return Ok(()); }
                    state.dirty = true;
                }
                Event::Mouse(mouse) => {
                    handle_mouse(mouse, &mut state);
                    state.dirty = true;
                }
                _ => {}
            }
        }
    }
}

/// returns true when the tui should exit.
fn handle_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match (code, modifiers) {
        // quit is ctrl+c only (standard). lowercase q is the sidebar toggle
        // below; never bind it to quit.
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,

        // wasd + arrows. j/k are intentionally not bound.
        (KeyCode::Char('s'), KeyModifiers::NONE) | (KeyCode::Down, _) => state.move_focused(1),
        (KeyCode::Char('w'), KeyModifiers::NONE) | (KeyCode::Up, _) => state.move_focused(-1),
        (KeyCode::PageDown, _) => state.move_focused(10),
        (KeyCode::PageUp, _) => state.move_focused(-10),
        // tracker tab: a = add tracker prompt, d = remove selected tracker
        (KeyCode::Char('a'), KeyModifiers::NONE)
            if (state.focus == Pane::Detail && state.detail_tab == DetailTab::Trackers) =>
        {
            open_add_tracker_prompt(state);
        }
        (KeyCode::Char('d'), KeyModifiers::NONE) | (KeyCode::Delete, _)
            if (state.focus == Pane::Detail && state.detail_tab == DetailTab::Trackers) =>
        {
            remove_selected_tracker(state);
        }
        // a/d (or arrows) collapse/expand the focused folder in the content tab
        (KeyCode::Char('a'), KeyModifiers::NONE) | (KeyCode::Left, _) => collapse_focused(state, true),
        (KeyCode::Char('d'), KeyModifiers::NONE) | (KeyCode::Right, _) => collapse_focused(state, false),

        // pane cycling + visibility toggles. q+e sit on the QWE row just
        // above wasd so neither hand has to leave the home position.
        (KeyCode::Tab, _) => state.cycle_focus(),
        (KeyCode::Char('q'), KeyModifiers::NONE) => {
            state.show_sidebar = !state.show_sidebar;
            if (!state.show_sidebar && state.focus == Pane::Sidebar) { state.focus = Pane::List; }
        }
        (KeyCode::Char('e'), KeyModifiers::NONE) => {
            state.show_detail = !state.show_detail;
            if (!state.show_detail && state.focus == Pane::Detail) { state.focus = Pane::List; }
            if (state.show_detail) {
                state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
            }
        }

        // detail tab cycling
        (KeyCode::Char('['), KeyModifiers::NONE) => state.cycle_detail_tab(-1),
        (KeyCode::Char(']'), KeyModifiers::NONE) => state.cycle_detail_tab(1),

        // confirmations
        (KeyCode::Enter, _) if state.focus == Pane::Sidebar => state.apply_sidebar_selection(),

        // open the settings overlay. ',' is the binding (mnemonic: same key as the
        // 'preferences' shortcut in most editors); ctrl+, also works.
        (KeyCode::Char(','), KeyModifiers::NONE) | (KeyCode::Char(','), KeyModifiers::CONTROL) => {
            match SettingsState::load() {
                Ok(settings) => state.mode = Mode::Settings(Box::new(settings)),
                Err(error) => state.error = Some(format!("settings: {}", error)),
            }
        }

        // open the feeds page
        (KeyCode::Char('u'), KeyModifiers::NONE) => {
            state.mode = Mode::Feeds(Box::new(FeedsState::new()));
        }

        // actions on the selected torrent
        (KeyCode::Char('p'), KeyModifiers::NONE) => toggle_pause(state),
        (KeyCode::Char('r'), KeyModifiers::NONE) | (KeyCode::F(2), _) => {
            // route based on focus: in the content tab, rename the selected
            // file or folder. otherwise, fall through to torrent rename.
            if (state.focus == Pane::Detail && state.detail_tab == DetailTab::Content) {
                open_content_rename_prompt(state);
            } else {
                open_rename_prompt(state);
            }
        }
        (KeyCode::Char('m'), KeyModifiers::NONE) => open_move_prompt(state),
        (KeyCode::Char('n'), KeyModifiers::NONE)
        | (KeyCode::Char('n'), KeyModifiers::CONTROL) => open_add_prompt(state),
        (KeyCode::Char('R'), KeyModifiers::SHIFT) => force_recheck(state),
        // 'a' is reserved for future wasd-left (tree collapse). use T = "tracker"
        (KeyCode::Char('T'), KeyModifiers::SHIFT) => reannounce(state),
        (KeyCode::Char('g'), KeyModifiers::NONE) => show_magnet(state),
        // shift+s for sequential toggle (lowercase s is wasd-down)
        (KeyCode::Char('S'), KeyModifiers::SHIFT) => toggle_sequential(state),
        (KeyCode::Char('C'), KeyModifiers::SHIFT) => state.column_picker = Some(0),
        (KeyCode::Char('L'), KeyModifiers::SHIFT) => open_rate_limit_prompt(state),
        (KeyCode::Delete, _) | (KeyCode::Char('x'), KeyModifiers::NONE) | (KeyCode::Char('X'), KeyModifiers::SHIFT) => open_delete_confirm(state),
        (KeyCode::Char('?'), _) => state.show_help = true,
        // file/folder priority — only when the content tab has focus. digits
        // map to qbittorrent's priority levels; libtorrent's 0..=7 is folded
        // into the five buckets the user actually cares about. on folder rows
        // every descendant file is updated atomically.
        (KeyCode::Char(character), KeyModifiers::NONE)
            if (state.focus == Pane::Detail
                && state.detail_tab == DetailTab::Content
                && matches!(character, '0' | '1' | '2' | '3' | '4')) =>
        {
            let priority = match character {
                '0' => 0u8,
                '1' => 1u8,
                '2' => 4u8,
                '3' => 6u8,
                '4' => 7u8,
                _ => unreachable!(),
            };
            set_focused_priority(state, priority);
        }
        // open the context-appropriate search bar. routes to torrent filter
        // (list pane), file filter (content tab), or no-op (sidebar).
        (KeyCode::Char('/'), KeyModifiers::NONE) => open_search(state),

        _ => {}
    }
    false
}

fn open_search(state: &mut AppState) {
    match state.focus {
        Pane::Sidebar => {}  // not wired yet
        Pane::Detail if state.detail_tab == DetailTab::Content => {
            state.active_input = Some(TextInput {
                purpose: InputPurpose::ContentFilter,
                buffer: state.content_filter.clone(),
            });
        }
        _ => {
            state.active_input = Some(TextInput {
                purpose: InputPurpose::ListFilter,
                buffer: state.name_filter.clone(),
            });
        }
    }
}

fn open_rename_prompt(state: &mut AppState) {
    let Some(index) = state.selected_torrent_index() else {
        state.error = Some("no torrent selected".to_string());
        return;
    };
    let current = state.torrents.get(index).map(|torrent| torrent.name.clone()).unwrap_or_default();
    state.prompt = Some(Prompt {
        title: format!("rename torrent #{}", index),
        helper: "files inside are not renamed; use the content tab + F2 for individual files".to_string(),
        lines: vec![current],
        cursor_line: 0,
        action: PromptAction::Rename,
        torrent_index: index,
        allow_multiline: false,
    });
}

fn open_move_prompt(state: &mut AppState) {
    let Some(index) = state.selected_torrent_index() else {
        state.error = Some("no torrent selected".to_string());
        return;
    };
    let current = state.torrents.get(index)
        .map(|torrent| torrent.save_path.clone()).unwrap_or_default();
    state.prompt = Some(Prompt {
        title: format!("move torrent #{} — new save directory", index),
        helper: "absolute path. files will be moved on disk by libtorrent.".to_string(),
        lines: vec![current],
        cursor_line: 0,
        action: PromptAction::Move,
        torrent_index: index,
        allow_multiline: false,
    });
}

/// open a rename prompt for the selected file or folder inside the content
/// tab. routes to RenameFile/RenameFolder depending on the selected row.
fn open_content_rename_prompt(state: &mut AppState) {
    let Some(torrent_index) = state.selected_torrent_index() else {
        state.error = Some("no torrent selected".to_string());
        return;
    };
    let Some(detail) = &state.detail else {
        state.error = Some("file list not loaded".to_string());
        return;
    };
    let rows = if state.content_filter.is_empty() {
        build_tree_rows(detail, &state.collapsed_folders)
    } else {
        filter_content_rows(detail, &state.content_filter_matches)
    };
    let Some(row) = state.detail_files_state.selected().and_then(|index| rows.get(index)) else {
        state.error = Some("no file selected".to_string());
        return;
    };

    if (row.is_folder) {
        state.prompt = Some(Prompt {
            title: format!("rename folder \"{}\"", row.full_path),
            helper: "new folder path (relative to the torrent root). renaming into an existing folder merges automatically; file-on-file collisions are rejected.".to_string(),
            lines: vec![row.full_path.clone()],
            cursor_line: 0,
            action: PromptAction::RenameFolder { old_prefix: row.full_path.clone() },
            torrent_index,
            allow_multiline: false,
        });
    } else if let Some(file_index) = row.file_index {
        let Some(file) = detail.files.get(file_index) else { return; };
        state.prompt = Some(Prompt {
            title: format!("rename file \"{}\"", row.label),
            helper: "new path relative to the torrent root. collisions with existing files are rejected.".to_string(),
            lines: vec![file.path.clone()],
            cursor_line: 0,
            action: PromptAction::RenameFile { file_index },
            torrent_index,
            allow_multiline: false,
        });
    }
}

fn clipboard_magnet_or_url() -> Option<String> {
    let mut board = arboard::Clipboard::new().ok()?;
    let text = board.get_text().ok()?;
    let trimmed = text.trim();
    let looks_valid = trimmed.starts_with("magnet:")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.ends_with(".torrent");
    if looks_valid { Some(trimmed.to_string()) } else { None }
}

fn open_rate_limit_prompt(state: &mut AppState) {
    let Some(index) = state.selected_torrent_index() else {
        state.error = Some("no torrent selected".to_string());
        return;
    };
    let (dl, ul) = state.torrents.get(index)
        .map(|torrent| (torrent.download_limit, torrent.upload_limit))
        .unwrap_or((-1, -1));
    let dl_str = dl.to_string();
    let ul_str = ul.to_string();
    state.prompt = Some(Prompt {
        title: format!("rate limits for torrent #{}", index),
        helper: "bytes/sec  0 = unlimited  -1 = inherit global  line 1 = download  line 2 = upload".to_string(),
        lines: vec![dl_str, ul_str],
        cursor_line: 0,
        action: PromptAction::SetRateLimit,
        torrent_index: index,
        allow_multiline: false,
    });
}

fn open_add_prompt(state: &mut AppState) {
    let prefill = clipboard_magnet_or_url().unwrap_or_default();
    state.prompt = Some(Prompt {
        title: "add torrent (shift+enter to add another line)".to_string(),
        helper: "magnet:, http(s)://, ftp(s)://, /abs/path, C:\\path, or ~/foo.torrent — one per line".to_string(),
        lines: vec![prefill],
        cursor_line: 0,
        action: PromptAction::Add,
        torrent_index: 0,
        allow_multiline: true,
    });
}

fn force_recheck(state: &mut AppState) {
    let Some(index) = state.selected_torrent_index() else { return; };
    if let Err(error) = client::send(Request::Recheck { index }) {
        state.error = Some(format!("recheck: {}", error));
    } else {
        state.error = Some(format!("recheck submitted for torrent {}", index));
    }
}

fn reannounce(state: &mut AppState) {
    let Some(index) = state.selected_torrent_index() else { return; };
    if let Err(error) = client::send(Request::Reannounce { index }) {
        state.error = Some(format!("reannounce: {}", error));
    } else {
        state.error = Some(format!("reannounce submitted for torrent {}", index));
    }
}

fn show_magnet(state: &mut AppState) {
    let Some(index) = state.selected_torrent_index() else { return; };
    match client::send(Request::Magnet { index }) {
        Ok(Response::Magnet(uri)) if !uri.is_empty() => {
            // surface the uri in the status bar — copy-to-clipboard would need
            // the `arboard` crate; opting to just print it for now and let
            // terminal-level selection handle the copy
            state.error = Some(format!("magnet: {}", uri));
        }
        Ok(Response::Magnet(_)) => {
            state.error = Some("magnet not ready (metadata still downloading?)".to_string());
        }
        Ok(Response::Err(message)) => state.error = Some(format!("magnet: {}", message)),
        Ok(_) => state.error = Some("unexpected response to magnet request".to_string()),
        Err(error) => state.error = Some(format!("magnet: {}", error)),
    }
}

fn open_add_tracker_prompt(state: &mut AppState) {
    let Some(index) = state.selected_torrent_index() else {
        state.error = Some("no torrent selected".to_string());
        return;
    };
    state.prompt = Some(Prompt {
        title: format!("add tracker to torrent #{}", index),
        helper: "announce url (e.g. udp://tracker.example.com:1337/announce)".to_string(),
        lines: vec![String::new()],
        cursor_line: 0,
        action: PromptAction::AddTracker,
        torrent_index: index,
        allow_multiline: false,
    });
}

fn remove_selected_tracker(state: &mut AppState) {
    let Some(torrent_index) = state.selected_torrent_index() else {
        state.error = Some("no torrent selected".to_string());
        return;
    };
    let Some(detail) = &state.detail else {
        state.error = Some("tracker list not loaded".to_string());
        return;
    };
    let Some(row_index) = state.detail_trackers_state.selected() else {
        state.error = Some("no tracker selected".to_string());
        return;
    };
    let Some(tracker) = detail.trackers.get(row_index) else {
        state.error = Some("tracker index out of range".to_string());
        return;
    };
    let url = tracker.url.clone();
    match client::send(Request::RemoveTracker { index: torrent_index, url: url.clone() }) {
        Ok(Response::Ok) => {
            state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
            state.error = Some(format!("removed tracker: {}", url));
        }
        Ok(Response::Err(message)) => state.error = Some(format!("remove tracker: {}", message)),
        Ok(_) => state.error = Some("unexpected response".to_string()),
        Err(error) => state.error = Some(format!("remove tracker: {}", error)),
    }
}

fn toggle_sequential(state: &mut AppState) {
    // there's no canonical "is_sequential" surfaced in TorrentInfo today, so
    // this keybind always toggles to ON. a future ipc roundtrip can flip back.
    let Some(index) = state.selected_torrent_index() else { return; };
    match client::send(Request::SetSequential { index, enabled: true }) {
        Ok(Response::Ok) => state.error = Some(format!("sequential ON for torrent {}", index)),
        Ok(Response::Err(message)) => state.error = Some(format!("sequential: {}", message)),
        Ok(_) => state.error = Some("unexpected response".to_string()),
        Err(error) => state.error = Some(format!("sequential: {}", error)),
    }
}

fn open_delete_confirm(state: &mut AppState) {
    let Some(index) = state.selected_torrent_index() else { return; };
    let name = state.torrents.get(index)
        .map(|t| t.name.clone())
        .unwrap_or_else(|| format!("torrent #{}", index));
    state.confirm_delete = Some(ConfirmDelete {
        torrent_index: index,
        torrent_name: name,
        delete_files: false,
    });
}

fn handle_delete_confirm_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match code {
        KeyCode::Char('c') if modifiers == KeyModifiers::CONTROL => return true,
        KeyCode::Esc | KeyCode::Char('n') => {
            state.confirm_delete = None;
        }
        KeyCode::Enter | KeyCode::Char('y') => {
            if let Some(confirm) = state.confirm_delete.take() {
                match client::send(Request::Remove {
                    index: confirm.torrent_index,
                    delete_files: confirm.delete_files,
                }) {
                    Ok(Response::Ok) => {}
                    Ok(Response::Err(message)) => state.error = Some(format!("delete: {}", message)),
                    Ok(_) => state.error = Some("unexpected response to delete".to_string()),
                    Err(error) => state.error = Some(format!("delete: {}", error)),
                }
            }
        }
        KeyCode::Tab | KeyCode::Char(' ') => {
            if let Some(confirm) = state.confirm_delete.as_mut() {
                confirm.delete_files = !confirm.delete_files;
            }
        }
        _ => {}
    }
    false
}

fn draw_delete_confirm(frame: &mut ratatui::Frame, state: &AppState) {
    let Some(confirm) = &state.confirm_delete else { return; };
    let area = frame.area();
    let width = 54u16.min(area.width.saturating_sub(4));
    let height = 8u16;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect { x, y, width, height };

    frame.render_widget(ratatui::widgets::Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Red))
        .title(" delete torrent ");
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    let name = if (confirm.torrent_name.len() > inner.width as usize - 2) {
        format!("{}…", &confirm.torrent_name[..inner.width as usize - 3])
    } else {
        confirm.torrent_name.clone()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))),
        layout[0],
    );

    let delete_files_style = if (confirm.delete_files) {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let delete_files_marker = if (confirm.delete_files) { "[x]" } else { "[ ]" };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{} ", delete_files_marker), delete_files_style),
            Span::styled("also delete files from disk", Style::default().fg(Color::White)),
        ])),
        layout[2],
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "tab toggle files   y/enter confirm   n/esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        layout[4],
    );
}

fn submit_prompt(prompt: &Prompt, state: &mut AppState) -> Result<()> {
    match &prompt.action {
        PromptAction::Rename => {
            match client::send(Request::RenameTorrent {
                index: prompt.torrent_index,
                new_name: prompt.single_line_buffer(),
            })? {
                Response::Ok => Ok(()),
                Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
                _ => Err(anyhow::anyhow!("unexpected response")),
            }
        }
        PromptAction::RenameFile { file_index } => {
            match client::send(Request::RenameFile {
                index: prompt.torrent_index,
                file_index: *file_index,
                new_name: prompt.single_line_buffer(),
            })? {
                Response::Ok => Ok(()),
                Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
                _ => Err(anyhow::anyhow!("unexpected response")),
            }
        }
        PromptAction::RenameFolder { old_prefix } => {
            match client::send(Request::RenameFolder {
                index: prompt.torrent_index,
                old_prefix: old_prefix.clone(),
                new_prefix: prompt.single_line_buffer(),
                decisions: None,
            })? {
                Response::Ok => Ok(()),
                Response::RenameResult { renamed, rejected } => {
                    if (rejected.is_empty()) {
                        state.error = Some(format!("renamed {} file(s)", renamed.len()));
                        Ok(())
                    } else {
                        let summary = rejected.iter()
                            .map(|(file_index, reason)| format!("#{}: {}", file_index, reason))
                            .collect::<Vec<_>>()
                            .join("; ");
                        Err(anyhow::anyhow!("rejected: {}", summary))
                    }
                }
                Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
                _ => Err(anyhow::anyhow!("unexpected response")),
            }
        }
        PromptAction::Move => {
            match client::send(Request::Move {
                index: prompt.torrent_index,
                new_save_path: prompt.single_line_buffer(),
                decisions: None,
            })? {
                Response::Ok => Ok(()),
                Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
                _ => Err(anyhow::anyhow!("unexpected response")),
            }
        }
        PromptAction::Add => {
            // collect non-empty entries and open the per-torrent options
            // form. nothing hits the daemon yet — the options form owns
            // the dispatch on its own confirmation.
            let entries: Vec<String> = prompt.lines.iter()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect();
            if (entries.is_empty()) {
                return Err(anyhow::anyhow!("no sources provided"));
            }
            let options = vec![AddOptions::default(); entries.len()];
            state.add_options = Some(AddOptionsForm {
                entries,
                options,
                current: 0,
                field: 0,
                edit_buffer: None,
            });
            Ok(())
        }
        PromptAction::SetRateLimit => {
            let download = prompt.lines.first()
                .and_then(|line| line.trim().parse::<i32>().ok())
                .unwrap_or(-1);
            let upload = prompt.lines.get(1)
                .and_then(|line| line.trim().parse::<i32>().ok())
                .unwrap_or(-1);
            match client::send(Request::SetTorrentRateLimit {
                index: prompt.torrent_index,
                download,
                upload,
            })? {
                Response::Ok => Ok(()),
                Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
                _ => Err(anyhow::anyhow!("unexpected response")),
            }
        }
        PromptAction::AddFeed => {
            let url = prompt.single_line_buffer().trim().to_string();
            if (url.is_empty()) {
                return Err(anyhow::anyhow!("url cannot be empty"));
            }
            match client::send(Request::AddFeed {
                url,
                filter: String::new(),
                category: None,
                save_path: None,
                poll_interval_minutes: 30,
                start_paused: false,
            })? {
                Response::Ok => {
                    // refresh the feeds list immediately
                    if let Mode::Feeds(feeds) = &mut state.mode {
                        feeds.last_poll = Instant::now() - Duration::from_secs(10);
                        feeds.status = Some("feed added".to_string());
                    }
                    Ok(())
                }
                Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
                _ => Err(anyhow::anyhow!("unexpected response")),
            }
        }
        PromptAction::AddTracker => {
            let url = prompt.single_line_buffer().trim().to_string();
            if (url.is_empty()) {
                return Err(anyhow::anyhow!("tracker url cannot be empty"));
            }
            match client::send(Request::AddTracker {
                index: prompt.torrent_index,
                url,
                tier: 0,
            })? {
                Response::Ok => {
                    // force re-poll so the new tracker shows up immediately
                    state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
                    Ok(())
                }
                Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
                _ => Err(anyhow::anyhow!("unexpected response")),
            }
        }
    }
}

/// total fields on the add-options form. update both this and the field
/// renderer when adding/removing rows.
const ADD_OPTIONS_FIELD_COUNT: usize = 6;

fn handle_add_options_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    let Some(form) = state.add_options.as_mut() else { return false; };

    // text-edit mode for the save_path field
    if (form.edit_buffer.is_some()) {
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Esc, _) => form.edit_buffer = None,
            (KeyCode::Enter, _) => {
                let buffer = form.edit_buffer.take().unwrap_or_default();
                if let Some(options) = form.options.get_mut(form.current) {
                    options.save_path = buffer;
                }
            }
            (KeyCode::Backspace, _) => {
                if let Some(buffer) = form.edit_buffer.as_mut() { buffer.pop(); }
            }
            (KeyCode::Char(character), modifiers)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(buffer) = form.edit_buffer.as_mut() { buffer.push(character); }
            }
            (KeyCode::Tab, _) => {
                if let Some(buffer) = form.edit_buffer.as_mut() {
                    *buffer = tab_complete_path(buffer);
                }
            }
            _ => {}
        }
        return false;
    }

    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Esc, _) => { state.add_options = None; }
        (KeyCode::Char('s'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
            form.field = (form.field + 1) % ADD_OPTIONS_FIELD_COUNT;
        }
        (KeyCode::Char('w'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
            form.field = (form.field + ADD_OPTIONS_FIELD_COUNT - 1) % ADD_OPTIONS_FIELD_COUNT;
        }
        // tab cycles focus just like s/down
        (KeyCode::Tab, _) => {
            form.field = (form.field + 1) % ADD_OPTIONS_FIELD_COUNT;
        }
        // space/enter toggles or activates the focused field
        (KeyCode::Enter, _) | (KeyCode::Char(' '), KeyModifiers::NONE) => activate_add_options_field(state),
        _ => {}
    }
    false
}

fn activate_add_options_field(state: &mut AppState) {
    let field = {
        let Some(form) = state.add_options.as_ref() else { return; };
        form.field
    };
    if field == 5 {
        advance_add_options(state);
        return;
    }
    let Some(form) = state.add_options.as_mut() else { return; };
    let current = form.current;
    let Some(options) = form.options.get_mut(current) else { return; };
    match field {
        0 => options.start = !options.start,
        1 => options.sequential = !options.sequential,
        2 => options.first_last = !options.first_last,
        3 => options.content_layout = options.content_layout.cycle(),
        4 => form.edit_buffer = Some(options.save_path.clone()),
        _ => {}
    }
}

/// move to the next pending entry. if there are no more, dispatch every
/// queued Add (and the post-add tweaks for sequential / start) in order.
fn advance_add_options(state: &mut AppState) {
    let Some(form) = state.add_options.as_mut() else { return; };
    if (form.current + 1 < form.entries.len()) {
        form.current += 1;
        form.field = 0;
        return;
    }
    // last entry confirmed — dispatch everything
    let Some(form) = state.add_options.take() else { return; };
    dispatch_add_options(form, state);
}

fn dispatch_add_options(form: AddOptionsForm, state: &mut AppState) {
    let mut succeeded: usize = 0;
    let mut failures: Vec<String> = Vec::new();
    let mut paused_indices: Vec<usize> = Vec::new();
    let mut paused_entries: Vec<String> = Vec::new();
    for (entry_index, uri) in form.entries.iter().enumerate() {
        let options = &form.options[entry_index];
        let save_path = if (options.save_path.trim().is_empty()) { None } else { Some(options.save_path.clone()) };
        let added_id = match client::send(Request::Add {
            uri: uri.clone(),
            save_path,
            category: None,
            start_paused: !options.start,
            content_layout: options.content_layout,
        }) {
            Ok(Response::Added { id }) => Some(id),
            Ok(Response::Err(message)) => { failures.push(format!("{}: {}", uri, message)); None }
            Ok(_) => { failures.push(format!("{}: unexpected response", uri)); None }
            Err(error) => { failures.push(format!("{}: {}", uri, error)); None }
        };
        if (added_id.is_none()) { continue; }
        succeeded += 1;
        // do the List roundtrip whenever we need the new index
        if (options.sequential || options.first_last || !options.start) {
            let new_index = match client::send(Request::List) {
                Ok(Response::TorrentList(list)) => list.len().saturating_sub(1),
                _ => continue,
            };
            if (options.sequential) {
                let _ = client::send(Request::SetSequential { index: new_index, enabled: true });
            }
            if (options.first_last) {
                let _ = client::send(Request::SetFirstLastPriority { index: new_index, enabled: true });
            }
            if (!options.start) {
                paused_indices.push(new_index);
                paused_entries.push(uri.clone());
            }
        }
    }
    if (failures.is_empty()) {
        state.error = Some(format!("added {} torrent(s)", succeeded));
    } else if (succeeded == 0) {
        state.error = Some(format!("all sources failed: {}", failures.join("; ")));
    } else {
        state.error = Some(format!(
            "added {} ok, {} failed: {}",
            succeeded, failures.len(), failures.join("; ")
        ));
    }
    state.last_poll = Instant::now() - POLL_INTERVAL;
    if (!paused_indices.is_empty()) {
        state.priority_step = Some(Box::new(PriorityStep::new(paused_entries, paused_indices)));
    }
}

fn handle_prompt_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Esc, _) => state.prompt = None,
        // shift+enter inserts a new line in multi-line prompts; plain enter
        // always submits (consistent with the rest of the app).
        (KeyCode::Enter, KeyModifiers::SHIFT) => {
            if let Some(prompt) = state.prompt.as_mut() {
                if (prompt.allow_multiline) {
                    let insert_at = prompt.cursor_line + 1;
                    prompt.lines.insert(insert_at, String::new());
                    prompt.cursor_line = insert_at;
                }
            }
        }
        (KeyCode::Enter, _) => {
            if let Some(prompt) = state.prompt.take() {
                match submit_prompt(&prompt, state) {
                    Ok(_) => {
                        state.last_poll = Instant::now() - POLL_INTERVAL;
                        state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
                    }
                    Err(error) => {
                        state.error = Some(error.to_string());
                        state.prompt = Some(prompt);
                    }
                }
            }
        }
        (KeyCode::Up, _) => {
            if let Some(prompt) = state.prompt.as_mut() {
                prompt.cursor_line = prompt.cursor_line.saturating_sub(1);
            }
        }
        (KeyCode::Down, _) => {
            if let Some(prompt) = state.prompt.as_mut() {
                prompt.cursor_line = (prompt.cursor_line + 1).min(prompt.lines.len().saturating_sub(1));
            }
        }
        (KeyCode::Backspace, _) => {
            if let Some(prompt) = state.prompt.as_mut() {
                let cursor = prompt.cursor_line;
                let line_empty = prompt.lines.get(cursor).map(|line| line.is_empty()).unwrap_or(true);
                if (line_empty && cursor > 0 && prompt.lines.len() > 1) {
                    // backspace at the start of an empty line removes the line
                    prompt.lines.remove(cursor);
                    prompt.cursor_line = cursor - 1;
                } else if let Some(line) = prompt.lines.get_mut(cursor) {
                    line.pop();
                }
            }
        }
        (KeyCode::Char(character), modifiers)
            if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
        {
            if let Some(prompt) = state.prompt.as_mut() {
                if let Some(line) = prompt.lines.get_mut(prompt.cursor_line) {
                    line.push(character);
                }
            }
        }
        (KeyCode::Tab, _) => {
            if let Some(prompt) = state.prompt.as_mut() {
                if matches!(prompt.action, PromptAction::Move | PromptAction::AddFeed) {
                    if let Some(line) = prompt.lines.get_mut(prompt.cursor_line) {
                        *line = tab_complete_path(line);
                    }
                }
            }
        }
        _ => {}
    }
    false
}

/// when the focused row in the content tab is a folder, toggle its collapsed
/// state. `collapse` true means a-or-left (collapse), false means d-or-right
/// (expand). on file rows the key is a no-op.
/// set the priority of the currently-selected row in the content tab. on a
/// folder row this fans out to every descendant file (recursive). errors from
/// individual rpc calls are accumulated into state.error.
fn set_focused_priority(state: &mut AppState, priority: u8) {
    let Some(torrent_index) = state.selected_torrent_index() else { return; };
    let Some(detail) = &state.detail else { return; };
    let rows = if state.content_filter.is_empty() {
        build_tree_rows(detail, &state.collapsed_folders)
    } else {
        filter_content_rows(detail, &state.content_filter_matches)
    };
    let Some(selected_row) = state.detail_files_state.selected().and_then(|index| rows.get(index)) else { return; };

    // collect target file indices. for a leaf row, that's just file_index.
    // for a folder, walk every descendant file (regardless of the collapsed
    // view — collapse is presentational, the operation still affects all
    // descendants).
    let targets: Vec<usize> = if (selected_row.is_folder) {
        let prefix = format!("{}/", selected_row.full_path);
        detail.files.iter().enumerate()
            .filter(|(_, file)| file.path == selected_row.full_path || file.path.starts_with(&prefix))
            .map(|(file_index, _)| file_index)
            .collect()
    } else if let Some(file_index) = selected_row.file_index {
        vec![file_index]
    } else {
        Vec::new()
    };

    if (targets.is_empty()) { return; }
    let priorities: Vec<(usize, u8)> = targets.iter().map(|&file_index| (file_index, priority)).collect();
    let count = priorities.len();
    match client::send(Request::SetFilePrioritiesBatch { index: torrent_index, priorities }) {
        Ok(Response::Ok) => {
            state.error = Some(format!("priority {} set on {} file(s)", priority, count));
        }
        Ok(Response::Err(message)) => state.error = Some(format!("priority: {}", message)),
        Ok(_) => state.error = Some("unexpected response to batch priority".to_string()),
        Err(error) => state.error = Some(format!("priority: {}", error)),
    }
    state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
}

fn collapse_focused(state: &mut AppState, collapse: bool) {
    if (state.focus != Pane::Detail || state.detail_tab != DetailTab::Content) {
        return;
    }
    // in filter mode there's no tree structure to collapse
    if (!state.content_filter.is_empty()) { return; }
    let Some(detail) = &state.detail else { return; };
    let rows = build_tree_rows(detail, &state.collapsed_folders);
    let Some(row) = state.detail_files_state.selected().and_then(|index| rows.get(index)) else { return; };
    if (!row.is_folder) { return; }
    if (collapse) {
        state.collapsed_folders.insert(row.full_path.clone());
    } else {
        state.collapsed_folders.remove(&row.full_path);
    }
}

/// inline input bar handler. captures every printable char, backspace,
/// enter, and esc. for incremental-filter inputs (like ListFilter) the
/// buffer is propagated to the live filter on every keystroke so the
/// torrent list updates as you type.
fn handle_active_input_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Tab, _) => {
            let purpose = state.active_input.as_ref().map(|i| i.purpose);
            if purpose == Some(InputPurpose::ContentFilter) {
                tab_complete_content_filter(state);
            }
        }
        (KeyCode::Esc, _) => {
            // cancel: revert filter to its previous committed value
            if let Some(input) = state.active_input.take() {
                if (input.purpose == InputPurpose::ListFilter) {
                    // nothing to revert — name_filter was being mirrored live
                    // and the user can simply press / again to edit it
                }
            }
        }
        (KeyCode::Enter, _) => {
            // commit closes the input bar; the live buffer was already applied
            state.active_input = None;
        }
        (KeyCode::Backspace, _) => {
            if let Some(input) = state.active_input.as_mut() {
                input.buffer.pop();
                match input.purpose {
                    InputPurpose::ListFilter => {
                        state.name_filter = input.buffer.clone();
                        let visible = state.filtered_indices().len();
                        if let Some(selected) = state.table_state.selected() {
                            if (visible == 0) {
                                state.table_state.select(None);
                            } else if (selected >= visible) {
                                state.table_state.select(Some(visible - 1));
                            }
                        }
                    }
                    InputPurpose::ContentFilter => {
                        state.content_filter = input.buffer.clone();
                        state.content_filter_lc = state.content_filter.to_lowercase();
                        rebuild_content_matches(state);
                        state.detail_files_state.select(Some(0));
                    }
                }
            }
        }
        (KeyCode::Char(character), modifiers)
            if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
        {
            if let Some(input) = state.active_input.as_mut() {
                input.buffer.push(character);
                match input.purpose {
                    InputPurpose::ListFilter => {
                        state.name_filter = input.buffer.clone();
                    }
                    InputPurpose::ContentFilter => {
                        state.content_filter = input.buffer.clone();
                        state.content_filter_lc = state.content_filter.to_lowercase();
                        rebuild_content_matches(state);
                        state.detail_files_state.select(Some(0));
                    }
                }
            }
        }
        _ => {}
    }
    false
}

fn handle_picker_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    let Some(selected) = state.column_picker else { return false; };
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
            state.column_picker = None;
            persist_visible_columns(&state.visible_columns);
        }
        (KeyCode::Char('s'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
            state.column_picker = Some((selected + 1).min(Column::ALL.len() - 1));
        }
        (KeyCode::Char('w'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
            state.column_picker = Some(selected.saturating_sub(1));
        }
        (KeyCode::Char(' '), _) | (KeyCode::Enter, _) => {
            let column = Column::ALL[selected];
            if let Some(position) = state.visible_columns.iter().position(|c| *c == column) {
                // never let the user hide every column — the list would be empty
                if (state.visible_columns.len() > 1) {
                    state.visible_columns.remove(position);
                }
            } else {
                state.visible_columns.push(column);
            }
        }
        // shift+up/down reorders the selected column up or down within the visible set
        (KeyCode::Char('W'), KeyModifiers::SHIFT) => {
            move_visible_column(state, Column::ALL[selected], -1);
        }
        (KeyCode::Char('S'), KeyModifiers::SHIFT) => {
            move_visible_column(state, Column::ALL[selected], 1);
        }
        _ => {}
    }
    false
}

fn move_visible_column(state: &mut AppState, column: Column, delta: isize) {
    if let Some(position) = state.visible_columns.iter().position(|c| *c == column) {
        let target = (position as isize + delta).clamp(0, state.visible_columns.len() as isize - 1) as usize;
        if (target != position) { state.visible_columns.swap(position, target); }
    }
}

fn persist_visible_columns(visible: &[Column]) {
    // best-effort save — ignore errors so a r/o config dir doesn't crash the tui
    let Ok(mut config) = Config::load() else { return; };
    config.tui_columns = visible.iter().map(|column| column.key().to_string()).collect();
    let _ = config.save();
}

/// route a mouse event. only fires in main mode — overlays (prompt, settings)
/// are keyboard-only for now to keep input flow predictable.
fn handle_mouse(event: MouseEvent, state: &mut AppState) {
    if (state.prompt.is_some() || matches!(state.mode, Mode::Settings(_)) || matches!(state.mode, Mode::Feeds(_))) {
        return;
    }
    let column = event.column;
    let row = event.row;
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => mouse_left_down(column, row, state),
        MouseEventKind::Drag(MouseButton::Left) => mouse_drag(column, state),
        MouseEventKind::Up(MouseButton::Left) => mouse_left_up(state),
        // right-click on the header row of the torrent list opens the column
        // picker. matches the qbittorrent convention.
        MouseEventKind::Down(MouseButton::Right) => {
            if (row == state.header_y && rect_contains(state.list_rect, column, row)) {
                state.column_picker = Some(0);
            }
        }
        MouseEventKind::ScrollUp => mouse_scroll(column, row, state, -3),
        MouseEventKind::ScrollDown => mouse_scroll(column, row, state, 3),
        _ => {}
    }
}

/// while a drag-resize is active, recompute the dragged column's width
/// from the cursor's horizontal delta. the column to the left of the
/// divider is the one being resized (per the user spec); the column to
/// the right absorbs the leftover via `compute_column_widths`.
fn mouse_drag(column: u16, state: &mut AppState) {
    let Some(drag) = state.column_drag else { return; };
    let dx = column as i32 - drag.start_x as i32;
    let new_width = (drag.start_width as i32 + dx).max(MIN_COLUMN_WIDTH as i32) as u16;
    if let Some(target) = state.visible_columns.get(drag.column_index).copied() {
        state.column_width_overrides.insert(target.key().to_string(), new_width);
    }
}

/// commit the drag-resize: persist the new column_widths to config, clear
/// the active drag.
fn mouse_left_up(state: &mut AppState) {
    if (state.column_drag.is_some()) {
        state.column_drag = None;
        persist_column_widths(&state.column_width_overrides);
    }
}

fn persist_column_widths(overrides: &std::collections::BTreeMap<String, u16>) {
    let Ok(mut config) = Config::load() else { return; };
    config.tui_column_widths = overrides.clone();
    let _ = config.save();
}

fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    if (rect.width == 0 || rect.height == 0) { return false; }
    column >= rect.x
        && column < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

fn mouse_left_down(column: u16, row: u16, state: &mut AppState) {
    // column-divider drag-resize: when the click lands on the header row at
    // a known boundary x-position, start a drag instead of selecting a row.
    // boundaries on the data rows are ignored (they're spaces anyway).
    if (row == state.header_y || row == state.header_y + 1) {
        if let Some(position) = state.column_boundaries.iter().position(|x| *x == column) {
            let current_width = state.column_width_overrides
                .get(state.visible_columns[position].key())
                .copied()
                .unwrap_or_else(|| state.visible_columns[position].default_width_cells());
            state.column_drag = Some(ColumnDrag {
                column_index: position,
                start_x: column,
                start_width: current_width,
            });
            return;
        }
    }
    // tab bar click — switch detail tab. tab bar lives at the top of the
    // detail pane; each label takes label.width + separator.
    if (rect_contains(state.detail_tab_bar_rect, column, row)) {
        // tabs are separated by " │ " and the first tab is left-aligned at x.
        // for a robust hit-test we measure label widths in order.
        let mut x_cursor = state.detail_tab_bar_rect.x;
        for tab in DetailTab::ALL.iter() {
            let width = tab.label().len() as u16;
            let end = x_cursor + width;
            if (column >= x_cursor && column < end) {
                state.detail_tab = *tab;
                state.focus = Pane::Detail;
                return;
            }
            x_cursor = end + 3; // " │ " separator
        }
        state.focus = Pane::Detail;
        return;
    }
    // sidebar row click — select and apply filter
    if (rect_contains(state.sidebar_rect, column, row)) {
        let row_in_pane = row.saturating_sub(state.sidebar_rect.y + 1);
        let target = row_in_pane as usize;
        let item_count = state.sidebar_items().len();
        if (target < item_count) {
            state.sidebar_state.select(Some(target));
            state.apply_sidebar_selection();
        }
        state.focus = Pane::Sidebar;
        return;
    }
    // torrent list row click — select that torrent. detect double-click for
    // open-detail (qBT-style).
    if (rect_contains(state.list_rect, column, row)) {
        // border + column header + divider, then data rows
        let header_offset = 2;
        let row_in_data = row.saturating_sub(state.list_rect.y + header_offset);
        let visible = state.filtered_indices();
        // the table scrolls to keep the selection visible, so the first
        // on-screen data row corresponds to table_state.offset(), not 0
        let target = state.table_state.offset() + row_in_data as usize;
        if (target < visible.len()) {
            state.table_state.select(Some(target));
        }
        state.focus = Pane::List;
        let now = Instant::now();
        let is_double = state.last_click
            .map(|(when, prev_col, prev_row)| {
                prev_col == column && prev_row == row && now.duration_since(when) < Duration::from_millis(400)
            })
            .unwrap_or(false);
        state.last_click = Some((now, column, row));
        if (is_double) {
            // open the detail pane if it wasn't already open
            if (!state.show_detail) {
                state.show_detail = true;
                state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
            }
        }
        return;
    }
    // click inside the detail pane (not on the tab bar) — focus it
    if (rect_contains(state.detail_rect, column, row)) {
        state.focus = Pane::Detail;
    }
}

fn mouse_scroll(column: u16, row: u16, state: &mut AppState, delta: isize) {
    // route the scroll to whichever pane the cursor is over so independent
    // list/detail scrolling works without focus juggling
    if (rect_contains(state.list_rect, column, row)) {
        let length = state.filtered_indices().len();
        move_table(&mut state.table_state, length, delta);
    } else if (rect_contains(state.detail_rect, column, row)) {
        match state.detail_tab {
            DetailTab::Content => {
                let count = if state.content_filter.is_empty() {
                    state.detail.as_ref()
                        .map(|detail| build_tree_rows(detail, &state.collapsed_folders).len())
                        .unwrap_or(0)
                } else {
                    state.content_filter_matches.len()
                };
                move_table(&mut state.detail_files_state, count, delta);
            }
            DetailTab::Peers => {
                let count = state.detail.as_ref().map(|detail| detail.peers.len()).unwrap_or(0);
                move_table(&mut state.detail_peers_state, count, delta);
            }
            DetailTab::Trackers => {
                let count = state.detail.as_ref().map(|detail| detail.trackers.len()).unwrap_or(0);
                move_table(&mut state.detail_trackers_state, count, delta);
            }
        }
    } else if (rect_contains(state.sidebar_rect, column, row)) {
        let count = state.sidebar_items().len();
        move_list(&mut state.sidebar_state, count, delta);
        state.apply_sidebar_selection();
    }
}

fn toggle_pause(state: &mut AppState) {
    let Some(index) = state.selected_torrent_index() else { return; };
    let Some(torrent) = state.torrents.get(index) else { return; };
    let request = if (torrent.is_paused) {
        Request::Resume { index }
    } else {
        Request::Pause { index }
    };
    if let Err(error) = client::send(request) {
        state.error = Some(format!("pause/resume: {}", error));
    } else {
        state.last_poll = Instant::now() - POLL_INTERVAL;
    }
}

fn poll_daemon(state: &mut AppState) {
    state.last_poll = Instant::now();
    match client::send(Request::List) {
        Ok(Response::TorrentList(list)) => {
            state.torrents = list;
            state.daemon_unreachable = false;
            state.error = None;
            let visible = state.filtered_indices().len();
            if let Some(selected) = state.table_state.selected() {
                if (visible == 0) {
                    state.table_state.select(None);
                } else if (selected >= visible) {
                    state.table_state.select(Some(visible - 1));
                }
            } else if (visible > 0) {
                state.table_state.select(Some(0));
            }
        }
        Ok(Response::Err(message)) => state.error = Some(message),
        Ok(_) => state.error = Some("unexpected response from daemon".to_string()),
        Err(error) => {
            state.daemon_unreachable = true;
            state.error = Some(format!("daemon: {}", error));
        }
    }
    match client::send(Request::Stats) {
        Ok(Response::Stats(stats)) => state.stats = Some(stats),
        Ok(_) => {}
        Err(_) => state.stats = None,
    }
    if let Ok(Response::Categories(categories)) = client::send(Request::ListCategories) {
        let mut names: Vec<String> = categories.into_iter().map(|c| c.name).collect();
        names.sort();
        state.sidebar_categories = names;
    }
}

fn poll_detail(state: &mut AppState) {
    state.last_detail_poll = Instant::now();
    let Some(index) = state.selected_torrent_index() else {
        state.detail = None;
        state.detail_paths_lc.clear();
        state.content_filter_matches.clear();
        return;
    };
    match client::send(Request::Info { index }) {
        Ok(Response::TorrentDetail(detail)) => {
            state.detail = Some(*detail);
            rebuild_detail_cache(state);
        }
        Ok(_) => {}
        Err(_) => {}
    }
}

fn poll_feeds_page(state: &mut AppState) {
    let Mode::Feeds(feeds) = &mut state.mode else { return; };
    if (feeds.last_poll.elapsed() < Duration::from_secs(2)) { return; }
    feeds.last_poll = Instant::now();
    if let Ok(Response::Feeds(list)) = client::send(Request::ListFeeds) {
        let selected = feeds.table_state.selected().unwrap_or(0);
        feeds.feeds = list;
        if (!feeds.feeds.is_empty()) {
            feeds.table_state.select(Some(selected.min(feeds.feeds.len() - 1)));
        } else {
            feeds.table_state.select(None);
        }
    }
}

fn handle_feeds_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
            state.mode = Mode::Main;
        }
        (KeyCode::Char('s'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
            if let Mode::Feeds(feeds) = &mut state.mode { feeds.move_selection(1); }
        }
        (KeyCode::Char('w'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
            if let Mode::Feeds(feeds) = &mut state.mode { feeds.move_selection(-1); }
        }
        (KeyCode::PageDown, _) => {
            if let Mode::Feeds(feeds) = &mut state.mode { feeds.move_selection(10); }
        }
        (KeyCode::PageUp, _) => {
            if let Mode::Feeds(feeds) = &mut state.mode { feeds.move_selection(-10); }
        }
        (KeyCode::Char('n'), KeyModifiers::NONE) => {
            state.prompt = Some(Prompt {
                title: " add feed — enter url ".to_string(),
                helper: "url of the rss/atom feed to subscribe to".to_string(),
                lines: vec![String::new()],
                cursor_line: 0,
                action: PromptAction::AddFeed,
                torrent_index: 0,
                allow_multiline: false,
            });
        }
        (KeyCode::Delete, _) | (KeyCode::Char('x'), KeyModifiers::NONE) => {
            let selected = if let Mode::Feeds(feeds) = &state.mode { feeds.selected() } else { None };
            if let Some(index) = selected {
                match client::send(Request::RemoveFeed { index }) {
                    Ok(Response::Ok) => {
                        if let Mode::Feeds(feeds) = &mut state.mode {
                            feeds.last_poll = Instant::now() - Duration::from_secs(10);
                            feeds.status = Some(format!("feed {} removed", index));
                        }
                    }
                    Ok(Response::Err(message)) => {
                        if let Mode::Feeds(feeds) = &mut state.mode {
                            feeds.status = Some(format!("error: {}", message));
                        }
                    }
                    _ => {}
                }
            }
        }
        (KeyCode::Char('p'), KeyModifiers::NONE) => {
            if let Ok(Response::Ok) = client::send(Request::PollFeeds) {
                if let Mode::Feeds(feeds) = &mut state.mode {
                    feeds.status = Some("poll triggered".to_string());
                }
            }
        }
        _ => {}
    }
    false
}

fn draw_feeds(frame: &mut ratatui::Frame, state: &mut AppState) {
    let Mode::Feeds(feeds) = &mut state.mode else { return; };
    let area = frame.area();

    let layout = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Min(0),    // feed list
        Constraint::Length(1), // detail row for selected feed
        Constraint::Length(1), // hint / status bar
    ])
    .split(area);

    // title bar
    let title = Line::from(vec![
        Span::styled(" feeds ", Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled("esc to return", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(title), layout[0]);

    // feed list
    let header_cells = [
        Cell::from("index").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("interval").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("filter").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("url").style(Style::default().add_modifier(Modifier::BOLD)),
    ];
    let header = Row::new(header_cells)
        .style(Style::default().fg(Color::DarkGray))
        .height(1);

    let rows: Vec<Row> = feeds.feeds.iter().map(|feed| {
        let interval = format!("{}min", feed.poll_interval_minutes);
        let filter = if (feed.filter.is_empty()) { "(any)".to_string() } else { feed.filter.clone() };
        Row::new([
            Cell::from(feed.index.to_string()),
            Cell::from(interval),
            Cell::from(filter),
            Cell::from(feed.url.clone()),
        ])
    }).collect();

    let empty_msg = if (feeds.feeds.is_empty()) {
        vec![Row::new([Cell::from(""), Cell::from(""), Cell::from(""), Cell::from("no feeds — press n to add one")])]
    } else {
        vec![]
    };

    let all_rows: Vec<Row> = if (feeds.feeds.is_empty()) { empty_msg } else { rows };

    let table = Table::new(all_rows, [
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(28),
        Constraint::Min(20),
    ])
    .header(header)
    .row_highlight_style(selected_row_style())
    .block(Block::default().borders(Borders::NONE));

    frame.render_stateful_widget(table, layout[1], &mut feeds.table_state);

    // detail row: show category/save_path/paused for selected feed
    let detail_line = feeds.table_state.selected()
        .and_then(|i| feeds.feeds.get(i))
        .map(|feed| {
            let mut parts = Vec::new();
            if let Some(cat) = &feed.category { parts.push(format!("category: {}", cat)); }
            if let Some(path) = &feed.save_path { parts.push(format!("save path: {}", path)); }
            if (feed.start_paused) { parts.push("start paused".to_string()); }
            if (parts.is_empty()) { String::new() } else { format!("  {}", parts.join("  ·  ")) }
        })
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(detail_line, Style::default().fg(Color::DarkGray)))),
        layout[2],
    );

    // hint / status
    let hint = if let Some(status) = &feeds.status {
        Line::from(Span::styled(format!(" {}", status), Style::default().fg(Color::Yellow)))
    } else {
        Line::from(vec![
            Span::styled(" n", Style::default().fg(Color::Cyan)),
            Span::styled(" add  ", Style::default().fg(Color::DarkGray)),
            Span::styled("x", Style::default().fg(Color::Cyan)),
            Span::styled(" remove  ", Style::default().fg(Color::DarkGray)),
            Span::styled("p", Style::default().fg(Color::Cyan)),
            Span::styled(" poll all  ", Style::default().fg(Color::DarkGray)),
            Span::styled("esc", Style::default().fg(Color::Cyan)),
            Span::styled(" back", Style::default().fg(Color::DarkGray)),
        ])
    };
    frame.render_widget(Paragraph::new(hint), layout[3]);
}

fn poll_priority_step(state: &mut AppState) {
    let Some(step) = state.priority_step.as_mut() else { return; };
    if (step.last_poll.elapsed() < DETAIL_POLL_INTERVAL) { return; }
    step.last_poll = Instant::now();
    let Some(torrent_index) = step.torrent_index() else { return; };
    if let Ok(Response::TorrentDetail(detail)) = client::send(Request::Info { index: torrent_index }) {
        let detail = *detail;
        step.paths_lc = detail.files.iter().map(|file| file.path.to_lowercase()).collect();
        step.detail = Some(detail);
        step.rebuild_filter_matches();
    }
}

/// advance to the next torrent in the priority step, or close it when done.
fn advance_priority_step(state: &mut AppState) {
    let Some(step) = state.priority_step.as_mut() else { return; };
    if (step.current + 1 < step.indices.len()) {
        step.current += 1;
        let mut files_state = TableState::default();
        files_state.select(Some(0));
        step.files_state = files_state;
        step.detail = None;
        step.paths_lc.clear();
        step.filter_matches.clear();
        step.filter.clear();
        step.filter_lc.clear();
        step.collapsed_folders.clear();
        step.last_poll = Instant::now() - DETAIL_POLL_INTERVAL;
        step.filter_active = false;
    } else {
        state.priority_step = None;
        state.last_poll = Instant::now() - POLL_INTERVAL;
        state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
    }
}

fn handle_priority_step_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    // rename-input mode — handle before filter/nav so esc cancels rename first
    {
        let Some(step) = state.priority_step.as_mut() else { return false; };
        if step.rename_buffer.is_some() {
            match (code, modifiers) {
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
                (KeyCode::Esc, _) => { step.rename_buffer = None; step.rename_target = None; }
                (KeyCode::Enter, _) => { /* handled below — need to reborrow */ }
                (KeyCode::Backspace, _) => { step.rename_buffer.as_mut().unwrap().pop(); }
                (KeyCode::Char(character), modifiers)
                    if !modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::ALT) =>
                { step.rename_buffer.as_mut().unwrap().push(character); }
                _ => {}
            }
            // handle commit separately to avoid borrow issues
            if code == KeyCode::Enter {
                commit_priority_step_rename(state);
            }
            return false;
        }
    }
    // filter-input mode — handle before the navigation block so esc closes
    // the filter rather than skipping the torrent
    {
        let Some(step) = state.priority_step.as_mut() else { return false; };
        if step.filter_active {
            match (code, modifiers) {
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
                (KeyCode::Esc, _) | (KeyCode::Enter, _) => step.filter_active = false,
                (KeyCode::Backspace, _) => {
                    step.filter.pop();
                    step.filter_lc = step.filter.to_lowercase();
                    step.rebuild_filter_matches();
                    step.files_state.select(Some(0));
                }
                (KeyCode::Char(character), modifiers)
                    if !modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    step.filter.push(character);
                    step.filter_lc = step.filter.to_lowercase();
                    step.rebuild_filter_matches();
                    step.files_state.select(Some(0));
                }
                _ => {}
            }
            return false;
        }
    }
    // navigation mode — state-mutating calls checked before re-borrowing so
    // borrow checker doesn't see a live &mut PriorityStep during them
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Tab, _) | (KeyCode::Enter, _) | (KeyCode::Esc, _) => {
            advance_priority_step(state);
            return false;
        }
        (KeyCode::Char('r'), KeyModifiers::NONE) | (KeyCode::F(2), _) => {
            open_priority_step_rename(state);
            return false;
        }
        (KeyCode::Char('t'), KeyModifiers::NONE) => {
            open_priority_step_torrent_rename(state);
            return false;
        }
        _ => {}
    }
    let Some(step) = state.priority_step.as_mut() else { return false; };
    match (code, modifiers) {
        (KeyCode::Char('s'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
            let count = step.row_count();
            move_table(&mut step.files_state, count, 1);
        }
        (KeyCode::Char('w'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
            let count = step.row_count();
            move_table(&mut step.files_state, count, -1);
        }
        (KeyCode::PageDown, _) => {
            let count = step.row_count();
            move_table(&mut step.files_state, count, 10);
        }
        (KeyCode::PageUp, _) => {
            let count = step.row_count();
            move_table(&mut step.files_state, count, -10);
        }
        (KeyCode::Char('a'), KeyModifiers::NONE) | (KeyCode::Left, _) => {
            if step.filter.is_empty() {
                let rows = step.current_rows();
                if let Some(row) = step.files_state.selected().and_then(|i| rows.get(i)) {
                    if row.is_folder { step.collapsed_folders.insert(row.full_path.clone()); }
                }
            }
        }
        (KeyCode::Char('d'), KeyModifiers::NONE) | (KeyCode::Right, _) => {
            if step.filter.is_empty() {
                let rows = step.current_rows();
                if let Some(row) = step.files_state.selected().and_then(|i| rows.get(i)) {
                    if row.is_folder { step.collapsed_folders.remove(&row.full_path); }
                }
            }
        }
        (KeyCode::Char('/'), KeyModifiers::NONE) => step.filter_active = true,
        (KeyCode::Char(character), KeyModifiers::NONE)
            if matches!(character, '0' | '1' | '2' | '3' | '4') =>
        {
            let priority = match character {
                '0' => 0u8, '1' => 1u8, '2' => 4u8, '3' => 6u8, '4' => 7u8,
                _ => unreachable!(),
            };
            set_step_priority(step, priority);
        }
        _ => {}
    }
    false
}

/// set priority on the focused row in the priority step — same cascading
/// folder logic as set_focused_priority, but uses the step's own state.
fn set_step_priority(step: &mut PriorityStep, priority: u8) {
    let Some(torrent_index) = step.torrent_index() else { return; };
    let targets: Vec<usize> = {
        let Some(detail) = &step.detail else { return; };
        let rows = step.current_rows();
        let Some(row) = step.files_state.selected().and_then(|i| rows.get(i)) else { return; };
        if row.is_folder {
            let prefix = format!("{}/", row.full_path);
            detail.files.iter().enumerate()
                .filter(|(_, file)| file.path == row.full_path || file.path.starts_with(&prefix))
                .map(|(i, _)| i)
                .collect()
        } else if let Some(file_index) = row.file_index {
            vec![file_index]
        } else {
            Vec::new()
        }
    };
    if targets.is_empty() { return; }
    let priorities = targets.iter().map(|&i| (i, priority)).collect();
    let _ = client::send(Request::SetFilePrioritiesBatch { index: torrent_index, priorities });
    step.last_poll = Instant::now() - DETAIL_POLL_INTERVAL;
}

fn open_priority_step_rename(state: &mut AppState) {
    let Some(step) = state.priority_step.as_mut() else { return; };
    let rows = step.current_rows();
    let Some(row) = step.files_state.selected().and_then(|i| rows.get(i)).cloned() else { return; };
    if row.is_folder {
        step.rename_target = Some(PriorityRenameTarget::Folder { old_prefix: row.full_path.clone() });
        step.rename_buffer = Some(row.label.clone());
    } else {
        let file_index = match &step.detail {
            Some(detail) => detail.files.iter().enumerate()
                .find(|(_, file)| file.path == row.full_path)
                .map(|(i, _)| i),
            None => None,
        };
        let Some(file_index) = file_index else { return; };
        // use just the filename component as the initial buffer
        let filename = row.full_path.rsplit('/').next().unwrap_or(&row.full_path).to_string();
        step.rename_target = Some(PriorityRenameTarget::File { file_index });
        step.rename_buffer = Some(filename);
    }
}

fn open_priority_step_torrent_rename(state: &mut AppState) {
    let Some(step) = state.priority_step.as_mut() else { return; };
    let name = step.detail.as_ref().map(|detail| detail.info.name.clone()).unwrap_or_default();
    step.rename_target = Some(PriorityRenameTarget::Torrent);
    step.rename_buffer = Some(name);
}

fn commit_priority_step_rename(state: &mut AppState) {
    let Some(step) = state.priority_step.as_mut() else { return; };
    let buffer = match step.rename_buffer.take() {
        Some(b) => b,
        None => return,
    };
    let target = match step.rename_target.take() {
        Some(t) => t,
        None => return,
    };
    let Some(torrent_index) = step.torrent_index() else { return; };
    step.last_poll = Instant::now() - DETAIL_POLL_INTERVAL;
    match target {
        PriorityRenameTarget::Torrent =>
            { let _ = client::send(Request::RenameTorrent { index: torrent_index, new_name: buffer }); }
        PriorityRenameTarget::File { file_index } =>
            { let _ = client::send(Request::RenameFile { index: torrent_index, file_index, new_name: buffer }); }
        PriorityRenameTarget::Folder { old_prefix } =>
            { let _ = client::send(Request::RenameFolder { index: torrent_index, old_prefix, new_prefix: buffer, decisions: None }); }
    }
}

/// rebuild detail_paths_lc from the current detail, then recompute
/// content_filter_matches if a filter is active. call whenever detail
/// or content_filter changes.
fn rebuild_detail_cache(state: &mut AppState) {
    state.detail_paths_lc = state.detail.as_ref()
        .map(|detail| detail.files.iter().map(|file| file.path.to_lowercase()).collect())
        .unwrap_or_default();
    rebuild_content_matches(state);
}

/// recompute content_filter_matches from the precomputed lowercase paths.
/// O(n * m) with zero allocations: paths are already lowercase, needle
/// is lowercased once into content_filter_lc.
fn rebuild_content_matches(state: &mut AppState) {
    if state.content_filter.is_empty() {
        state.content_filter_matches.clear();
        return;
    }
    state.content_filter_matches = state.detail_paths_lc.iter().enumerate()
        .filter(|(_, path_lc)| fuzzy_match_lc(path_lc, &state.content_filter_lc))
        .map(|(i, _)| i)
        .collect();
}

// expand the content filter buffer to the longest common prefix of all
// currently matching paths. case-insensitive comparison, case from first match.
fn tab_complete_content_filter(state: &mut AppState) {
    if state.content_filter_matches.is_empty() { return; }
    let paths: Vec<String> = {
        let Some(detail) = &state.detail else { return; };
        state.content_filter_matches.iter()
            .map(|&i| detail.files[i].path.clone())
            .collect()
    };
    let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let prefix = longest_common_prefix_ci(&path_refs);
    if prefix.chars().count() > state.content_filter.chars().count() {
        state.content_filter = prefix;
        state.content_filter_lc = state.content_filter.to_lowercase();
        if let Some(input) = state.active_input.as_mut() {
            input.buffer = state.content_filter.clone();
        }
        rebuild_content_matches(state);
        state.detail_files_state.select(Some(0));
    }
}

// expand `buffer` to the longest common prefix of all filesystem entries
// that start with the partial name after the last `/`.
fn tab_complete_path(buffer: &str) -> String {
    let (dir_part, prefix) = match buffer.rfind('/') {
        None => (".", ""),
        Some(index) => (&buffer[..=index], &buffer[index + 1..]),
    };
    let Ok(entries) = std::fs::read_dir(dir_part) else { return buffer.to_string(); };
    let prefix_lc = prefix.to_lowercase();
    let mut candidates: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.to_lowercase().starts_with(&prefix_lc) { return None; }
            let is_dir = entry.file_type().map(|file_type| file_type.is_dir()).unwrap_or(false);
            let candidate = if dir_part == "." {
                if is_dir { format!("{}/", name) } else { name }
            } else {
                if is_dir { format!("{}{}/", dir_part, name) } else { format!("{}{}", dir_part, name) }
            };
            Some(candidate)
        })
        .collect();
    candidates.sort();
    match candidates.len() {
        0 => buffer.to_string(),
        1 => candidates.remove(0),
        _ => {
            let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
            longest_common_prefix_ci(&refs)
        }
    }
}

fn longest_common_prefix_ci(paths: &[&str]) -> String {
    let Some(first) = paths.first() else { return String::new(); };
    let first_chars: Vec<char> = first.chars().collect();
    let mut common = first_chars.len();
    for path in &paths[1..] {
        let count = first_chars.iter().zip(path.chars())
            .take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
            .count();
        common = common.min(count);
        if common == 0 { break; }
    }
    first_chars[..common].iter().collect()
}

fn draw(frame: &mut ratatui::Frame, state: &mut AppState) {
    if (state.priority_step.is_some()) {
        draw_priority_step(frame, state);
        return;
    }
    if (matches!(state.mode, Mode::Settings(_))) {
        draw_settings(frame, state);
    } else if (matches!(state.mode, Mode::Feeds(_))) {
        draw_feeds(frame, state);
    } else {
        draw_main(frame, state);
    }
    if (state.prompt.is_some()) {
        draw_prompt(frame, state);
    }
    if (state.column_picker.is_some()) {
        draw_column_picker(frame, state);
    }
    if (state.confirm_delete.is_some()) {
        draw_delete_confirm(frame, state);
    }
    if (state.show_help) {
        draw_help_overlay(frame);
    }
    if (state.add_options.is_some()) {
        draw_add_options_form(frame, state);
    }
}

fn draw_priority_step(frame: &mut ratatui::Frame, state: &mut AppState) {
    let Some(step) = state.priority_step.as_mut() else { return; };
    let area = frame.area();

    let layout = Layout::vertical([
        Constraint::Length(1), // title bar
        Constraint::Length(1), // subtitle
        Constraint::Min(0),    // file table
        Constraint::Length(1), // filter bar
        Constraint::Length(1), // hint bar
    ])
    .split(area);

    // title
    let total = step.entries.len();
    let current_num = step.current + 1;
    let entry_label = step.entries.get(step.current).cloned().unwrap_or_default();
    let title = Line::from(vec![
        Span::styled(" set file priorities ", Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(
            format!("torrent {}/{}", current_num, total),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), layout[0]);

    // subtitle shows the URI/name being configured
    let subtitle = Line::from(vec![
        Span::raw(" "),
        Span::styled(&entry_label, Style::default().fg(Color::Yellow)),
    ]);
    frame.render_widget(Paragraph::new(subtitle), layout[1]);

    // file table
    if step.detail.is_none() {
        let waiting = Paragraph::new("waiting for metadata...")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(waiting, layout[2]);
    } else {
        let rows_data = step.current_rows();

        let header = Row::new([
            Cell::from("name").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("size").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("priority").style(Style::default().add_modifier(Modifier::BOLD)),
        ]);

        let rows: Vec<Row> = rows_data.iter().map(|tree_row| {
            let indent = "  ".repeat(tree_row.indent);
            let priority_label = if tree_row.is_mixed {
                "mixed".to_string()
            } else {
                match tree_row.priority {
                    None => "—".to_string(),
                    Some(0) => "skip".to_string(),
                    Some(1..=3) => format!("low/{}", tree_row.priority.unwrap()),
                    Some(4) => "normal".to_string(),
                    Some(5..=6) => format!("high/{}", tree_row.priority.unwrap()),
                    Some(7) => "max".to_string(),
                    Some(other) => other.to_string(),
                }
            };
            let row_style = if tree_row.is_folder {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else if tree_row.priority == Some(0) {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(format!("{}{}", indent, tree_row.label)),
                Cell::from(crate::display::format_bytes(tree_row.total_size)),
                Cell::from(priority_label),
            ])
            .style(row_style)
        }).collect();

        let widths = [
            Constraint::Min(30),
            Constraint::Length(10),
            Constraint::Length(10),
        ];

        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(selected_row_style())
            .highlight_symbol("▌ ");

        frame.render_stateful_widget(table, layout[2], &mut step.files_state);
    }

    // filter/rename bar — rename takes priority
    if let Some(rename_buf) = &step.rename_buffer {
        let rename_line = Line::from(vec![
            Span::styled(" rename ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(" "),
            Span::raw(rename_buf.as_str()),
            Span::styled("█", Style::default().fg(Color::Yellow)),
            Span::raw("   "),
            Span::styled("esc cancel / enter confirm", Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(rename_line), layout[3]);
    } else if step.filter_active {
        let filter_line = Line::from(vec![
            Span::styled(" files ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(" "),
            Span::raw(step.filter.as_str()),
            Span::styled("█", Style::default().fg(Color::Yellow)),
            Span::raw("   "),
            Span::styled("esc cancel / enter close", Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(filter_line), layout[3]);
    } else if !step.filter.is_empty() {
        let filter_line = Line::from(vec![
            Span::styled(" files ", Style::default().fg(Color::Black).bg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(step.filter.as_str(), Style::default().fg(Color::DarkGray)),
            Span::raw("   "),
            Span::styled("/ to edit", Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(filter_line), layout[3]);
    } else {
        let hint = Span::styled(" / to filter files", Style::default().fg(Color::DarkGray));
        frame.render_widget(Paragraph::new(Line::from(vec![hint])), layout[3]);
    }

    // hint bar
    let hint = Line::from(vec![
        Span::styled(" 0 ", Style::default().fg(Color::Yellow)), Span::raw("skip  "),
        Span::styled("1 ", Style::default().fg(Color::Yellow)), Span::raw("low  "),
        Span::styled("2 ", Style::default().fg(Color::Yellow)), Span::raw("normal  "),
        Span::styled("3 ", Style::default().fg(Color::Yellow)), Span::raw("high  "),
        Span::styled("4 ", Style::default().fg(Color::Yellow)), Span::raw("max  "),
        Span::styled("r ", Style::default().fg(Color::Yellow)), Span::raw("rename  "),
        Span::styled("t ", Style::default().fg(Color::Yellow)), Span::raw("rename torrent  "),
        Span::styled("enter/esc ", Style::default().fg(Color::Yellow)),
        Span::raw(if total > 1 { "next torrent  " } else { "done  " }),
    ]);
    frame.render_widget(Paragraph::new(hint), layout[4]);
}

fn draw_add_options_form(frame: &mut ratatui::Frame, state: &AppState) {
    let Some(form) = state.add_options.as_ref() else { return; };
    let area = frame.area();
    let width = (area.width * 70 / 100).clamp(50, area.width.saturating_sub(4));
    let height = 18u16.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect { x, y, width, height };

    frame.render_widget(ratatui::widgets::Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(format!(" add options  ({}/{}) ", form.current + 1, form.entries.len()));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let layout = Layout::vertical([
        Constraint::Length(1), // uri
        Constraint::Length(1), // helper
        Constraint::Length(1), // gap
        Constraint::Min(0),    // fields
        Constraint::Length(1), // hint
    ]).split(inner);

    let uri = form.entries.get(form.current).cloned().unwrap_or_default();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" source: ", Style::default().fg(Color::DarkGray)),
            Span::styled(uri, Style::default().fg(Color::Cyan)),
        ])),
        layout[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " w/s/tab move · enter confirm · esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        layout[1],
    );

    let options = form.options.get(form.current).cloned().unwrap_or_default();
    let editing_path = form.edit_buffer.is_some();
    let path_display = if (editing_path) {
        format!("[ {}_ ]", form.edit_buffer.as_deref().unwrap_or(""))
    } else if (options.save_path.is_empty()) {
        "(default — daemon's default_save_path)".to_string()
    } else {
        options.save_path.clone()
    };

    let button_label = if (form.current + 1 < form.entries.len()) { "[ next → ]" } else { "[ add ]" };
    let rows: Vec<(&str, String)> = vec![
        ("start",          format_bool(options.start)),
        ("sequential",     format_bool(options.sequential)),
        ("first/last",     format_bool(options.first_last).to_string()),
        ("create subfolder", options.content_layout.label().to_string()),
        ("download path",  path_display),
        ("",               button_label.to_string()),
    ];
    let lines: Vec<Line> = rows.iter().enumerate().map(|(index, (label, value))| {
        let is_focused = index == form.field;
        let marker = if (is_focused) { "▌ " } else { "  " };
        let label_style = if (is_focused) {
            Style::default().add_modifier(Modifier::BOLD).fg(Color::White)
        } else {
            Style::default()
        };
        let value_style = if (is_focused && editing_path && index == 4) {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else if (index == 5 && is_focused) {
            Style::default().fg(Color::Green)
        } else if (is_focused) {
            Style::default().fg(Color::Cyan)
        } else if (index == 5) {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Gray)
        };
        Line::from(vec![
            Span::styled(marker, Style::default().fg(Color::Cyan)),
            Span::styled(format!("{:18}", label), label_style),
            Span::raw("  "),
            Span::styled(value.clone(), value_style),
        ])
    }).collect();
    frame.render_widget(Paragraph::new(lines), layout[3]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " enter on [ add ] to dispatch",
            Style::default().fg(Color::DarkGray),
        ))),
        layout[4],
    );
}

fn format_bool(value: bool) -> String {
    if (value) { "● on".to_string() } else { "○ off".to_string() }
}

fn draw_help_overlay(frame: &mut ratatui::Frame) {
    const BINDS: &[(&str, &str)] = &[
        ("w/s  ↑/↓", "move selection"),
        ("a/d  ←/→", "collapse/expand tree or nav"),
        ("tab", "cycle pane focus"),
        ("enter", "confirm / apply"),
        ("p", "pause / resume"),
        ("n  ctrl+n", "add torrent"),
        ("x  del", "delete torrent"),
        ("r  F2", "rename torrent (or file/folder in content tab)"),
        ("m", "move save path"),
        ("R", "force recheck"),
        ("T", "reannounce"),
        ("L", "set per-torrent rate limits"),
        ("g", "copy magnet link"),
        ("S", "toggle sequential download"),
        ("0-4", "set file priority (content tab)"),
        ("/", "filter torrents by name"),
        ("[  ]", "cycle detail tabs"),
        ("q", "toggle sidebar"),
        ("e", "toggle detail pane"),
        (",  ctrl+,", "open settings"),
        ("u", "open feeds page"),
        ("C", "column picker"),
        ("?", "this help"),
        ("ctrl+c", "quit (daemon keeps running)"),
    ];
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let height = (BINDS.len() as u16 + 4).min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect { x, y, width, height };

    frame.render_widget(ratatui::widgets::Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" keybinds ");
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let layout = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);

    let key_width = 16usize;
    let lines: Vec<Line> = BINDS.iter().map(|(key, desc)| {
        Line::from(vec![
            Span::styled(format!("{:width$}", key, width = key_width), Style::default().fg(Color::Yellow)),
            Span::raw(*desc),
        ])
    }).collect();
    frame.render_widget(Paragraph::new(lines), layout[0]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "any key to close",
            Style::default().fg(Color::DarkGray),
        ))),
        layout[1],
    );
}

fn draw_column_picker(frame: &mut ratatui::Frame, state: &AppState) {
    let Some(selected) = state.column_picker else { return; };
    let area = frame.area();
    let width = 48u16.min(area.width.saturating_sub(4));
    let height: u16 = (Column::ALL.len() as u16 + 5).min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect { x, y, width, height };

    frame.render_widget(ratatui::widgets::Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" columns ");
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " space toggle  shift+w/s reorder  esc save+close",
            Style::default().fg(Color::DarkGray),
        ))),
        layout[0],
    );

    let lines: Vec<Line> = Column::ALL.iter().enumerate().map(|(index, column)| {
        let visible_position = state.visible_columns.iter().position(|c| *c == *column);
        let marker = match visible_position {
            Some(position) => format!(" [{}] ", position + 1),
            None => " [ ] ".to_string(),
        };
        let marker_style = if (visible_position.is_some()) {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let label_style = if (index == selected) {
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        Line::from(vec![
            Span::styled(marker, marker_style),
            Span::styled(format!("{:18}", column.label()), label_style),
            Span::styled(format!("  {}", column.key()), Style::default().fg(Color::DarkGray)),
        ])
    }).collect();
    frame.render_widget(Paragraph::new(lines), layout[1]);

    let count = state.visible_columns.len();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {} visible", count), Style::default().fg(Color::Cyan)),
            Span::raw("    "),
            Span::styled("(saved to config.toml on close)", Style::default().fg(Color::DarkGray)),
        ])),
        layout[2],
    );
}

fn draw_prompt(frame: &mut ratatui::Frame, state: &AppState) {
    let Some(prompt) = &state.prompt else { return; };
    let area = frame.area();
    // height grows with the number of lines, clamped to the available area
    let line_count = prompt.lines.len().max(1) as u16;
    let body_height = line_count.min(12);
    let height = (body_height + 5).min(area.height.saturating_sub(2));
    let width = (area.width * 70 / 100).clamp(40, area.width.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect { x, y, width, height };

    frame.render_widget(ratatui::widgets::Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(format!(" {} ", prompt.title));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let layout = Layout::vertical([
        Constraint::Length(1),         // helper
        Constraint::Length(1),         // gap
        Constraint::Min(body_height),  // text buffer (one row per line)
        Constraint::Length(1),         // hint
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            prompt.helper.as_str(),
            Style::default().fg(Color::DarkGray),
        ))),
        layout[0],
    );

    let body_lines: Vec<Line> = prompt.lines.iter().enumerate().map(|(index, content)| {
        let is_cursor = index == prompt.cursor_line;
        let marker = if (is_cursor) { "› " } else { "  " };
        let mut spans = vec![
            Span::styled(marker, Style::default().fg(Color::Yellow)),
            Span::raw(content.as_str()),
        ];
        if (is_cursor) {
            spans.push(Span::styled("█", Style::default().fg(Color::Yellow)));
        }
        Line::from(spans)
    }).collect();
    frame.render_widget(Paragraph::new(body_lines), layout[2]);

    let hint = if (prompt.allow_multiline) {
        Line::from(vec![
            Span::styled(" enter ", Style::default().fg(Color::Yellow)),
            Span::raw("submit  "),
            Span::styled("shift+enter ", Style::default().fg(Color::Yellow)),
            Span::raw("new line  "),
            Span::styled("↑↓ ", Style::default().fg(Color::Yellow)),
            Span::raw("move  "),
            Span::styled("esc ", Style::default().fg(Color::Yellow)),
            Span::raw("cancel"),
        ])
    } else {
        Line::from(vec![
            Span::styled(" enter ", Style::default().fg(Color::Yellow)),
            Span::raw("submit  "),
            Span::styled("esc ", Style::default().fg(Color::Yellow)),
            Span::raw("cancel"),
        ])
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::Gray)),
        layout[3],
    );
}

fn draw_main(frame: &mut ratatui::Frame, state: &mut AppState) {
    let area = frame.area();
    let outer = Layout::vertical([
        Constraint::Length(1), // title bar
        Constraint::Min(0),    // main area
        Constraint::Length(1), // status bar
        Constraint::Length(1), // hint bar
    ])
    .split(area);

    draw_title(frame, outer[0]);

    let main = outer[1];
    let with_sidebar = if (state.show_sidebar) {
        Layout::horizontal([Constraint::Length(22), Constraint::Min(0)]).split(main)
    } else {
        Layout::horizontal([Constraint::Length(0), Constraint::Min(0)]).split(main)
    };

    if (state.show_sidebar) {
        state.sidebar_rect = with_sidebar[0];
        draw_sidebar(frame, with_sidebar[0], state);
    } else {
        state.sidebar_rect = Rect::default();
    }

    let center = with_sidebar[1];
    let center_split = if (state.show_detail) {
        Layout::vertical([Constraint::Min(5), Constraint::Percentage(40)]).split(center)
    } else {
        Layout::vertical([Constraint::Min(0), Constraint::Length(0)]).split(center)
    };

    // when an inline input is active, carve a 1-row strip off the bottom of
    // the torrent list for the input bar. otherwise the list takes the full
    // center area.
    let list_area = center_split[0];
    let (list_inner, input_bar) = if (state.active_input.is_some()) {
        let split = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(list_area);
        (split[0], Some(split[1]))
    } else {
        (list_area, None)
    };
    state.list_rect = list_inner;
    draw_torrent_list(frame, list_inner, state);
    if let Some(input_rect) = input_bar {
        draw_input_bar(frame, input_rect, state);
    }
    if (state.show_detail) {
        state.detail_rect = center_split[1];
        draw_detail(frame, center_split[1], state);
    } else {
        state.detail_rect = Rect::default();
        state.detail_tab_bar_rect = Rect::default();
    }

    draw_status_bar(frame, outer[2], state);
    draw_hint_bar(frame, outer[3], state);
}

fn focus_border_style(focused: bool) -> Style {
    if (focused) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// selection highlight used by every selectable table. forces an explicit
/// fg+bg so rows greyed out via fg(DarkGray) (paused torrents, skip-priority
/// files) stay legible when selected instead of going DarkGray-on-DarkGray.
fn selected_row_style() -> Style {
    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
}

fn draw_title(frame: &mut ratatui::Frame, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            " monsoon ",
            Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(crate::VERSION, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn draw_sidebar(frame: &mut ratatui::Frame, area: Rect, state: &mut AppState) {
    let items: Vec<ListItem> = state.sidebar_items().iter().map(|item| {
        match item {
            SidebarItem::StatusHeader => {
                let line = Line::from(Span::styled(
                    "  STATUS",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                ));
                ListItem::new(line)
            }
            SidebarItem::CategoryHeader => {
                let line = Line::from(Span::styled(
                    "  CATEGORIES",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                ));
                ListItem::new(line)
            }
            SidebarItem::Status(filter) => {
                let active = *filter == state.status_filter && state.category_filter.is_none();
                let count = state.torrents.iter().filter(|t| filter.matches(t)).count();
                let mark = if (active) { "● " } else { "  " };
                let line = Line::from(vec![
                    Span::styled(mark, Style::default().fg(Color::Cyan)),
                    Span::raw(filter.label()),
                    Span::raw("  "),
                    Span::styled(format!("({})", count), Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            }
            SidebarItem::CategoryAll => {
                let active = state.category_filter.is_none();
                let count = state.torrents.len();
                let mark = if (active) { "● " } else { "  " };
                let line = Line::from(vec![
                    Span::styled(mark, Style::default().fg(Color::Cyan)),
                    Span::raw("all"),
                    Span::raw("  "),
                    Span::styled(format!("({})", count), Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            }
            SidebarItem::CategoryUncategorized => {
                let active = state.category_filter == Some(None);
                let count = state.torrents.iter().filter(|t| t.category.is_none()).count();
                let mark = if (active) { "● " } else { "  " };
                let line = Line::from(vec![
                    Span::styled(mark, Style::default().fg(Color::Cyan)),
                    Span::styled("(none)", Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::styled(format!("({})", count), Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            }
            SidebarItem::Category(name) => {
                let active = state.category_filter.as_ref().and_then(|c| c.as_deref()) == Some(name.as_str());
                let count = state.torrents.iter()
                    .filter(|t| t.category.as_deref() == Some(name.as_str()))
                    .count();
                let mark = if (active) { "● " } else { "  " };
                let line = Line::from(vec![
                    Span::styled(mark, Style::default().fg(Color::Cyan)),
                    Span::raw(name.clone()),
                    Span::raw("  "),
                    Span::styled(format!("({})", count), Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            }
            SidebarItem::TagHeader => {
                let line = Line::from(Span::styled(
                    "  TAGS",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                ));
                ListItem::new(line)
            }
            SidebarItem::TagAll => {
                let active = state.tag_filter.is_none();
                let mark = if (active) { "● " } else { "  " };
                let line = Line::from(vec![
                    Span::styled(mark, Style::default().fg(Color::Cyan)),
                    Span::raw("all"),
                ]);
                ListItem::new(line)
            }
            SidebarItem::Tag(tag) => {
                let active = state.tag_filter.as_deref() == Some(tag.as_str());
                let count = state.torrents.iter()
                    .filter(|t| t.tags.contains(tag.as_str()))
                    .count();
                let mark = if (active) { "● " } else { "  " };
                let line = Line::from(vec![
                    Span::styled(mark, Style::default().fg(Color::Cyan)),
                    Span::raw(tag.clone()),
                    Span::raw("  "),
                    Span::styled(format!("({})", count), Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            }
        }
    }).collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focus_border_style(state.focus == Pane::Sidebar))
        .title(" filters ");

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▌ ");

    frame.render_stateful_widget(list, area, &mut state.sidebar_state);

    // apply immediately on nav so the list reacts without pressing enter
    state.apply_sidebar_selection();
}

fn draw_torrent_list(frame: &mut ratatui::Frame, area: Rect, state: &mut AppState) {
    let visible = state.filtered_indices();
    let filter_label = match &state.category_filter {
        Some(None) => "(uncategorized)".to_string(),
        Some(Some(name)) => name.clone(),
        None => state.status_filter.label().to_string(),
    };
    let title = if (state.daemon_unreachable) {
        format!(" torrents — {} (daemon unreachable) ", filter_label)
    } else if (visible.is_empty()) {
        format!(" torrents — {} (none) ", filter_label)
    } else {
        format!(" torrents — {} ({}) ", filter_label, visible.len())
    };
    let border_style = focus_border_style(state.focus == Pane::List);

    // 1. outer rounded block (gives ╭╮╰╯ corners and side borders)
    let title_width = crate::display::display_width(&title) as u16;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if (inner.width < 4 || inner.height < 3) { return; }

    // 2. column widths + boundary x-positions (right edge of each non-last
    //    column = x where a │ divider sits)
    let widths = compute_column_widths(
        &state.visible_columns,
        &state.column_width_overrides,
        inner.width,
    );
    let mut boundaries: Vec<u16> = Vec::with_capacity(widths.len().saturating_sub(1));
    let mut cursor_x = inner.x;
    for (index, width) in widths.iter().enumerate() {
        cursor_x += width;
        if (index < widths.len() - 1) {
            boundaries.push(cursor_x);
            cursor_x += 1;
        }
    }
    state.column_boundaries = boundaries.clone();
    state.header_y = inner.y;

    // 3. overdraw ┬ on the top border at each boundary x. skip any x that
    //    falls inside the rendered title text, otherwise the divider chops
    //    a glyph out of the title (e.g. "torre┬ts").
    {
        // ratatui renders Block titles starting one cell in from the left
        // corner. account for that + the rendered width.
        let title_start = area.x + 1;
        let title_end = title_start + title_width;
        let buffer = frame.buffer_mut();
        for boundary_x in &boundaries {
            if (*boundary_x >= title_start && *boundary_x < title_end) { continue; }
            if let Some(cell) = buffer.cell_mut((*boundary_x, area.y)) {
                cell.set_char('┬').set_style(border_style);
            }
        }
    }

    // 4. header row at inner.y. each column label fills its width, with │
    //    separators between columns
    let header_y = inner.y;
    let mut header_x = inner.x;
    for (index, column) in state.visible_columns.iter().enumerate() {
        let width = widths[index];
        let rect = Rect { x: header_x, y: header_y, width, height: 1 };
        let label_text = crate::display::truncate_to_width(column.label(), width as usize);
        frame.render_widget(
            Paragraph::new(Span::styled(label_text, Style::default().add_modifier(Modifier::BOLD))),
            rect,
        );
        header_x += width;
        if (index < widths.len() - 1) {
            let buffer = frame.buffer_mut();
            if let Some(cell) = buffer.cell_mut((header_x, header_y)) {
                cell.set_char('│').set_style(border_style);
            }
            header_x += 1;
        }
    }

    // 5. horizontal divider under the header: ├─┼─┼─┤
    let divider_y = inner.y + 1;
    if (divider_y < area.y + area.height - 1) {
        let buffer = frame.buffer_mut();
        for column_x in inner.x..(inner.x + inner.width) {
            if let Some(cell) = buffer.cell_mut((column_x, divider_y)) {
                cell.set_char('─').set_style(border_style);
            }
        }
        // ┴ (not ┼) at each boundary: the vertical divider terminates here;
        // data rows below the divider have no column separators.
        for boundary_x in &boundaries {
            if let Some(cell) = buffer.cell_mut((*boundary_x, divider_y)) {
                cell.set_char('┴').set_style(border_style);
            }
        }
        if let Some(cell) = buffer.cell_mut((area.x, divider_y)) {
            cell.set_char('├').set_style(border_style);
        }
        if let Some(cell) = buffer.cell_mut((area.x + area.width - 1, divider_y)) {
            cell.set_char('┤').set_style(border_style);
        }
    }

    // 6. data rows: render via Table inside the remaining area below the
    //    divider. Table handles selection highlight + scroll, with
    //    column_spacing(1) so the gaps line up with the header │ positions.
    let data_y = inner.y + 2;
    if (data_y >= area.y + area.height - 1) { return; }
    let data_area = Rect {
        x: inner.x,
        y: data_y,
        width: inner.width,
        height: (area.y + area.height - 1).saturating_sub(data_y),
    };
    let rows: Vec<Row> = visible.iter().map(|index| {
        let torrent = &state.torrents[*index];
        let row_style = if (torrent.is_paused) {
            Style::default().fg(Color::DarkGray)
        } else if (torrent.is_seeding) {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        let cells: Vec<Cell> = state.visible_columns.iter()
            .map(|column| Cell::from(column.render(*index, torrent, state.nerd_font)))
            .collect();
        Row::new(cells).style(row_style)
    }).collect();
    let constraints: Vec<Constraint> = widths.iter().map(|w| Constraint::Length(*w)).collect();
    let table = Table::new(rows, constraints)
        .column_spacing(1)
        .row_highlight_style(selected_row_style());
    frame.render_stateful_widget(table, data_area, &mut state.table_state);
}

fn draw_detail(frame: &mut ratatui::Frame, area: Rect, state: &mut AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focus_border_style(state.focus == Pane::Detail))
        .title(" detail ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // check if the selected torrent has active rate limits to show an info line
    let rate_limit_line: Option<String> = state.selected_torrent_index()
        .and_then(|index| state.torrents.get(index))
        .and_then(|torrent| {
            let dl = torrent.download_limit;
            let ul = torrent.upload_limit;
            if (dl == -1 && ul == -1) { return None; }
            let mut parts = Vec::new();
            if (dl != -1) {
                let text = if (dl == 0) { "∞".to_string() } else { crate::display::format_rate(dl as i64) };
                parts.push(format!("↓ limit: {}", text));
            }
            if (ul != -1) {
                let text = if (ul == 0) { "∞".to_string() } else { crate::display::format_rate(ul as i64) };
                parts.push(format!("↑ limit: {}", text));
            }
            Some(parts.join("  "))
        });

    let (tab_bar_height, info_height) = if (rate_limit_line.is_some()) { (2, 1) } else { (2, 0) };
    let split = Layout::vertical([
        Constraint::Length(tab_bar_height),
        Constraint::Length(info_height),
        Constraint::Min(0),
    ]).split(inner);

    state.detail_tab_bar_rect = split[0];
    let tab_titles: Vec<Line> = DetailTab::ALL.iter()
        .map(|tab| Line::from(tab.label()))
        .collect();
    let selected = DetailTab::ALL.iter().position(|tab| *tab == state.detail_tab).unwrap_or(0);
    let tabs = Tabs::new(tab_titles)
        .select(selected)
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .divider("│");
    frame.render_widget(tabs, split[0]);

    if let Some(text) = rate_limit_line {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(text, Style::default().fg(Color::Yellow)))),
            split[1],
        );
    }

    let body = split[2];
    match state.detail_tab {
        DetailTab::Content => draw_content_tab(frame, body, state),
        DetailTab::Peers => draw_peers_tab(frame, body, state),
        DetailTab::Trackers => draw_trackers_tab(frame, body, state),
    }
}

/// one row in the rendered file tree: either a folder header (with collapse
/// state) or a leaf file row. file_index is None for folders.
#[derive(Clone)]
struct TreeRow {
    indent: usize,
    label: String,
    full_path: String,
    is_folder: bool,
    file_index: Option<usize>,
    total_size: i64,
    total_done: i64,
    /// for files: the libtorrent priority (0..=7).
    /// for folders: Some(p) when every descendant file shares priority p;
    /// None when the folder is empty *or* its descendants disagree
    /// (rendered as "mixed" — see `is_mixed`).
    priority: Option<u8>,
    /// true only for folder rows whose descendants have differing priorities.
    is_mixed: bool,
}

/// zero-alloc fuzzy match on pre-lowercased inputs: true if needle_lc is a
/// substring of haystack_lc OR all characters of needle_lc appear in
/// haystack_lc in order. substring check runs first as a fast path.
fn fuzzy_match_lc(haystack_lc: &str, needle_lc: &str) -> bool {
    if needle_lc.is_empty() { return true; }
    if haystack_lc.contains(needle_lc) { return true; }
    let mut needle_chars = needle_lc.chars();
    let mut current = needle_chars.next();
    for ch in haystack_lc.chars() {
        if Some(ch) == current {
            current = needle_chars.next();
            if current.is_none() { return true; }
        }
    }
    false
}

/// flat list of file rows for the precomputed match indices. O(k) where k is
/// the number of matches — no searching, no string lowercasing at draw time.
fn filter_content_rows(detail: &TorrentDetail, matches: &[usize]) -> Vec<TreeRow> {
    matches.iter().map(|&file_index| {
        let file = &detail.files[file_index];
        let total_done = (file.size as f64 * file.progress as f64) as i64;
        TreeRow {
            indent: 0,
            label: file.path.clone(),
            full_path: file.path.clone(),
            is_folder: false,
            file_index: Some(file_index),
            total_size: file.size,
            total_done,
            priority: Some(file.priority),
            is_mixed: false,
        }
    }).collect()
}

/// build a tree of files from their flat paths. folders aggregate size +
/// progress from their children so the listing reads at a glance.
fn build_tree_rows(detail: &TorrentDetail, collapsed: &std::collections::BTreeSet<String>) -> Vec<TreeRow> {
    // 1. group files by directory
    let mut by_folder: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
    let mut all_folders: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (file_index, file) in detail.files.iter().enumerate() {
        let folder = std::path::Path::new(&file.path)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_default();
        by_folder.entry(folder.clone()).or_default().push(file_index);
        // record every ancestor folder so we can render the tree
        let mut accumulated = String::new();
        for component in folder.split('/').filter(|component| !component.is_empty()) {
            if (!accumulated.is_empty()) { accumulated.push('/'); }
            accumulated.push_str(component);
            all_folders.insert(accumulated.clone());
        }
    }

    // 2. walk folders in lexical order, emitting folder + child file rows.
    // for v1 we render folders + their direct files; deeper nesting is folded
    // into the indent. paths like "a/b/c.bin" produce folder "a", folder "a/b",
    // then leaf "c.bin". this matches qBT's tree behaviour.
    let mut rows: Vec<TreeRow> = Vec::new();

    // emit folder rows in depth order (sorted strings give us that)
    let mut visited_folders: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut skip_prefix: Option<String> = None;
    for folder in all_folders.iter() {
        // skip files inside a collapsed ancestor
        if let Some(prefix) = &skip_prefix {
            if (folder.starts_with(prefix.as_str())) { continue; }
            skip_prefix = None;
        }
        if (!visited_folders.insert(folder.clone())) { continue; }
        let indent = folder.matches('/').count();
        let name = folder.rsplit('/').next().unwrap_or(folder).to_string();
        // folder size/progress = sum across descendants
        let (total_size, total_done) = detail.files.iter()
            .filter(|file| file.path == *folder
                || file.path.starts_with(&format!("{}/", folder)))
            .fold((0i64, 0i64), |(size_sum, done_sum), file| {
                let done = (file.size as f64 * file.progress as f64) as i64;
                (size_sum + file.size, done_sum + done)
            });
        let is_collapsed = collapsed.contains(folder);
        let prefix = if (is_collapsed) { "▸ " } else { "▾ " };
        // aggregate descendant priorities. all-same → Some(p); any disagreement → mixed.
        let (folder_priority, is_mixed) = {
            let mut iterator = detail.files.iter()
                .filter(|file| file.path == *folder
                    || file.path.starts_with(&format!("{}/", folder)))
                .map(|file| file.priority);
            match iterator.next() {
                None => (None, false),
                Some(first) => {
                    let all_same = iterator.all(|priority| priority == first);
                    if (all_same) { (Some(first), false) } else { (None, true) }
                }
            }
        };
        rows.push(TreeRow {
            indent,
            label: format!("{}{}", prefix, name),
            full_path: folder.clone(),
            is_folder: true,
            file_index: None,
            total_size,
            total_done,
            priority: folder_priority,
            is_mixed,
        });
        if (is_collapsed) {
            skip_prefix = Some(format!("{}/", folder));
        }
    }

    // 3. emit file leaves; skip any file inside a collapsed folder
    for (file_index, file) in detail.files.iter().enumerate() {
        let folder = std::path::Path::new(&file.path)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_default();
        // check if any ancestor is collapsed
        let mut hidden = false;
        let mut accumulated = String::new();
        for component in folder.split('/').filter(|component| !component.is_empty()) {
            if (!accumulated.is_empty()) { accumulated.push('/'); }
            accumulated.push_str(component);
            if (collapsed.contains(&accumulated)) { hidden = true; break; }
        }
        if (hidden) { continue; }
        let indent = if (folder.is_empty()) { 0 } else { folder.matches('/').count() + 1 };
        let name = std::path::Path::new(&file.path)
            .file_name()
            .map(|os| os.to_string_lossy().to_string())
            .unwrap_or_else(|| file.path.clone());
        let total_done = (file.size as f64 * file.progress as f64) as i64;
        rows.push(TreeRow {
            indent,
            label: name,
            full_path: file.path.clone(),
            is_folder: false,
            file_index: Some(file_index),
            total_size: file.size,
            total_done,
            priority: Some(file.priority),
            is_mixed: false,
        });
    }

    // sort by path so folders and their files appear together. fall back to
    // file_index for stable order within the same path.
    rows.sort_by(|left, right| {
        left.full_path.cmp(&right.full_path).then_with(|| left.is_folder.cmp(&right.is_folder).reverse())
    });

    rows
}

fn draw_content_tab(frame: &mut ratatui::Frame, area: Rect, state: &mut AppState) {
    let Some(detail) = &state.detail else {
        frame.render_widget(Paragraph::new("no torrent selected").style(Style::default().fg(Color::DarkGray)), area);
        return;
    };
    if (detail.files.is_empty()) {
        frame.render_widget(
            Paragraph::new("no files (metadata not yet downloaded?)").style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let tree_rows = if state.content_filter.is_empty() {
        build_tree_rows(detail, &state.collapsed_folders)
    } else {
        filter_content_rows(detail, &state.content_filter_matches)
    };

    let header = Row::new([
        Cell::from("name").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("size").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("progress").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("priority").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("remaining").style(Style::default().add_modifier(Modifier::BOLD)),
    ]);

    let rows: Vec<Row> = tree_rows.iter().map(|tree_row| {
        let indent = "  ".repeat(tree_row.indent);
        let remaining = (tree_row.total_size - tree_row.total_done).max(0);
        let progress = if (tree_row.total_size > 0) {
            tree_row.total_done as f64 / tree_row.total_size as f64 * 100.0
        } else { 0.0 };
        let priority_label = if (tree_row.is_mixed) {
            "mixed".to_string()
        } else {
            match tree_row.priority {
                None => "—".to_string(),
                Some(0) => "skip".to_string(),
                Some(1..=3) => format!("low/{}", tree_row.priority.unwrap()),
                Some(4) => "normal".to_string(),
                Some(5..=6) => format!("high/{}", tree_row.priority.unwrap()),
                Some(7) => "max".to_string(),
                Some(other) => other.to_string(),
            }
        };
        let row_style = if (tree_row.is_folder) {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else if (tree_row.priority == Some(0)) {
            Style::default().fg(Color::DarkGray)
        } else if (progress >= 100.0) {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        Row::new(vec![
            Cell::from(format!("{}{}", indent, tree_row.label)),
            Cell::from(crate::display::format_bytes(tree_row.total_size)),
            Cell::from(format!("{:>5.1}%", progress)),
            Cell::from(priority_label),
            Cell::from(crate::display::format_bytes(remaining)),
        ])
        .style(row_style)
    }).collect();

    let widths = [
        Constraint::Min(20),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Length(12),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(selected_row_style())
        .highlight_symbol("▌ ");

    frame.render_stateful_widget(table, area, &mut state.detail_files_state);
}

fn draw_peers_tab(frame: &mut ratatui::Frame, area: Rect, state: &mut AppState) {
    let Some(detail) = &state.detail else {
        frame.render_widget(Paragraph::new("no torrent selected").style(Style::default().fg(Color::DarkGray)), area);
        return;
    };
    if (detail.peers.is_empty()) {
        frame.render_widget(
            Paragraph::new("no connected peers").style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let header = Row::new([
        Cell::from("address").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("down").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("up").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("client").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("progress").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("flags").style(Style::default().add_modifier(Modifier::BOLD)),
    ]);

    let rows: Vec<Row> = detail.peers.iter().map(|peer: &IpcPeerInfo| {
        Row::new(vec![
            Cell::from(format!("{}:{}", peer.ip, peer.port)),
            Cell::from(crate::display::format_rate(peer.download_rate)),
            Cell::from(crate::display::format_rate(peer.upload_rate)),
            Cell::from(peer.client.clone()),
            Cell::from(format!("{:.1}%", peer.progress * 100.0)),
            Cell::from(peer.flags.clone()),
        ])
    }).collect();

    let widths = [
        Constraint::Length(24),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Min(16),
        Constraint::Length(9),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(selected_row_style())
        .highlight_symbol("▌ ");

    frame.render_stateful_widget(table, area, &mut state.detail_peers_state);
}

fn draw_trackers_tab(frame: &mut ratatui::Frame, area: Rect, state: &mut AppState) {
    let Some(detail) = &state.detail else {
        frame.render_widget(Paragraph::new("no torrent selected").style(Style::default().fg(Color::DarkGray)), area);
        return;
    };
    if (detail.trackers.is_empty()) {
        frame.render_widget(
            Paragraph::new("no trackers (dht/lsd only?)").style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }
    let header = Row::new([
        Cell::from("tier").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("url").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("fails").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("state").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("last error").style(Style::default().add_modifier(Modifier::BOLD)),
    ]);
    let rows: Vec<Row> = detail.trackers.iter().map(|tracker: &TrackerInfo| {
        let state_label = if (tracker.updating) {
            "updating"
        } else if (tracker.fails > 0) {
            "failing"
        } else {
            "ok"
        };
        let row_style = if (tracker.fails > 0) {
            Style::default().fg(Color::Red)
        } else if (tracker.updating) {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Green)
        };
        Row::new(vec![
            Cell::from(tracker.tier.to_string()),
            Cell::from(tracker.url.clone()),
            Cell::from(tracker.fails.to_string()),
            Cell::from(state_label),
            Cell::from(tracker.message.clone()),
        ])
        .style(row_style)
    }).collect();
    let widths = [
        Constraint::Length(5),
        Constraint::Min(30),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Min(20),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(selected_row_style())
        .highlight_symbol("▌ ");
    frame.render_stateful_widget(table, area, &mut state.detail_trackers_state);
}

fn draw_input_bar(frame: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let Some(input) = &state.active_input else { return; };
    let prefix = match input.purpose {
        InputPurpose::ListFilter => "/",
        InputPurpose::ContentFilter => "files",
    };
    let line = Line::from(vec![
        Span::styled(format!(" {} ", prefix), Style::default().fg(Color::Black).bg(Color::Yellow)),
        Span::raw(" "),
        Span::raw(input.buffer.as_str()),
        Span::styled("█", Style::default().fg(Color::Yellow)),
        Span::raw("   "),
        Span::styled("esc cancel / enter close", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_status_bar(frame: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let mut spans: Vec<Span> = Vec::new();
    if let Some(stats) = &state.stats {
        spans.push(Span::raw(format!(
            " {} torrents  {} active  {} paused  ↓ {}  ↑ {}  dht {}  peers {} ",
            stats.num_torrents,
            stats.active_torrents,
            stats.paused_torrents,
            crate::display::format_rate(stats.download_rate),
            crate::display::format_rate(stats.upload_rate),
            stats.total_dht_nodes,
            stats.num_peers,
        )));
    } else if let Some(error) = &state.error {
        spans.push(Span::styled(format!(" {} ", error), Style::default().fg(Color::Red)));
    }
    let bar = Paragraph::new(Line::from(spans))
        .style(Style::default().bg(Color::Black).fg(Color::White));
    frame.render_widget(bar, area);
}

fn draw_hint_bar(frame: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let in_trackers = state.focus == Pane::Detail && state.detail_tab == DetailTab::Trackers;
    let mut spans = vec![
        Span::styled(" w/s ", Style::default().fg(Color::Yellow)),
        Span::raw("move  "),
        Span::styled("tab ", Style::default().fg(Color::Yellow)),
        Span::raw("pane  "),
        Span::styled("[/] ", Style::default().fg(Color::Yellow)),
        Span::raw("tabs  "),
        Span::styled("n ", Style::default().fg(Color::Yellow)),
        Span::raw("add  "),
        Span::styled("x ", Style::default().fg(Color::Yellow)),
        Span::raw("delete  "),
        Span::styled("p ", Style::default().fg(Color::Yellow)),
        Span::raw("pause  "),
        Span::styled("r ", Style::default().fg(Color::Yellow)),
        Span::raw("rename  "),
        Span::styled("m ", Style::default().fg(Color::Yellow)),
        Span::raw("move  "),
        Span::styled("R ", Style::default().fg(Color::Yellow)),
        Span::raw("recheck  "),
        Span::styled("T ", Style::default().fg(Color::Yellow)),
        Span::raw("reann  "),
        Span::styled("L ", Style::default().fg(Color::Yellow)),
        Span::raw("rate limit  "),
        Span::styled("q/e ", Style::default().fg(Color::Yellow)),
        Span::raw("sidebar/detail  "),
        Span::styled("0-4 ", Style::default().fg(Color::Yellow)),
        Span::raw("file prio  "),
        Span::styled("/ ", Style::default().fg(Color::Yellow)),
        Span::raw("filter  "),
        Span::styled(", ", Style::default().fg(Color::Yellow)),
        Span::raw("settings  "),
        Span::styled("^c ", Style::default().fg(Color::Yellow)),
        Span::raw("quit  "),
        Span::styled("? ", Style::default().fg(Color::Yellow)),
        Span::raw("help"),
    ];
    if (in_trackers) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled("a ", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw("add  "));
        spans.push(Span::styled("d ", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw("del"));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .alignment(Alignment::Left)
            .style(Style::default().fg(Color::Gray)),
        area,
    );
}

// ─── settings overlay ──────────────────────────────────────────────────────

fn handle_settings_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    let Mode::Settings(settings) = &mut state.mode else { return false; };

    // interface dropdown — captures all input until dismissed
    if (settings.interface_picker.is_some()) {
        return handle_interface_picker_key(code, modifiers, settings);
    }

    // watch-dir inline editor — captures all input until committed/cancelled
    if (settings.watch_dir_editing) {
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Esc, _) => {
                settings.watch_dir_editing = false;
                settings.watch_dir_buffer.clear();
            }
            (KeyCode::Enter, _) => {
                let value = settings.watch_dir_buffer.trim().to_string();
                settings.watch_dir_editing = false;
                settings.watch_dir_buffer.clear();
                if (!value.is_empty()) {
                    let index = settings.watch_dir_selected;
                    if (index < settings.watch_dir_list.len()) {
                        settings.watch_dir_list[index] = value;
                    } else {
                        settings.watch_dir_list.push(value);
                        settings.watch_dir_selected = settings.watch_dir_list.len() - 1;
                    }
                    submit_watch_dirs(settings);
                }
            }
            (KeyCode::Backspace, _) => { settings.watch_dir_buffer.pop(); }
            (KeyCode::Char(character), modifiers)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                settings.watch_dir_buffer.push(character);
            }
            _ => {}
        }
        return false;
    }

    // watch-dir navigation — when the list field is focused, w/s/a move within the list
    if (settings.current_field().is_list) {
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Esc, _)
            | (KeyCode::Char('q'), KeyModifiers::NONE)
            | (KeyCode::Char(','), KeyModifiers::NONE) => {
                state.mode = Mode::Main;
                state.last_poll = Instant::now() - POLL_INTERVAL;
            }
            // move selection within the list
            (KeyCode::Char('w'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
                if (!settings.watch_dir_list.is_empty()) {
                    settings.watch_dir_selected = settings.watch_dir_selected.saturating_sub(1);
                }
            }
            (KeyCode::Char('s'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
                if (!settings.watch_dir_list.is_empty()) {
                    settings.watch_dir_selected = (settings.watch_dir_selected + 1)
                        .min(settings.watch_dir_list.len().saturating_sub(1));
                }
            }
            // tab / shift+tab still cycle settings tabs
            (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::BackTab, _) => settings.switch_tab(-1),
            (KeyCode::Tab, _) => settings.switch_tab(1),
            // left/right cycle tabs; when on a list field a/d are reserved for add/del
            (KeyCode::Left, _) => settings.switch_tab(-1),
            (KeyCode::Right, _) => settings.switch_tab(1),
            // add a new entry
            (KeyCode::Char('a'), KeyModifiers::NONE) => {
                settings.watch_dir_selected = settings.watch_dir_list.len();
                settings.watch_dir_editing = true;
                settings.watch_dir_buffer.clear();
            }
            // delete selected entry
            (KeyCode::Char('d'), KeyModifiers::NONE) | (KeyCode::Delete, _) => {
                let index = settings.watch_dir_selected;
                if (index < settings.watch_dir_list.len()) {
                    settings.watch_dir_list.remove(index);
                    settings.watch_dir_selected = index.min(
                        settings.watch_dir_list.len().saturating_sub(1)
                    );
                    submit_watch_dirs(settings);
                }
            }
            // edit selected entry
            (KeyCode::Enter, _) | (KeyCode::Char('i'), KeyModifiers::NONE) => {
                let index = settings.watch_dir_selected;
                let initial = settings.watch_dir_list.get(index).cloned().unwrap_or_default();
                settings.watch_dir_buffer = initial;
                settings.watch_dir_editing = true;
            }
            // move field selection up to leave the list
            (KeyCode::PageUp, _) => settings.move_selection(-5),
            (KeyCode::PageDown, _) => settings.move_selection(5),
            (KeyCode::Home, _) => {
                if let Some(first) = settings.current_tab_indices().first().copied() {
                    settings.selected = first;
                }
            }
            (KeyCode::End, _) => {
                if let Some(last) = settings.current_tab_indices().last().copied() {
                    settings.selected = last;
                }
            }
            // number keys jump direct to tab
            (KeyCode::Char(character), KeyModifiers::NONE) if character.is_ascii_digit() => {
                let target = character.to_digit(10).unwrap_or(0) as usize;
                if (target >= 1 && target <= section_tabs().len()) {
                    settings.current_tab = target - 1;
                    if let Some(first) = settings.current_tab_indices().first().copied() {
                        settings.selected = first;
                    }
                    settings.scroll = 0;
                }
            }
            _ => {}
        }
        return false;
    }

    // active text editor — capture printable input, commit on enter, cancel on esc
    if (settings.edit_buffer.is_some()) {
        match (code, modifiers) {
            (KeyCode::Esc, _) => settings.edit_buffer = None,
            (KeyCode::Enter, _) => {
                let buffer = settings.edit_buffer.take().unwrap_or_default();
                commit_edit(settings, &buffer);
            }
            (KeyCode::Backspace, _) => {
                if let Some(buffer) = settings.edit_buffer.as_mut() { buffer.pop(); }
            }
            (KeyCode::Char(character), modifiers)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(buffer) = settings.edit_buffer.as_mut() { buffer.push(character); }
            }
            _ => {}
        }
        return false;
    }

    // navigation mode
    match (code, modifiers) {
        // ctrl+c still hard-exits the entire app, matching shell convention
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        // esc / q / , close the overlay and return to the main view
        (KeyCode::Esc, _)
        | (KeyCode::Char('q'), KeyModifiers::NONE)
        | (KeyCode::Char(','), KeyModifiers::NONE) => {
            state.mode = Mode::Main;
            // force a fresh poll so the main view reflects any changes
            state.last_poll = Instant::now() - POLL_INTERVAL;
        }
        (KeyCode::Char('s'), KeyModifiers::NONE) | (KeyCode::Down, _) => settings.move_selection(1),
        (KeyCode::Char('w'), KeyModifiers::NONE) | (KeyCode::Up, _) => settings.move_selection(-1),
        (KeyCode::PageDown, _) => settings.move_selection(5),
        (KeyCode::PageUp, _) => settings.move_selection(-5),
        (KeyCode::Home, _) => {
            if let Some(first) = settings.current_tab_indices().first().copied() {
                settings.selected = first;
            }
        }
        (KeyCode::End, _) => {
            if let Some(last) = settings.current_tab_indices().last().copied() {
                settings.selected = last;
            }
        }
        // tab cycles tabs forward; shift+tab cycles backward
        (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::BackTab, _) => settings.switch_tab(-1),
        (KeyCode::Tab, _) => settings.switch_tab(1),
        // a / d  or  ←/→ also cycle tabs (consistent with wasd-left/right)
        (KeyCode::Char('a'), KeyModifiers::NONE) | (KeyCode::Left, _) => settings.switch_tab(-1),
        (KeyCode::Char('d'), KeyModifiers::NONE) | (KeyCode::Right, _) => settings.switch_tab(1),
        // number keys jump direct
        (KeyCode::Char(character), KeyModifiers::NONE) if character.is_ascii_digit() => {
            let target = character.to_digit(10).unwrap_or(0) as usize;
            if (target >= 1 && target <= section_tabs().len()) {
                settings.current_tab = target - 1;
                if let Some(first) = settings.current_tab_indices().first().copied() {
                    settings.selected = first;
                }
                settings.scroll = 0;
            }
        }
        (KeyCode::Enter, _) => activate_field(settings),
        _ => {}
    }
    false
}

/// join watch_dir_list with newlines and submit to daemon
fn submit_watch_dirs(settings: &mut SettingsState) {
    let joined = settings.watch_dir_list.join("\n");
    match submit_set("watch_directories", &joined) {
        Ok(_) => {
            settings.status = Some("saved watch_directories".to_string());
            settings.refresh_config();
        }
        Err(error) => settings.status = Some(format!("error: {}", error)),
    }
}

fn handle_interface_picker_key(code: KeyCode, _modifiers: KeyModifiers, settings: &mut SettingsState) -> bool {
    let Some(picker) = settings.interface_picker.as_mut() else { return false; };
    match code {
        KeyCode::Char('c') if _modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Esc | KeyCode::Char('q') => settings.interface_picker = None,
        KeyCode::Char('s') | KeyCode::Down => {
            picker.selected = (picker.selected + 1).min(picker.items.len().saturating_sub(1));
        }
        KeyCode::Char('w') | KeyCode::Up => {
            picker.selected = picker.selected.saturating_sub(1);
        }
        KeyCode::Home => picker.selected = 0,
        KeyCode::End => picker.selected = picker.items.len().saturating_sub(1),
        KeyCode::Enter => {
            let Some((_, value)) = picker.items.get(picker.selected).cloned() else { return false; };
            settings.interface_picker = None;
            let key = settings.current_field().key;
            if (value == "__specific__") {
                // drop into the text editor seeded with the current raw value
                settings.edit_buffer = Some(config_value_string(&settings.config, key));
            } else {
                commit_value(settings, key, &value);
            }
        }
        _ => {}
    }
    false
}

fn activate_field(settings: &mut SettingsState) {
    let field = settings.current_field();
    let current = config_value_string(&settings.config, field.key);
    match field.kind {
        FieldKind::Bool => {
            let toggled = if (current == "true") { "false" } else { "true" };
            commit_value(settings, field.key, toggled);
        }
        FieldKind::Choice(options) => {
            let position = options.iter().position(|option| *option == current).unwrap_or(0);
            let next = options[(position + 1) % options.len()];
            commit_value(settings, field.key, next);
        }
        FieldKind::Integer
        | FieldKind::IntegerUnlimited
        | FieldKind::Float
        | FieldKind::Text => {
            settings.edit_buffer = Some(current);
        }
        FieldKind::Interface => {
            let mut picker = InterfacePickerState::build();
            // preselect the entry whose persisted value matches the current
            // config (so the cursor lands on what's already active).
            if let Some(index) = picker.items.iter().position(|(_, value)| *value == current) {
                picker.selected = index;
            }
            settings.interface_picker = Some(picker);
        }
    }
}

fn commit_edit(settings: &mut SettingsState, buffer: &str) {
    let field = settings.current_field();
    commit_value(settings, field.key, buffer);
}

fn commit_value(settings: &mut SettingsState, key: &str, value: &str) {
    if key == "autostart" {
        let result = if value == "true" { crate::autostart::enable() } else { crate::autostart::disable() };
        settings.status = Some(match result {
            Ok(_) => format!("autostart {}", if value == "true" { "enabled" } else { "disabled" }),
            Err(error) => format!("error: {}", error),
        });
        return;
    }
    match submit_set(key, value) {
        Ok(_) => {
            settings.status = Some(format!("saved {} = {}", key, value));
            settings.refresh_config();
        }
        Err(error) => settings.status = Some(format!("error: {}", error)),
    }
}

fn draw_settings(frame: &mut ratatui::Frame, state: &mut AppState) {
    let Mode::Settings(settings) = &mut state.mode else { return; };
    let area = frame.area();

    let outer = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(2), // tab bar
        Constraint::Min(0),    // body
        Constraint::Length(2), // description + status
        Constraint::Length(1), // hint
    ])
    .split(area);

    let title = Line::from(vec![
        Span::styled(
            " settings ",
            Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray),
        ),
        Span::raw("  "),
        Span::styled("esc to return", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(title), outer[0]);

    draw_settings_tab_bar(frame, outer[1], settings);
    draw_settings_body(frame, outer[2], settings);
    draw_settings_footer(frame, outer[3], settings);
    draw_settings_hint(frame, outer[4], settings);

    if (settings.interface_picker.is_some()) {
        draw_interface_picker(frame, settings);
    }
}

fn draw_interface_picker(frame: &mut ratatui::Frame, settings: &SettingsState) {
    let Some(picker) = settings.interface_picker.as_ref() else { return; };
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let height = ((picker.items.len() as u16) + 4).min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect { x, y, width, height };

    frame.render_widget(ratatui::widgets::Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" listen interface ");
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ]).split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " w/s move  enter pick  esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        layout[0],
    );

    let lines: Vec<Line> = picker.items.iter().enumerate().map(|(index, (label, _))| {
        let style = if (index == picker.selected) {
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        Line::from(Span::styled(format!(" {} ", label), style))
    }).collect();
    frame.render_widget(Paragraph::new(lines), layout[1]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " 'specific ip' lets you type a raw address",
            Style::default().fg(Color::DarkGray),
        ))),
        layout[2],
    );
}

/// Netflix-row style tab bar: previous tab dimmed on the left, current
/// centered + highlighted, next dimmed on the right. wraps around so the
/// last tab's "next" is the first tab.
fn draw_settings_tab_bar(frame: &mut ratatui::Frame, area: Rect, settings: &SettingsState) {
    let tabs = section_tabs();
    if (tabs.is_empty()) { return; }
    let count = tabs.len();
    let current_index = settings.current_tab.min(count - 1);
    let previous_index = (current_index + count - 1) % count;
    let next_index = (current_index + 1) % count;

    let previous = tabs[previous_index];
    let current = tabs[current_index];
    let next = tabs[next_index];

    let position_marker = format!("[{}/{}]", current_index + 1, count);

    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled("‹ ", Style::default().fg(Color::DarkGray)),
        Span::styled(previous, Style::default().fg(Color::DarkGray)),
        Span::raw("   "),
        Span::styled(
            format!(" {} ", current),
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Black)
                .bg(Color::Cyan),
        ),
        Span::raw("   "),
        Span::styled(next, Style::default().fg(Color::DarkGray)),
        Span::styled(" ›", Style::default().fg(Color::DarkGray)),
        Span::raw("   "),
        Span::styled(position_marker, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(
        Paragraph::new(line).alignment(Alignment::Center),
        area,
    );
}

fn draw_settings_body(frame: &mut ratatui::Frame, area: Rect, settings: &mut SettingsState) {
    let indices = settings.current_tab_indices();
    let mut lines: Vec<Line> = Vec::new();
    let mut field_to_row: std::collections::HashMap<usize, u16> = std::collections::HashMap::new();

    for index in &indices {
        let field = &SETTING_FIELDS[*index];
        let is_selected = *index == settings.selected;
        let marker = if (is_selected) { "▌ " } else { "  " };
        let label_style = if (is_selected) {
            Style::default().add_modifier(Modifier::BOLD).fg(Color::White)
        } else {
            Style::default()
        };

        if (field.is_list) {
            // header row: label + entry count
            let count_text = match settings.watch_dir_list.len() {
                0 => "(none)".to_string(),
                n => format!("{} entr{}", n, if n == 1 { "y" } else { "ies" }),
            };
            let count_style = if (is_selected) {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::styled(format!("{:32}", field.label), label_style),
                Span::raw("  "),
                Span::styled(count_text, count_style),
            ]));
            field_to_row.insert(*index, (lines.len() - 1) as u16);

            // one row per entry, indented under the label
            if (is_selected) {
                for (entry_index, entry) in settings.watch_dir_list.iter().enumerate() {
                    let row_is_selected = entry_index == settings.watch_dir_selected;
                    let (entry_text, entry_style) = if (settings.watch_dir_editing && row_is_selected) {
                        (
                            format!("[ {}_ ]", settings.watch_dir_buffer),
                            Style::default().fg(Color::Black).bg(Color::Yellow),
                        )
                    } else if (row_is_selected) {
                        (
                            format!("▸ {}", entry),
                            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                        )
                    } else {
                        (
                            format!("  {}", entry),
                            Style::default().fg(Color::Gray),
                        )
                    };
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled(entry_text, entry_style),
                    ]));
                }
                // show a blank editing row when appending a new entry
                if (settings.watch_dir_editing && settings.watch_dir_selected >= settings.watch_dir_list.len()) {
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled(
                            format!("[ {}_ ]", settings.watch_dir_buffer),
                            Style::default().fg(Color::Black).bg(Color::Yellow),
                        ),
                    ]));
                }
            }
        } else {
            let value = config_value_string(&settings.config, field.key);
            let display_value = render_value(settings, *index, &value);
            let value_style = if (settings.edit_buffer.is_some() && is_selected) {
                Style::default().fg(Color::Black).bg(Color::Yellow)
            } else if (is_selected) {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Gray)
            };
            let mut spans = vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::styled(format!("{:32}", field.label), label_style),
                Span::raw("  "),
                Span::styled(display_value, value_style),
            ];
            if (field.restart_required) {
                spans.push(Span::styled(
                    "  ⟳ restart",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC),
                ));
            }
            lines.push(Line::from(spans));
            field_to_row.insert(*index, (lines.len() - 1) as u16);
        }
    }

    let viewport_height = area.height.saturating_sub(2);
    if let Some(selected_row) = field_to_row.get(&settings.selected).copied() {
        if (selected_row < settings.scroll) {
            settings.scroll = selected_row;
        } else if (viewport_height > 0 && selected_row >= settings.scroll + viewport_height) {
            settings.scroll = selected_row + 1 - viewport_height;
        }
        let max_scroll = (lines.len() as u16).saturating_sub(viewport_height);
        if (settings.scroll > max_scroll) { settings.scroll = max_scroll; }
    } else {
        settings.scroll = 0;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan));
    let paragraph = Paragraph::new(lines).block(block).scroll((settings.scroll, 0));
    frame.render_widget(paragraph, area);
}

fn render_value(settings: &SettingsState, index: usize, value: &str) -> String {
    if (settings.edit_buffer.is_some() && index == settings.selected) {
        let buffer = settings.edit_buffer.as_deref().unwrap_or("");
        return format!("[ {}_ ]", buffer);
    }
    let field = &SETTING_FIELDS[index];
    match field.kind {
        FieldKind::Choice(options) => {
            let alternatives: Vec<String> = options.iter()
                .map(|option| if (*option == value) { format!("[{}]", option) } else { option.to_string() })
                .collect();
            alternatives.join(" ")
        }
        FieldKind::Bool => {
            if (value == "true") { "● on".to_string() } else { "○ off".to_string() }
        }
        FieldKind::IntegerUnlimited => {
            match value.parse::<i32>() {
                Ok(-1) => "∞  unlimited".to_string(),
                Ok(0) => "0  (none allowed)".to_string(),
                Ok(other) => other.to_string(),
                Err(_) => value.to_string(),
            }
        }
        FieldKind::Interface => {
            if (value.trim().is_empty()) {
                return "any (all interfaces)".to_string();
            }
            // if the stored value matches an interface name, show "name (ip)";
            // if it parses as an ip directly, show "ip  [specific ip]"; else as-is.
            let interfaces = crate::sources::enumerate_interfaces();
            if let Some((name, ip)) = interfaces.iter().find(|(name, _)| name == value) {
                return format!("{}  ({})", name, ip);
            }
            if (value.parse::<std::net::IpAddr>().is_ok()) {
                return format!("{}  [specific ip]", value);
            }
            value.to_string()
        }
        _ => value.to_string(),
    }
}

fn draw_settings_footer(frame: &mut ratatui::Frame, area: Rect, settings: &SettingsState) {
    let field = settings.current_field();
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(field.description, Style::default().fg(Color::DarkGray)),
        ]),
    ];
    if let Some(status) = &settings.status {
        let style = if (status.starts_with("error")) {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        lines.push(Line::from(Span::styled(format!(" {}", status), style)));
    } else {
        lines.push(Line::from(""));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_settings_hint(frame: &mut ratatui::Frame, area: Rect, settings: &SettingsState) {
    let hint = if (settings.watch_dir_editing) {
        Line::from(vec![
            Span::styled(" type ", Style::default().fg(Color::Yellow)),
            Span::raw("edit path  "),
            Span::styled("enter ", Style::default().fg(Color::Yellow)),
            Span::raw("save  "),
            Span::styled("esc ", Style::default().fg(Color::Yellow)),
            Span::raw("cancel"),
        ])
    } else if (settings.current_field().is_list) {
        Line::from(vec![
            Span::styled(" a ", Style::default().fg(Color::Yellow)),
            Span::raw("add  "),
            Span::styled("d ", Style::default().fg(Color::Yellow)),
            Span::raw("del  "),
            Span::styled("enter/i ", Style::default().fg(Color::Yellow)),
            Span::raw("edit  "),
            Span::styled("w/s ", Style::default().fg(Color::Yellow)),
            Span::raw("move entry  "),
            Span::styled("tab ", Style::default().fg(Color::Yellow)),
            Span::raw("switch tab  "),
            Span::styled("esc ", Style::default().fg(Color::Yellow)),
            Span::raw("return"),
        ])
    } else if (settings.edit_buffer.is_some()) {
        Line::from(vec![
            Span::styled(" type ", Style::default().fg(Color::Yellow)),
            Span::raw("edit value  "),
            Span::styled("enter ", Style::default().fg(Color::Yellow)),
            Span::raw("save  "),
            Span::styled("esc ", Style::default().fg(Color::Yellow)),
            Span::raw("cancel edit"),
        ])
    } else {
        Line::from(vec![
            Span::styled(" w/s ", Style::default().fg(Color::Yellow)),
            Span::raw("move  "),
            Span::styled("a/d / tab ", Style::default().fg(Color::Yellow)),
            Span::raw("switch tab  "),
            Span::styled("1..9 ", Style::default().fg(Color::Yellow)),
            Span::raw("jump  "),
            Span::styled("enter ", Style::default().fg(Color::Yellow)),
            Span::raw("edit/toggle  "),
            Span::styled("esc ", Style::default().fg(Color::Yellow)),
            Span::raw("return  "),
            Span::styled("^c ", Style::default().fg(Color::Yellow)),
            Span::raw("quit"),
        ])
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::Gray)),
        area,
    );
}

/// state label. natural-english words, no abbreviations. when nerd_font
/// is true the result is a single glyph so wide-state-name columns can
/// stay narrow; users without a nerd-font-aware terminal get the full
/// word instead.
fn format_state_with(state: &str, is_paused: bool, nerd_font: bool) -> String {
    if (is_paused) {
        return if (nerd_font) { "\u{f04c}".to_string() } else { "paused".to_string() };
    }
    if (nerd_font) {
        return match state {
            "downloading" => "\u{f019}".to_string(),
            "seeding" => "\u{f093}".to_string(),
            "finished" => "\u{f00c}".to_string(),
            "downloading_metadata" => "\u{f1ce}".to_string(),
            "checking_files" | "checking_resume_data" => "\u{f021}".to_string(),
            "allocating" => "\u{f0c7}".to_string(),
            other => other.to_string(),
        };
    }
    // libtorrent uses underscore_case; render as the human-readable form
    match state {
        "downloading" => "downloading".to_string(),
        "seeding" => "seeding".to_string(),
        "finished" => "finished".to_string(),
        "downloading_metadata" => "downloading metadata".to_string(),
        "checking_files" => "checking files".to_string(),
        "checking_resume_data" => "checking resume".to_string(),
        "allocating" => "allocating".to_string(),
        other => other.replace('_', " "),
    }
}
