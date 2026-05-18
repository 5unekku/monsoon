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
    FileInfo, PeerInfo as IpcPeerInfo, Request, Response, StatsInfo, TorrentDetail, TorrentInfo,
    TrackerInfo,
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
            Column::Index => "#",
            Column::Name => "name",
            Column::State => "st",
            Column::Progress => "prog",
            Column::Down => "down",
            Column::Up => "up",
            Column::Peers => "peers",
            Column::Seeds => "seeds",
            Column::Size => "size",
            Column::Downloaded => "downloaded",
            Column::Uploaded => "uploaded",
            Column::AddedOn => "added on",
            Column::CompletedOn => "completed",
            Column::SavePath => "save path",
            Column::Category => "cat",
            Column::Tags => "tags",
            Column::InfoHash => "info hash",
        }
    }

    fn width(&self) -> Constraint {
        match self {
            Column::Index => Constraint::Length(4),
            Column::Name => Constraint::Min(20),
            Column::State => Constraint::Length(4),
            Column::Progress => Constraint::Length(7),
            Column::Down | Column::Up => Constraint::Length(12),
            Column::Peers => Constraint::Length(8),
            Column::Seeds => Constraint::Length(7),
            Column::Size | Column::Downloaded | Column::Uploaded => Constraint::Length(10),
            Column::AddedOn | Column::CompletedOn => Constraint::Length(19),
            Column::SavePath => Constraint::Min(20),
            Column::Category => Constraint::Length(12),
            Column::Tags => Constraint::Min(10),
            Column::InfoHash => Constraint::Length(40),
        }
    }

    fn render(&self, index: usize, torrent: &TorrentInfo, nerd_font: bool) -> String {
        match self {
            Column::Index => format!("{:>3}", index),
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
    Float,
    Text,
    /// dropdown of fixed string options; enter cycles to the next one
    Choice(&'static [&'static str]),
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
    },
    SettingField {
        section: "security & anonymity",
        key: "encryption_mode",
        label: "encryption mode",
        description: "protocol encryption between peers. 'forced' refuses plaintext peers entirely (recommended). independent of anonymous mode — does not affect tracker traffic or fingerprinting.",
        kind: FieldKind::Choice(&["enabled", "forced", "disabled"]),
        restart_required: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "ssrf_mitigation",
        label: "ssrf mitigation",
        description: "reject tracker responses that redirect to private/local addresses.",
        kind: FieldKind::Bool,
        restart_required: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "validate_https_tracker_certificate",
        label: "validate https tracker cert",
        description: "verify TLS certificates for HTTPS trackers.",
        kind: FieldKind::Bool,
        restart_required: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "announce_to_all_trackers",
        label: "announce to all trackers",
        description: "announce to every tracker rather than stopping at the first success.",
        kind: FieldKind::Bool,
        restart_required: false,
    },
    SettingField {
        section: "security & anonymity",
        key: "announce_to_all_tiers",
        label: "announce to all tiers",
        description: "announce to all tracker tiers even when an earlier tier succeeds.",
        kind: FieldKind::Bool,
        restart_required: false,
    },

    // ── connection (interface binding is a vpn kill-switch) ──
    SettingField {
        section: "connection",
        key: "listen_address",
        label: "listen address (interface)",
        description: "bind to a specific NIC ip (e.g. wireguard's interface) to kill-switch traffic if the vpn drops. requires daemon restart.",
        kind: FieldKind::Text,
        restart_required: true,
    },
    SettingField {
        section: "connection",
        key: "listen_port",
        label: "listen port",
        description: "incoming peer port. requires daemon restart to re-bind.",
        kind: FieldKind::Integer,
        restart_required: true,
    },
    SettingField {
        section: "connection",
        key: "enable_upnp",
        label: "upnp port forwarding",
        description: "automatic LAN router port forwarding via UPnP. opt-in.",
        kind: FieldKind::Bool,
        restart_required: false,
    },
    SettingField {
        section: "connection",
        key: "enable_natpmp",
        label: "nat-pmp port forwarding",
        description: "automatic LAN router port forwarding via NAT-PMP. opt-in.",
        kind: FieldKind::Bool,
        restart_required: false,
    },
    SettingField {
        section: "connection",
        key: "max_connections",
        label: "max connections",
        description: "global peer connection ceiling.",
        kind: FieldKind::Integer,
        restart_required: false,
    },
    SettingField {
        section: "connection",
        key: "max_uploads",
        label: "max upload slots",
        description: "global upload slot ceiling. -1 means unlimited.",
        kind: FieldKind::Integer,
        restart_required: false,
    },
    SettingField {
        section: "connection",
        key: "download_rate_limit",
        label: "download cap (KiB/s)",
        description: "global download rate ceiling in KiB/s. 0 means unlimited.",
        kind: FieldKind::Integer,
        restart_required: false,
    },
    SettingField {
        section: "connection",
        key: "upload_rate_limit",
        label: "upload cap (KiB/s)",
        description: "global upload rate ceiling in KiB/s. 0 means unlimited.",
        kind: FieldKind::Integer,
        restart_required: false,
    },

    // ── bittorrent ──
    SettingField {
        section: "bittorrent",
        key: "enable_dht",
        label: "dht",
        description: "distributed hash table for trackerless discovery.",
        kind: FieldKind::Bool,
        restart_required: false,
    },
    SettingField {
        section: "bittorrent",
        key: "enable_lsd",
        label: "local service discovery",
        description: "find peers on the same LAN via multicast.",
        kind: FieldKind::Bool,
        restart_required: false,
    },
    SettingField {
        section: "bittorrent",
        key: "enable_incoming_utp",
        label: "incoming µTP",
        description: "accept incoming µTP (UDP) connections.",
        kind: FieldKind::Bool,
        restart_required: false,
    },
    SettingField {
        section: "bittorrent",
        key: "enable_outgoing_utp",
        label: "outgoing µTP",
        description: "open outgoing connections over µTP.",
        kind: FieldKind::Bool,
        restart_required: false,
    },

    // ── limits ──
    SettingField {
        section: "limits",
        key: "max_active_downloads",
        label: "max active downloads",
        description: "concurrent active downloads.",
        kind: FieldKind::Integer,
        restart_required: false,
    },
    SettingField {
        section: "limits",
        key: "max_active_uploads",
        label: "max active uploads",
        description: "concurrent active uploads/seeds.",
        kind: FieldKind::Integer,
        restart_required: false,
    },
    SettingField {
        section: "limits",
        key: "max_active_torrents",
        label: "max active torrents",
        description: "concurrent active torrents (downloads + uploads).",
        kind: FieldKind::Integer,
        restart_required: false,
    },
    SettingField {
        section: "limits",
        key: "seed_ratio_limit",
        label: "seed ratio limit",
        description: "stop seeding at this ratio. 0 means unlimited.",
        kind: FieldKind::Float,
        restart_required: false,
    },
    SettingField {
        section: "limits",
        key: "seed_time_limit",
        label: "seed time limit (minutes)",
        description: "stop seeding after this many minutes. 0 means unlimited.",
        kind: FieldKind::Integer,
        restart_required: false,
    },

    // ── paths ──
    SettingField {
        section: "paths",
        key: "default_save_path",
        label: "default save path",
        description: "where new torrents save their files by default.",
        kind: FieldKind::Text,
        restart_required: false,
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
        Ok(Self {
            config,
            selected: 0,
            current_tab: 0,
            edit_buffer: None,
            status: None,
            scroll: 0,
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
enum Mode {
    Main,
    Settings(SettingsState),
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
        self.lines.get(0).cloned().unwrap_or_default()
    }
}

#[derive(Clone, Copy)]
enum PromptAction {
    RenameTorrent,
    MoveTorrent,
    AddTorrent,
}

struct AppState {
    mode: Mode,
    prompt: Option<Prompt>,
    torrents: Vec<TorrentInfo>,
    stats: Option<StatsInfo>,
    detail: Option<TorrentDetail>,
    table_state: TableState,
    sidebar_state: ListState,
    detail_files_state: TableState,
    detail_peers_state: TableState,
    last_poll: Instant,
    last_detail_poll: Instant,
    error: Option<String>,
    daemon_unreachable: bool,
    show_sidebar: bool,
    show_detail: bool,
    focus: Pane,
    status_filter: StatusFilter,
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
    /// when Some, the column picker overlay is open (selection index)
    column_picker: Option<usize>,
    /// folder paths that are currently collapsed in the content tab
    collapsed_folders: std::collections::BTreeSet<String>,
    /// terminal capabilities probed at startup. truecolor is recorded but
    /// not yet used; gates a future richer hsl palette.
    #[allow(dead_code)]
    truecolor: bool,
    nerd_font: bool,
}

impl AppState {
    fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut sidebar_state = ListState::default();
        sidebar_state.select(Some(0));

        // load [tui] defaults from config.toml. failure here is non-fatal —
        // worst case the user sees built-in defaults until they fix the file.
        let (show_sidebar, show_detail, configured_columns, nerd_font) = Config::load()
            .map(|config| (
                config.tui_show_sidebar,
                config.tui_show_detail,
                config.tui_columns,
                config.tui_nerd_font,
            ))
            .unwrap_or((false, false, Vec::new(), false));

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
            torrents: Vec::new(),
            stats: None,
            detail: None,
            table_state,
            sidebar_state,
            detail_files_state: TableState::default(),
            detail_peers_state: TableState::default(),
            last_poll: Instant::now() - POLL_INTERVAL,
            last_detail_poll: Instant::now() - DETAIL_POLL_INTERVAL,
            error: None,
            daemon_unreachable: false,
            show_sidebar,
            show_detail,
            focus: Pane::List,
            status_filter: StatusFilter::All,
            detail_tab: DetailTab::Content,
            sidebar_rect: Rect::default(),
            list_rect: Rect::default(),
            detail_rect: Rect::default(),
            detail_tab_bar_rect: Rect::default(),
            last_click: None,
            visible_columns,
            column_picker: None,
            collapsed_folders: std::collections::BTreeSet::new(),
            truecolor,
            nerd_font,
        }
    }

    fn filtered_indices(&self) -> Vec<usize> {
        self.torrents.iter()
            .enumerate()
            .filter(|(_, torrent)| self.status_filter.matches(torrent))
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
            Pane::Sidebar => move_list(&mut self.sidebar_state, StatusFilter::ALL.len(), delta),
            Pane::Detail => match self.detail_tab {
                DetailTab::Content => {
                    let count = self.detail.as_ref()
                        .map(|detail| build_tree_rows(detail, &self.collapsed_folders).len())
                        .unwrap_or(0);
                    move_table(&mut self.detail_files_state, count, delta);
                }
                DetailTab::Peers => {
                    let count = self.detail.as_ref().map(|detail| detail.peers.len()).unwrap_or(0);
                    move_table(&mut self.detail_peers_state, count, delta);
                }
                DetailTab::Trackers => {}
            },
        }
    }

    fn apply_sidebar_selection(&mut self) {
        if let Some(index) = self.sidebar_state.selected() {
            if let Some(filter) = StatusFilter::ALL.get(index).copied() {
                if (filter != self.status_filter) {
                    self.status_filter = filter;
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
            }
            if (state.show_detail && state.last_detail_poll.elapsed() >= DETAIL_POLL_INTERVAL) {
                poll_detail(&mut state);
            }
        }

        terminal.draw(|frame| draw(frame, &mut state))?;

        if (event::poll(EVENT_TICK)?) {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let exit = if (state.column_picker.is_some()) {
                        handle_picker_key(key.code, key.modifiers, &mut state)
                    } else if (state.prompt.is_some()) {
                        handle_prompt_key(key.code, key.modifiers, &mut state)
                    } else if (matches!(state.mode, Mode::Settings(_))) {
                        handle_settings_key(key.code, key.modifiers, &mut state)
                    } else {
                        handle_key(key.code, key.modifiers, &mut state)
                    };
                    if (exit) { return Ok(()); }
                }
                Event::Mouse(mouse) => handle_mouse(mouse, &mut state),
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
                Ok(settings) => state.mode = Mode::Settings(settings),
                Err(error) => state.error = Some(format!("settings: {}", error)),
            }
        }

        // actions on the selected torrent
        (KeyCode::Char('p'), KeyModifiers::NONE) => toggle_pause(state),
        (KeyCode::Char('r'), KeyModifiers::NONE) | (KeyCode::F(2), _) => open_rename_prompt(state),
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

        _ => {}
    }
    false
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
        action: PromptAction::RenameTorrent,
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
        action: PromptAction::MoveTorrent,
        torrent_index: index,
        allow_multiline: false,
    });
}

fn open_add_prompt(state: &mut AppState) {
    state.prompt = Some(Prompt {
        title: "add torrent (shift+enter to add another line)".to_string(),
        helper: "magnet:, http(s)://, ftp(s)://, /abs/path, C:\\path, or ~/foo.torrent — one per line".to_string(),
        lines: vec![String::new()],
        cursor_line: 0,
        action: PromptAction::AddTorrent,
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

/// rename the torrent display name. libtorrent does not expose a stable
/// "set display name" api today, so this is implemented as a server-side
/// metadata update. for the v1 we just store nothing — the .torrent name
/// is canonical. surfacing this is a TODO; for now the prompt is a no-op
/// that explains itself.
fn submit_prompt(prompt: &Prompt, state: &mut AppState) -> Result<()> {
    match prompt.action {
        PromptAction::RenameTorrent => {
            Err(anyhow::anyhow!(
                "renaming the torrent name itself is not yet wired. \
                 use the content tab (e, ]) and F2 to rename individual files."
            ))
        }
        PromptAction::MoveTorrent => {
            match client::send(Request::Move {
                index: prompt.torrent_index,
                new_save_path: prompt.single_line_buffer(),
            })? {
                Response::Ok => Ok(()),
                Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
                _ => Err(anyhow::anyhow!("unexpected response")),
            }
        }
        PromptAction::AddTorrent => {
            // every non-blank line dispatches a separate Add. errors are
            // collected so a single bad line doesn't abort the batch.
            let mut succeeded: Vec<String> = Vec::new();
            let mut failed: Vec<String> = Vec::new();
            for line in &prompt.lines {
                let item = line.trim();
                if (item.is_empty()) { continue; }
                let response = client::send(Request::Add {
                    uri: item.to_string(),
                    save_path: None,
                    category: None,
                });
                match response {
                    Ok(Response::Added { id }) => succeeded.push(id),
                    Ok(Response::Err(message)) => failed.push(format!("{}: {}", item, message)),
                    Ok(_) => failed.push(format!("{}: unexpected response", item)),
                    Err(error) => failed.push(format!("{}: {}", item, error)),
                }
            }
            if (succeeded.is_empty() && failed.is_empty()) {
                return Err(anyhow::anyhow!("no sources provided"));
            }
            if (failed.is_empty()) {
                state.error = Some(format!("added {} torrent(s)", succeeded.len()));
                Ok(())
            } else if (succeeded.is_empty()) {
                Err(anyhow::anyhow!("all sources failed: {}", failed.join("; ")))
            } else {
                state.error = Some(format!(
                    "added {} ok, {} failed: {}",
                    succeeded.len(), failed.len(), failed.join("; ")
                ));
                Ok(())
            }
        }
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
        _ => {}
    }
    false
}

/// when the focused row in the content tab is a folder, toggle its collapsed
/// state. `collapse` true means a-or-left (collapse), false means d-or-right
/// (expand). on file rows the key is a no-op.
fn collapse_focused(state: &mut AppState, collapse: bool) {
    if (state.focus != Pane::Detail || state.detail_tab != DetailTab::Content) {
        return;
    }
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
    if (state.prompt.is_some() || matches!(state.mode, Mode::Settings(_))) {
        return;
    }
    let column = event.column;
    let row = event.row;
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => mouse_left_down(column, row, state),
        MouseEventKind::ScrollUp => mouse_scroll(column, row, state, -3),
        MouseEventKind::ScrollDown => mouse_scroll(column, row, state, 3),
        _ => {}
    }
}

fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    if (rect.width == 0 || rect.height == 0) { return false; }
    column >= rect.x
        && column < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

fn mouse_left_down(column: u16, row: u16, state: &mut AppState) {
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
    // sidebar row click — select the status filter
    if (rect_contains(state.sidebar_rect, column, row)) {
        // sidebar has a 1-row border on top, then one row per filter
        let row_in_pane = row.saturating_sub(state.sidebar_rect.y + 1);
        let target = row_in_pane as usize;
        if (target < StatusFilter::ALL.len()) {
            state.sidebar_state.select(Some(target));
            state.apply_sidebar_selection();
        }
        state.focus = Pane::Sidebar;
        return;
    }
    // torrent list row click — select that torrent. detect double-click for
    // open-detail (qBT-style).
    if (rect_contains(state.list_rect, column, row)) {
        // list has a 1-row border, then a 1-row header, then data rows
        let header_offset = 2;
        let row_in_data = row.saturating_sub(state.list_rect.y + header_offset);
        let visible = state.filtered_indices();
        let target = row_in_data as usize;
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
                let count = state.detail.as_ref().map(|detail| detail.files.len()).unwrap_or(0);
                move_table(&mut state.detail_files_state, count, delta);
            }
            DetailTab::Peers => {
                let count = state.detail.as_ref().map(|detail| detail.peers.len()).unwrap_or(0);
                move_table(&mut state.detail_peers_state, count, delta);
            }
            _ => {}
        }
    } else if (rect_contains(state.sidebar_rect, column, row)) {
        move_list(&mut state.sidebar_state, StatusFilter::ALL.len(), delta);
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
}

fn poll_detail(state: &mut AppState) {
    state.last_detail_poll = Instant::now();
    let Some(index) = state.selected_torrent_index() else {
        state.detail = None;
        return;
    };
    match client::send(Request::Info { index }) {
        Ok(Response::TorrentDetail(detail)) => state.detail = Some(*detail),
        Ok(_) => {}
        Err(_) => {}
    }
}

fn draw(frame: &mut ratatui::Frame, state: &mut AppState) {
    if (matches!(state.mode, Mode::Settings(_))) {
        draw_settings(frame, state);
    } else {
        draw_main(frame, state);
    }
    if (state.prompt.is_some()) {
        draw_prompt(frame, state);
    }
    if (state.column_picker.is_some()) {
        draw_column_picker(frame, state);
    }
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

    state.list_rect = center_split[0];
    draw_torrent_list(frame, center_split[0], state);
    if (state.show_detail) {
        state.detail_rect = center_split[1];
        draw_detail(frame, center_split[1], state);
    } else {
        state.detail_rect = Rect::default();
        state.detail_tab_bar_rect = Rect::default();
    }

    draw_status_bar(frame, outer[2], state);
    draw_hint_bar(frame, outer[3]);
}

fn focus_border_style(focused: bool) -> Style {
    if (focused) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_title(frame: &mut ratatui::Frame, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            " rustor ",
            Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(crate::VERSION, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn draw_sidebar(frame: &mut ratatui::Frame, area: Rect, state: &mut AppState) {
    let items: Vec<ListItem> = StatusFilter::ALL.iter().map(|filter| {
        let count = state.torrents.iter().filter(|torrent| filter.matches(torrent)).count();
        let mark = if (*filter == state.status_filter) { "● " } else { "  " };
        let line = Line::from(vec![
            Span::styled(mark, Style::default().fg(Color::Cyan)),
            Span::raw(filter.label()),
            Span::raw("  "),
            Span::styled(
                format!("({})", count),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        ListItem::new(line)
    }).collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focus_border_style(state.focus == Pane::Sidebar))
        .title(" status ");

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▌ ");

    frame.render_stateful_widget(list, area, &mut state.sidebar_state);

    // selecting a sidebar entry with up/down should also apply it immediately
    // (qBT does this — you don't have to press enter for the filter to take effect)
    state.apply_sidebar_selection();
}

fn draw_torrent_list(frame: &mut ratatui::Frame, area: Rect, state: &mut AppState) {
    let header_cells: Vec<Cell> = state.visible_columns.iter()
        .map(|column| Cell::from(column.label()).style(Style::default().add_modifier(Modifier::BOLD)))
        .collect();
    let header = Row::new(header_cells).height(1);

    let visible = state.filtered_indices();
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

    let widths: Vec<Constraint> = state.visible_columns.iter().map(|column| column.width()).collect();

    let title = if (state.daemon_unreachable) {
        format!(" torrents — {} (daemon unreachable) ", state.status_filter.label())
    } else if (visible.is_empty()) {
        format!(" torrents — {} (none) ", state.status_filter.label())
    } else {
        format!(" torrents — {} ({}) ", state.status_filter.label(), visible.len())
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(focus_border_style(state.focus == Pane::List))
                .title(title),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▌ ");

    frame.render_stateful_widget(table, area, &mut state.table_state);
}

fn draw_detail(frame: &mut ratatui::Frame, area: Rect, state: &mut AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focus_border_style(state.focus == Pane::Detail))
        .title(" detail ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let split = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(inner);

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

    let body = split[1];
    match state.detail_tab {
        DetailTab::Content => draw_content_tab(frame, body, state),
        DetailTab::Peers => draw_peers_tab(frame, body, state),
        DetailTab::Trackers => draw_trackers_tab(frame, body, state),
    }
}

/// one row in the rendered file tree: either a folder header (with collapse
/// state) or a leaf file row. file_index is None for folders.
struct TreeRow {
    indent: usize,
    label: String,
    full_path: String,
    is_folder: bool,
    #[allow(dead_code)] // wired up for per-file priority editing in a future iteration
    file_index: Option<usize>,
    total_size: i64,
    total_done: i64,
    priority: Option<u8>,
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
        rows.push(TreeRow {
            indent,
            label: format!("{}{}", prefix, name),
            full_path: folder.clone(),
            is_folder: true,
            file_index: None,
            total_size,
            total_done,
            priority: None,
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

    let tree_rows = build_tree_rows(detail, &state.collapsed_folders);

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
        let priority_label = match tree_row.priority {
            None => "—".to_string(),
            Some(0) => "skip".to_string(),
            Some(1..=3) => format!("low/{}", tree_row.priority.unwrap()),
            Some(4) => "normal".to_string(),
            Some(5..=6) => format!("high/{}", tree_row.priority.unwrap()),
            Some(7) => "max".to_string(),
            Some(other) => other.to_string(),
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
        .row_highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
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
        Cell::from("ip:port").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("down").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("up").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("client").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("prog").style(Style::default().add_modifier(Modifier::BOLD)),
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
        Constraint::Length(7),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
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
    let table = Table::new(rows, widths).header(header);
    frame.render_widget(table, area);
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

fn draw_hint_bar(frame: &mut ratatui::Frame, area: Rect) {
    let hint = Line::from(vec![
        Span::styled(" w/s ", Style::default().fg(Color::Yellow)),
        Span::raw("move  "),
        Span::styled("tab ", Style::default().fg(Color::Yellow)),
        Span::raw("pane  "),
        Span::styled("[/] ", Style::default().fg(Color::Yellow)),
        Span::raw("tabs  "),
        Span::styled("n ", Style::default().fg(Color::Yellow)),
        Span::raw("add  "),
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
        Span::styled("q/e ", Style::default().fg(Color::Yellow)),
        Span::raw("sidebar/detail  "),
        Span::styled(", ", Style::default().fg(Color::Yellow)),
        Span::raw("settings  "),
        Span::styled("^c ", Style::default().fg(Color::Yellow)),
        Span::raw("quit"),
    ]);
    frame.render_widget(
        Paragraph::new(hint)
            .alignment(Alignment::Left)
            .style(Style::default().fg(Color::Gray)),
        area,
    );
}

// ─── settings overlay ──────────────────────────────────────────────────────

fn handle_settings_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    let Mode::Settings(settings) = &mut state.mode else { return false; };

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
        FieldKind::Integer | FieldKind::Float | FieldKind::Text => {
            settings.edit_buffer = Some(current);
        }
    }
}

fn commit_edit(settings: &mut SettingsState, buffer: &str) {
    let field = settings.current_field();
    commit_value(settings, field.key, buffer);
}

fn commit_value(settings: &mut SettingsState, key: &str, value: &str) {
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
        let value = config_value_string(&settings.config, field.key);
        let display_value = render_value(settings, *index, &value);
        let is_selected = *index == settings.selected;
        let marker = if (is_selected) { "▌ " } else { "  " };
        let label_style = if (is_selected) {
            Style::default().add_modifier(Modifier::BOLD).fg(Color::White)
        } else {
            Style::default()
        };
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
    let editing = settings.edit_buffer.is_some();
    let hint = if (editing) {
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

/// state label that optionally substitutes nerd font icons for the ascii
/// short codes. when nerd_font is false the result is plain ascii so users
/// without a nerd-font-aware terminal don't see tofu boxes.
fn format_state_with(state: &str, is_paused: bool, nerd_font: bool) -> String {
    if (is_paused) {
        return if (nerd_font) { "\u{f04c}".to_string() } else { "PA".to_string() };
    }
    if (nerd_font) {
        return match state {
            "downloading" => "\u{f019}".to_string(),
            "seeding" => "\u{f093}".to_string(),
            "finished" => "\u{f00c}".to_string(),
            "downloading_metadata" => "\u{f1ce}".to_string(),
            "checking_files" | "checking_resume_data" => "\u{f021}".to_string(),
            "allocating" => "\u{f0c7}".to_string(),
            other => other.chars().take(2).collect::<String>().to_uppercase(),
        };
    }
    match state {
        "downloading" => "DL".to_string(),
        "seeding" => "SE".to_string(),
        "finished" => "FN".to_string(),
        "downloading_metadata" => "MD".to_string(),
        "checking_files" => "CK".to_string(),
        "checking_resume_data" => "CR".to_string(),
        "allocating" => "AL".to_string(),
        other => other.chars().take(2).collect::<String>().to_uppercase(),
    }
}
