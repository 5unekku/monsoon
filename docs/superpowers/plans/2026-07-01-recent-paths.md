# Recent Save Paths (qBittorrent-style MRU list) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One global most-recently-used list of save paths, shared by the add-options form's
`save_path` editor and the move prompt, capped by a configurable entry count. The daemon
records the list at the two points where it already owns the operation (explicit-path add,
move commit); the TUI reads it through the existing `GetConfig` round trip and shows it in a
`ctrl+r` modal picker cloned from the interface-picker pattern.

**Architecture:** Two new fields on `Config` (`src/config.rs`) plus one pure, unit-tested
helper (`record_recent_path`) that owns all MRU arithmetic. `src/server.rs` gains a small
best-effort wrapper (`record_recent_save_path`) called from the `Request::Add` handler arm
(only when the request carried an explicit `save_path`) and from `move_storage`'s commit
point, plus a `"recent_paths_limit"` arm in `apply_config_change`. `src/tui.rs` gains a
`RecentPathPicker` overlay struct on `AppState`, key routing at the top of the two host
handlers, a centered-modal draw function, and one new `SETTING_FIELDS` row. No new IPC
requests or responses; `src/ipc.rs` and `src/sources.rs` are untouched.

**Tech Stack:** Rust 2021, `ratatui`/`crossterm` (TUI), `serde`/`toml` for persistence,
existing unix-socket line-delimited json IPC.

**Spec:** `docs/superpowers/specs/2026-07-01-recent-paths-design.md`

---

## Sequencing

Tasks run strictly in order (1 through 6); there is nothing to parallelize inside this plan.

- **Task 1** touches `src/config.rs` only.
- **Task 2** touches `src/server.rs` only, and needs Task 1's fields and helper.
- **Tasks 3, 4, 5** all touch `src/tui.rs`, in order (3, then 4, then 5). Task 3 needs
  Task 1: the picker reads the new `Config` fields through `fetch_config`.
- **Task 6** is verification only.
- `src/ipc.rs` and `src/sources.rs` are never modified: recording happens inside existing
  `Add`/`Move` handling and the TUI reads the list via the existing `GetConfig` request.

---

## Task 1: config fields and the `record_recent_path` helper

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Write the failing tests**

`src/config.rs` currently has no tests module. Append one at the end of the file
(after the closing brace of `impl Config`, line 412):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn list(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|entry| entry.to_string()).collect()
    }

    #[test]
    fn record_inserts_at_front() {
        let mut paths = list(&["/data/movies"]);
        record_recent_path(&mut paths, "/data/tv", 5);
        assert_eq!(paths, list(&["/data/tv", "/data/movies"]));
    }

    #[test]
    fn record_moves_existing_entry_to_front_without_duplicating() {
        let mut paths = list(&["/data/tv", "/data/movies", "/data/music"]);
        record_recent_path(&mut paths, "/data/music", 5);
        assert_eq!(paths, list(&["/data/music", "/data/tv", "/data/movies"]));
    }

    #[test]
    fn record_existing_front_entry_is_stable() {
        let mut paths = list(&["/data/tv", "/data/movies"]);
        record_recent_path(&mut paths, "/data/tv", 5);
        assert_eq!(paths, list(&["/data/tv", "/data/movies"]));
    }

    #[test]
    fn record_truncates_to_limit() {
        let mut paths = list(&["/one", "/two", "/three"]);
        record_recent_path(&mut paths, "/zero", 3);
        assert_eq!(paths, list(&["/zero", "/one", "/two"]));
    }

    #[test]
    fn limit_zero_clears_the_list() {
        let mut paths = list(&["/one", "/two"]);
        record_recent_path(&mut paths, "/three", 0);
        assert!(paths.is_empty());
    }

    #[test]
    fn trailing_slash_and_whitespace_dedup_to_one_entry() {
        let mut paths = Vec::new();
        record_recent_path(&mut paths, "/data/tv", 5);
        record_recent_path(&mut paths, "  /data/tv/  ", 5);
        assert_eq!(paths, list(&["/data/tv"]));
    }

    #[test]
    fn bare_root_slash_is_kept_intact() {
        let mut paths = Vec::new();
        record_recent_path(&mut paths, "/", 5);
        assert_eq!(paths, list(&["/"]));
    }

    #[test]
    fn empty_input_after_trimming_is_a_noop() {
        let mut paths = list(&["/data/tv"]);
        record_recent_path(&mut paths, "   ", 5);
        assert_eq!(paths, list(&["/data/tv"]));
    }

    #[test]
    fn sanitize_truncates_recent_paths_to_limit() {
        let mut config = Config {
            recent_paths_limit: 2,
            recent_save_paths: list(&["/one", "/two", "/three"]),
            ..Config::default()
        };
        config.sanitize();
        assert_eq!(config.recent_save_paths, list(&["/one", "/two"]));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test config::`
Expected: FAIL to compile. `cannot find function record_recent_path in this scope` plus
`struct Config has no field named recent_paths_limit / recent_save_paths`.

- [ ] **Step 3: Implement**

Three edits in `src/config.rs`:

**3a.** Add the two fields to the `Config` struct, directly after `default_content_layout`
(line 28):

```rust
    /// most-recently-used save paths, front = most recent. written by the
    /// daemon on successful add-with-explicit-path and on successful move.
    #[serde(default)]
    pub recent_save_paths: Vec<String>,
    /// how many recent save paths to keep. 0 disables recording and the picker.
    #[serde(default = "default_recent_paths_limit")]
    pub recent_paths_limit: u16,
```

**3b.** Add the default fn alongside the existing ones (after `default_ask()`, line 183),
and the pure helper right after it:

```rust
fn default_recent_paths_limit() -> u16 { 5 }

/// move-to-front dedup, then truncate to limit. limit 0 clears the list.
/// normalization is whitespace trim plus trailing-slash trim (a bare "/"
/// stays intact) so "/data/tv" and "/data/tv/" dedup to one entry. empty
/// input after trimming is a no-op. no canonicalization on purpose: the
/// path may not even be mounted, and the list mirrors what the user typed.
pub fn record_recent_path(list: &mut Vec<String>, path: &str, limit: u16) {
    let trimmed = path.trim();
    let normalized = if (trimmed == "/") { "/" } else { trimmed.trim_end_matches('/') };
    if (normalized.is_empty()) { return; }
    list.retain(|entry| entry.as_str() != normalized);
    list.insert(0, normalized.to_string());
    list.truncate(limit as usize);
}
```

**3c.** Initialize both fields in `impl Default for Config`, directly after
`default_content_layout: default_content_layout(),` (line 200):

```rust
            recent_save_paths: Vec::new(),
            recent_paths_limit: default_recent_paths_limit(),
```

**3d.** Heal hand-edited lists in `sanitize()`, after the tui sanity block (line 313) and
before the `listen_address` non-empty check:

```rust
        // recent save paths never exceed their cap, even after hand-edits
        self.recent_save_paths.truncate(self.recent_paths_limit as usize);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test config::`
Expected: PASS, all 9 tests. Note: a plain `cargo build` at this point may warn that
`record_recent_path` is never used (the only caller so far is the cfg(test) module, and pub
items in a binary crate still get dead-code analysis). That is expected and resolves when
Task 2 wires it into the server; do not silence it.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "config: recent save paths mru fields and record helper"
```

---

## Task 2: daemon-side recording

**Files:**
- Modify: `src/server.rs`

There is no unit-test seam for the daemon (it needs a live libtorrent session), so this task
is compile-verified here and behavior-verified manually in Task 6. The MRU arithmetic itself
is already covered by Task 1's tests.

- [ ] **Step 1: Add the best-effort wrapper**

In `src/server.rs`, insert after the closing brace of `persist_torrent_list` (line 202) and
before the `resolve_add_target` doc comment (line 204):

```rust
    /// push a user-typed save path onto the mru list and persist. best-effort:
    /// a failed config write logs a warning and never fails the add or move
    /// that triggered it (the operation already succeeded).
    fn record_recent_save_path(&mut self, path: &str) {
        // limit 0 disables recording entirely
        if (self.config.recent_paths_limit == 0) { return; }
        let limit = self.config.recent_paths_limit;
        crate::config::record_recent_path(&mut self.config.recent_save_paths, path, limit);
        if let Err(error) = self.config.save() {
            tracing::warn!("failed to persist recent save paths: {}", error);
        }
    }
```

- [ ] **Step 2: Record in the `Request::Add` handler arm**

Replace the whole `Request::Add` arm in `handle_request` (`src/server.rs:1201-1219`).
Recording lives here, not inside `add_magnet`/`add_file`, precisely so RSS and
watch-directory adds (which call those functions directly) never record. Category-resolved
and default-resolved paths arrive as `save_path: None`, so they are excluded for free.

Old:

```rust
            Request::Add { uri, save_path, category, start_paused, content_layout } => {
                // delegate scheme + path resolution to the sources module so
                // http/https/ftp/sftp urls and ~ expansion work uniformly.
                match crate::sources::resolve(&uri) {
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

New:

```rust
            Request::Add { uri, save_path, category, start_paused, content_layout } => {
                // delegate scheme + path resolution to the sources module so
                // http/https/ftp/sftp urls and ~ expansion work uniformly.
                let result = match crate::sources::resolve(&uri) {
                    Ok(crate::sources::Source::Magnet(magnet)) => {
                        self.add_magnet(&magnet, save_path.as_deref(), category.as_deref(), start_paused, content_layout)
                    }
                    Ok(crate::sources::Source::File(path)) => {
                        self.add_file(&path.to_string_lossy(), save_path.as_deref(), category.as_deref(), start_paused, content_layout)
                    }
                    Err(error) => Err(error),
                };
                match result {
                    Ok(hash) => {
                        // record only explicit user paths: category-resolved and
                        // default-resolved adds arrive with save_path = None
                        if let Some(path) = save_path.as_deref() {
                            self.record_recent_save_path(path);
                        }
                        Response::Added { id: hash }
                    }
                    Err(error) => Response::Err(error.to_string()),
                }
            }
```

- [ ] **Step 3: Record at `move_storage`'s commit point**

In `move_storage` (`src/server.rs:616`), the tail of the function currently reads
(lines 684-691):

```rust
        let torrent = self.torrents.get_mut(index).unwrap();
        torrent.handle.move_storage(trimmed);
        torrent.save_path = trimmed.to_string();
        // outcome arrives via storage_moved_alert; persist now so a daemon
        // restart before completion still points at the right location
        self.persist_torrent_list();
        tracing::info!(index, new_save_path = trimmed, "submitted move_storage");
        Ok(Response::Ok)
```

Insert one call after `self.persist_torrent_list();`:

```rust
        let torrent = self.torrents.get_mut(index).unwrap();
        torrent.handle.move_storage(trimmed);
        torrent.save_path = trimmed.to_string();
        // outcome arrives via storage_moved_alert; persist now so a daemon
        // restart before completion still points at the right location
        self.persist_torrent_list();
        // record at the commit point only. the earlier returns never get here:
        // the same-canonical-path skip is a no-op, the RenameConfirmation
        // return is phase one of the two-phase flow (the decision re-send
        // lands back here and records then), and a declined merge (the
        // `(_, Some(_)) => return Ok(Response::Ok)` arm above) records nothing.
        self.record_recent_save_path(trimmed);
        tracing::info!(index, new_save_path = trimmed, "submitted move_storage");
        Ok(Response::Ok)
```

- [ ] **Step 4: Add the `"recent_paths_limit"` arm to `apply_config_change`**

In `apply_config_change` (`src/server.rs:352`), insert a new arm directly after the
`"watch_directories"` arm (lines 393-398):

```rust
            "recent_paths_limit" => {
                self.config.recent_paths_limit = value.parse()?;
                // shrink immediately so the on-disk list never exceeds the cap
                // (0 clears it); the save() at the end of this function
                // persists both fields together
                let limit = self.config.recent_paths_limit as usize;
                self.config.recent_save_paths.truncate(limit);
            }
```

No arm for `recent_save_paths`: it is daemon-written only. A `SetConfig` for it keeps
hitting the existing `_ => return Err(anyhow::anyhow!("unknown config key: {}", key))` arm,
which is the intended behavior.

- [ ] **Step 5: Verify it compiles**

Run: `cargo build`
Expected: PASS (the C++ bridge makes this take minutes; let it run).

- [ ] **Step 6: Commit**

```bash
git add src/server.rs
git commit -m "server: record recent save paths on explicit add and move"
```

---

## Task 3: TUI picker state, opening, and key handling

**Files:**
- Modify: `src/tui.rs`

After this task the picker opens, navigates, and picks, but has no draw function yet
(Task 4), so interactively it is invisible until Task 4 lands. That mid-plan state is fine;
each task still compiles.

- [ ] **Step 1: Add the `RecentPathPicker` struct and the `AppState` field**

**1a.** Insert the struct after the closing brace of `ConfirmRevertLayout`
(`src/tui.rs:1013`):

```rust
/// recent-save-path dropdown opened with ctrl+r from the add-options
/// save_path editor or the move prompt. items come from
/// config.recent_save_paths, front of the list first (most recent on top).
/// deliberately its own small struct rather than a reuse of
/// InterfacePickerState, which is settings-bound and carries the magic
/// __specific__ value.
struct RecentPathPicker {
    items: Vec<String>,
    selected: usize,
}
```

**1b.** Add the field to `AppState`, after `confirm_revert_layout` (`src/tui.rs:1209`):

```rust
    /// when Some, the recent-save-paths dropdown is open
    recent_paths_picker: Option<RecentPathPicker>,
```

**1c.** Initialize it in `AppState::new`, after `confirm_revert_layout: None,`
(`src/tui.rs:1307`):

```rust
            recent_paths_picker: None,
```

- [ ] **Step 2: Add the open, apply, and key-handler functions**

Insert all three after the closing brace of `open_move_prompt` (`src/tui.rs:1747`):

```rust
/// fetch the mru list fresh (one GetConfig round trip, so a second tui or a
/// cli add is picked up with zero cache logic) and open the picker. reports
/// via the status line when the list is empty or recording is disabled.
fn open_recent_paths_picker(state: &mut AppState) {
    match fetch_config() {
        Ok(config) => {
            if (config.recent_paths_limit == 0) {
                state.error = Some("recent paths disabled (recent_paths_limit = 0)".to_string());
            } else if (config.recent_save_paths.is_empty()) {
                state.error = Some("no recent save paths".to_string());
            } else {
                state.recent_paths_picker = Some(RecentPathPicker {
                    items: config.recent_save_paths,
                    selected: 0,
                });
            }
        }
        Err(error) => state.error = Some(error.to_string()),
    }
}

/// replace the active save-path field with the picked entry. the target is
/// whichever surface hosted the ctrl+r: the add-options edit buffer when the
/// form is up, the move prompt's single line otherwise. picking fills the
/// buffer only; nothing is submitted and nothing is recorded until the
/// eventual add/move succeeds daemon-side.
fn apply_picked_recent_path(state: &mut AppState, picked: String) {
    // with_completion places the cursor at the end, so the user can keep
    // typing a subdirectory or tab-complete immediately
    let field = TextField::with_completion(picked, CompletionSource::Filesystem);
    if let Some(form) = state.add_options.as_mut() {
        if (form.edit_buffer.is_some()) {
            form.edit_buffer = Some(field);
            return;
        }
    }
    if let Some(prompt) = state.prompt.as_mut() {
        if (matches!(prompt.action, PromptAction::Move)) {
            if let Some(line) = prompt.lines.get_mut(0) { *line = field; }
        }
    }
}

/// keys for the recent-paths picker. mirrors handle_interface_picker_key:
/// w/s/arrows navigate, home/end jump, esc/q closes, enter picks.
/// returns true when the tui should exit (ctrl+c).
fn handle_recent_paths_picker_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    let Some(picker) = state.recent_paths_picker.as_mut() else { return false; };
    match code {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Esc | KeyCode::Char('q') => state.recent_paths_picker = None,
        KeyCode::Char('s') | KeyCode::Down => {
            picker.selected = (picker.selected + 1).min(picker.items.len().saturating_sub(1));
        }
        KeyCode::Char('w') | KeyCode::Up => {
            picker.selected = picker.selected.saturating_sub(1);
        }
        KeyCode::Home => picker.selected = 0,
        KeyCode::End => picker.selected = picker.items.len().saturating_sub(1),
        KeyCode::Enter => {
            let Some(picked) = picker.items.get(picker.selected).cloned() else { return false; };
            state.recent_paths_picker = None;
            apply_picked_recent_path(state, picked);
        }
        _ => {}
    }
    false
}
```

- [ ] **Step 3: Route the picker and bind ctrl+r in `handle_prompt_key`**

`handle_prompt_key` (`src/tui.rs:2615`) currently opens with:

```rust
fn handle_prompt_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
```

Change that opening to:

```rust
fn handle_prompt_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    // recent-paths dropdown captures all input until dismissed, so picker
    // keys never leak into the underlying text field (same routing style
    // handle_settings_key uses for the interface picker)
    if (state.recent_paths_picker.is_some()) {
        return handle_recent_paths_picker_key(code, modifiers, state);
    }
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
            // gated on the move prompt: rename and every other prompt kind
            // ignores ctrl+r
            let is_move = state.prompt.as_ref()
                .map(|prompt| matches!(prompt.action, PromptAction::Move))
                .unwrap_or(false);
            if (is_move) { open_recent_paths_picker(state); }
        }
```

Everything from `(KeyCode::Esc, _) => state.prompt = None,` down is unchanged. Note the
input-routing ladder in the event loop (`src/tui.rs:1533`) checks `state.prompt.is_some()`
before `state.add_options.is_some()`, so when the picker is open over the move prompt this
top-of-handler check is the one that runs.

- [ ] **Step 4: Route the picker and bind ctrl+r in `handle_add_options_key`**

`handle_add_options_key` (`src/tui.rs:2449`) currently opens with:

```rust
fn handle_add_options_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    let Some(form) = state.add_options.as_mut() else { return false; };

    // text-edit mode for the save_path field
    if (form.edit_buffer.is_some()) {
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Esc, _) => form.edit_buffer = None,
```

Change that opening to:

```rust
fn handle_add_options_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    // recent-paths dropdown captures all input until dismissed
    if (state.recent_paths_picker.is_some()) {
        return handle_recent_paths_picker_key(code, modifiers, state);
    }
    let Some(form) = state.add_options.as_mut() else { return false; };

    // text-edit mode for the save_path field
    if (form.edit_buffer.is_some()) {
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            // ctrl+r only in edit mode: the picker fills the edit buffer, so
            // it only makes sense while the buffer exists
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => open_recent_paths_picker(state),
            (KeyCode::Esc, _) => form.edit_buffer = None,
```

The rest of the function is unchanged. (The `open_recent_paths_picker(state)` arm inside a
match whose other arms use `form` compiles fine under NLL: on that arm's path `form` is never
used again, exactly like the existing `activate_add_options_field(state)` arm at
`src/tui.rs:2516`.)

- [ ] **Step 5: Verify it compiles**

Run: `cargo build`
Expected: PASS with no warnings; every new function is referenced.

- [ ] **Step 6: Commit**

```bash
git add src/tui.rs
git commit -m "tui: recent-paths picker state, ctrl+r wiring, and key handling"
```

---

## Task 4: picker drawing and hint mentions

**Files:**
- Modify: `src/tui.rs`

- [ ] **Step 1: Add `draw_recent_paths_picker`**

Insert after the closing brace of `draw_prompt` (`src/tui.rs:4092`), cloning
`draw_interface_picker`'s centered-modal geometry (rounded yellow border, hint line,
highlighted selected row). Height is items + 3 (two border rows plus the hint row; the
interface picker's +4 includes a bottom hint this picker does not have):

```rust
/// centered modal listing config.recent_save_paths, most recent on top.
/// clones draw_interface_picker's geometry; rendered last in draw() so it
/// sits on top of the add-options form and the move prompt.
fn draw_recent_paths_picker(frame: &mut ratatui::Frame, state: &AppState) {
    let Some(picker) = state.recent_paths_picker.as_ref() else { return; };
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let height = ((picker.items.len() as u16) + 3).min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect { x, y, width, height };

    frame.render_widget(ratatui::widgets::Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" recent save paths ");
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
    ]).split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " w/s move  enter pick  esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        layout[0],
    );

    let lines: Vec<Line> = picker.items.iter().enumerate().map(|(index, path)| {
        let style = if (index == picker.selected) {
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        Line::from(Span::styled(format!(" {} ", path), style))
    }).collect();
    frame.render_widget(Paragraph::new(lines), layout[1]);
}
```

- [ ] **Step 2: Render it last in `draw`**

In `draw` (`src/tui.rs:3587`), the overlay chain currently ends with (lines 3624-3626):

```rust
    if (state.add_options.is_some()) {
        draw_add_options_form(frame, state);
    }
}
```

Append the picker after it, so it draws on top of both host surfaces:

```rust
    if (state.add_options.is_some()) {
        draw_add_options_form(frame, state);
    }
    if (state.recent_paths_picker.is_some()) {
        draw_recent_paths_picker(frame, state);
    }
}
```

(The `priority_step` early return at the top of `draw` is fine: the picker can only be open
while the move prompt or the add-options form is up, and neither coexists with the organize
step.)

- [ ] **Step 3: Mention ctrl+r in the two host hint lines**

**3a.** Add-options form helper. In `draw_add_options_form` (`src/tui.rs:3757`), the helper
line currently reads (lines 3791-3797):

```rust
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " w/s/tab move · enter confirm · esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        layout[1],
    );
```

Make it edit-mode aware:

```rust
    let helper = if (form.edit_buffer.is_some()) {
        " tab complete · ctrl+r recent · enter confirm · esc cancel"
    } else {
        " w/s/tab move · enter confirm · esc cancel"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            helper,
            Style::default().fg(Color::DarkGray),
        ))),
        layout[1],
    );
```

**3b.** Move prompt helper. In `open_move_prompt` (`src/tui.rs:1731`), change:

```rust
        helper: "absolute path. files will be moved on disk by libtorrent.".to_string(),
```

to:

```rust
        helper: "absolute path. files will be moved on disk by libtorrent. ctrl+r: recent paths.".to_string(),
```

- [ ] **Step 4: Build and manual smoke test**

Run: `cargo build`
Expected: PASS.

Then, with a running daemon (`cargo run -- daemon`, or the already-running one) and the TUI
(`cargo run -- tui`):
1. add a torrent with a typed save path (n, enter a magnet, set download path on the form),
   confirm the add succeeds.
2. select a torrent, press `m`, then `ctrl+r`: the picker opens with the typed path on top.
3. `w`/`s` move, `esc` closes without touching the prompt line.
4. `ctrl+r` again, `enter`: the prompt line is replaced with the picked path, cursor at the
   end; type `/sub` and tab-complete to confirm the field is live.
5. open the add form again, activate the download path field, `ctrl+r`, pick: the edit
   buffer is replaced the same way.
6. press `r` (rename prompt), `ctrl+r`: nothing happens.

- [ ] **Step 5: Commit**

```bash
git add src/tui.rs
git commit -m "tui: draw recent-paths picker and mention ctrl+r in host hints"
```

---

## Task 5: settings exposure of `recent_paths_limit`

**Files:**
- Modify: `src/tui.rs`

- [ ] **Step 1: Add the `SETTING_FIELDS` row**

In the "paths" section of `SETTING_FIELDS` (`src/tui.rs:625`), insert between the
`default_save_path` entry (ends line 634) and the `watch_directories` entry:

```rust
    SettingField {
        section: "paths",
        key: "recent_paths_limit",
        label: "recent save paths limit",
        description: "recent save paths to remember (0 disables). the list is shown by ctrl+r in the add form's download path editor and the move prompt.",
        kind: FieldKind::Integer,
        restart_required: false,
        is_list: false,
    },
```

- [ ] **Step 2: Add the `config_value_string` arm**

In `config_value_string` (`src/tui.rs:657`), after the `"default_save_path"` arm (line 665):

```rust
        "recent_paths_limit" => config.recent_paths_limit.to_string(),
```

Without this arm the settings row would render blank and edits would start from an empty
buffer. `recent_save_paths` deliberately gets no settings row and no value arm: it is
daemon-written only.

- [ ] **Step 3: Build and manual check**

Run: `cargo build`
Expected: PASS.

Then in the TUI: open settings (`,`), go to the paths tab, confirm the "recent save paths
limit" row shows `5`, edit it to `2`, and confirm `recent_save_paths` in config.toml is
truncated to two entries immediately (the daemon-side arm from Task 2 Step 4 does this).

- [ ] **Step 4: Commit**

```bash
git add src/tui.rs
git commit -m "settings: expose recent_paths_limit on the paths tab"
```

---

## Task 6: final verification pass

**Files:** none (verification only)

- [ ] **Step 1: Full build**

Run: `cargo build`
Expected: PASS, no warnings.

- [ ] **Step 2: Full test suite**

Run: `cargo test`
Expected: PASS, including the 9 `config::tests` cases from Task 1 and every pre-existing
test.

- [ ] **Step 3: Clippy**

Run: `cargo clippy --all-targets -- -D warnings` (match the project's actual clippy
invocation if CI uses a looser setting).
Expected: PASS.

- [ ] **Step 4: Full manual pass through the spec's testing checklist**

With a fresh daemon and TUI, run the spec's manual list end to end:
1. add a torrent with a typed save path; the path appears at the top of the picker on both
   surfaces (add form editor and move prompt).
2. move a torrent to a new directory; the destination is recorded at the front.
3. re-use an existing entry (type it again on an add); it moves to the front instead of
   duplicating, and `/data/tv/` vs `/data/tv` dedup to one entry.
4. add via a category (no explicit path) and via an RSS feed or watch directory; confirm
   `recent_save_paths` in config.toml is unchanged.
5. set the limit to 2 in settings; the on-disk list truncates immediately.
6. set the limit to 0; `ctrl+r` reports "recent paths disabled" and no overlay opens; a
   subsequent explicit-path add records nothing.
7. restore the limit to 5; `ctrl+r` with an empty list reports "no recent save paths".
8. `ctrl+r` in a rename prompt does nothing.
9. pick an entry, append a subdirectory, tab-complete it, submit the move; the daemon then
   records the final typed destination (move-to-front, subdirectory included).
10. two-phase move: move into a directory holding unrelated files, decline the merge
    (nothing recorded), repeat and accept (destination recorded on the commit re-send).

- [ ] **Step 5: Commit (only if Step 3 required fixes; otherwise nothing to commit)**

```bash
git add -A
git commit -m "fix clippy warnings from the recent-paths work"
```
