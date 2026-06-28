# Content Layout, Rename/Move Overhaul, and TUI Bug Fixes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the subfolder/content-layout feature (the `todo!()` at `src/tui.rs:2271`), overhaul folder/file rename + torrent move with plain-name input and merge/untracked-file handling, and fix two TUI bugs (click hit-test, greyed-text contrast).

**Architecture:** Pure path logic lives in a new dependency-light module `src/layout.rs`, unit-tested without libtorrent. The daemon (`src/server.rs`) applies content layout after a torrent is verified, and drives the rename/move merge/conflict analysis (it owns the filesystem, since the TUI can be remote). The TUI (`src/tui.rs`) gains the 4-way picker, plain-name rename input, and a two-phase confirmation flow. Shared types live in `src/ipc.rs`.

**Tech Stack:** Rust 2021, cxx bridge to libtorrent-rasterbar (C++), ratatui 0.29 + crossterm 0.28 TUI, serde/serde_json IPC over a Unix socket, toml config.

## Global Constraints

- **Code style (from CLAUDE.md):** lowercase casual comments; no abbreviated names (`request` not `req`); one-liners without braces use expression/arrow bodies; control-flow keeps a space before `(` and brace (`if (x) {`), functions do not (`fn name(args){`); when braces are required, always break lines. Match the surrounding file's existing idiom (this codebase already wraps `if`/`for` conditions in parens — follow that).
- **Commits:** lowercase, casual, succinct; **no attribution trailers**; never push.
- **No existing tests:** this introduces the first `#[cfg(test)]` modules. `cargo test` compiles the cxx bridge and links `torrent-rasterbar`, so libtorrent-rasterbar dev libraries must be installed (already required to build the project).
- **Build:** `cargo build`. **Test:** `cargo test`. **Lint:** `cargo clippy` if available.
- **IPC back-compat:** every new field on an existing `Request`/`Response`/record variant gets `#[serde(default)]` so old persisted data and in-flight messages still deserialize.
- **Implement phases in order A → B → C → D.** Within a phase, tasks are ordered by dependency.

---

## File Structure

- **Create `src/layout.rs`** — pure path logic, no libtorrent/filesystem: `resolve_rename_input`, `sanitize_path_component`, `compute_content_layout_renames`, `common_root`. Unit-tested.
- **Modify `src/ipc.rs`** — add `ContentLayout`, `RenameDecisions`, `UntrackedChoice`, `RenameConcern` enums/structs; extend `Request::Add`, `Request::RenameFolder`, `Request::Move`; add `Response::RenameConfirmation`.
- **Modify `src/config.rs`** — four new fields + `#[serde(default)]` defaults.
- **Modify `src/server.rs`** — config apply arms; `ManagedTorrent.pending_layout` + `TorrentRecord` field; thread layout through add; per-poll `apply_pending_layouts`; rewrite `rename_folder` and `move_storage` to the two-phase analyze/commit flow; concern-detection helpers.
- **Modify `src/tui.rs`** — `mod` use; replace `SubfolderMode` with `ContentLayout`; plain-name rename prompts; click scroll-offset fix; selection-highlight helper; confirmation-prompt sequencing.
- **Modify `src/main.rs`** — `mod layout;` declaration.

---

# Phase A — TUI bug fixes

### Task A1: Fix click hit-test ignoring vertical scroll offset

**Files:**
- Modify: `src/tui.rs` — `mouse_left_down`, the torrent-list branch (around `src/tui.rs:2665-2672`).

**Interfaces:**
- Consumes: `state.table_state: TableState` (ratatui 0.29 exposes `TableState::offset() -> usize`), `state.filtered_indices()`.
- Produces: corrected selection index. No new public signatures.

**Context:** Render geometry (`src/tui.rs:4046-4095`) is: border, then header row at `inner.y`, divider at `inner.y+1`, data at `inner.y+2`. `state.list_rect = inner` (`src/tui.rs:3828`), so `header_offset = 2` is correct. The bug is that `target` is used as an absolute index into `filtered_indices()` while ignoring the table's scroll offset, so clicks are wrong once the list scrolls.

- [ ] **Step 1: Reproduce.** `cargo run` (or attach to a running daemon), add enough torrents to overflow the list, scroll down with `j`/arrow until the viewport scrolls, then click a visible row. Confirm the selection lands on a different row than clicked. Note the behavior.

- [ ] **Step 2: Read the current branch.** Open `src/tui.rs` around line 2665:

```rust
    if (rect_contains(state.list_rect, column, row)) {
        // list has a 1-row border, then a 1-row header, then data rows
        let header_offset = 2;
        let row_in_data = row.saturating_sub(state.list_rect.y + header_offset);
        let visible = state.filtered_indices();
        let target = row_in_data as usize;
        if (target < visible.len()) {
            state.table_state.select(Some(target));
        }
```

- [ ] **Step 3: Add the scroll offset.** Replace the `let target = row_in_data as usize;` line with:

```rust
        // the table scrolls to keep the selection visible, so the first
        // on-screen data row corresponds to table_state.offset(), not 0
        let target = state.table_state.offset() + row_in_data as usize;
```

Leave `header_offset = 2` unchanged (it matches the header+divider geometry). Update the comment above `header_offset` to read `// border + column header + divider, then data rows`.

- [ ] **Step 4: Build.** Run: `cargo build`. Expected: compiles clean (`TableState::offset()` exists in ratatui 0.29).

- [ ] **Step 5: Verify manually.** Repeat Step 1's reproduction: scroll, click a row at top and bottom of the viewport, confirm the clicked row is selected. Also verify clicking with no scroll still selects correctly.

- [ ] **Step 6: Commit.**

```bash
git add src/tui.rs
git commit -m "fix: torrent list click ignored scroll offset"
```

### Task A2: Fix greyed text invisible on the selected row

**Files:**
- Modify: `src/tui.rs` — add a shared `selected_row_style()` helper; replace the `row_highlight_style` at lines `2933`, `3413`, `4120`, `4439`, `4489`, `4547`.

**Interfaces:**
- Produces: `fn selected_row_style() -> ratatui::style::Style` — used by every selectable table.

**Context:** Greyed rows use `fg(Color::DarkGray)` (e.g. priority-0 at `src/tui.rs:3393`, paused torrents at `src/tui.rs:4106`). Several tables highlight with `bg(Color::DarkGray)` and no `fg`, so the selected greyed row is DarkGray-on-DarkGray. Forcing an explicit `fg(Black).bg(Cyan)` (which the sidebar/feeds tables at `src/tui.rs:2933` already use) makes selection legible everywhere while signalling selection via the cyan background.

- [ ] **Step 1: Add the helper.** Near the other small style helpers in `src/tui.rs` (e.g. next to `focus_border_style`), add:

```rust
/// selection highlight used by every selectable table. forces an explicit
/// fg+bg so rows greyed out via fg(DarkGray) (paused torrents, skip-priority
/// files) stay legible when selected instead of going DarkGray-on-DarkGray.
fn selected_row_style() -> Style {
    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
}
```

- [ ] **Step 2: Replace the DarkGray highlights.** At each of `src/tui.rs:3413`, `4120`, `4439`, `4489`, `4547`, replace the argument:

```rust
        .row_highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
```

with:

```rust
        .row_highlight_style(selected_row_style())
```

- [ ] **Step 3: Replace the existing cyan highlights for consistency.** At `src/tui.rs:2933` (and any other `fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)` used as `row_highlight_style`), replace the inline style with `selected_row_style()`.

- [ ] **Step 4: Build.** Run: `cargo build`. Expected: compiles clean.

- [ ] **Step 5: Verify manually.** In the post-add file-priority view, set a file to priority 0 (`0` = skip) and move the selection onto it — the row text must stay readable. Pause a torrent in the main list and select it — readable. Repeat for trackers/peers tables.

- [ ] **Step 6: Commit.**

```bash
git add src/tui.rs
git commit -m "fix: greyed rows unreadable when selected; unify selection style"
```

---

# Phase B — Subfolder / content layout

### Task B1: Add the `ContentLayout` type to IPC

**Files:**
- Modify: `src/ipc.rs` — add enum + impls after the `Request` enum (around `src/ipc.rs:175`).
- Modify: `src/main.rs` — add `mod layout;` (needed by later tasks; declare now).
- Test: inline `#[cfg(test)]` in `src/ipc.rs`.

**Interfaces:**
- Produces:
  - `pub enum ContentLayout { Default, Always, Never, IfMultiple }` (derives `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`).
  - `impl Default for ContentLayout` → `Default`.
  - `pub fn ContentLayout::label(self) -> &'static str`
  - `pub fn ContentLayout::cycle(self) -> Self`
  - `pub fn ContentLayout::resolve(self, default_setting: &str) -> ContentLayout` — turns `Default` into a concrete variant from the config string (`"always"`/`"never"`/anything-else → `IfMultiple`); passes other variants through.

- [ ] **Step 1: Write the failing test.** Add to the bottom of `src/ipc.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_layout_cycle_is_four_way() {
        let order = [ContentLayout::Default, ContentLayout::Always, ContentLayout::Never, ContentLayout::IfMultiple];
        for (index, layout) in order.iter().enumerate() {
            assert_eq!(layout.cycle(), order[(index + 1) % order.len()]);
        }
    }

    #[test]
    fn content_layout_resolve_maps_default_to_setting() {
        assert_eq!(ContentLayout::Default.resolve("always"), ContentLayout::Always);
        assert_eq!(ContentLayout::Default.resolve("never"), ContentLayout::Never);
        assert_eq!(ContentLayout::Default.resolve("if_multiple"), ContentLayout::IfMultiple);
        assert_eq!(ContentLayout::Default.resolve("garbage"), ContentLayout::IfMultiple);
        // non-default passes through untouched
        assert_eq!(ContentLayout::Never.resolve("always"), ContentLayout::Never);
    }
}
```

- [ ] **Step 2: Run test to verify it fails.** Run: `cargo test --lib content_layout`. Expected: FAIL to compile — `ContentLayout` not found.

- [ ] **Step 3: Implement.** After the `Request` enum in `src/ipc.rs`, add:

```rust
/// whether to wrap a torrent's content in a folder named after the torrent.
/// `Default` resolves to the `default_content_layout` config setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentLayout {
    Default,
    Always,
    Never,
    IfMultiple,
}

impl Default for ContentLayout {
    fn default() -> Self { ContentLayout::Default }
}

impl ContentLayout {
    pub fn label(self) -> &'static str {
        match self {
            ContentLayout::Default => "default",
            ContentLayout::Always => "always",
            ContentLayout::Never => "never",
            ContentLayout::IfMultiple => "if multiple files",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            ContentLayout::Default => ContentLayout::Always,
            ContentLayout::Always => ContentLayout::Never,
            ContentLayout::Never => ContentLayout::IfMultiple,
            ContentLayout::IfMultiple => ContentLayout::Default,
        }
    }

    /// turn `Default` into a concrete layout using the config string;
    /// any unrecognised setting falls back to the natural `IfMultiple`.
    pub fn resolve(self, default_setting: &str) -> ContentLayout {
        match self {
            ContentLayout::Default => match default_setting {
                "always" => ContentLayout::Always,
                "never" => ContentLayout::Never,
                _ => ContentLayout::IfMultiple,
            },
            other => other,
        }
    }
}
```

- [ ] **Step 4: Declare the layout module.** In `src/main.rs`, alongside the existing `mod` declarations, add:

```rust
mod layout;
```

Create an empty `src/layout.rs` with a header comment so the build succeeds:

```rust
//! pure path logic shared by the tui (rename input resolution) and the daemon
//! (content layout + folder-rename planning). no libtorrent or filesystem
//! access — just string work over `/`-separated torrent paths.
```

- [ ] **Step 5: Run test to verify it passes.** Run: `cargo test --lib content_layout`. Expected: PASS (2 tests).

- [ ] **Step 6: Commit.**

```bash
git add src/ipc.rs src/main.rs src/layout.rs
git commit -m "ipc: add ContentLayout enum + empty layout module"
```

### Task B2: Pure content-layout rewrite + name sanitization in `layout.rs`

**Files:**
- Modify: `src/layout.rs`.
- Test: inline `#[cfg(test)]` in `src/layout.rs`.

**Interfaces:**
- Produces:
  - `pub fn sanitize_path_component(name: &str) -> Option<String>`
  - `pub fn common_root(files: &[String]) -> Option<String>`
  - `pub fn compute_content_layout_renames(files: &[String], name: &str, resolved: ContentLayout) -> Vec<(usize, String)>` — returns `(file_index, new_path)` only for files whose path changes. Caller must pass a resolved (non-`Default`) layout.

- [ ] **Step 1: Write the failing tests.** Add to `src/layout.rs`:

```rust
use crate::ipc::ContentLayout;

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> { list.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn never_strips_root_on_multi_file() {
        let files = paths(&["Show/ep1.mkv", "Show/sub/ep2.mkv"]);
        let out = compute_content_layout_renames(&files, "Show", ContentLayout::Never);
        assert_eq!(out, vec![(0, "ep1.mkv".to_string()), (1, "sub/ep2.mkv".to_string())]);
    }

    #[test]
    fn never_is_noop_on_single_file() {
        let files = paths(&["movie.mkv"]);
        assert!(compute_content_layout_renames(&files, "movie.mkv", ContentLayout::Never).is_empty());
    }

    #[test]
    fn always_wraps_single_file_using_torrent_name() {
        // name differs from filename (e.g. a renamed torrent)
        let files = paths(&["movie.mkv"]);
        let out = compute_content_layout_renames(&files, "My Movie", ContentLayout::Always);
        assert_eq!(out, vec![(0, "My Movie/movie.mkv".to_string())]);
    }

    #[test]
    fn always_is_noop_on_multi_file() {
        let files = paths(&["Show/ep1.mkv", "Show/ep2.mkv"]);
        assert!(compute_content_layout_renames(&files, "Show", ContentLayout::Always).is_empty());
    }

    #[test]
    fn if_multiple_is_always_noop() {
        assert!(compute_content_layout_renames(&paths(&["a.mkv"]), "a.mkv", ContentLayout::IfMultiple).is_empty());
        assert!(compute_content_layout_renames(&paths(&["X/a.mkv", "X/b.mkv"]), "X", ContentLayout::IfMultiple).is_empty());
    }

    #[test]
    fn sanitize_strips_separators_and_dots() {
        assert_eq!(sanitize_path_component("a/b"), Some("a_b".to_string()));
        assert_eq!(sanitize_path_component("  ..  "), None);
        assert_eq!(sanitize_path_component("Normal Name"), Some("Normal Name".to_string()));
        assert_eq!(sanitize_path_component(""), None);
    }
}
```

- [ ] **Step 2: Run to verify it fails.** Run: `cargo test --lib layout::`. Expected: FAIL to compile (functions missing).

- [ ] **Step 3: Implement.** Add to `src/layout.rs` (above the `#[cfg(test)]` block):

```rust
/// sanitize a torrent display name into one safe on-disk path component:
/// replace separators/nulls with `_`, trim surrounding dots and whitespace.
/// returns None when nothing usable remains.
pub fn sanitize_path_component(name: &str) -> Option<String> {
    let replaced: String = name.chars()
        .map(|character| if (character == '/' || character == '\\' || character == '\0') { '_' } else { character })
        .collect();
    let trimmed = replaced.trim().trim_matches('.').trim();
    if (trimmed.is_empty() || trimmed == "..") {
        return None;
    }
    Some(trimmed.to_string())
}

/// the single path component shared as the first segment by every file
/// (the torrent's root folder for a multi-file torrent). None when files
/// don't all share a top folder (including single-file torrents).
pub fn common_root(files: &[String]) -> Option<String> {
    let first = files.first()?;
    if (!first.contains('/')) { return None; }
    let root = first.split('/').next()?;
    if (root.is_empty()) { return None; }
    let all_share = files.iter().all(|path| path.contains('/') && path.split('/').next() == Some(root));
    if (all_share) { Some(root.to_string()) } else { None }
}

/// compute file renames to put `files` into `resolved` layout. `name` is the
/// torrent's effective display name. only changed files are returned, as
/// (file_index, new_path). pass a resolved (non-Default) layout.
pub fn compute_content_layout_renames(
    files: &[String],
    name: &str,
    resolved: ContentLayout,
) -> Vec<(usize, String)> {
    let multi = files.len() > 1;
    // the natural layout already satisfies these — nothing to do
    match resolved {
        ContentLayout::Default | ContentLayout::IfMultiple => return Vec::new(),
        ContentLayout::Always if multi => return Vec::new(),
        ContentLayout::Never if !multi => return Vec::new(),
        _ => {}
    }
    let mut renames: Vec<(usize, String)> = Vec::new();
    match resolved {
        ContentLayout::Never => {
            if let Some(root) = common_root(files) {
                let prefix = format!("{}/", root);
                for (index, path) in files.iter().enumerate() {
                    if let Some(rest) = path.strip_prefix(&prefix) {
                        renames.push((index, rest.to_string()));
                    }
                }
            }
        }
        ContentLayout::Always => {
            // only reached for a single-file torrent
            if let Some(folder) = sanitize_path_component(name) {
                let path = &files[0];
                let filename = path.rsplit('/').next().unwrap_or(path);
                renames.push((0, format!("{}/{}", folder, filename)));
            }
        }
        _ => {}
    }
    renames
}
```

- [ ] **Step 4: Run to verify it passes.** Run: `cargo test --lib layout::`. Expected: PASS (6 tests).

- [ ] **Step 5: Commit.**

```bash
git add src/layout.rs
git commit -m "layout: content-layout rewrite + name sanitization (pure)"
```

### Task B3: Add the `default_content_layout` config field

**Files:**
- Modify: `src/config.rs` — struct field (around `src/config.rs:25`), `Default` impl (around `src/config.rs:181`).
- Modify: `src/server.rs` — `apply_config_change` match (around `src/server.rs:331`).

**Interfaces:**
- Produces: `Config.default_content_layout: String` (default `"if_multiple"`), settable via `SetConfig { key: "default_content_layout" }`.

- [ ] **Step 1: Add the struct field.** In `src/config.rs`, near `default_save_path` (line 25), add with a serde default so existing config files still load:

```rust
    /// default content layout for new torrents: always | never | if_multiple
    #[serde(default = "default_content_layout")]
    pub default_content_layout: String,
```

Add the default function near the other `default_*` helpers in `src/config.rs`:

```rust
fn default_content_layout() -> String { "if_multiple".to_string() }
```

- [ ] **Step 2: Add to the `Default` impl.** In `src/config.rs` `impl Default for Config` (around line 185), add:

```rust
            default_content_layout: default_content_layout(),
```

- [ ] **Step 3: Add the config-apply arm.** In `src/server.rs` `apply_config_change` (after the `"default_save_path"` arm at line 338), add:

```rust
            "default_content_layout" => {
                if (!matches!(value, "always" | "never" | "if_multiple")) {
                    return Err(anyhow::anyhow!("default_content_layout must be: always | never | if_multiple"));
                }
                self.config.default_content_layout = value.to_string();
            }
```

- [ ] **Step 4: Build.** Run: `cargo build`. Expected: compiles clean.

- [ ] **Step 5: Verify the round-trip.** Run: `cargo run -- daemon` in one shell; in another, `cargo run -- config set default_content_layout never` then `cargo run -- config get default_content_layout` (or use the TUI settings overlay). Expected: value persists as `never`; an invalid value is rejected with the error message.

- [ ] **Step 6: Commit.**

```bash
git add src/config.rs src/server.rs
git commit -m "config: add default_content_layout setting"
```

### Task B4: Thread `content_layout` through Add into the torrent record

**Files:**
- Modify: `src/ipc.rs` — `Request::Add` (line 103).
- Modify: `src/server.rs` — `ManagedTorrent` (line 21), `TorrentRecord` (line 39), `add_magnet`/`add_file` signatures (lines 212/246) and bodies, the `Request::Add` handler dispatch, and the RSS/watch callers (lines 514/517).

**Interfaces:**
- Consumes: `ContentLayout` (Task B1), `Config.default_content_layout` (Task B3).
- Produces: `ManagedTorrent.pending_layout: Option<ContentLayout>` set to the resolved layout (None when `IfMultiple`/no-op), consumed by Task B5.

- [ ] **Step 1: Extend `Request::Add`.** In `src/ipc.rs:103`, change to:

```rust
    Add { uri: String, save_path: Option<String>, category: Option<String>, start_paused: bool, #[serde(default)] content_layout: ContentLayout },
```

- [ ] **Step 2: Add fields to the records.** In `src/server.rs`, add to `ManagedTorrent` (after `was_finished`, line 35):

```rust
    /// resolved content layout still to apply once the torrent is verified.
    /// None = nothing pending (already laid out, or IfMultiple no-op).
    pending_layout: Option<crate::ipc::ContentLayout>,
```

Add to `TorrentRecord` (after `display_name`, line 52), with a serde default so old `torrents.json` loads:

```rust
    #[serde(default)]
    pending_layout: Option<crate::ipc::ContentLayout>,
```

- [ ] **Step 3: Plumb through `add_magnet`/`add_file`.** Add a parameter `content_layout: crate::ipc::ContentLayout` to both `add_magnet` (line 212) and `add_file` (line 246). In each body, where the `ManagedTorrent` is constructed, resolve and store it:

```rust
        let resolved = content_layout.resolve(&self.config.default_content_layout);
        let pending_layout = if (matches!(resolved, crate::ipc::ContentLayout::IfMultiple)) { None } else { Some(resolved) };
```

Set `pending_layout` in the `ManagedTorrent { … }` literal, and set `pending_layout` in the `TorrentRecord` written by `persist_torrent_list` (find where records are built — search `TorrentRecord {` in `src/server.rs` — and add `pending_layout: torrent.pending_layout,`). When reloading records on startup (search where `ManagedTorrent` is built from a `TorrentRecord`, around `src/server.rs:153`), set `pending_layout: record.pending_layout`.

- [ ] **Step 4: Update the `Request::Add` handler.** In the `Request::Add { … }` arm of `src/server.rs`, destructure the new field and pass it down:

```rust
            Request::Add { uri, save_path, category, start_paused, content_layout } => {
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

- [ ] **Step 5: Update internal callers (RSS/watch) to default.** At `src/server.rs:514` and `:517` (the RSS path) and any watch-directory add, pass `crate::ipc::ContentLayout::Default` as the new argument so they honor the preference. Example:

```rust
                        self.add_magnet(&magnet, feed.save_path.as_deref(), feed.category.as_deref(), feed.start_paused, crate::ipc::ContentLayout::Default)
```

- [ ] **Step 6: Build.** Run: `cargo build`. Expected: compiles clean once every `add_magnet`/`add_file` call site passes the new argument (the compiler will list any you missed).

- [ ] **Step 7: Commit.**

```bash
git add src/ipc.rs src/server.rs
git commit -m "server: carry resolved content layout on the torrent record"
```

### Task B5: Apply pending layout after verification (per-poll scan)

**Files:**
- Modify: `src/server.rs` — add `apply_pending_layouts`; call it from the main poll loop next to `process_alerts`.

**Interfaces:**
- Consumes: `ManagedTorrent.pending_layout` (Task B4), `compute_content_layout_renames` (Task B2), `handle.files()` (returns `ffi::TorrentFile` with `.path: String`), `handle.status()` (`.state: String`, `.has_metadata: bool`), `handle.rename_file(i32, &str)`.

**Context:** The daemon already iterates torrents each poll (e.g. the completion-script scan at `src/server.rs:429`). Apply the layout when the torrent is past metadata + checking. States seen in `state_to_string` (`src/bridge.cpp:265`): `downloading_metadata`, `checking_files`, `checking_resume_data`, `downloading`, `finished`, `seeding`. "Verified" = has metadata and not in a metadata/checking state. Clear `pending_layout` on apply (a latch) so the renames it triggers don't re-fire it.

- [ ] **Step 1: Implement the apply pass.** Add this method to `impl App` in `src/server.rs`:

```rust
    /// apply any pending content layout once a torrent is verified (metadata
    /// present and past the initial check). issues rename_file calls and clears
    /// the latch so the resulting re-check doesn't re-trigger.
    fn apply_pending_layouts(&mut self) {
        let mut changed = false;
        for torrent in self.torrents.iter_mut() {
            let Some(layout) = torrent.pending_layout else { continue; };
            let status = torrent.handle.status();
            if (!status.has_metadata) { continue; }
            if (matches!(status.state.as_str(), "downloading_metadata" | "checking_files" | "checking_resume_data")) {
                continue;
            }
            let files: Vec<String> = torrent.handle.files().iter().map(|file| file.path.clone()).collect();
            if (files.is_empty()) { continue; }
            let name = torrent.display_name.clone().unwrap_or(status.name);
            let renames = crate::layout::compute_content_layout_renames(&files, &name, layout);
            for (file_index, new_path) in &renames {
                torrent.handle.rename_file(*file_index as i32, new_path);
                tracing::info!(torrent = %torrent.info_hash, file_index, new_path, "content layout rename");
            }
            torrent.pending_layout = None;
            changed = true;
        }
        if (changed) { self.persist_torrent_list(); }
    }
```

- [ ] **Step 2: Call it from the poll loop.** Find the daemon's per-tick maintenance (where `process_alerts`/`poll_rss_feeds`/the completion scan are called — search the main `run`/poll loop in `src/server.rs`). Add a call once per tick:

```rust
        self.apply_pending_layouts();
```

- [ ] **Step 3: Build.** Run: `cargo build`. Expected: compiles clean. (Confirm `ffi::TorrentFile` has a public `path` field by checking the existing usage at `src/server.rs:669`.)

- [ ] **Step 4: Verify manually (single-file Always).** Start the daemon. From the TUI, add a single-file `.torrent` with layout `always` (Task B6 wires the picker — until then, temporarily test by setting `default_content_layout=always` and adding with `Default`). After the recheck completes, inspect the file list in the detail pane: the file should now sit under `<torrent name>/`. For a magnet, confirm the rename happens shortly after metadata + check.

- [ ] **Step 5: Verify manually (Never on multi-file).** Add a multi-file torrent with `never`; confirm files end up directly under the save path (root folder stripped).

- [ ] **Step 6: Commit.**

```bash
git add src/server.rs
git commit -m "server: apply content layout after torrent verification"
```

### Task B6: Replace `SubfolderMode` with the 4-way picker and dispatch the layout

**Files:**
- Modify: `src/tui.rs` — remove `enum SubfolderMode` (lines 997-1015); `AddOptions.subfolder` → `content_layout` (line 981, 991); activation (line 2213); dispatch (lines 2241-2271, delete the `todo!()`); summary label (line 3526).

**Interfaces:**
- Consumes: `ContentLayout` from `crate::ipc` (label/cycle), `Request::Add { …, content_layout }` (Task B4).

- [ ] **Step 1: Import and swap the field.** At the top of `src/tui.rs`, ensure `ContentLayout` is in scope (the file already imports from `crate::ipc`; add `ContentLayout` to that use). Change `AddOptions.subfolder: SubfolderMode` (line 981) to:

```rust
    content_layout: ContentLayout,
```

and its `Default` (line 991) to:

```rust
            content_layout: ContentLayout::default(),
```

- [ ] **Step 2: Delete `SubfolderMode`.** Remove the whole `enum SubfolderMode { … }` and its `impl` block (`src/tui.rs:997-1015`).

- [ ] **Step 3: Update the field activation.** At `src/tui.rs:2213`, change:

```rust
        3 => options.content_layout = options.content_layout.cycle(),
```

- [ ] **Step 4: Send the layout and delete the `todo!()`.** In `dispatch_add_options` (`src/tui.rs:2241`), add `content_layout` to the `Request::Add { … }` literal:

```rust
        let added_id = match client::send(Request::Add {
            uri: uri.clone(),
            save_path,
            category: None,
            start_paused: !options.start,
            content_layout: options.content_layout,
        }) {
```

Delete the line `if (!matches!(options.subfolder, SubfolderMode::Default)) { todo!("subfolder mode") }` (`src/tui.rs:2271`).

- [ ] **Step 5: Update the summary label.** At `src/tui.rs:3526`, change:

```rust
        ("create subfolder", options.content_layout.label().to_string()),
```

- [ ] **Step 6: Build.** Run: `cargo build`. Expected: compiles clean; the `todo!()` is gone.

- [ ] **Step 7: Verify manually.** Add a torrent via the TUI; in the options form, the "create subfolder" field cycles `default → always → never → if multiple files → default`. Pick `always` for a single-file torrent and confirm (after verification) the file is wrapped in `<name>/`.

- [ ] **Step 8: Commit.**

```bash
git add src/tui.rs
git commit -m "tui: 4-way content layout picker; remove subfolder todo"
```

---

# Phase C — Rename / move overhaul

### Task C1: Pure rename-input resolution in `layout.rs`

**Files:**
- Modify: `src/layout.rs` (+ tests).

**Interfaces:**
- Produces: `pub fn resolve_rename_input(parent: &str, input: &str) -> Result<String, String>` — resolves a typed name relative to `parent`, supports `../`, rejects escaping the root, returns the root-relative path.

- [ ] **Step 1: Write failing tests.** Add to the `tests` module in `src/layout.rs`:

```rust
    #[test]
    fn resolve_keeps_sibling_in_same_parent() {
        assert_eq!(resolve_rename_input("Show", "Season 2"), Ok("Show/Season 2".to_string()));
        assert_eq!(resolve_rename_input("", "Renamed"), Ok("Renamed".to_string()));
    }

    #[test]
    fn resolve_ascends_with_dotdot() {
        assert_eq!(resolve_rename_input("Show/Season 1", "../Extras"), Ok("Show/Extras".to_string()));
        assert_eq!(resolve_rename_input("Show", "../Top"), Ok("Top".to_string()));
    }

    #[test]
    fn resolve_rejects_escaping_root() {
        assert!(resolve_rename_input("Show", "../../Escape").is_err());
        assert!(resolve_rename_input("", "../x").is_err());
    }

    #[test]
    fn resolve_rejects_empty_or_root() {
        assert!(resolve_rename_input("Show", "  ").is_err());
        assert!(resolve_rename_input("Show", "..").is_err());
    }
```

- [ ] **Step 2: Run to verify it fails.** Run: `cargo test --lib layout::`. Expected: FAIL to compile (`resolve_rename_input` missing).

- [ ] **Step 3: Implement.** Add to `src/layout.rs`:

```rust
/// resolve a user-typed rename against the parent directory of the item being
/// renamed. `parent` is the root-relative parent path ("" at the torrent
/// root). supports `.` and `..`; rejects ascending above the root or resolving
/// to the root itself. returns the new root-relative path.
pub fn resolve_rename_input(parent: &str, input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if (trimmed.is_empty()) {
        return Err("name cannot be empty".to_string());
    }
    if (trimmed.contains('\0')) {
        return Err("name cannot contain null bytes".to_string());
    }
    let mut segments: Vec<&str> = Vec::new();
    for raw in parent.split('/').chain(trimmed.split('/')) {
        match raw {
            "" | "." => {}
            ".." => {
                if (segments.pop().is_none()) {
                    return Err("cannot ascend above the torrent root".to_string());
                }
            }
            other => segments.push(other),
        }
    }
    if (segments.is_empty()) {
        return Err("name cannot resolve to the torrent root".to_string());
    }
    Ok(segments.join("/"))
}
```

- [ ] **Step 4: Run to verify it passes.** Run: `cargo test --lib layout::`. Expected: PASS (all layout tests, including the 4 new ones).

- [ ] **Step 5: Commit.**

```bash
git add src/layout.rs
git commit -m "layout: resolve_rename_input with ../ ascent + root-escape guard"
```

### Task C2: Plain-name rename prompts in the TUI

**Files:**
- Modify: `src/tui.rs` — `open_content_rename_prompt` (lines 1716-1756); the rename submit path that builds `Request::RenameFolder`/`RenameFile` (around `src/tui.rs:2011-2027`).

**Interfaces:**
- Consumes: `resolve_rename_input` (Task C1). The `PromptAction::RenameFolder { old_prefix }` already carries the full old path; add the parent to the action or recompute it at submit.

**Context:** Today the folder prompt seeds `row.full_path` and the file prompt seeds `file.path` (full paths). We seed the basename and resolve against the parent on submit. `PromptAction::RenameFile { file_index }` looks up the file path from the live detail; `RenameFolder { old_prefix }` carries the old path.

- [ ] **Step 1: Seed basenames.** In `open_content_rename_prompt` (`src/tui.rs:1735-1755`), change the seeded `lines` to the basename. For the folder branch:

```rust
        let basename = row.full_path.rsplit('/').next().unwrap_or(&row.full_path).to_string();
        state.prompt = Some(Prompt {
            title: format!("rename folder \"{}\"", row.full_path),
            helper: "new name (relative to this folder's parent). use ../ to move up; cannot leave the torrent root. merging into an existing folder warns; file collisions are rejected.".to_string(),
            lines: vec![basename],
            cursor_line: 0,
            action: PromptAction::RenameFolder { old_prefix: row.full_path.clone() },
            torrent_index,
            allow_multiline: false,
        });
```

For the file branch, seed the file's basename:

```rust
        let basename = file.path.rsplit('/').next().unwrap_or(&file.path).to_string();
        state.prompt = Some(Prompt {
            title: format!("rename file \"{}\"", row.label),
            helper: "new name (relative to this file's folder). use ../ to move up; cannot leave the torrent root. collisions with existing files are rejected.".to_string(),
            lines: vec![basename],
            cursor_line: 0,
            action: PromptAction::RenameFile { file_index },
            torrent_index,
            allow_multiline: false,
        });
```

- [ ] **Step 2: Resolve on submit (folder).** Find the prompt-submit handler that builds `Request::RenameFolder` (around `src/tui.rs:2022`). Compute the parent from `old_prefix`, resolve the typed input, and send the resolved `new_prefix`:

```rust
        PromptAction::RenameFolder { old_prefix } => {
            let parent = old_prefix.rsplit_once('/').map(|(head, _)| head).unwrap_or("");
            let new_prefix = match crate::layout::resolve_rename_input(parent, &buffer) {
                Ok(path) => path,
                Err(error) => { state.error = Some(error); return; }
            };
            match client::send(Request::RenameFolder { index: torrent_index, old_prefix: old_prefix.clone(), new_prefix, decisions: None }) {
                // ... existing response handling (RenameResult), plus the new
                //     RenameConfirmation arm added in Task C8
            }
        }
```

(`decisions: None` is added in Task C4; if C4 isn't merged yet, this line won't compile — implement C3/C4 before finishing C2's submit wiring, or stub the field. Recommended order: C3 → C4 → finish C2/C8 together. See note below.)

- [ ] **Step 3: Resolve on submit (file).** For `PromptAction::RenameFile { file_index }`, look up the current path from `state.detail`, compute its parent, resolve, and send:

```rust
        PromptAction::RenameFile { file_index } => {
            let current = state.detail.as_ref().and_then(|detail| detail.files.get(file_index)).map(|file| file.path.clone());
            let Some(current) = current else { state.error = Some("file list not loaded".to_string()); return; };
            let parent = current.rsplit_once('/').map(|(head, _)| head).unwrap_or("");
            let new_name = match crate::layout::resolve_rename_input(parent, &buffer) {
                Ok(path) => path,
                Err(error) => { state.error = Some(error); return; }
            };
            match client::send(Request::RenameFile { index: torrent_index, file_index, new_name }) {
                // ... existing response handling
            }
        }
```

> **Sequencing note:** Step 2's `decisions: None` depends on Task C4. Implement **C3 and C4 first**, then complete C2's submit wiring together with C8. The basename-seeding (Step 1) and file-rename resolution (Step 3) compile independently and can be committed now.

- [ ] **Step 4: Build the independent parts.** Run: `cargo build`. Expected: Step 1 + Step 3 compile (RenameFile is unchanged in IPC). Defer Step 2's `decisions` field until C4.

- [ ] **Step 5: Verify manually (file rename).** Rename a file inside a torrent using just its name; confirm it stays in the same folder. Type `../newname` and confirm it moves up one level; confirm `../../x` from a shallow path is rejected with a clear message.

- [ ] **Step 6: Commit.**

```bash
git add src/tui.rs
git commit -m "tui: rename prompts use plain names + ../ resolution"
```

### Task C3: Add the three rename preferences

**Files:**
- Modify: `src/config.rs` — fields + defaults; `src/server.rs` — apply arms.

**Interfaces:**
- Produces: `Config.rename_merge_same: String` (`always`|`ask`, default `ask`), `Config.rename_merge_unrelated: String` (`always`|`ask`, default `ask`), `Config.rename_untracked_files: String` (`always_move`|`always_leave`|`ask`, default `ask`).

- [ ] **Step 1: Add struct fields.** In `src/config.rs` (near `default_content_layout`), add:

```rust
    /// confirm merging a rename into a folder already holding this torrent's files
    #[serde(default = "default_ask")]
    pub rename_merge_same: String,
    /// confirm merging into an on-disk folder that also holds unrelated files
    #[serde(default = "default_ask")]
    pub rename_merge_unrelated: String,
    /// what to do with untracked files inside a renamed folder
    #[serde(default = "default_ask")]
    pub rename_untracked_files: String,
```

Add helpers near the other defaults:

```rust
fn default_ask() -> String { "ask".to_string() }
```

- [ ] **Step 2: Add to `Default` impl.** In `impl Default for Config`:

```rust
            rename_merge_same: default_ask(),
            rename_merge_unrelated: default_ask(),
            rename_untracked_files: default_ask(),
```

- [ ] **Step 3: Add config-apply arms.** In `src/server.rs` `apply_config_change`:

```rust
            "rename_merge_same" => {
                if (!matches!(value, "always" | "ask")) {
                    return Err(anyhow::anyhow!("rename_merge_same must be: always | ask"));
                }
                self.config.rename_merge_same = value.to_string();
            }
            "rename_merge_unrelated" => {
                if (!matches!(value, "always" | "ask")) {
                    return Err(anyhow::anyhow!("rename_merge_unrelated must be: always | ask"));
                }
                self.config.rename_merge_unrelated = value.to_string();
            }
            "rename_untracked_files" => {
                if (!matches!(value, "always_move" | "always_leave" | "ask")) {
                    return Err(anyhow::anyhow!("rename_untracked_files must be: always_move | always_leave | ask"));
                }
                self.config.rename_untracked_files = value.to_string();
            }
```

- [ ] **Step 4: Build + verify.** Run: `cargo build`. Then round-trip one via `config set rename_merge_same always` and confirm it persists and that an invalid value is rejected.

- [ ] **Step 5: Commit.**

```bash
git add src/config.rs src/server.rs
git commit -m "config: add three rename merge/untracked preferences"
```

### Task C4: IPC types for the two-phase rename/move flow

**Files:**
- Modify: `src/ipc.rs` — new types; extend `Request::RenameFolder` and `Request::Move`; add `Response::RenameConfirmation`.

**Interfaces:**
- Produces:
  - `pub enum UntrackedChoice { Move, Leave }` (default `Leave`).
  - `pub struct RenameDecisions { merge_same: bool, merge_unrelated: bool, untracked: UntrackedChoice }` (all `pub`).
  - `pub enum RenameConcern { MergeSame, MergeUnrelated { unrelated_count: usize }, UntrackedFiles { count: usize } }`.
  - `Request::RenameFolder { …, decisions: Option<RenameDecisions> }`, `Request::Move { …, decisions: Option<RenameDecisions> }`.
  - `Response::RenameConfirmation { concerns: Vec<RenameConcern> }`.

- [ ] **Step 1: Add the shared types.** In `src/ipc.rs` (near `ContentLayout`), add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UntrackedChoice { Move, Leave }

impl Default for UntrackedChoice {
    fn default() -> Self { UntrackedChoice::Leave }
}

/// the user's resolved answers for a rename/move that needed confirmation.
/// the bools mean "the user approved this merge".
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RenameDecisions {
    pub merge_same: bool,
    pub merge_unrelated: bool,
    pub untracked: UntrackedChoice,
}

/// a single thing the daemon wants the user to confirm before a rename/move.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RenameConcern {
    MergeSame,
    MergeUnrelated { unrelated_count: usize },
    UntrackedFiles { count: usize },
}
```

- [ ] **Step 2: Extend the requests.** Change `Request::RenameFolder` (line 118) and `Request::Move` (line 121):

```rust
    RenameFolder { index: usize, old_prefix: String, new_prefix: String, #[serde(default)] decisions: Option<RenameDecisions> },
    Move { index: usize, new_save_path: String, #[serde(default)] decisions: Option<RenameDecisions> },
```

- [ ] **Step 3: Add the response.** In the `Response` enum (after `RenameResult`, line 207):

```rust
    /// the daemon needs the user to confirm one or more merge/untracked concerns
    /// before it will commit the rename or move. resend the original request
    /// with `decisions` filled in.
    RenameConfirmation { concerns: Vec<RenameConcern> },
```

- [ ] **Step 4: Build.** Run: `cargo build`. Expected: compile errors at the `RenameFolder`/`Move` construction sites in `src/tui.rs` and the handler match in `src/server.rs` — these are fixed in Tasks C6/C7/C8. To keep this commit green, update the **server handler** match arms now to destructure the new field and ignore it temporarily (`decisions: _`), and add `decisions: None` to the TUI `Move`/`RenameFolder` call sites. Then `cargo build` should pass.

- [ ] **Step 5: Commit.**

```bash
git add src/ipc.rs src/tui.rs src/server.rs
git commit -m "ipc: two-phase rename/move decision + confirmation types"
```

### Task C5: Server-side concern-detection helpers

**Files:**
- Modify: `src/server.rs` — free functions + temp-dir tests.

**Interfaces:**
- Produces:
  - `fn folder_merge_same(static_files: &[String], new_prefix: &str) -> bool` — true if any non-renamed torrent file already lives under `new_prefix`.
  - `fn scan_unrelated_in_dir(dir: &std::path::Path, tracked: &std::collections::HashSet<String>) -> Vec<std::path::PathBuf>` — files physically present under `dir` whose torrent-relative path isn't in `tracked`. (`tracked` holds root-relative torrent paths; `dir` is the on-disk directory; the caller passes the `save_path` so relative paths can be derived.)
  - For untracked-in-source: reuse `scan_unrelated_in_dir` over the old folder's on-disk path.

- [ ] **Step 1: Write failing tests.** Add to `src/server.rs` (bottom, in a `#[cfg(test)] mod rename_tests`):

```rust
#[cfg(test)]
mod rename_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn merge_same_detects_existing_torrent_file_under_prefix() {
        let static_files = vec!["Dest/already.txt".to_string(), "Other/x.txt".to_string()];
        assert!(folder_merge_same(&static_files, "Dest"));
        assert!(!folder_merge_same(&static_files, "Fresh"));
    }

    #[test]
    fn scan_unrelated_lists_only_untracked_files() {
        let dir = std::env::temp_dir().join(format!("monsoon_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("tracked.txt"), b"x").unwrap();
        std::fs::write(dir.join("stray.txt"), b"y").unwrap();
        std::fs::write(dir.join("sub/stray2.txt"), b"z").unwrap();

        let mut tracked = HashSet::new();
        tracked.insert("tracked.txt".to_string());

        let mut found: Vec<String> = scan_unrelated_in_dir(&dir, &tracked, &dir)
            .into_iter()
            .map(|path| path.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        found.sort();
        assert_eq!(found, vec!["stray.txt".to_string(), "sub/stray2.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Run to verify it fails.** Run: `cargo test rename_tests`. Expected: FAIL to compile (functions missing).

- [ ] **Step 3: Implement.** Add to `src/server.rs` (free functions near the other rename helpers):

```rust
/// true if any file that isn't part of this rename already lives under
/// `new_prefix` — i.e. the rename merges into an existing (same-torrent) folder.
fn folder_merge_same(static_files: &[String], new_prefix: &str) -> bool {
    let dir_prefix = format!("{}/", new_prefix);
    static_files.iter().any(|path| path.starts_with(&dir_prefix))
}

/// physically-present files under `dir` whose path relative to `save_root`
/// isn't in `tracked` (the torrent's file paths). recurses. `save_root` is the
/// torrent save path so on-disk paths map back to torrent-relative paths.
fn scan_unrelated_in_dir(
    dir: &std::path::Path,
    tracked: &std::collections::HashSet<String>,
    save_root: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out; };
    for entry in entries.flatten() {
        let path = entry.path();
        if (path.is_dir()) {
            out.extend(scan_unrelated_in_dir(&path, tracked, save_root));
        } else if let Ok(relative) = path.strip_prefix(save_root) {
            let key = relative.to_string_lossy().replace('\\', "/");
            if (!tracked.contains(&key)) {
                out.push(path);
            }
        }
    }
    out
}
```

- [ ] **Step 4: Run to verify it passes.** Run: `cargo test rename_tests`. Expected: PASS (2 tests).

- [ ] **Step 5: Commit.**

```bash
git add src/server.rs
git commit -m "server: rename merge/untracked detection helpers"
```

### Task C6: Two-phase `rename_folder` (analyze → confirm or commit)

**Files:**
- Modify: `src/server.rs` — `rename_folder` (lines 612-715) + its handler arm.

**Interfaces:**
- Consumes: `folder_merge_same`, `scan_unrelated_in_dir` (Task C5); `Config.rename_merge_same`/`rename_merge_unrelated`/`rename_untracked_files` (Task C3); `RenameDecisions`, `RenameConcern`, `Response::RenameConfirmation` (Task C4).
- Produces: `rename_folder(index, old_prefix, new_prefix, decisions) -> Response`.

**Behavior:** (1) validate + build the plan exactly as today; (2) **file-on-file conflict → hard reject** (existing `rejected` path) with a "rename the file manually, then retry" message; (3) detect concerns; (4) for each concern consult its preference — `always*` auto-resolves, `ask` needs a decision; (5) if any `ask` concern lacks a decision → return `RenameConfirmation`; (6) else commit: move untracked files per the decision, submit the libtorrent renames, defer empty-dir cleanup.

- [ ] **Step 1: Update the signature + handler.** Change `rename_folder` to accept `decisions: Option<crate::ipc::RenameDecisions>`. Update the `Request::RenameFolder` arm to pass it:

```rust
            Request::RenameFolder { index, old_prefix, new_prefix, decisions } =>
                self.rename_folder(index, &old_prefix, &new_prefix, decisions),
```

(Note `rename_folder` returns `Result<Response>` today; keep returning `Response` directly or `Ok(Response)` consistent with the existing arm — match the current style at `src/server.rs`.)

- [ ] **Step 2: Keep validation + plan, route conflicts to hard reject.** Preserve the existing plan-building and collision checks (`src/server.rs:624-703`). When `rejected` is non-empty, return the hard-reject result but with the guidance message. Replace the atomic-reject block (lines 700-703) with:

```rust
        // file-on-file conflicts are never auto-merged. reject the whole
        // operation and tell the user to rename the conflicting file first.
        if (!rejected.is_empty()) {
            let rejected = rejected.into_iter()
                .map(|(file_index, reason)| (file_index, format!("{} — rename that file manually, then retry the merge", reason)))
                .collect();
            return Ok(Response::RenameResult { renamed: Vec::new(), rejected });
        }
```

- [ ] **Step 3: Detect concerns.** After the conflict reject, before submitting, compute the concerns. Add (using `torrent.save_path`, `filtered_plan`, and `static_files`):

```rust
        let tracked: std::collections::HashSet<String> = files.iter().map(|file| file.path.clone()).collect();
        let save_root = std::path::Path::new(&torrent.save_path);

        let merge_same = folder_merge_same(&static_files.iter().map(|s| s.to_string()).collect::<Vec<_>>(), trimmed_new);

        let dest_dir = save_root.join(trimmed_new);
        let unrelated_in_dest = scan_unrelated_in_dir(&dest_dir, &tracked, save_root);
        let merge_unrelated = !unrelated_in_dest.is_empty();

        let source_dir = save_root.join(trimmed_old);
        let untracked_in_source = scan_unrelated_in_dir(&source_dir, &tracked, save_root);
```

- [ ] **Step 4: Resolve concerns against preferences/decisions.** Add a helper that returns either the resolved decisions or the list of concerns still needing an answer:

```rust
        let mut needs: Vec<crate::ipc::RenameConcern> = Vec::new();
        let mut approved_merge_same = true;
        let mut approved_merge_unrelated = true;
        let mut untracked_choice = crate::ipc::UntrackedChoice::Leave;

        if (merge_same) {
            match (self.config.rename_merge_same.as_str(), decisions) {
                ("always", _) => {}
                (_, Some(d)) => approved_merge_same = d.merge_same,
                (_, None) => needs.push(crate::ipc::RenameConcern::MergeSame),
            }
        }
        if (merge_unrelated) {
            match (self.config.rename_merge_unrelated.as_str(), decisions) {
                ("always", _) => {}
                (_, Some(d)) => approved_merge_unrelated = d.merge_unrelated,
                (_, None) => needs.push(crate::ipc::RenameConcern::MergeUnrelated { unrelated_count: unrelated_in_dest.len() }),
            }
        }
        if (!untracked_in_source.is_empty()) {
            match (self.config.rename_untracked_files.as_str(), decisions) {
                ("always_move", _) => untracked_choice = crate::ipc::UntrackedChoice::Move,
                ("always_leave", _) => untracked_choice = crate::ipc::UntrackedChoice::Leave,
                (_, Some(d)) => untracked_choice = d.untracked,
                (_, None) => needs.push(crate::ipc::RenameConcern::UntrackedFiles { count: untracked_in_source.len() }),
            }
        }

        if (!needs.is_empty()) {
            return Ok(Response::RenameConfirmation { concerns: needs });
        }
        // a declined merge cancels the whole operation
        if ((merge_same && !approved_merge_same) || (merge_unrelated && !approved_merge_unrelated)) {
            return Ok(Response::RenameResult { renamed: Vec::new(), rejected: vec![(0, "rename cancelled".to_string())] });
        }
```

- [ ] **Step 5: Commit untracked moves, then tracked renames.** Replace the final submit loop (`src/server.rs:705-714`) with:

```rust
        // move untracked files first (independent of libtorrent's async renames)
        if (matches!(untracked_choice, crate::ipc::UntrackedChoice::Move)) {
            let _ = std::fs::create_dir_all(&dest_dir);
            for source in &untracked_in_source {
                if let Ok(relative) = source.strip_prefix(&source_dir) {
                    let target = dest_dir.join(relative);
                    if let Some(parent) = target.parent() { let _ = std::fs::create_dir_all(parent); }
                    if let Err(error) = std::fs::rename(source, &target) {
                        tracing::warn!(source = %source.display(), "untracked move failed: {}", error);
                    }
                }
            }
        }

        let mut renamed: Vec<usize> = Vec::new();
        for (file_index, new_path) in filtered_plan {
            torrent.handle.rename_file(file_index as i32, &new_path);
            tracing::info!(torrent = %torrent.info_hash, file_index, new_name = %new_path, "submitted rename (folder)");
            renamed.push(file_index);
        }
        // best-effort: the now-empty source dir is removed opportunistically;
        // libtorrent's renames complete asynchronously, so this may fail the
        // first time and is retried by a later rename or left to the user.
        let _ = std::fs::remove_dir(&source_dir);
        Ok(Response::RenameResult { renamed, rejected: Vec::new() })
```

- [ ] **Step 6: Build.** Run: `cargo build`. Expected: compiles. Fix borrow issues by cloning `torrent.save_path` and `static_files` into owned values before the `torrent.handle` mutable use if the borrow checker complains (the existing function already separates `files`/`static_files` snapshots).

- [ ] **Step 7: Verify manually.** With `rename_merge_same=ask`, rename folder `A` to an existing `B` that holds same-torrent files → daemon returns a confirmation (Task C8 renders it). With a file-on-file collision, confirm the hard-reject message. Place a stray non-torrent file inside `A`, set `rename_untracked_files=ask`, rename `A`, choose Move, and confirm the stray file is relocated.

- [ ] **Step 8: Commit.**

```bash
git add src/server.rs
git commit -m "server: two-phase folder rename with merge/untracked handling"
```

### Task C7: Pre-check `move_storage` for merge/conflict

**Files:**
- Modify: `src/server.rs` — `move_storage` (lines 536-576) + handler arm.

**Interfaces:**
- Consumes: `scan_unrelated_in_dir` (Task C5), the three prefs, `RenameDecisions`/`RenameConcern` (Task C4).
- Produces: `move_storage(index, new_save_path, decisions) -> Result<Response>` (returns `RenameConfirmation` when a merge needs confirming, else proceeds).

**Behavior:** moving the torrent root to `new_save_path`: if the destination already contains files → merge warning (use `rename_merge_unrelated` pref; the destination's files are by definition not yet this torrent's); if a torrent file would land exactly on an existing destination file → hard reject; else submit `move_storage` as today.

- [ ] **Step 1: Change the signature + handler.** Update the `Request::Move` arm to pass `decisions`, and change `move_storage` to `fn move_storage(&mut self, index: usize, new_save_path: &str, decisions: Option<crate::ipc::RenameDecisions>) -> Result<crate::ipc::Response>`. The arm becomes:

```rust
            Request::Move { index, new_save_path, decisions } => match self.move_storage(index, &new_save_path, decisions) {
                Ok(response) => response,
                Err(error) => Response::Err(error.to_string()),
            },
```

- [ ] **Step 2: Pre-check before submitting.** In `move_storage`, after the existing empty/absolute/same-path checks and `create_dir_all(path)` (line 553), and before `torrent.handle.move_storage` (line 569), insert:

```rust
        let tracked: std::collections::HashSet<String> = torrent.handle.files().iter().map(|file| file.path.clone()).collect();
        let dest = std::path::Path::new(trimmed);

        // file-on-file conflict: a torrent file already exists at the destination
        let mut conflict: Option<String> = None;
        for relative in &tracked {
            if (dest.join(relative).is_file()) { conflict = Some(relative.clone()); break; }
        }
        if let Some(path) = conflict {
            return Err(anyhow::anyhow!("\"{}\" already exists at the destination — rename or remove it, then retry the move", path));
        }

        // merge warning: destination already holds unrelated files
        let unrelated = scan_unrelated_in_dir(dest, &tracked, dest);
        if (!unrelated.is_empty()) {
            match (self.config.rename_merge_unrelated.as_str(), decisions) {
                ("always", _) => {}
                (_, Some(d)) if d.merge_unrelated => {}
                (_, Some(_)) => return Ok(Response::Ok), // declined: cancel quietly
                (_, None) => return Ok(Response::RenameConfirmation {
                    concerns: vec![crate::ipc::RenameConcern::MergeUnrelated { unrelated_count: unrelated.len() }],
                }),
            }
        }
```

Then keep the existing `torrent.handle.move_storage(trimmed)` etc., and at the end return `Ok(Response::Ok)` instead of `Ok(())` (update the function's return type usage accordingly — the success path now yields `Response::Ok`).

- [ ] **Step 3: Build.** Run: `cargo build`. Expected: compiles. Reconcile the `&mut self` borrow: take the `tracked`/`save_path` snapshots via an immutable `self.torrents.get(index)` before the later `get_mut` (mirror the existing pattern at lines 546/568).

- [ ] **Step 4: Verify manually.** Move a torrent to an empty dir → succeeds silently. Move to a dir containing unrelated files with `rename_merge_unrelated=ask` → confirmation prompt. Move to a dir already holding one of the torrent's files → hard-reject message.

- [ ] **Step 5: Commit.**

```bash
git add src/server.rs
git commit -m "server: pre-check move for merge warning + file conflict"
```

### Task C8: TUI confirmation-prompt sequencing + persist "Always" choices

**Files:**
- Modify: `src/tui.rs` — response handling for `RenameFolder`/`Move`; a small confirmation overlay + state; persistence of "Always" choices via `SetConfig`.

**Interfaces:**
- Consumes: `Response::RenameConfirmation { concerns }`, `RenameDecisions`, `RenameConcern`, `UntrackedChoice` (Task C4); `set_config` (the helper used at `src/tui.rs:819`).

**Design:** When a rename/move returns `RenameConfirmation`, store the pending request + the list of concerns + a partially-built `RenameDecisions`, and present one concern at a time. Each answer fills a field; "Always…" also sends a `SetConfig`. When all concerns are answered, re-send the original request with `decisions: Some(...)`. "No" on a merge cancels.

- [ ] **Step 1: Add confirmation state.** Add to `AppState` (near `prompt`):

```rust
    /// in-flight rename/move awaiting per-concern confirmation
    rename_confirm: Option<RenameConfirm>,
```

and a struct + initializer (`rename_confirm: None` in the constructor):

```rust
struct RenameConfirm {
    /// the request to resend once all concerns are answered. carries the
    /// already-resolved index/paths; only `decisions` is filled in here.
    kind: RenameConfirmKind,
    concerns: std::collections::VecDeque<crate::ipc::RenameConcern>,
    decisions: crate::ipc::RenameDecisions,
}

enum RenameConfirmKind {
    Folder { index: usize, old_prefix: String, new_prefix: String },
    Move { index: usize, new_save_path: String },
}
```

Initialize `decisions` with `merge_same: true, merge_unrelated: true, untracked: UntrackedChoice::Leave` (defaults that only change when the user answers).

- [ ] **Step 2: Capture `RenameConfirmation` on submit.** In the `RenameFolder` submit handling (Task C2 Step 2) and the `Move` submit handling, add a match arm for the new response:

```rust
                Ok(Response::RenameConfirmation { concerns }) => {
                    state.rename_confirm = Some(RenameConfirm {
                        kind: RenameConfirmKind::Folder { index: torrent_index, old_prefix: old_prefix.clone(), new_prefix: new_prefix.clone() },
                        concerns: concerns.into_iter().collect(),
                        decisions: crate::ipc::RenameDecisions { merge_same: true, merge_unrelated: true, untracked: crate::ipc::UntrackedChoice::Leave },
                    });
                }
```

(and the analogous `RenameConfirmKind::Move { … }` in the Move handler.)

- [ ] **Step 3: Draw the confirmation overlay.** Add a `draw_rename_confirm` that, when `state.rename_confirm` is `Some`, renders the front concern with its prompt text and key hints:
  - `MergeSame` / `MergeUnrelated { unrelated_count }`: "merge into existing folder" (+ `unrelated_count` unrelated files for the latter) — keys `a` Always, `y` Yes, `n` No.
  - `UntrackedFiles { count }`: "N untracked files in this folder" — keys `a` always move, `l` always leave, `m` move, `e` leave.
  Call it from the main draw when `state.rename_confirm.is_some()` (mirror how `state.prompt` is drawn).

- [ ] **Step 4: Handle confirmation keys.** Add a handler (called before the normal key routing when `rename_confirm.is_some()`) that pops the front concern, records the answer, and persists "Always" choices:

```rust
fn handle_rename_confirm_key(key: KeyCode, state: &mut AppState) {
    let Some(confirm) = state.rename_confirm.as_mut() else { return; };
    let Some(concern) = confirm.concerns.front().cloned() else { return; };
    let mut cancelled = false;
    match concern {
        crate::ipc::RenameConcern::MergeSame => match key {
            KeyCode::Char('a') => { confirm.decisions.merge_same = true; let _ = set_config("rename_merge_same", "always"); }
            KeyCode::Char('y') => confirm.decisions.merge_same = true,
            KeyCode::Char('n') => { confirm.decisions.merge_same = false; cancelled = true; }
            _ => return,
        },
        crate::ipc::RenameConcern::MergeUnrelated { .. } => match key {
            KeyCode::Char('a') => { confirm.decisions.merge_unrelated = true; let _ = set_config("rename_merge_unrelated", "always"); }
            KeyCode::Char('y') => confirm.decisions.merge_unrelated = true,
            KeyCode::Char('n') => { confirm.decisions.merge_unrelated = false; cancelled = true; }
            _ => return,
        },
        crate::ipc::RenameConcern::UntrackedFiles { .. } => match key {
            KeyCode::Char('a') => { confirm.decisions.untracked = crate::ipc::UntrackedChoice::Move; let _ = set_config("rename_untracked_files", "always_move"); }
            KeyCode::Char('l') => { confirm.decisions.untracked = crate::ipc::UntrackedChoice::Leave; let _ = set_config("rename_untracked_files", "always_leave"); }
            KeyCode::Char('m') => confirm.decisions.untracked = crate::ipc::UntrackedChoice::Move,
            KeyCode::Char('e') => confirm.decisions.untracked = crate::ipc::UntrackedChoice::Leave,
            _ => return,
        },
    }
    confirm.concerns.pop_front();
    if (cancelled) { state.rename_confirm = None; state.error = Some("rename cancelled".to_string()); return; }
    if (confirm.concerns.is_empty()) { resend_rename_confirm(state); }
}
```

- [ ] **Step 5: Resend with decisions.** Add:

```rust
fn resend_rename_confirm(state: &mut AppState) {
    let Some(confirm) = state.rename_confirm.take() else { return; };
    let decisions = Some(confirm.decisions);
    let response = match confirm.kind {
        RenameConfirmKind::Folder { index, old_prefix, new_prefix } =>
            client::send(Request::RenameFolder { index, old_prefix, new_prefix, decisions }),
        RenameConfirmKind::Move { index, new_save_path } =>
            client::send(Request::Move { index, new_save_path, decisions }),
    };
    match response {
        Ok(Response::RenameResult { renamed, rejected }) if rejected.is_empty() => state.error = Some(format!("renamed {} file(s)", renamed.len())),
        Ok(Response::RenameResult { rejected, .. }) => state.error = rejected.first().map(|(_, reason)| reason.clone()),
        Ok(Response::Ok) => state.error = Some("moved".to_string()),
        Ok(Response::Err(message)) => state.error = Some(message),
        Ok(_) => state.error = Some("unexpected response".to_string()),
        Err(error) => state.error = Some(error.to_string()),
    }
}
```

- [ ] **Step 6: Build.** Run: `cargo build`. Expected: compiles clean once `set_config` (the helper at `src/tui.rs:819`) is in scope.

- [ ] **Step 7: Verify manually (end to end).** With all three prefs `ask`: rename a folder that triggers all three concerns at once; confirm the prompts appear in order (merge-same → merge-unrelated → untracked), that "Always" answers persist (re-check config), and that the rename then commits. Test "No" cancels. Test the Move confirmation path.

- [ ] **Step 8: Commit.**

```bash
git add src/tui.rs
git commit -m "tui: sequential rename/move confirmation prompts"
```

---

# Phase D — Doc cleanup

### Task D1: Fix the dangling `plans/TODO-tui.md` reference

**Files:**
- Modify: `src/tui.rs:339`.

- [ ] **Step 1: Read the comment.** Open `src/tui.rs` around line 339:

```rust
/// see plans/TODO-tui.md and the security-anonymity-priorities memory.
```

- [ ] **Step 2: Drop the dead path.** Replace with (keeping the memory pointer):

```rust
/// see the security-anonymity-priorities memory.
```

- [ ] **Step 3: Build.** Run: `cargo build`. Expected: compiles clean (comment-only change).

- [ ] **Step 4: Commit.**

```bash
git add src/tui.rs
git commit -m "docs: drop reference to nonexistent plans/TODO-tui.md"
```

---

## Self-Review

**Spec coverage:**
- A1 click bug → Task A1. A2 greying → Task A2.
- B picker (4-way) → B6; `default_content_layout` pref → B3; apply after verification → B5; path-rewrite rules incl. single-file `Always`=`name/filename` → B2; internal add paths default → B4 Step 5; deletes `todo!()` → B6.
- C1 plain names + `../` + root-escape → C1/C2. C2 merge warn + file-conflict hard reject + logical/disk detection → C5/C6. C3 untracked move/leave incl. empty-dir cleanup → C6 Step 5. C4 three prefs → C3. C5 two-phase flow → C4/C6/C8. C6 move in scope → C7. RenameDecisions/RenameConcern/RenameConfirmation → C4.
- D doc cleanup → D1.

**Placeholder scan:** No "TBD/TODO/handle edge cases" left as instructions; the one explicit sequencing dependency (C2 Step 2 needs C4) is called out with the recommended order. Pure-logic tasks carry full code + tests; integration tasks carry full code with exact anchors.

**Type consistency:** `ContentLayout` (ipc) used by layout.rs + tui + server. `compute_content_layout_renames(&[String], &str, ContentLayout) -> Vec<(usize, String)>` consumed in B5. `resolve_rename_input(&str, &str) -> Result<String,String>` consumed in C2. `RenameDecisions { merge_same: bool, merge_unrelated: bool, untracked: UntrackedChoice }`, `UntrackedChoice { Move, Leave }`, `RenameConcern { MergeSame, MergeUnrelated{unrelated_count}, UntrackedFiles{count} }`, `Response::RenameConfirmation { concerns }` are defined in C4 and consumed identically in C6/C7/C8. `selected_row_style() -> Style` defined + used in A2. `folder_merge_same`/`scan_unrelated_in_dir` defined in C5, consumed in C6/C7.

**Known integration risks to watch during execution (not blockers):** borrow-checker juggling around `torrent.handle` in C6/C7 (snapshot owned values first, per the existing pattern); confirm `ffi::TorrentFile.path` and `status.has_metadata`/`status.state`/`status.name` field names against the live bridge structs when wiring B5; the empty-source-dir cleanup in C6 is best-effort by design.
