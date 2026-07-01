# Glob Expansion, Live Add Validation, and Add-Result Review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the add-torrent prompt shell-style glob expansion for local paths, instant
per-line validity feedback while typing (magnet / url / file ok / glob with N matches / not
found), and a post-dispatch results overlay with per-entry ok/fail outcomes, dismissable
per entry, all at once, or permanently via a persisted `add_result_review = "never"`
preference.

**Architecture:** One shared classify function is the spine. `src/sources.rs` gains
`SourceKind` + `classify()` (extracted from `resolve()`, which is refactored on top of it)
and `expand_glob()` (the rust-lang `glob` crate). The TUI consumes that same function three
ways: the live per-line indicator cache on `Prompt`, the submit-time expansion/rejection in
`submit_prompt`'s `PromptAction::Add` arm, and (indirectly) the daemon's `resolve()`, so the
indicator can never predict something different from what dispatch does. The results
overlay is pure client state (`AddResultsReview` on `AppState`) fed by the per-entry
responses `dispatch_add_options` already receives; no new IPC. The preference persists via
the existing `Request::SetConfig` path, mirroring the `rename_merge_same` "always"/"ask"
pattern.

**Tech Stack:** Rust 2021, `ratatui`/`crossterm` (TUI), libtorrent via the existing `cxx`
bridge, `serde`/`toml` for persistence, new dependency: `glob` (rust-lang owned, pure safe
rust, zero transitive dependencies).

**Spec:** `docs/superpowers/specs/2026-07-01-add-validation-design.md`

**Build note:** the C++ bridge makes full builds take minutes; let them run. The worktree
has a local cargo target dir configured. This is a plain binary crate: use `cargo test`
(optionally with a filter like `cargo test sources::`), never `cargo test --lib`.

---

## Sequencing

Tasks run strictly in order 1 → 7. No parallelization: tasks 3, 4, and 6 all edit
`src/tui.rs`, and everything downstream of task 1 consumes `classify()`.

| Task | Files touched |
|---|---|
| 1. `SourceKind` + `classify()` + `resolve()` refactor | `src/sources.rs` |
| 2. `glob` dependency + `expand_glob()` | `Cargo.toml`, `src/sources.rs` |
| 3. submit-time expansion and rejection | `src/tui.rs` |
| 4. live per-line indicators | `src/tui.rs` |
| 5. `add_result_review` config field | `src/config.rs`, `src/server.rs` |
| 6. add-result review overlay | `src/tui.rs` |
| 7. final verification pass | none |

`src/ipc.rs` is untouched: the spec adds no new IPC. Line numbers below are anchors as of
the branch tip (commit 34b4e2d); tasks 4 and 6 land after task 3 has already shifted
`src/tui.rs`, so locate by function name first, line number second.

---

## Task 1: `SourceKind` + `classify()`, `resolve()` refactored on top

**Files:**
- Modify: `src/sources.rs`

Spec section A1. The magnet/url/tilde classification logic moves out of `resolve()` into a
shared `classify()`; it is moved, not duplicated. Classification order for non-magnet,
non-url input after tilde expansion and normalisation: literal-path-exists wins (bracket
filenames are common), then glob metacharacters, then nonexistent `LocalPath`.

- [ ] **Step 1: Write the failing tests**

Append to the end of `src/sources.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::{classify, SourceKind};
    use std::path::PathBuf;

    /// tiny scoped tempdir, removed on drop. avoids a tempfile dev-dependency
    /// for what a ten-line Drop impl covers.
    pub struct TestDir(pub PathBuf);

    impl TestDir {
        pub fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("monsoon-sources-test-{}-{}", label, std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).unwrap(); }
            std::fs::write(&path, b"x").unwrap();
            path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn empty_and_whitespace_classify_as_none() {
        assert_eq!(classify(""), None);
        assert_eq!(classify("   "), None);
    }

    #[test]
    fn magnet_uris_classify_as_magnet() {
        assert_eq!(classify("magnet:?xt=urn:btih:abcdef"), Some(SourceKind::Magnet));
    }

    #[test]
    fn each_url_scheme_classifies_as_url_case_insensitively() {
        for input in [
            "http://example.com/a.torrent",
            "https://example.com/a.torrent",
            "ftp://example.com/a.torrent",
            "sftp://example.com/a.torrent",
            "HTTPS://EXAMPLE.COM/A.TORRENT",
        ] {
            assert_eq!(classify(input), Some(SourceKind::Url), "input: {}", input);
        }
    }

    #[test]
    fn tilde_paths_expand_to_home() {
        match classify("~/monsoon-classify-test.torrent") {
            Some(SourceKind::LocalPath(path)) => {
                assert!(
                    !path.to_string_lossy().starts_with('~'),
                    "tilde not expanded: {}",
                    path.display()
                );
            }
            other => panic!("expected LocalPath, got {:?}", other),
        }
    }

    #[test]
    fn windows_drive_paths_classify_as_local_path_with_folded_drive_letter() {
        match classify(r"c:\downloads\a.torrent") {
            Some(SourceKind::LocalPath(path)) => {
                assert!(path.to_string_lossy().starts_with('C'));
            }
            other => panic!("expected LocalPath, got {:?}", other),
        }
    }

    #[test]
    fn existing_literal_path_with_brackets_beats_glob_interpretation() {
        let dir = TestDir::new("brackets");
        let real = dir.file("[Group] Show.torrent");
        assert_eq!(
            classify(real.to_str().unwrap()),
            Some(SourceKind::LocalPath(real.clone()))
        );
    }

    #[test]
    fn nonexistent_path_with_metacharacters_classifies_as_glob() {
        let pattern = "/monsoon-does-not-exist/*.torrent";
        assert_eq!(classify(pattern), Some(SourceKind::LocalGlob(pattern.to_string())));
    }

    #[test]
    fn nonexistent_plain_path_stays_local_path() {
        let path = "/monsoon-does-not-exist/a.torrent";
        assert_eq!(classify(path), Some(SourceKind::LocalPath(PathBuf::from(path))));
    }
}
```

Run: `cargo test sources::`
Expected: FAIL to compile (`classify` and `SourceKind` do not exist yet). That compile
error is the failing state for this step.

- [ ] **Step 2: Implement `SourceKind` + `classify()`, refactor `resolve()`**

In `src/sources.rs`, directly below the `Source` enum (currently lines 63-68) and above
`resolve()`, insert:

```rust
/// what one input line will be treated as. shared by the tui's live
/// indicator, the tui's submit-time expansion, and the daemon's resolve(),
/// so all three always agree.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceKind {
    /// magnet uri, passed through untouched
    Magnet,
    /// http/https/ftp/sftp, fetched daemon-side via libcurl
    Url,
    /// tilde-expanded, normalised local path (may or may not exist)
    LocalPath(PathBuf),
    /// tilde-expanded pattern containing glob metacharacters
    LocalGlob(String),
}

/// classify one input line the same way resolve() will treat it. returns
/// None for empty (after trim) input. the literal-path-exists check runs
/// before glob detection so bracket-laden real filenames ("[Group] Show")
/// are never reinterpreted as character classes.
pub fn classify(input: &str) -> Option<SourceKind> {
    let trimmed = input.trim();
    if (trimmed.is_empty()) { return None; }
    if (trimmed.starts_with("magnet:")) { return Some(SourceKind::Magnet); }
    if (is_url(trimmed)) { return Some(SourceKind::Url); }
    // tilde expansion failure (no resolvable home dir) falls back to the raw
    // input; the path simply won't exist and surfaces as not-found
    let expanded = expand_tilde(trimmed).unwrap_or_else(|_| trimmed.to_string());
    let path = normalise_path(&expanded);
    if (path.exists()) { return Some(SourceKind::LocalPath(path)); }
    if (expanded.chars().any(|character| matches!(character, '*' | '?' | '['))) {
        return Some(SourceKind::LocalGlob(expanded));
    }
    Some(SourceKind::LocalPath(path))
}
```

Then replace the whole body of `resolve()` (currently lines 70-101, from the doc comment
through the closing brace) with:

```rust
/// classify and resolve one user input string into a Source. on http/ftp
/// the file is downloaded to a temp path; the caller owns cleanup. glob
/// patterns are the client's job to expand and are rejected here.
pub fn resolve(input: &str) -> Result<Source> {
    match classify(input) {
        None => anyhow::bail!("empty source"),
        Some(SourceKind::Magnet) => Ok(Source::Magnet(input.trim().to_string())),
        Some(SourceKind::Url) => {
            let temp = std::env::temp_dir().join(format!(
                "monsoon-fetch-{}.torrent",
                std::process::id()
            ));
            fetch_url(&temp, input.trim())
                .inspect_err(|_| { let _ = std::fs::remove_file(&temp); })?;
            Ok(Source::File(temp))
        }
        Some(SourceKind::LocalPath(path)) => {
            if (!path.exists()) {
                anyhow::bail!("file not found: {}", path.display());
            }
            Ok(Source::File(path))
        }
        Some(SourceKind::LocalGlob(_)) => {
            anyhow::bail!("glob patterns must be expanded by the client")
        }
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test sources::`
Expected: PASS, all 8 tests. Also run `cargo build` once here since `resolve()` changed
shape; expected: clean build, daemon callers unaffected (same signature).

- [ ] **Step 4: Commit**

```bash
git add src/sources.rs
git commit -m "sources: extract shared classify() from resolve()"
```

---

## Task 2: `glob` dependency + `expand_glob()`

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/sources.rs`

Spec section A2. The `glob` crate is the matcher; `*`, `?`, `[...]`, and recursive `**`
all come free. Unreadable entries are skipped, directory matches are not filtered (the
daemon rejects them with a real error and the review overlay surfaces it).

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, in the `[dependencies]` section after `regex = "1"` (line 36), add:

```toml
glob = "0.3"
```

- [ ] **Step 2: Write the failing tests**

Add to the `tests` module at the end of `src/sources.rs` (inside `mod tests`), and extend
the `use` line at the top of the module from
`use super::{classify, SourceKind};` to
`use super::{classify, expand_glob, SourceKind};`:

```rust
    #[test]
    fn expand_glob_returns_alphabetical_matches() {
        let dir = TestDir::new("expand-order");
        dir.file("b.torrent");
        dir.file("a.torrent");
        dir.file("notes.txt");
        let pattern = format!("{}/*.torrent", dir.0.display());
        let matches = expand_glob(&pattern).unwrap();
        assert_eq!(matches, vec![dir.0.join("a.torrent"), dir.0.join("b.torrent")]);
    }

    #[test]
    fn expand_glob_zero_matches_is_ok_and_empty() {
        let dir = TestDir::new("expand-empty");
        dir.file("a.torrent");
        let pattern = format!("{}/*.nope", dir.0.display());
        assert_eq!(expand_glob(&pattern).unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn expand_glob_recursive_double_star_descends() {
        let dir = TestDir::new("expand-recursive");
        dir.file("sub/inner/c.torrent");
        let pattern = format!("{}/**/c.torrent", dir.0.display());
        let matches = expand_glob(&pattern).unwrap();
        assert!(matches.contains(&dir.0.join("sub/inner/c.torrent")));
    }

    #[test]
    fn expand_glob_bad_pattern_is_a_clear_error() {
        // unclosed character class is a pattern error, not a silent empty
        assert!(expand_glob("/tmp/[").is_err());
    }
```

Run: `cargo test sources::`
Expected: FAIL to compile (`expand_glob` does not exist yet).

- [ ] **Step 3: Implement `expand_glob()`**

In `src/sources.rs`, directly below `classify()`, insert:

```rust
/// expand a glob pattern against the local filesystem. matches come back in
/// the glob crate's deterministic per-directory alphabetical order.
/// unreadable entries yielded by the walk are skipped; they would fail at
/// add time anyway. directory matches are not filtered out on purpose: the
/// daemon rejects them with a real error the results overlay can show.
pub fn expand_glob(pattern: &str) -> Result<Vec<PathBuf>, String> {
    let paths = glob::glob(pattern)
        .map_err(|error| format!("bad glob pattern: {}", error))?;
    Ok(paths.filter_map(|entry| entry.ok()).collect())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test sources::`
Expected: PASS, all 12 tests (8 from task 1 plus these 4).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/sources.rs
git commit -m "sources: add expand_glob() via the glob crate"
```

---

## Task 3: submit-time expansion and rejection in the add prompt

**Files:**
- Modify: `src/tui.rs`

Spec section A3. Expansion happens in `submit_prompt`'s `PromptAction::Add` arm before the
`AddOptionsForm` is built, so everything downstream (per-entry options, dispatch, organize
step, and later the results overlay) already operates on expanded entries. Zero-match
globs and nonexistent local paths return `Err`, which the existing error-keeps-prompt path
in `handle_prompt_key` (line 2638) turns into "prompt stays open with the error in the
status line". Magnets and urls always pass through unverified.

- [ ] **Step 1: Replace the `PromptAction::Add` arm**

In `src/tui.rs`, in `submit_prompt` (line 2260), replace the entire
`PromptAction::Add => { ... }` arm (currently lines 2360-2380) with:

```rust
        PromptAction::Add => {
            // classify each line exactly the way the daemon's resolve() will,
            // expand globs client-side, and reject verifiably-bad local
            // entries while the prompt is still open. remote sources always
            // pass through unverified; the daemon's fetch is the judge there.
            use crate::sources::{classify, expand_glob, SourceKind};
            let mut entries: Vec<String> = Vec::new();
            for field in &prompt.lines {
                let line = field.buffer().trim();
                match classify(line) {
                    None => continue,
                    Some(SourceKind::Magnet) | Some(SourceKind::Url) => {
                        entries.push(line.to_string());
                    }
                    Some(SourceKind::LocalPath(path)) => {
                        if (!path.exists()) {
                            return Err(anyhow::anyhow!("file not found: {}", path.display()));
                        }
                        entries.push(path.to_string_lossy().to_string());
                    }
                    Some(SourceKind::LocalGlob(pattern)) => {
                        let matches = expand_glob(&pattern)
                            .map_err(|error| anyhow::anyhow!("{}", error))?;
                        if (matches.is_empty()) {
                            return Err(anyhow::anyhow!("no matches: {}", line));
                        }
                        entries.extend(
                            matches.into_iter().map(|path| path.to_string_lossy().to_string()),
                        );
                    }
                }
            }
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
```

- [ ] **Step 2: Mention globs in the add prompt's helper text**

In `open_add_prompt` (line 1942), replace the `helper:` line:

```rust
        helper: "magnet:, http(s)://, ftp(s)://, /abs/path, C:\\path, or ~/foo.torrent — one per line".to_string(),
```

with:

```rust
        helper: "magnet:, http(s)://, ftp(s)://, /abs/path, C:\\path, ~/foo.torrent, or a glob (*.torrent); one per line".to_string(),
```

- [ ] **Step 3: Build and spot-check**

Run: `cargo build`
Expected: PASS.

Manual spot check (daemon running, `target/debug/monsoon` in another terminal):
- press `n`, type a glob matching several local `.torrent` files, enter: the options form
  walks one entry per match, and after confirming, that many torrents appear.
- type a glob matching nothing, enter: prompt stays open, status line shows
  `no matches: <pattern>`.
- type a nonexistent plain path, enter: prompt stays open, `file not found: <path>`.
- a magnet line still submits untouched.

- [ ] **Step 4: Commit**

```bash
git add src/tui.rs
git commit -m "tui: expand globs and verify local paths at add-prompt submit"
```

---

## Task 4: live per-line indicators in the add prompt

**Files:**
- Modify: `src/tui.rs`

Spec sections B1 and B2. `Prompt` gains an `indicators: Vec<LineIndicator>` cache parallel
to `lines`, filled only for `PromptAction::Add` prompts (empty vec everywhere else, drawing
nothing). Recomputation is scoped to the edited line: mutating keys reclassify
`cursor_line`, paste reclassifies each line it touched, shift+enter inserts an `Empty`
indicator, line-removal removes one, and cursor movement recomputes nothing.

- [ ] **Step 1: Write the failing tests**

In `src/tui.rs`, these will live right after the new `classify_line` function (added in
step 2; for now append after `reclassify_prompt_line` once it exists, or write them first
and watch the compile fail, which is the TDD failing state):

```rust
#[cfg(test)]
mod line_indicator_tests {
    use super::{classify_line, LineIndicator};

    #[test]
    fn magnet_and_url_lines() {
        assert_eq!(classify_line("magnet:?xt=urn:btih:abc"), LineIndicator::Magnet);
        assert_eq!(classify_line("https://example.com/x.torrent"), LineIndicator::Url);
    }

    #[test]
    fn blank_lines_are_empty() {
        assert_eq!(classify_line(""), LineIndicator::Empty);
        assert_eq!(classify_line("   "), LineIndicator::Empty);
    }

    #[test]
    fn nonexistent_plain_path_is_not_found() {
        // deliberate semantics: garbage that is neither magnet nor url is a
        // local path that does not exist, exactly what resolve() would say
        assert_eq!(
            classify_line("/monsoon-test-does-not-exist/x.torrent"),
            LineIndicator::NotFound
        );
    }

    #[test]
    fn zero_match_glob_reports_glob_zero() {
        assert_eq!(
            classify_line("/monsoon-test-does-not-exist/*.torrent"),
            LineIndicator::Glob(0)
        );
    }

    #[test]
    fn existing_file_and_matching_glob() {
        let dir = std::env::temp_dir()
            .join(format!("monsoon-indicator-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.torrent");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(classify_line(file.to_str().unwrap()), LineIndicator::FileOk);
        assert_eq!(
            classify_line(&format!("{}/*.torrent", dir.display())),
            LineIndicator::Glob(1)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

Run: `cargo test line_indicator`
Expected: FAIL to compile (`classify_line` and `LineIndicator` do not exist yet).

- [ ] **Step 2: Add `LineIndicator`, `classify_line`, `reclassify_prompt_line`**

In `src/tui.rs`, directly above `struct Prompt` (line 924), insert:

```rust
/// cached classification for one add-prompt line. a thin projection of
/// sources::classify, the same function submit and the daemon use, so the
/// indicator can never disagree with what dispatch actually does.
#[derive(Clone, Debug, PartialEq)]
enum LineIndicator {
    /// blank line, draw nothing
    Empty,
    Magnet,
    Url,
    FileOk,
    /// glob pattern with this many matches right now
    Glob(usize),
    NotFound,
}

impl LineIndicator {
    /// suffix label text and color; None draws nothing
    fn label(&self) -> Option<(String, Color)> {
        match self {
            LineIndicator::Empty => None,
            LineIndicator::Magnet => Some(("magnet".to_string(), Color::Green)),
            LineIndicator::Url => Some(("url".to_string(), Color::Green)),
            LineIndicator::FileOk => Some(("file ok".to_string(), Color::Green)),
            LineIndicator::Glob(0) => Some(("glob: 0 matches".to_string(), Color::Red)),
            LineIndicator::Glob(count) => Some((format!("glob: {} matches", count), Color::Green)),
            LineIndicator::NotFound => Some(("not found".to_string(), Color::Red)),
        }
    }
}

/// derive the indicator for one line: one stat for paths, one glob walk for
/// patterns. runs inline per edit on the edited line only; the spec's risks
/// note names debouncing as the upgrade path if huge directories ever lag.
fn classify_line(line: &str) -> LineIndicator {
    use crate::sources::{classify, expand_glob, SourceKind};
    match classify(line) {
        None => LineIndicator::Empty,
        Some(SourceKind::Magnet) => LineIndicator::Magnet,
        Some(SourceKind::Url) => LineIndicator::Url,
        Some(SourceKind::LocalPath(path)) => {
            if (path.exists()) { LineIndicator::FileOk } else { LineIndicator::NotFound }
        }
        Some(SourceKind::LocalGlob(pattern)) => LineIndicator::Glob(
            expand_glob(&pattern).map(|matches| matches.len()).unwrap_or(0),
        ),
    }
}

/// recompute the cached indicator for the focused prompt line. only the add
/// prompt keeps indicators; every other prompt is a no-op.
fn reclassify_prompt_line(state: &mut AppState) {
    let Some(prompt) = state.prompt.as_mut() else { return; };
    if (!matches!(prompt.action, PromptAction::Add)) { return; }
    let index = prompt.cursor_line;
    let Some(field) = prompt.lines.get(index) else { return; };
    let indicator = classify_line(field.buffer());
    if (prompt.indicators.len() < prompt.lines.len()) {
        prompt.indicators.resize(prompt.lines.len(), LineIndicator::Empty);
    }
    prompt.indicators[index] = indicator;
}
```

Place the `mod line_indicator_tests` block from step 1 directly after
`reclassify_prompt_line`.

- [ ] **Step 3: Add the `indicators` field to `Prompt` and every constructor**

In `struct Prompt` (line 924), after the `allow_multiline` field, add:

```rust
    /// per-line classification cache, parallel to `lines`. only the add
    /// prompt fills this (empty vec elsewhere, drawing nothing); recomputed
    /// per edited line, never on cursor movement between lines.
    indicators: Vec<LineIndicator>,
```

Then add `indicators: Vec::new(),` after the `allow_multiline:` line in each of these
seven construction sites:

1. `build_torrent_rename_prompt` (line 1720)
2. `open_move_prompt` (line 1738)
3. `build_rename_prompt`, folder branch (line 1824)
4. `build_rename_prompt`, file branch (line 1836)
5. `open_rate_limit_prompt` (line 1931)
6. `open_add_tracker_prompt` (line 1996)
7. the add-feed prompt in `handle_feeds_key` (line 3246)

And replace `open_add_prompt` (line 1942) entirely with:

```rust
fn open_add_prompt(state: &mut AppState) {
    let prefill = clipboard_magnet_or_url().unwrap_or_default();
    // classify the prefill up front so a clipboard magnet shows its
    // indicator before the first keystroke
    let indicators = vec![classify_line(&prefill)];
    state.prompt = Some(Prompt {
        title: "add torrent (shift+enter to add another line)".to_string(),
        helper: "magnet:, http(s)://, ftp(s)://, /abs/path, C:\\path, ~/foo.torrent, or a glob (*.torrent); one per line".to_string(),
        lines: vec![TextField::with_completion(prefill, CompletionSource::Filesystem)],
        cursor_line: 0,
        action: PromptAction::Add,
        torrent_index: 0,
        allow_multiline: true,
        indicators,
    });
}
```

- [ ] **Step 4: Hook reclassification into every mutating key**

In `handle_prompt_key` (line 2615), make these changes.

The shift+enter arm gains a mirrored indicator insert. Replace the
`(KeyCode::Enter, KeyModifiers::SHIFT)` arm body with:

```rust
        (KeyCode::Enter, KeyModifiers::SHIFT) => {
            if let Some(prompt) = state.prompt.as_mut() {
                if (prompt.allow_multiline) {
                    let completion = prompt.lines.get(prompt.cursor_line)
                        .map(|field| field.completion_source())
                        .unwrap_or(CompletionSource::None);
                    let insert_at = prompt.cursor_line + 1;
                    prompt.lines.insert(insert_at, TextField::with_completion(String::new(), completion));
                    // new line is empty, so Empty is its correct indicator
                    if (insert_at <= prompt.indicators.len()) {
                        prompt.indicators.insert(insert_at, LineIndicator::Empty);
                    }
                    prompt.cursor_line = insert_at;
                }
            }
        }
```

Append `reclassify_prompt_line(state);` as the last statement inside each of these five
arms (content-mutating, single line):

```rust
        (KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Backspace, KeyModifiers::ALT) => {
            if let Some(field) = current_prompt_field(state) { field.delete_word_backward(); }
            reclassify_prompt_line(state);
        }
        (KeyCode::Delete, KeyModifiers::CONTROL) | (KeyCode::Delete, KeyModifiers::ALT) => {
            if let Some(field) = current_prompt_field(state) { field.delete_word_forward(); }
            reclassify_prompt_line(state);
        }
        (KeyCode::Delete, _) => {
            if let Some(field) = current_prompt_field(state) { field.delete_forward(); }
            reclassify_prompt_line(state);
        }
        (KeyCode::Tab, _) => {
            if let Some(field) = current_prompt_field(state) { field.tab_complete(); }
            reclassify_prompt_line(state);
        }
        (KeyCode::Char(character), modifiers)
            if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
        {
            if let Some(field) = current_prompt_field(state) { field.insert_char(character); }
            reclassify_prompt_line(state);
        }
```

Replace the plain `(KeyCode::Backspace, _)` arm with (mirrors the line removal onto
`indicators`, then reclassifies whichever line the cursor lands on):

```rust
        (KeyCode::Backspace, _) => {
            if let Some(prompt) = state.prompt.as_mut() {
                let cursor_line = prompt.cursor_line;
                let at_line_start = prompt.lines.get(cursor_line)
                    .map(|field| field.buffer().is_empty() && field.cursor() == 0)
                    .unwrap_or(true);
                if (at_line_start && cursor_line > 0 && prompt.lines.len() > 1) {
                    prompt.lines.remove(cursor_line);
                    if (cursor_line < prompt.indicators.len()) {
                        prompt.indicators.remove(cursor_line);
                    }
                    prompt.cursor_line = cursor_line - 1;
                    let end = prompt.lines[prompt.cursor_line].buffer().chars().count();
                    prompt.lines[prompt.cursor_line].set_cursor(end);
                } else if let Some(field) = prompt.lines.get_mut(cursor_line) {
                    field.backspace();
                }
            }
            reclassify_prompt_line(state);
        }
```

Movement arms (`Up`, `Down`, `Left`, `Right`, `Home`, `End`, and the word-movement
variants) stay untouched: cursor movement recomputes nothing.

- [ ] **Step 5: Paste reclassifies each line it created**

Replace `paste_into_prompt` (line 2719) entirely with:

```rust
/// paste clipboard text into the focused prompt line. in a multi-line-capable
/// prompt, embedded newlines split into new lines (so pasting a list of paths,
/// one per line, populates the add-torrent prompt the way typing them would);
/// in a single-line prompt, newlines are stripped by `TextField::paste`.
/// every line the paste touched gets its indicator reclassified.
fn paste_into_prompt(state: &mut AppState) {
    let Ok(mut clipboard) = arboard::Clipboard::new() else { return; };
    let Ok(text) = clipboard.get_text() else { return; };
    let Some(prompt) = state.prompt.as_mut() else { return; };
    if (prompt.allow_multiline && text.contains('\n')) {
        let completion = prompt.lines.get(prompt.cursor_line)
            .map(|field| field.completion_source())
            .unwrap_or(CompletionSource::None);
        let first_line = prompt.cursor_line;
        let mut pieces: Vec<&str> = text.split('\n').collect();
        let first = pieces.remove(0);
        if let Some(field) = prompt.lines.get_mut(prompt.cursor_line) { field.paste(first); }
        let mut insert_at = prompt.cursor_line + 1;
        for piece in pieces {
            let piece = piece.trim_end_matches('\r').to_string();
            prompt.lines.insert(insert_at, TextField::with_completion(piece, completion.clone()));
            if (insert_at <= prompt.indicators.len()) {
                prompt.indicators.insert(insert_at, LineIndicator::Empty);
            }
            insert_at += 1;
        }
        prompt.cursor_line = insert_at - 1;
        if (matches!(prompt.action, PromptAction::Add)) {
            for index in first_line..=prompt.cursor_line {
                let indicator = classify_line(prompt.lines[index].buffer());
                if (index < prompt.indicators.len()) { prompt.indicators[index] = indicator; }
            }
        }
        return;
    }
    if let Some(field) = prompt.lines.get_mut(prompt.cursor_line) { field.paste(&text); }
    reclassify_prompt_line(state);
}
```

- [ ] **Step 6: Render the suffix in `draw_prompt`**

In `draw_prompt` (line 4018), replace the `body_lines` construction (lines 4056-4066)
with:

```rust
    let body_lines: Vec<Line> = prompt.lines.iter().enumerate().map(|(index, field)| {
        let is_cursor = index == prompt.cursor_line;
        let marker = if (is_cursor) { "› " } else { "  " };
        let mut spans = vec![Span::styled(marker, Style::default().fg(Color::Yellow))];
        if (is_cursor) {
            spans.extend(render_field_with_cursor(field));
        } else {
            spans.push(Span::raw(field.buffer().to_string()));
        }
        // per-line validity suffix, e.g. "  [glob: 4 matches]". empty vec
        // on non-add prompts means .get() misses and nothing is drawn.
        if let Some((label, color)) = prompt.indicators.get(index).and_then(LineIndicator::label) {
            spans.push(Span::styled(
                format!("  [{}]", label),
                Style::default().fg(color).add_modifier(Modifier::DIM),
            ));
        }
        Line::from(spans)
    }).collect();
```

- [ ] **Step 7: Run tests and build**

Run: `cargo test line_indicator`
Expected: PASS, all 5 tests.

Run: `cargo build`
Expected: PASS.

Manual spot check: open the add prompt, type a magnet, watch `[magnet]` appear green;
break it into garbage, watch `[not found]` red; type a glob over a real directory, watch
the match count change as you narrow the pattern; paste three lines, each shows its own
indicator; arrow between lines and confirm no lag (nothing recomputes).

- [ ] **Step 8: Commit**

```bash
git add src/tui.rs
git commit -m "tui: live per-line indicators in the add prompt"
```

---

## Task 5: `add_result_review` config field

**Files:**
- Modify: `src/config.rs`
- Modify: `src/server.rs`

Spec section C3, daemon side: the persisted preference, `sanitize()` healing, and the
`apply_config_change` validation arm mirroring `rename_merge_same`.

- [ ] **Step 1: Write the failing tests**

Append to the end of `src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn add_result_review_defaults_to_always() {
        assert_eq!(Config::default().add_result_review, "always");
    }

    #[test]
    fn sanitize_resets_invalid_add_result_review() {
        let mut config = Config::default();
        config.add_result_review = "sometimes".to_string();
        config.sanitize();
        assert_eq!(config.add_result_review, "always");
    }

    #[test]
    fn sanitize_keeps_valid_add_result_review_values() {
        for value in ["always", "never"] {
            let mut config = Config::default();
            config.add_result_review = value.to_string();
            config.sanitize();
            assert_eq!(config.add_result_review, value);
        }
    }
}
```

Run: `cargo test config::`
Expected: FAIL to compile (no `add_result_review` field yet).

- [ ] **Step 2: Add the field, default, and sanitize arm**

In `src/config.rs`:

After the `rename_untracked_files` field (line 37), add:

```rust
    /// show the per-entry results overlay after adding torrents: always | never
    #[serde(default = "default_always")]
    pub add_result_review: String,
```

Next to `fn default_ask()` (line 183), add:

```rust
fn default_always() -> String { "always".to_string() }
```

In `impl Default for Config`, after `rename_untracked_files: default_ask(),` (line 203),
add:

```rust
            add_result_review: default_always(),
```

In `sanitize()`, in the enum-shaped strings section, after the `proxy_type` check
(lines 302-307), add:

```rust
        if (!matches!(self.add_result_review.as_str(), "always" | "never")) {
            self.add_result_review = default.add_result_review.clone();
        }
```

- [ ] **Step 3: Add the `apply_config_change` arm**

In `src/server.rs`, in `apply_config_change` (line 352), after the
`"rename_untracked_files"` arm (lines 387-392), add:

```rust
            "add_result_review" => {
                if (!matches!(value, "always" | "never")) {
                    return Err(anyhow::anyhow!("add_result_review must be: always | never"));
                }
                self.config.add_result_review = value.to_string();
            }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test config::`
Expected: PASS, all 3 tests. `cargo build` also passes (serde default keeps old config
files loading cleanly, and `Config::load()` rewrites the file with the new key present).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/server.rs
git commit -m "config: add_result_review preference with sanitize and setconfig arms"
```

---

## Task 6: add-result review overlay

**Files:**
- Modify: `src/tui.rs`

Spec sections C1, C2, and the client half of C3. `dispatch_add_options` collects a
per-entry outcome in dispatch order (entries are already glob-expanded by task 3), the
overlay renders with the same modal recipe as `draw_rename_confirm`, and it slots into the
input ladder directly after `state.prompt` and before `state.priority_step`, so review
happens first and dismissing it reveals the organize step queued underneath. The existing
one-line summary in `state.error` is kept in both modes.

- [ ] **Step 1: Write the failing tests for `middle_truncate`**

Add to `src/tui.rs` (final placement: right after the `middle_truncate` function added in
step 5):

```rust
#[cfg(test)]
mod middle_truncate_tests {
    use super::middle_truncate;

    #[test]
    fn short_text_passes_through() {
        assert_eq!(middle_truncate("abc", 10), "abc");
        assert_eq!(middle_truncate("abcdefghij", 10), "abcdefghij");
    }

    #[test]
    fn long_text_keeps_head_and_tail() {
        assert_eq!(middle_truncate("abcdefghijk", 7), "abc…ijk");
    }

    #[test]
    fn tiny_budget_degrades_to_ellipsis() {
        assert_eq!(middle_truncate("abcdef", 1), "…");
        assert_eq!(middle_truncate("abcdef", 0), "…");
    }

    #[test]
    fn unicode_counts_chars_not_bytes() {
        assert_eq!(middle_truncate("日本語テスト表示", 5), "日本…表示");
    }
}
```

Run: `cargo test middle_truncate`
Expected: FAIL to compile (`middle_truncate` does not exist yet).

- [ ] **Step 2: Add the structs and `AppState` fields**

In `src/tui.rs`, directly below the `AddOptionsForm` struct (line 1041), insert:

```rust
/// one dispatched add and how it went. `source` is the uri/path exactly as
/// dispatched, post glob expansion.
struct AddResultEntry {
    source: String,
    outcome: Result<(), String>,
}

/// post-dispatch review overlay. entries stay in dispatch order, which
/// equals add order plus glob-expansion order by construction.
struct AddResultsReview {
    entries: Vec<AddResultEntry>,
    focused: usize,
}
```

In `struct AppState` (line 1131), after the `priority_step` field, add:

```rust
    /// per-entry results overlay shown after dispatching adds. None when
    /// dismissed or when add_result_review is "never".
    add_results: Option<AddResultsReview>,
    /// mirror of config's add_result_review ("always" | "never"). read once
    /// at startup, flipped in memory when ctrl+d persists "never" so the
    /// very next add this session already skips the overlay.
    add_result_review: String,
```

In `AppState::new` (line 1223), extend the config tuple. Replace:

```rust
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
```

with:

```rust
        let (show_sidebar, show_detail, configured_columns, nerd_font, configured_widths, add_result_review) =
            Config::load()
                .map(|config| (
                    config.tui_show_sidebar,
                    config.tui_show_detail,
                    config.tui_columns,
                    config.tui_nerd_font,
                    config.tui_column_widths,
                    config.add_result_review,
                ))
                .unwrap_or((
                    false,
                    false,
                    Vec::new(),
                    false,
                    std::collections::BTreeMap::new(),
                    "always".to_string(),
                ));
```

And in the `Self { ... }` literal at the end of `AppState::new`, after
`priority_step: None,`, add:

```rust
            add_results: None,
            add_result_review,
```

- [ ] **Step 3: Collect outcomes in `dispatch_add_options`**

Replace `dispatch_add_options` (line 2558) entirely with:

```rust
fn dispatch_add_options(form: AddOptionsForm, state: &mut AppState) {
    let mut succeeded: usize = 0;
    let mut failures: Vec<String> = Vec::new();
    let mut results: Vec<AddResultEntry> = Vec::new();
    let mut organize_indices: Vec<usize> = Vec::new();
    let mut organize_entries: Vec<String> = Vec::new();
    let mut organize_resume: Vec<bool> = Vec::new();
    for (entry_index, uri) in form.entries.iter().enumerate() {
        let options = &form.options[entry_index];
        let save_path = if (options.save_path.trim().is_empty()) { None } else { Some(options.save_path.clone()) };
        // always add paused: the organize step must run before any data
        // downloads, regardless of the user's requested start/pause option.
        // `options.start` is remembered below and applied once the step
        // concludes for this entry.
        let added_id = match client::send(Request::Add {
            uri: uri.clone(),
            save_path,
            category: None,
            start_paused: true,
            content_layout: options.content_layout,
        }) {
            Ok(Response::Added { id }) => Some(id),
            Ok(Response::Err(message)) => {
                failures.push(format!("{}: {}", uri, message));
                results.push(AddResultEntry { source: uri.clone(), outcome: Err(message) });
                None
            }
            Ok(_) => {
                failures.push(format!("{}: unexpected response", uri));
                results.push(AddResultEntry { source: uri.clone(), outcome: Err("unexpected response".to_string()) });
                None
            }
            Err(error) => {
                failures.push(format!("{}: {}", uri, error));
                results.push(AddResultEntry { source: uri.clone(), outcome: Err(error.to_string()) });
                None
            }
        };
        if (added_id.is_none()) { continue; }
        results.push(AddResultEntry { source: uri.clone(), outcome: Ok(()) });
        succeeded += 1;
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
        organize_indices.push(new_index);
        organize_entries.push(uri.clone());
        organize_resume.push(options.start);
    }
    // one-line summary stays in both modes: it is the status line and it
    // keeps the status bar meaningful after the overlay closes
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
    // per-entry review overlay, unless the user opted out permanently. the
    // organize step is created in the same call; the overlay outranks it in
    // the input ladder, so review happens first and dismissing reveals the
    // step already queued underneath.
    if (state.add_result_review != "never" && !results.is_empty()) {
        state.add_results = Some(AddResultsReview { entries: results, focused: 0 });
    }
    if (!organize_indices.is_empty()) {
        state.priority_step = Some(Box::new(PriorityStep::new(organize_entries, organize_indices, organize_resume)));
    }
}
```

- [ ] **Step 4: Key handler and ladder slot**

Directly below `dispatch_add_options`, add:

```rust
/// key handler for the add-results review overlay. returns true to quit.
fn handle_add_results_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    let Some(review) = state.add_results.as_mut() else { return false; };
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        // ctrl+d: dismiss all, close, and persist the opt-out. flipping the
        // in-memory mirror makes the very next add this session skip the
        // overlay too. re-enabling is a config.toml edit, symmetric with
        // how rename_merge_same = "always" is undone.
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            state.add_results = None;
            state.add_result_review = "never".to_string();
            let _ = submit_set("add_result_review", "never");
        }
        // shift+d and esc: dismiss all, close (esc exits every modal in the app)
        (KeyCode::Char('D'), _) | (KeyCode::Esc, _) => {
            state.add_results = None;
        }
        // enter/d: dismiss the focused entry; removing the last one closes
        (KeyCode::Enter, _) | (KeyCode::Char('d'), KeyModifiers::NONE) => {
            review.entries.remove(review.focused);
            if (review.entries.is_empty()) {
                state.add_results = None;
            } else if (review.focused >= review.entries.len()) {
                review.focused = review.entries.len() - 1;
            }
        }
        (KeyCode::Char('s'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
            if (review.focused + 1 < review.entries.len()) { review.focused += 1; }
        }
        (KeyCode::Char('w'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
            review.focused = review.focused.saturating_sub(1);
        }
        _ => {}
    }
    false
}
```

In the input-routing ladder in `run()` (lines 1530-1536), replace:

```rust
                    } else if (state.prompt.is_some()) {
                        handle_prompt_key(key.code, key.modifiers, &mut state)
                    } else if (state.priority_step.is_some()) {
```

with:

```rust
                    } else if (state.prompt.is_some()) {
                        handle_prompt_key(key.code, key.modifiers, &mut state)
                    } else if (state.add_results.is_some()) {
                        handle_add_results_key(key.code, key.modifiers, &mut state)
                    } else if (state.priority_step.is_some()) {
```

- [ ] **Step 5: `middle_truncate` + `draw_add_results` + draw ordering**

Directly above `draw_rename_confirm` (line 4094), add:

```rust
/// truncate `text` to `max_chars` by cutting the middle and inserting an
/// ellipsis, keeping head and tail visible (paths differ at both ends).
fn middle_truncate(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if (count <= max_chars) { return text.to_string(); }
    if (max_chars <= 1) { return "…".to_string(); }
    let keep = max_chars - 1;
    let head_length = keep - keep / 2;
    let tail_length = keep / 2;
    let head: String = text.chars().take(head_length).collect();
    let tail: String = text.chars().skip(count - tail_length).collect();
    format!("{}…{}", head, tail)
}

fn draw_add_results(frame: &mut ratatui::Frame, state: &AppState) {
    let Some(review) = &state.add_results else { return; };
    let area = frame.area();
    let height = ((review.entries.len() as u16) + 4).clamp(6, area.height.saturating_sub(2));
    let width = (area.width * 70 / 100).clamp(50, area.width.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect { x, y, width, height };

    frame.render_widget(ratatui::widgets::Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" add results ");
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let layout = Layout::vertical([
        Constraint::Min(1),    // entry rows
        Constraint::Length(1), // hint
    ])
    .split(inner);

    // scroll window that keeps the focused row visible
    let visible = layout[0].height as usize;
    let first = if (visible == 0 || review.focused < visible) { 0 } else { review.focused + 1 - visible };
    let rows: Vec<Line> = review.entries.iter().enumerate()
        .skip(first)
        .take(visible.max(1))
        .map(|(index, entry)| {
            let marker = if (index == review.focused) { "› " } else { "  " };
            let (verdict, verdict_color) = match &entry.outcome {
                Ok(()) => ("ok  ", Color::Green),
                Err(_) => ("fail", Color::Red),
            };
            let reason = match &entry.outcome {
                Ok(()) => String::new(),
                Err(message) => format!("  {}", message),
            };
            let source_budget = (layout[0].width as usize)
                .saturating_sub(2 + 5 + reason.chars().count())
                .max(8);
            let mut spans = vec![
                Span::styled(marker, Style::default().fg(Color::Yellow)),
                Span::styled(verdict, Style::default().fg(verdict_color)),
                Span::raw(" "),
                Span::raw(middle_truncate(&entry.source, source_budget)),
            ];
            if (!reason.is_empty()) {
                spans.push(Span::styled(reason, Style::default().fg(Color::Red)));
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), layout[0]);

    let hint = Line::from(vec![
        Span::styled(" w/s ", Style::default().fg(Color::Yellow)),
        Span::raw("move  "),
        Span::styled("enter/d ", Style::default().fg(Color::Yellow)),
        Span::raw("dismiss  "),
        Span::styled("shift+d ", Style::default().fg(Color::Yellow)),
        Span::raw("dismiss all  "),
        Span::styled("ctrl+d ", Style::default().fg(Color::Yellow)),
        Span::raw("never show again  "),
        Span::styled("esc ", Style::default().fg(Color::Yellow)),
        Span::raw("close"),
    ]);
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::Gray)),
        layout[1],
    );
}
```

Place the `mod middle_truncate_tests` block from step 1 directly after `middle_truncate`.

Then mirror the ladder in `draw()` (line 3587). Draw order is bottom-to-top, so the
overlay renders after the priority step but before the prompt. Replace the top of `draw()`
with:

```rust
fn draw(frame: &mut ratatui::Frame, state: &mut AppState) {
    if (state.priority_step.is_some()) {
        draw_priority_step(frame, state);
        // review overlay outranks the organize step in the input ladder,
        // so it also renders on top of it
        if (state.add_results.is_some()) {
            draw_add_results(frame, state);
        }
        // renames opened from inside the step use the shared prompt overlay
        if (state.prompt.is_some()) {
            draw_prompt(frame, state);
        }
        if (state.rename_confirm.is_some()) {
            draw_rename_confirm(frame, state);
        }
        return;
    }
```

and in the non-priority-step path below it, insert before the `if (state.prompt.is_some())`
block:

```rust
    if (state.add_results.is_some()) {
        draw_add_results(frame, state);
    }
```

- [ ] **Step 6: Run tests and build**

Run: `cargo test middle_truncate`
Expected: PASS, all 4 tests.

Run: `cargo build`
Expected: PASS.

Manual spot check (daemon running):
- add a mixed batch: one real local file, one glob over several files, one garbage magnet
  (`magnet:?xt=urn:btih:zzzz`). The overlay lists every entry in add plus expansion order
  with green ok / red fail markers and the failure reason on the failed row.
- w/s and arrows move focus; enter/d removes one row; removing the last closes the overlay
  and reveals the organize step for the successful adds; shift+d closes at once; esc same.
- ctrl+d closes, writes `add_result_review = "never"` into config.toml (check the file),
  and the next add in the same session shows only the one-line summary.
- set `add_result_review = "always"` back in config.toml, restart the daemon and tui, and
  confirm the overlay returns.

- [ ] **Step 7: Commit**

```bash
git add src/tui.rs
git commit -m "tui: add-result review overlay"
```

---

## Task 7: final verification pass

**Files:** none (verification only)

- [ ] **Step 1: Full build**

Run: `cargo build`
Expected: PASS with no warnings (every new function from tasks 1-6 is in active use).

- [ ] **Step 2: Full test suite**

Run: `cargo test`
Expected: PASS. New tests from this plan: `sources::tests` (12), `config::tests` (3),
`line_indicator_tests` (5), `middle_truncate_tests` (4), plus every pre-existing test.

- [ ] **Step 3: Clippy**

Run: `cargo clippy --all-targets -- -D warnings` (check CI/packaging config first for the
project's actual clippy invocation and lint level, and match that instead if it differs)
Expected: PASS.

- [ ] **Step 4: Full manual pass through the spec's testing checklist**

Re-run every manual check in the "Testing" section of
`docs/superpowers/specs/2026-07-01-add-validation-design.md` end to end, in particular the
cross-cutting ones: the indicator updates only for the edited line; multi-line paste
classifies each pasted line; submit with a zero-match glob or missing file keeps the
prompt open with the error; a glob line expands into per-entry options passes and
dispatches in order; the results overlay lists entries in add plus expansion order with
correct ok/fail reasons; enter/d, shift+d, ctrl+d, and esc behave per the spec's key
table; ctrl+d persists and the same session's next add skips the overlay; dismissing the
overlay reveals the organize step for the successful adds.

- [ ] **Step 5: Commit (only if step 3 required fixes; otherwise nothing to commit)**

```bash
git add -A
git commit -m "fix clippy warnings from the add-validation work"
```
