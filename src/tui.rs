use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
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
    execute!(out, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(out);
    Terminal::new(backend).context("create terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
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
    selected: usize,
    /// when Some, an inline editor for the selected field's value is active
    edit_buffer: Option<String>,
    /// last action outcome (success message or daemon error)
    status: Option<String>,
    /// scroll offset for the settings body (in terms of display lines)
    scroll: u16,
}

impl SettingsState {
    fn load() -> Result<Self> {
        let config = fetch_config()?;
        Ok(Self { config, selected: 0, edit_buffer: None, status: None, scroll: 0 })
    }

    fn refresh_config(&mut self) {
        if let Ok(config) = fetch_config() {
            self.config = config;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let length = SETTING_FIELDS.len() as isize;
        if (length == 0) { return; }
        let next = (self.selected as isize + delta).rem_euclid(length) as usize;
        self.selected = next;
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
    buffer: String,
    /// the request to send when the user submits. takes the buffer as argument
    /// and produces an ipc Request.
    action: PromptAction,
    /// torrent the action targets (so the prompt remembers it across redraws)
    torrent_index: usize,
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
}

impl AppState {
    fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut sidebar_state = ListState::default();
        sidebar_state.select(Some(0));

        // load [tui] defaults from config.toml. failure here is non-fatal —
        // worst case the user sees built-in defaults until they fix the file.
        let (show_sidebar, show_detail) = Config::load()
            .map(|config| (config.tui_show_sidebar, config.tui_show_detail))
            .unwrap_or((false, false));

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
                    let count = self.detail.as_ref().map(|detail| detail.files.len()).unwrap_or(0);
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
                    let exit = if (state.prompt.is_some()) {
                        handle_prompt_key(key.code, key.modifiers, &mut state)
                    } else if (matches!(state.mode, Mode::Settings(_))) {
                        handle_settings_key(key.code, key.modifiers, &mut state)
                    } else {
                        handle_key(key.code, key.modifiers, &mut state)
                    };
                    if (exit) { return Ok(()); }
                }
                _ => {}
            }
        }
    }
}

/// returns true when the tui should exit.
fn handle_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match (code, modifiers) {
        (KeyCode::Char('q'), KeyModifiers::NONE)
        | (KeyCode::Char('q'), KeyModifiers::CONTROL)
        | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,

        // wasd + arrows. j/k are intentionally not bound.
        (KeyCode::Char('s'), KeyModifiers::NONE) | (KeyCode::Down, _) => state.move_focused(1),
        (KeyCode::Char('w'), KeyModifiers::NONE) | (KeyCode::Up, _) => state.move_focused(-1),
        (KeyCode::PageDown, _) => state.move_focused(10),
        (KeyCode::PageUp, _) => state.move_focused(-10),

        // pane cycling + visibility toggles
        (KeyCode::Tab, _) => state.cycle_focus(),
        (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
            state.show_sidebar = !state.show_sidebar;
            // collapsing the focused pane drops focus back to the list
            if (!state.show_sidebar && state.focus == Pane::Sidebar) { state.focus = Pane::List; }
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            state.show_detail = !state.show_detail;
            if (!state.show_detail && state.focus == Pane::Detail) { state.focus = Pane::List; }
            if (state.show_detail) {
                // force an immediate fetch so the pane isn't blank
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
        buffer: current,
        action: PromptAction::RenameTorrent,
        torrent_index: index,
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
        buffer: current,
        action: PromptAction::MoveTorrent,
        torrent_index: index,
    });
}

fn open_add_prompt(state: &mut AppState) {
    state.prompt = Some(Prompt {
        title: "add torrent".to_string(),
        helper: "magnet uri or absolute path to .torrent file".to_string(),
        buffer: String::new(),
        action: PromptAction::AddTorrent,
        // index is ignored for add
        torrent_index: 0,
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
            // for now we don't support renaming the torrent itself (only files
            // and folders inside it, via the existing rename ipc). surface the
            // limitation explicitly rather than silently doing nothing.
            Err(anyhow::anyhow!(
                "renaming the torrent name itself is not yet wired. \
                 use the content tab (ctrl+d, ]) and F2 to rename individual files."
            ))
        }
        PromptAction::MoveTorrent => {
            match client::send(Request::Move {
                index: prompt.torrent_index,
                new_save_path: prompt.buffer.clone(),
            })? {
                Response::Ok => Ok(()),
                Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
                _ => Err(anyhow::anyhow!("unexpected response")),
            }
        }
        PromptAction::AddTorrent => {
            match client::send(Request::Add {
                uri: prompt.buffer.clone(),
                save_path: None,
                category: None,
            })? {
                Response::Added { id } => {
                    state.error = Some(format!("added torrent {}", id));
                    Ok(())
                }
                Response::Err(message) => Err(anyhow::anyhow!("{}", message)),
                _ => Err(anyhow::anyhow!("unexpected response")),
            }
        }
    }
}

fn handle_prompt_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Esc, _) => state.prompt = None,
        (KeyCode::Enter, _) => {
            if let Some(prompt) = state.prompt.take() {
                match submit_prompt(&prompt, state) {
                    Ok(_) => {
                        state.last_poll = Instant::now() - POLL_INTERVAL;
                        state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
                    }
                    Err(error) => {
                        state.error = Some(error.to_string());
                        // put the prompt back so the user can correct + retry
                        state.prompt = Some(prompt);
                    }
                }
            }
        }
        (KeyCode::Backspace, _) => {
            if let Some(prompt) = state.prompt.as_mut() { prompt.buffer.pop(); }
        }
        (KeyCode::Char(character), modifiers)
            if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
        {
            if let Some(prompt) = state.prompt.as_mut() { prompt.buffer.push(character); }
        }
        _ => {}
    }
    false
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
}

fn draw_prompt(frame: &mut ratatui::Frame, state: &AppState) {
    let Some(prompt) = &state.prompt else { return; };
    let area = frame.area();
    // center a 70%-wide, 7-tall box
    let width = (area.width * 70 / 100).clamp(40, area.width.saturating_sub(4));
    let height: u16 = 7;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect { x, y, width, height };

    // clear underneath so the previous frame doesn't bleed through
    frame.render_widget(ratatui::widgets::Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(format!(" {} ", prompt.title));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            prompt.helper.as_str(),
            Style::default().fg(Color::DarkGray),
        ))),
        layout[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("› ", Style::default().fg(Color::Yellow)),
            Span::raw(prompt.buffer.as_str()),
            Span::styled("█", Style::default().fg(Color::Yellow)),
        ])),
        layout[2],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" enter ", Style::default().fg(Color::Yellow)),
            Span::raw("submit  "),
            Span::styled("esc ", Style::default().fg(Color::Yellow)),
            Span::raw("cancel"),
        ]))
        .style(Style::default().fg(Color::Gray)),
        layout[4],
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

    if (state.show_sidebar) { draw_sidebar(frame, with_sidebar[0], state); }

    let center = with_sidebar[1];
    let center_split = if (state.show_detail) {
        Layout::vertical([Constraint::Min(5), Constraint::Percentage(40)]).split(center)
    } else {
        Layout::vertical([Constraint::Min(0), Constraint::Length(0)]).split(center)
    };

    draw_torrent_list(frame, center_split[0], state);
    if (state.show_detail) { draw_detail(frame, center_split[1], state); }

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
    let header_cells = [" #", "name", "st", "prog", "down", "up", "peers"]
        .into_iter()
        .map(|label| Cell::from(label).style(Style::default().add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1);

    let visible = state.filtered_indices();
    let rows: Vec<Row> = visible.iter().map(|index| {
        let torrent = &state.torrents[*index];
        let state_label = format_state(&torrent.state, torrent.is_paused);
        let progress = format!("{:>5.1}%", torrent.progress * 100.0);
        let down = crate::display::format_rate(torrent.download_rate);
        let up = crate::display::format_rate(torrent.upload_rate);
        let peers = format!("{}/{}", torrent.connected_peers, torrent.total_peers);

        let row_style = if (torrent.is_paused) {
            Style::default().fg(Color::DarkGray)
        } else if (torrent.is_seeding) {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };

        Row::new(vec![
            Cell::from(format!("{:>3}", index)),
            Cell::from(torrent.name.clone()),
            Cell::from(state_label),
            Cell::from(progress),
            Cell::from(down),
            Cell::from(up),
            Cell::from(peers),
        ])
        .style(row_style)
    }).collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Min(20),
        Constraint::Length(4),
        Constraint::Length(7),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(14),
    ];

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

    let header = Row::new([
        Cell::from("#").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("name").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("total size").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("progress").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("priority").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("remaining").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("avail").style(Style::default().add_modifier(Modifier::BOLD)),
    ]);

    // per-file progress, priority come from the bridge; remaining is derived;
    // availability stays placeholder until libtorrent's per-file availability
    // is plumbed through (would need a new bridge call for piece availability).
    let rows: Vec<Row> = detail.files.iter().map(|file: &FileInfo| {
        let remaining = file.size - ((file.size as f64 * file.progress as f64) as i64);
        let priority_label = match file.priority {
            0 => "skip".to_string(),
            1..=3 => format!("low/{}", file.priority),
            4 => "normal".to_string(),
            5..=6 => format!("high/{}", file.priority),
            7 => "max".to_string(),
            other => other.to_string(),
        };
        let row_style = if (file.priority == 0) {
            Style::default().fg(Color::DarkGray)
        } else if (file.progress >= 1.0) {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        Row::new(vec![
            Cell::from(file.index.to_string()),
            Cell::from(file.path.clone()),
            Cell::from(crate::display::format_bytes(file.size)),
            Cell::from(format!("{:>5.1}%", file.progress * 100.0)),
            Cell::from(priority_label),
            Cell::from(crate::display::format_bytes(remaining.max(0))),
            Cell::from("—"),
        ])
        .style(row_style)
    }).collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Min(20),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Length(12),
        Constraint::Length(7),
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
        Span::styled("^b/^d ", Style::default().fg(Color::Yellow)),
        Span::raw("panes  "),
        Span::styled(", ", Style::default().fg(Color::Yellow)),
        Span::raw("settings  "),
        Span::styled("q ", Style::default().fg(Color::Yellow)),
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
        (KeyCode::Home, _) => settings.selected = 0,
        (KeyCode::End, _) => settings.selected = SETTING_FIELDS.len().saturating_sub(1),
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

    draw_settings_body(frame, outer[1], settings);
    draw_settings_footer(frame, outer[2], settings);
    draw_settings_hint(frame, outer[3], settings);
}

fn draw_settings_body(frame: &mut ratatui::Frame, area: Rect, settings: &mut SettingsState) {
    // build display rows + a parallel index recording which display row contains
    // which field. used to drive the scroll offset.
    let mut lines: Vec<Line> = Vec::new();
    let mut field_to_row: Vec<u16> = vec![0; SETTING_FIELDS.len()];
    let mut current_section: &str = "";

    for (index, field) in SETTING_FIELDS.iter().enumerate() {
        if (field.section != current_section) {
            if (!lines.is_empty()) { lines.push(Line::from("")); }
            lines.push(Line::from(Span::styled(
                format!(" {} ", field.section),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            current_section = field.section;
        }
        let value = config_value_string(&settings.config, field.key);
        let display_value = render_value(settings, index, &value);
        let is_selected = index == settings.selected;
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
        field_to_row[index] = (lines.len() - 1) as u16;
    }

    // adjust scroll so the selected field stays inside the visible window.
    // viewport_height = inner height (area.height minus the rounded border on
    // top and bottom = 2).
    let viewport_height = area.height.saturating_sub(2);
    let selected_row = field_to_row[settings.selected];
    if (selected_row < settings.scroll) {
        settings.scroll = selected_row;
    } else if (viewport_height > 0 && selected_row >= settings.scroll + viewport_height) {
        settings.scroll = selected_row + 1 - viewport_height;
    }
    let max_scroll = (lines.len() as u16).saturating_sub(viewport_height);
    if (settings.scroll > max_scroll) { settings.scroll = max_scroll; }

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
            Span::styled("enter ", Style::default().fg(Color::Yellow)),
            Span::raw("edit / toggle  "),
            Span::styled("esc ", Style::default().fg(Color::Yellow)),
            Span::raw("return to main  "),
            Span::styled("^c ", Style::default().fg(Color::Yellow)),
            Span::raw("quit"),
        ])
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::Gray)),
        area,
    );
}

fn format_state(state: &str, is_paused: bool) -> String {
    if (is_paused) { return "PA".to_string(); }
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
