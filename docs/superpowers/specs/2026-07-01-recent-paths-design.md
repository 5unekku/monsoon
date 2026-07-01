# Recent save paths (qBittorrent-style MRU list): design

**Date:** 2026-07-01
**Status:** approved for planning

## Overview

Sub-project #2 of the five-part filename/path effort (see the 2026-06-30 universal-input
spec for the numbering). qBittorrent keeps one global most-recently-used list of save
paths, shared by the add dialog's "Save at" combobox and the Set-location dialog, with a
configurable entry count. This spec brings the same affordance to monsoon's two
save-path-declaring surfaces:

- the `save_path` field on the add-options form (edited via the `Option<TextField>`
  activated by field 4, `src/tui.rs:2539`), and
- the move prompt (`open_move_prompt`, `src/tui.rs:1731`).

The design is deliberately small: two new config keys, one recording helper on the
daemon, and one picker overlay on the TUI that clones the existing
`InterfacePickerState` interaction pattern. **No new IPC requests or responses**: the
daemon records at points where it already handles the operation, and the TUI reads the
list through the existing `GetConfig` round trip (`fetch_config`, `src/tui.rs:826`).

### Non-goals

- No merging with categories. Categories already map names to save paths
  (`src/categories.rs`) and stay a separate, deliberate mechanism; the MRU list only
  tracks paths the user typed by hand. Interplay is limited to one rule: adds that
  resolve their path *from* a category (or from `default_save_path`) are not recorded.
- No recording from automation paths (RSS feeds, watch directories). Those call
  `add_magnet`/`add_file` directly with configured paths; an MRU list of paths the user
  never typed is noise.
- No recents affordance on other path-shaped fields (`default_save_path` in settings,
  feed `save_path`, watch directories, the add-torrent URI prompt). They declare
  preferences or sources, not a per-operation save location.
- No per-category or per-tracker recent lists; one global list, matching qBittorrent.
- No editing of the list from the TUI (no delete-entry UI). The list is self-pruning by
  the cap; hand-editing config.toml covers the rare cleanup.

---

## A. Storage and recording (daemon-side)

### A1. Config fields

Two new fields in `Config` (`src/config.rs`), following the `watch_directories`
`Vec<String>` precedent (`src/config.rs:115`). They live in config.toml, not a separate
file: the list is tiny, changes rarely, and the daemon already rewrites config.toml on
every settings change, so the categories.toml separate-file precedent buys nothing here.

```rust
/// most-recently-used save paths, front = most recent. written by the
/// daemon on successful add-with-explicit-path and on successful move.
#[serde(default)]
pub recent_save_paths: Vec<String>,
/// how many recent save paths to keep. 0 disables recording and the picker.
#[serde(default = "default_recent_paths_limit")]
pub recent_paths_limit: u16,
```

with `fn default_recent_paths_limit() -> u16 { 5 }` alongside the existing default fns
(`src/config.rs:175`).

### A2. Recording helper

One pure function so the MRU arithmetic is unit-testable without a daemon:

```rust
/// move-to-front dedup, then truncate to limit. limit 0 clears the list.
pub fn record_recent_path(list: &mut Vec<String>, path: &str, limit: u16)
```

Semantics: trim whitespace, then trim trailing `/` (but keep a bare `/` intact) so
`"/data/tv"` and `"/data/tv/"` dedup to one entry; remove any existing equal entry;
insert at the front; truncate to `limit`. Empty input after trimming is a no-op.

The server wraps it in a small method that mutates `self.config.recent_save_paths` and
calls `self.config.save()` **best-effort**: a failed config write logs a warning and
never fails the add or move that triggered it (recording is a convenience, the
operation already succeeded).

### A3. Recording points

Both points are where the daemon already owns the operation and already persists state,
so recording is a one-line addition at each:

- **Add with explicit save path**: in the `Request::Add` handler arm
  (`src/server.rs:1201`), after a successful `add_magnet`/`add_file`, and only when the
  request's `save_path` was `Some`. Recording lives in the request arm, not inside
  `add_magnet`/`add_file`, precisely so RSS/watch-directory adds (which call those
  functions directly) never record. Category-resolved and default-resolved paths arrive
  as `save_path: None` (`resolve_add_target`, `src/server.rs:206`), so they are excluded
  for free.
- **Move**: in `move_storage` (`src/server.rs:616`), at the commit point where
  `handle.move_storage` is actually submitted and `persist_torrent_list` already runs
  (`src/server.rs:684`). The earlier returns do not record: the same-canonical-path
  skip is a no-op, the `RenameConfirmation` return is phase one of the two-phase flow
  (the decision re-send lands back here and records then), and a declined merge
  records nothing.

### A4. Limit changes

`apply_config_change` (`src/server.rs:352`) gains a `"recent_paths_limit"` arm: parse as
`u16`, assign, and truncate `recent_save_paths` to the new limit immediately (0 clears
it). The existing `self.config.save()` at the end of `apply_config_change` persists both
fields together. `recent_save_paths` itself gets no `SetConfig` arm; it is
daemon-written only (hand-editing config.toml still works, `Config::load` keeps unknown
content healed and valid).

---

## B. Picker overlay (TUI-side)

### B1. Interaction

`ctrl+r` opens the picker in exactly two contexts:

- while the add-options `save_path` field is in inline-edit mode (the
  `form.edit_buffer.is_some()` block in `handle_add_options_key`, `src/tui.rs:2452`), and
- while the move prompt is open (`handle_prompt_key`, `src/tui.rs:2615`, gated on
  `prompt.action == PromptAction::Move` so rename and other prompts never show it).

`ctrl+r` is currently unbound everywhere in `tui.rs` (plain `r` is rename, `F2` its
alias), so there is no conflict.

On open the TUI calls `fetch_config()` (one `GetConfig` round trip, negligible over the
unix socket) and reads `recent_save_paths`. Fetching fresh each time means a second TUI
instance or a CLI add updates the list with no cache invalidation. If the list is empty
or `recent_paths_limit` is 0, no overlay opens; a status/error line says
"no recent save paths" (or "recent paths disabled" for limit 0).

### B2. State and keys

One new struct plus an `Option` on `AppState`, cloning the `InterfacePickerState` shape
(`src/tui.rs:722`) rather than reusing the type itself (that one is settings-specific,
carries a magic `__specific__` value, and lives on `SettingsState`):

```rust
/// recent-save-path dropdown. items come from config.recent_save_paths,
/// front of the list first (most recent on top).
struct RecentPathPicker {
    items: Vec<String>,
    selected: usize,
}
```

Key handling mirrors `handle_interface_picker_key` (`src/tui.rs:5373`) exactly:
`w`/`s`/`Up`/`Down` navigate, `Home`/`End` jump, `Esc`/`q` closes, `Enter` picks,
`ctrl+c` quits. The check for an open picker runs at the top of the two host handlers
(the same routing style `handle_settings_key` uses at `src/tui.rs:5119`), so picker keys
never leak into the underlying text field.

### B3. Picking

`Enter` replaces the active field with
`TextField::with_completion(picked, CompletionSource::Filesystem)`; the constructor
already places the cursor at the end (`src/textfield.rs:25`), so the user can keep
typing a subdirectory or hit tab-complete immediately. The target field is whichever
surface hosted the `ctrl+r`: `form.edit_buffer` for the add-options form, `lines[0]` of
the move prompt otherwise. Picking fills the buffer only; nothing is submitted, and
nothing is recorded at pick time (the eventual successful add/move records it, which
performs the move-to-front naturally).

### B4. Drawing

`draw_recent_paths_picker` clones `draw_interface_picker`'s centered-modal geometry
(`src/tui.rs:5495`): `Clear`, rounded yellow border, title " recent save paths ", hint
line " w/s move  enter pick  esc cancel", highlighted selected row. Rendered last in the
frames that draw the add-options form and the prompt, so it sits on top. The two
existing hint lines (add-options save-path editor, move prompt helper) each gain a
"ctrl+r recent" mention.

---

## C. Settings exposure

`recent_paths_limit` joins `SETTING_FIELDS` (`src/tui.rs:346`) in the existing "paths"
section (`src/tui.rs:627`), `FieldKind::Integer`, description
"recent save paths to remember (0 disables)". That gives it the standard inline-edit +
`SetConfig` flow with no new UI. `recent_save_paths` is deliberately absent from the
settings overlay (see A4).

---

## Decisions

Every choice below is decided so implementation needs no follow-up questions; each is
revisitable.

1. **Storage: `recent_save_paths: Vec<String>` in config.toml**, not a separate file.
   Tiny list, daemon already owns and rewrites config.toml; a second file adds load/save
   plumbing for nothing.
2. **Order: MRU, front = most recent**, picker shows top-down in stored order with the
   top row preselected. Matches qBittorrent's combobox ordering.
3. **Cap: `recent_paths_limit: u16`, default 5, 0 disables** both recording and the
   picker. Matches qBittorrent's default and makes "off" a first-class value instead of
   a second boolean.
4. **Recording is daemon-side**, at the `Request::Add` arm and the `move_storage` commit
   point. The daemon owns config persistence and already persists state at exactly those
   points; TUI-side recording would race a second client.
5. **Record only explicit user paths**: `Add` with `save_path: Some(..)` and successful
   `Move`. Category-resolved, default-path, RSS, and watch-directory adds do not record.
   The list should contain paths the user typed, or it stops being a shortcut.
6. **Dedup normalization is trailing-slash-and-whitespace trim only**, exact string match
   otherwise. No canonicalization: the daemon may not have the path mounted yet, and
   symlink-resolving a convenience list is over-engineering.
7. **One global list** shared by both surfaces, like qBittorrent. Per-surface lists double
   the config surface for no observed need.
8. **Surface: modal picker overlay on `ctrl+r`**, cloning the `InterfacePickerState`
   pattern (new small struct, not the settings-bound type). An inline combobox would need
   a new widget; the modal pattern already exists and matches the app's overlay style.
9. **Picking fills the buffer, cursor at end, nothing submitted.** The user can append a
   subdirectory or tab-complete; confirmation stays on the existing Enter path.
10. **Picker data is fetched fresh via `GetConfig` on every open.** One cheap round trip
    buys cross-client freshness and zero cache logic.
11. **Recording failures are best-effort**: a failed config write logs and never fails
    the add/move. The operation the user asked for already succeeded.
12. **No delete-entry UI.** The cap self-prunes; config.toml is hand-editable for the
    rare surgical removal.

---

## IPC / data-model summary

- **New requests / responses: none.** Recording happens inside existing `Add` and `Move`
  handling; the TUI reads the list via the existing `GetConfig`.
- **New config fields (`src/config.rs`):** `recent_save_paths: Vec<String>` (serde
  default empty), `recent_paths_limit: u16` (default 5).
- **`apply_config_change` (`src/server.rs`):** new `"recent_paths_limit"` arm (parse +
  truncate). No arm for `recent_save_paths`.
- **New TUI state:** `recent_paths_picker: Option<RecentPathPicker>` on `AppState`;
  `recent_paths_limit` row in `SETTING_FIELDS`.

## Testing

- **Unit (Rust):** `record_recent_path` covering move-to-front dedup, cap truncation,
  limit 0 clearing, trailing-slash and whitespace normalization, bare `/`, empty input
  no-op, and re-recording an existing front entry.
- **Manual (TUI):** add a torrent with a typed save path, confirm it appears at the top
  of both pickers; move a torrent, confirm the destination is recorded; re-use an
  existing entry and confirm it moves to the front instead of duplicating; add via
  category and via RSS/watch and confirm nothing is recorded; set the limit to 2 and
  confirm the list truncates immediately; set it to 0 and confirm `ctrl+r` reports
  disabled; `ctrl+r` in a rename prompt does nothing; pick an entry, append a
  subdirectory, tab-complete, and submit.

## Risks / notes

- The two-phase move flow records on the commit call (the decision re-send), not the
  analysis call; if a future refactor collapses the phases, the recording call must stay
  on the path where `handle.move_storage` is actually submitted.
- `move_storage` requires an absolute path but `Add`'s `save_path` is passed through
  as typed; a relative add path would be recorded as typed. Accepted: libtorrent
  resolves it, and the list mirrors user input by design.
- Recording writes config.toml on every qualifying add/move. That is one small file
  write per user-initiated operation, in line with `persist_torrent_list` already
  running at the same points; no batching needed.
