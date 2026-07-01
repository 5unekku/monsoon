# Universal Input Widget and Content Organization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace every hand-rolled text buffer in `src/tui.rs` with one shared `TextField`
widget (real cursor position, word movement/deletion, paste, generalized tab-completion),
unify the add-time `PriorityStep` with the live Content tab into one component (fixing a
confirmed silent-failure bug), make every torrent add go through an organize step before it
starts, and add a persisted "default layout" snapshot with a revert action.

**Architecture:** A new `src/textfield.rs` module owns all cursor/completion logic as pure,
unit-tested functions with no `ratatui`/`crossterm` dependency. Every text-entry surface in
`src/tui.rs` (`Prompt`, `AddOptionsForm`, `TextInput`, `SettingsState`) is converted, one at a
time, from a raw `String`/`Option<String>` buffer to a `TextField`/`Vec<TextField>`. The
add-time `PriorityStep` and the live Content tab are unified onto one rename/priority code
path. Server-side (`src/server.rs`, `src/ipc.rs`) gains two new IPC requests and one new
persisted field, reusing the two-phase rename-confirmation machinery already in the codebase
(`RenameConfirmation`/`RenameDecisions`) rather than inventing a new one.

**Tech Stack:** Rust 2021, `ratatui`/`crossterm` (TUI), libtorrent via the existing `cxx`
bridge, `serde`/`toml` for persistence, `arboard` for clipboard (already a dependency).

**Spec:** `docs/superpowers/specs/2026-06-30-universal-input-and-content-organization-design.md`

---

## Parallelization plan

For engineers running this with `subagent-driven-development` / worktree isolation:

```
Task 1 ─┬─ Task 2 ─┬─ Task 3 ─┬─ Task 4 ─── Task 5
        │          │          │
        └──────────┴──────────┴─────────────┐
                                              ▼
Task 12 (independent) ───────────────────►  Task 6 ── Task 7 ── Task 8 ── Task 9 ── Task 10 ── Task 11
                                              │                                              │
                                              └──────────────────┬───────────────────────────┘
                                                                  ▼
                                                        Task 13 ── Task 14 ── Task 15
                                                                  │
                                    ┌─────────────────────────────┴─────────────────────────┐
                                    ▼                                                        ▼
                        Task 16 ── Task 18 (server, src/server.rs + src/ipc.rs)   Task 19 (client, src/tui.rs)
                                    │                                                        │
                                    └─────────────────────────────┬──────────────────────────┘
                                                                   ▼
                                                                Task 20
```

- **Tasks 1-5** (`src/textfield.rs`) have no dependency on the rest of the codebase — start
  immediately, in their own worktree.
- **Task 12** (priority-key remap, two isolated match arms in `src/tui.rs`) has no dependency
  on `TextField` at all — safe to run concurrently with Tasks 1-5 in a second worktree. Merge
  it before Task 6 starts so Task 13 doesn't touch stale key-match code.
- **Tasks 6-11** all depend on Task 5 (need `TextField`/`CompletionSource` to exist) and all
  touch `src/tui.rs`, so even though each task is independently testable, merge them
  sequentially (6→7→8→9→10→11) rather than fanning out to separate worktrees — five
  concurrent editors of the same file guarantees repeated conflict resolution that costs more
  than the parallelism saves.
- **Tasks 13-15** depend on Task 6 (Prompt-based rename) and Task 12 (priority keys) landing.
- **Tasks 16-18** (server: `src/server.rs`, `src/ipc.rs`) and **Task 19** (client:
  `src/tui.rs`) both depend on Task 15 defining the exact call sites, but touch disjoint files
  from that point on — **this is the real second parallel opportunity**: one worktree does
  16→18, another does 19, then merge both before Task 20.
- **Task 20** (final build/clippy/manual pass) runs after everything merges.

---

## Task 1: `TextField` core — struct, cursor movement, insert/delete

**Files:**
- Create: `src/textfield.rs`
- Modify: `src/main.rs:10` (insert `mod textfield;` alphabetically between `mod tags` — there
  is none, so insert between `mod sources;` (line 14) and `mod tui;` (line 15))

- [ ] **Step 1: Write the failing tests**

Create `src/textfield.rs`:

```rust
//! shared text-input widget used by every path/URL/text-entry surface in the
//! TUI. owns cursor position (char index, not byte index — unicode paths
//! must not panic on split) and a `CompletionSource` describing what, if
//! anything, Tab should complete against.

#[derive(Clone, Debug, PartialEq)]
pub enum CompletionSource {
    /// no completion behavior. still gets full cursor movement.
    None,
    /// tab-complete against the real filesystem.
    Filesystem,
    /// tab-complete against a fixed candidate list (e.g. a torrent's own
    /// sibling folder names) rather than the OS filesystem.
    SiblingFolders(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct TextField {
    buffer: String,
    cursor: usize,
    completion: CompletionSource,
}

impl TextField {
    pub fn new(initial: impl Into<String>) -> Self {
        let buffer: String = initial.into();
        let cursor = buffer.chars().count();
        Self { buffer, cursor, completion: CompletionSource::None }
    }

    pub fn with_completion(initial: impl Into<String>, completion: CompletionSource) -> Self {
        let mut field = Self::new(initial);
        field.completion = completion;
        field
    }

    pub fn buffer(&self) -> &str { &self.buffer }
    pub fn cursor(&self) -> usize { self.cursor }
    pub fn completion_source(&self) -> CompletionSource { self.completion.clone() }
    pub fn set_completion(&mut self, completion: CompletionSource) { self.completion = completion; }

    fn char_len(&self) -> usize { self.buffer.chars().count() }

    fn byte_offset(&self, char_index: usize) -> usize {
        self.buffer.char_indices().nth(char_index).map(|(byte_index, _)| byte_index).unwrap_or(self.buffer.len())
    }

    pub fn set_cursor(&mut self, cursor: usize) { self.cursor = cursor.min(self.char_len()); }

    pub fn insert_char(&mut self, character: char) {
        let byte_index = self.byte_offset(self.cursor);
        self.buffer.insert(byte_index, character);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if (self.cursor == 0) { return; }
        let end = self.byte_offset(self.cursor);
        let start = self.byte_offset(self.cursor - 1);
        self.buffer.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete_forward(&mut self) {
        if (self.cursor >= self.char_len()) { return; }
        let start = self.byte_offset(self.cursor);
        let end = self.byte_offset(self.cursor + 1);
        self.buffer.replace_range(start..end, "");
    }

    pub fn move_left(&mut self) { self.cursor = self.cursor.saturating_sub(1); }
    pub fn move_right(&mut self) { self.cursor = (self.cursor + 1).min(self.char_len()); }
    pub fn move_home(&mut self) { self.cursor = 0; }
    pub fn move_end(&mut self) { self.cursor = self.char_len(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_field_starts_with_cursor_at_end() {
        let field = TextField::new("ab");
        assert_eq!(field.cursor(), 2);
    }

    #[test]
    fn insert_at_cursor_not_just_append() {
        let mut field = TextField::new("ac");
        field.set_cursor(1);
        field.insert_char('b');
        assert_eq!(field.buffer(), "abc");
        assert_eq!(field.cursor(), 2);
    }

    #[test]
    fn backspace_removes_before_cursor_and_moves_it_back() {
        let mut field = TextField::new("abc");
        field.set_cursor(2);
        field.backspace();
        assert_eq!(field.buffer(), "ac");
        assert_eq!(field.cursor(), 1);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut field = TextField::new("abc");
        field.set_cursor(0);
        field.backspace();
        assert_eq!(field.buffer(), "abc");
        assert_eq!(field.cursor(), 0);
    }

    #[test]
    fn delete_forward_removes_after_cursor_without_moving_it() {
        let mut field = TextField::new("abc");
        field.set_cursor(1);
        field.delete_forward();
        assert_eq!(field.buffer(), "ac");
        assert_eq!(field.cursor(), 1);
    }

    #[test]
    fn delete_forward_at_end_is_noop() {
        let mut field = TextField::new("abc");
        field.set_cursor(3);
        field.delete_forward();
        assert_eq!(field.buffer(), "abc");
    }

    #[test]
    fn cursor_movement_clamps_at_both_ends() {
        let mut field = TextField::new("ab");
        field.move_right();
        assert_eq!(field.cursor(), 2);
        field.move_left();
        field.move_left();
        field.move_left();
        assert_eq!(field.cursor(), 0);
    }

    #[test]
    fn home_and_end() {
        let mut field = TextField::new("abc");
        field.move_home();
        assert_eq!(field.cursor(), 0);
        field.move_end();
        assert_eq!(field.cursor(), 3);
    }

    #[test]
    fn unicode_insert_and_backspace_do_not_panic_and_count_chars_not_bytes() {
        let mut field = TextField::new("日本語");
        assert_eq!(field.cursor(), 3);
        field.backspace();
        assert_eq!(field.buffer(), "日本");
        field.set_cursor(0);
        field.insert_char('★');
        assert_eq!(field.buffer(), "★日本");
        assert_eq!(field.cursor(), 1);
    }
}
```

- [ ] **Step 2: Register the module**

In `src/main.rs`, change:
```rust
mod sources;
mod tui;
```
to:
```rust
mod sources;
mod textfield;
mod tui;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --lib textfield::`
Expected: all tests pass (this is a new module with no external dependents yet, so `cargo
build` also succeeds trivially).

- [ ] **Step 4: Commit**

```bash
git add src/textfield.rs src/main.rs
git commit -m "textfield: add core TextField struct with real cursor position"
```

---

## Task 2: word-boundary movement and deletion

**Files:**
- Modify: `src/textfield.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/textfield.rs`:

```rust
    #[test]
    fn move_word_left_skips_separators_then_word() {
        let mut field = TextField::new("foo/bar baz");
        field.set_cursor(11); // end
        field.move_word_left();
        assert_eq!(field.cursor(), 8); // start of "baz"
        field.move_word_left();
        assert_eq!(field.cursor(), 4); // start of "bar"
        field.move_word_left();
        assert_eq!(field.cursor(), 0); // start of "foo"
    }

    #[test]
    fn move_word_right_skips_word_then_separators() {
        let mut field = TextField::new("foo/bar baz");
        field.set_cursor(0);
        field.move_word_right();
        assert_eq!(field.cursor(), 3); // end of "foo", before '/'
        field.move_word_right();
        assert_eq!(field.cursor(), 7); // end of "bar", before ' '
        field.move_word_right();
        assert_eq!(field.cursor(), 11); // end of "baz"
    }

    #[test]
    fn delete_word_backward_removes_the_word_behind_cursor() {
        let mut field = TextField::new("foo/bar");
        field.set_cursor(7);
        field.delete_word_backward();
        assert_eq!(field.buffer(), "foo/");
        assert_eq!(field.cursor(), 4);
    }

    #[test]
    fn delete_word_forward_removes_the_word_ahead_of_cursor() {
        let mut field = TextField::new("foo/bar");
        field.set_cursor(4);
        field.delete_word_forward();
        assert_eq!(field.buffer(), "foo/");
        assert_eq!(field.cursor(), 4);
    }

    #[test]
    fn word_operations_at_buffer_edges_are_noop_not_panic() {
        let mut field = TextField::new("abc");
        field.set_cursor(0);
        field.move_word_left();
        assert_eq!(field.cursor(), 0);
        field.delete_word_backward();
        assert_eq!(field.buffer(), "abc");
        field.set_cursor(3);
        field.move_word_right();
        assert_eq!(field.cursor(), 3);
        field.delete_word_forward();
        assert_eq!(field.buffer(), "abc");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib textfield::`
Expected: FAIL — `move_word_left`, `move_word_right`, `delete_word_backward`,
`delete_word_forward` not found.

- [ ] **Step 3: Implement**

Add to `impl TextField` in `src/textfield.rs`:

```rust
    pub fn move_word_left(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut index = self.cursor;
        while (index > 0 && !is_word_char(chars[index - 1])) { index -= 1; }
        while (index > 0 && is_word_char(chars[index - 1])) { index -= 1; }
        self.cursor = index;
    }

    pub fn move_word_right(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let length = chars.len();
        let mut index = self.cursor;
        while (index < length && !is_word_char(chars[index])) { index += 1; }
        while (index < length && is_word_char(chars[index])) { index += 1; }
        self.cursor = index;
    }

    pub fn delete_word_backward(&mut self) {
        let original_cursor = self.cursor;
        self.move_word_left();
        let new_cursor = self.cursor;
        let byte_start = self.byte_offset(new_cursor);
        let byte_end = self.byte_offset(original_cursor);
        self.buffer.replace_range(byte_start..byte_end, "");
        // cursor is already at new_cursor
    }

    pub fn delete_word_forward(&mut self) {
        let original_cursor = self.cursor;
        self.move_word_right();
        let boundary = self.cursor;
        self.cursor = original_cursor;
        let byte_start = self.byte_offset(original_cursor);
        let byte_end = self.byte_offset(boundary);
        self.buffer.replace_range(byte_start..byte_end, "");
    }
```

Add a free function near the top of the file (outside `impl TextField`):

```rust
fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib textfield::`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/textfield.rs
git commit -m "textfield: add word-boundary movement and deletion"
```

---

## Task 3: paste and filesystem tab-completion

**Files:**
- Modify: `src/textfield.rs`
- Modify: `src/tui.rs:3318-3362` (remove `tab_complete_path` and `longest_common_prefix_ci` —
  moved into `textfield.rs`)

- [ ] **Step 1: Write the failing tests**

Add to `src/textfield.rs` tests:

```rust
    #[test]
    fn paste_inserts_at_cursor_and_strips_newlines() {
        let mut field = TextField::new("ac");
        field.set_cursor(1);
        field.paste("b\nX\r\n");
        assert_eq!(field.buffer(), "abXc");
    }

    #[test]
    fn longest_common_prefix_ci_matches_case_insensitively() {
        assert_eq!(longest_common_prefix_ci(&["Downloads/Foo", "downloads/Bar"]), "Downloads/");
        assert_eq!(longest_common_prefix_ci(&[]), "");
        assert_eq!(longest_common_prefix_ci(&["abc"]), "abc");
    }

    #[test]
    fn filesystem_completion_only_expands_the_prefix_before_cursor() {
        let temp_dir = std::env::temp_dir().join(format!("textfield-test-{}", std::process::id()));
        std::fs::create_dir_all(temp_dir.join("downloads")).unwrap();
        let mut field = TextField::with_completion(
            format!("{}/down TAIL", temp_dir.display()),
            CompletionSource::Filesystem,
        );
        // put the cursor right after "down", before the space
        let prefix_char_len = format!("{}/down", temp_dir.display()).chars().count();
        field.set_cursor(prefix_char_len);
        field.tab_complete();
        assert!(field.buffer().starts_with(&format!("{}/downloads/", temp_dir.display())));
        assert!(field.buffer().ends_with(" TAIL"));
        std::fs::remove_dir_all(&temp_dir).ok();
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib textfield::`
Expected: FAIL — `paste`, `longest_common_prefix_ci`, `tab_complete` not found.

- [ ] **Step 3: Implement**

Add to `impl TextField`:

```rust
    pub fn paste(&mut self, text: &str) {
        for character in text.chars() {
            if (character != '\n' && character != '\r') {
                self.insert_char(character);
            }
        }
    }

    pub fn tab_complete(&mut self) {
        match self.completion.clone() {
            CompletionSource::None => {}
            CompletionSource::Filesystem => self.tab_complete_filesystem(),
            CompletionSource::SiblingFolders(candidates) => self.tab_complete_candidates(&candidates),
        }
    }

    fn tab_complete_filesystem(&mut self) {
        let byte_cursor = self.byte_offset(self.cursor);
        let before = self.buffer[..byte_cursor].to_string();
        let after = self.buffer[byte_cursor..].to_string();
        let completed = complete_filesystem_path(&before);
        if (completed == before) { return; }
        self.cursor = completed.chars().count();
        self.buffer = completed;
        self.buffer.push_str(&after);
    }

    fn tab_complete_candidates(&mut self, candidates: &[String]) {
        let prefix_lc = self.buffer.to_lowercase();
        let matches: Vec<&str> = candidates.iter()
            .map(|candidate| candidate.as_str())
            .filter(|candidate| candidate.to_lowercase().starts_with(&prefix_lc))
            .collect();
        let completed = match matches.len() {
            0 => return,
            1 => matches[0].to_string(),
            _ => longest_common_prefix_ci(&matches),
        };
        if (completed.chars().count() <= self.char_len()) { return; }
        self.buffer = completed;
        self.cursor = self.char_len();
    }
```

Move `tab_complete_path` out of `src/tui.rs` (delete it from there) and into `src/textfield.rs`
as a free function, renamed to make its narrower contract (it now only ever receives the
text *before* the cursor, not necessarily a whole path) explicit:

```rust
/// expand `prefix` to the longest common prefix of all filesystem entries
/// whose name starts with the partial name after the last `/` in `prefix`.
/// `prefix` is everything before the cursor, not necessarily the whole
/// buffer — `TextField::tab_complete_filesystem` splices the result back in
/// front of whatever was after the cursor.
fn complete_filesystem_path(prefix: &str) -> String {
    let (dir_part, partial) = match prefix.rfind('/') {
        None => (".", prefix),
        Some(index) => (&prefix[..=index], &prefix[index + 1..]),
    };
    let Ok(entries) = std::fs::read_dir(dir_part) else { return prefix.to_string(); };
    let partial_lc = partial.to_lowercase();
    let mut candidates: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if (!name.to_lowercase().starts_with(&partial_lc)) { return None; }
            let is_dir = entry.file_type().map(|file_type| file_type.is_dir()).unwrap_or(false);
            let candidate = if (dir_part == ".") {
                if (is_dir) { format!("{}/", name) } else { name }
            } else if (is_dir) {
                format!("{}{}/", dir_part, name)
            } else {
                format!("{}{}", dir_part, name)
            };
            Some(candidate)
        })
        .collect();
    candidates.sort();
    match candidates.len() {
        0 => prefix.to_string(),
        1 => candidates.remove(0),
        _ => longest_common_prefix_ci(&candidates.iter().map(|s| s.as_str()).collect::<Vec<_>>()),
    }
}

pub fn longest_common_prefix_ci(paths: &[&str]) -> String {
    let Some(first) = paths.first() else { return String::new(); };
    let first_chars: Vec<char> = first.chars().collect();
    let mut common = first_chars.len();
    for path in &paths[1..] {
        let count = first_chars.iter().zip(path.chars())
            .take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
            .count();
        common = common.min(count);
        if (common == 0) { break; }
    }
    first_chars[..common].iter().collect()
}
```

In `src/tui.rs`, delete the old `tab_complete_path` (lines 3318-3348) and `longest_common_prefix_ci`
(lines 3350-3362) entirely, and delete the now-orphaned call in `tab_complete_content_filter`'s
sibling usage — that function calls `longest_common_prefix_ci` too (`src/tui.rs:3304`), so add
`use crate::textfield::longest_common_prefix_ci;` near the top of `src/tui.rs`'s import block
instead of leaving a dangling reference.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib textfield::`
Expected: PASS. Then run `cargo check` to confirm `src/tui.rs` still compiles with the import
fix (it will still have compile errors from the not-yet-converted `Prompt`/etc. structs if you
look ahead, but at this point in the plan those haven't been touched yet, so `tab_complete_path`'s
removal only affects the one call site fixed above).

- [ ] **Step 5: Commit**

```bash
git add src/textfield.rs src/tui.rs
git commit -m "textfield: add paste and filesystem tab-completion; move path completion out of tui.rs"
```

---

## Task 4: sibling-folder completion source

Already implemented as part of Task 3 (`tab_complete_candidates` handles
`CompletionSource::SiblingFolders`). This task adds the tests confirming its exact matching
behavior (case-insensitivity, longest-common-prefix on multiple matches, no-op when nothing
matches or the buffer is already the longest match).

**Files:**
- Modify: `src/textfield.rs`

- [ ] **Step 1: Write the tests**

```rust
    #[test]
    fn sibling_folder_completion_single_match() {
        let mut field = TextField::with_completion(
            "Sea",
            CompletionSource::SiblingFolders(vec!["Season 1".to_string(), "Extras".to_string()]),
        );
        field.tab_complete();
        assert_eq!(field.buffer(), "Season 1");
    }

    #[test]
    fn sibling_folder_completion_case_insensitive() {
        let mut field = TextField::with_completion(
            "sea",
            CompletionSource::SiblingFolders(vec!["Season 1".to_string()]),
        );
        field.tab_complete();
        assert_eq!(field.buffer(), "Season 1");
    }

    #[test]
    fn sibling_folder_completion_multiple_matches_uses_common_prefix() {
        let mut field = TextField::with_completion(
            "Sea",
            CompletionSource::SiblingFolders(vec!["Season 1".to_string(), "Season 2".to_string()]),
        );
        field.tab_complete();
        assert_eq!(field.buffer(), "Season ");
    }

    #[test]
    fn sibling_folder_completion_no_match_is_noop() {
        let mut field = TextField::with_completion(
            "Zzz",
            CompletionSource::SiblingFolders(vec!["Season 1".to_string()]),
        );
        field.tab_complete();
        assert_eq!(field.buffer(), "Zzz");
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib textfield::`
Expected: PASS (implementation already exists from Task 3).

- [ ] **Step 3: Commit**

```bash
git add src/textfield.rs
git commit -m "textfield: test sibling-folder completion matching rules"
```

---

## Task 5: shared cursor-rendering helper

Every place that draws a `TextField` needs to render the cursor at its actual position (today,
every draw site appends a `"█"` block after the raw string, because there was no real cursor).
One shared function prevents this from being reimplemented differently at each of the five
draw sites touched in Tasks 6-11.

**Files:**
- Modify: `src/tui.rs` (add near the other small rendering helpers, e.g. just above `fn draw_prompt`)

- [ ] **Step 1: Implement**

```rust
/// render a TextField's buffer as spans with the cursor highlighted at its
/// actual position, instead of always appending a block at the end. shared
/// by every draw site so cursor rendering can't drift between them.
fn render_field_with_cursor(field: &TextField) -> Vec<Span<'static>> {
    let mut chars: Vec<char> = field.buffer().chars().collect();
    let cursor = field.cursor().min(chars.len());
    if (cursor == chars.len()) { chars.push(' '); }
    let before: String = chars[..cursor].iter().collect();
    let at: String = chars[cursor..=cursor].iter().collect();
    let after: String = chars[cursor + 1..].iter().collect();
    vec![
        Span::raw(before),
        Span::styled(at, Style::default().fg(Color::Black).bg(Color::Yellow)),
        Span::raw(after),
    ]
}
```

Add `use crate::textfield::{TextField, CompletionSource};` to `src/tui.rs`'s import block (near
the existing `use crate::ipc::{...}` line).

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles (the function is unused until Task 6 wires it up — allow the `dead_code`
warning for now; it resolves itself once Task 6 lands. If your toolchain treats warnings as
errors in this project, check `Cargo.toml`/CI config first — it doesn't, so a transient unused-
function warning here is expected and fine).

- [ ] **Step 3: Commit**

```bash
git add src/tui.rs
git commit -m "tui: add shared cursor-rendering helper for TextField"
```

---

## Task 6: convert `Prompt` to use `TextField`

This is the largest single task: `Prompt.lines: Vec<String>` becomes `Vec<TextField>`, and
`handle_prompt_key`/`draw_prompt`/`submit_prompt` all need updating.

**Files:**
- Modify: `src/tui.rs:918-940` (`Prompt` struct + impl)
- Modify: `src/tui.rs:2301-2375` (`handle_prompt_key`, replaced in full)
- Modify: `src/tui.rs:2119-2148` (`submit_prompt`'s `Add`/`SetRateLimit` arms)
- Modify: `src/tui.rs:3777-3827` (`draw_prompt`)

- [ ] **Step 1: Convert the struct**

Replace (`src/tui.rs:918-940`):

```rust
struct Prompt {
    title: String,
    helper: String,
    lines: Vec<String>,
    cursor_line: usize,
    action: PromptAction,
    torrent_index: usize,
    allow_multiline: bool,
}

impl Prompt {
    fn single_line_buffer(&self) -> String {
        self.lines.first().cloned().unwrap_or_default()
    }
}
```

with:

```rust
struct Prompt {
    title: String,
    helper: String,
    lines: Vec<TextField>,
    cursor_line: usize,
    action: PromptAction,
    torrent_index: usize,
    allow_multiline: bool,
}

impl Prompt {
    fn single_line_buffer(&self) -> String {
        self.lines.first().map(|field| field.buffer().to_string()).unwrap_or_default()
    }
}
```

- [ ] **Step 2: Replace `handle_prompt_key` in full**

Replace the entire function body (`src/tui.rs:2301-2375`) with:

```rust
fn handle_prompt_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Esc, _) => state.prompt = None,
        (KeyCode::Enter, KeyModifiers::SHIFT) => {
            if let Some(prompt) = state.prompt.as_mut() {
                if (prompt.allow_multiline) {
                    let completion = prompt.lines.get(prompt.cursor_line)
                        .map(|field| field.completion_source())
                        .unwrap_or(CompletionSource::None);
                    let insert_at = prompt.cursor_line + 1;
                    prompt.lines.insert(insert_at, TextField::with_completion(String::new(), completion));
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
                if (prompt.cursor_line > 0) {
                    let column = prompt.lines[prompt.cursor_line].cursor();
                    prompt.cursor_line -= 1;
                    prompt.lines[prompt.cursor_line].set_cursor(column);
                }
            }
        }
        (KeyCode::Down, _) => {
            if let Some(prompt) = state.prompt.as_mut() {
                if (prompt.cursor_line + 1 < prompt.lines.len()) {
                    let column = prompt.lines[prompt.cursor_line].cursor();
                    prompt.cursor_line += 1;
                    prompt.lines[prompt.cursor_line].set_cursor(column);
                }
            }
        }
        (KeyCode::Left, KeyModifiers::CONTROL) | (KeyCode::Left, KeyModifiers::ALT) => {
            if let Some(field) = current_prompt_field(state) { field.move_word_left(); }
        }
        (KeyCode::Right, KeyModifiers::CONTROL) | (KeyCode::Right, KeyModifiers::ALT) => {
            if let Some(field) = current_prompt_field(state) { field.move_word_right(); }
        }
        (KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Backspace, KeyModifiers::ALT) => {
            if let Some(field) = current_prompt_field(state) { field.delete_word_backward(); }
        }
        (KeyCode::Delete, KeyModifiers::CONTROL) | (KeyCode::Delete, KeyModifiers::ALT) => {
            if let Some(field) = current_prompt_field(state) { field.delete_word_forward(); }
        }
        (KeyCode::Left, _) => { if let Some(field) = current_prompt_field(state) { field.move_left(); } }
        (KeyCode::Right, _) => { if let Some(field) = current_prompt_field(state) { field.move_right(); } }
        (KeyCode::Home, _) => { if let Some(field) = current_prompt_field(state) { field.move_home(); } }
        (KeyCode::End, _) => { if let Some(field) = current_prompt_field(state) { field.move_end(); } }
        (KeyCode::Delete, _) => { if let Some(field) = current_prompt_field(state) { field.delete_forward(); } }
        (KeyCode::Backspace, _) => {
            if let Some(prompt) = state.prompt.as_mut() {
                let cursor_line = prompt.cursor_line;
                let at_line_start = prompt.lines.get(cursor_line)
                    .map(|field| field.buffer().is_empty() && field.cursor() == 0)
                    .unwrap_or(true);
                if (at_line_start && cursor_line > 0 && prompt.lines.len() > 1) {
                    prompt.lines.remove(cursor_line);
                    prompt.cursor_line = cursor_line - 1;
                    let end = prompt.lines[prompt.cursor_line].buffer().chars().count();
                    prompt.lines[prompt.cursor_line].set_cursor(end);
                } else if let Some(field) = prompt.lines.get_mut(cursor_line) {
                    field.backspace();
                }
            }
        }
        (KeyCode::Char('v'), KeyModifiers::CONTROL) => paste_into_prompt(state),
        (KeyCode::Tab, _) => { if let Some(field) = current_prompt_field(state) { field.tab_complete(); } }
        (KeyCode::Char(character), modifiers)
            if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
        {
            if let Some(field) = current_prompt_field(state) { field.insert_char(character); }
        }
        _ => {}
    }
    false
}

fn current_prompt_field(state: &mut AppState) -> Option<&mut TextField> {
    let prompt = state.prompt.as_mut()?;
    let cursor_line = prompt.cursor_line;
    prompt.lines.get_mut(cursor_line)
}

/// paste clipboard text into the focused prompt line. in a multi-line-capable
/// prompt, embedded newlines split into new lines (so pasting a list of paths
/// — one per line — populates the add-torrent prompt the way typing them
/// would); in a single-line prompt, newlines are stripped by `TextField::paste`.
fn paste_into_prompt(state: &mut AppState) {
    let Ok(mut clipboard) = arboard::Clipboard::new() else { return; };
    let Ok(text) = clipboard.get_text() else { return; };
    let Some(prompt) = state.prompt.as_mut() else { return; };
    if (prompt.allow_multiline && text.contains('\n')) {
        let completion = prompt.lines.get(prompt.cursor_line)
            .map(|field| field.completion_source())
            .unwrap_or(CompletionSource::None);
        let mut pieces: Vec<&str> = text.split('\n').collect();
        let first = pieces.remove(0);
        if let Some(field) = prompt.lines.get_mut(prompt.cursor_line) { field.paste(first); }
        let mut insert_at = prompt.cursor_line + 1;
        for piece in pieces {
            let piece = piece.trim_end_matches('\r').to_string();
            prompt.lines.insert(insert_at, TextField::with_completion(piece, completion.clone()));
            insert_at += 1;
        }
        prompt.cursor_line = insert_at - 1;
    } else if let Some(field) = prompt.lines.get_mut(prompt.cursor_line) {
        field.paste(&text);
    }
}
```

- [ ] **Step 3: Fix `submit_prompt`'s `Add` and `SetRateLimit` arms**

At `src/tui.rs:2123-2126` (inside `PromptAction::Add`), replace:
```rust
            let entries: Vec<String> = prompt.lines.iter()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect();
```
with:
```rust
            let entries: Vec<String> = prompt.lines.iter()
                .map(|field| field.buffer().trim().to_string())
                .filter(|line| !line.is_empty())
                .collect();
```

At `src/tui.rs:2141-2146` (inside `PromptAction::SetRateLimit`), replace:
```rust
            let download = prompt.lines.first()
                .and_then(|line| line.trim().parse::<i32>().ok())
                .unwrap_or(-1);
            let upload = prompt.lines.get(1)
                .and_then(|line| line.trim().parse::<i32>().ok())
                .unwrap_or(-1);
```
with:
```rust
            let download = prompt.lines.first()
                .and_then(|field| field.buffer().trim().parse::<i32>().ok())
                .unwrap_or(-1);
            let upload = prompt.lines.get(1)
                .and_then(|field| field.buffer().trim().parse::<i32>().ok())
                .unwrap_or(-1);
```

- [ ] **Step 4: Fix `draw_prompt`'s rendering**

At `src/tui.rs:3815-3826`, replace:
```rust
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
```
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
        Line::from(spans)
    }).collect();
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check`
Expected: errors only at the 8 `Prompt { lines: vec![...] }` construction sites (still passing
`Vec<String>` instead of `Vec<TextField>`) — that's Task 7. If any other error appears, it
means a `.lines`/`cursor_line` reference was missed; grep `\.lines\b` in `src/tui.rs` and
compare against the list gathered for Task 7 below.

- [ ] **Step 6: Commit**

```bash
git add src/tui.rs
git commit -m "tui: convert Prompt to use TextField (compiles once Task 7 fixes construction sites)"
```

---

## Task 7: assign `CompletionSource` at every `Prompt` construction site

**Files:**
- Modify: `src/tui.rs` at the 8 locations below.

- [ ] **Step 1: Convert the two path-carrying single-line prompts (worked in full)**

`open_move_prompt` (`src/tui.rs:1708-1724`), replace the `lines: vec![current],` line:
```rust
        lines: vec![TextField::with_completion(current, CompletionSource::Filesystem)],
```

`open_rename_prompt` (`src/tui.rs:1691-1706`, torrent display-name rename — not a filesystem
path), replace `lines: vec![current],`:
```rust
        lines: vec![TextField::new(current)],
```

- [ ] **Step 2: Convert the remaining 6 sites**

Apply the identical pattern (`lines: vec![String::new()]` → `lines: vec![TextField::new(String::new())]`,
or `lines: vec![TextField::with_completion(...)]` where a `CompletionSource` other than `None`
applies) at each site:

| Site (function, line) | Old | New |
|---|---|---|
| `open_content_rename_prompt`, folder arm, `src/tui.rs:1752` | `lines: vec![basename],` | `lines: vec![TextField::with_completion(basename, CompletionSource::SiblingFolders(sibling_folder_names(detail, &row.full_path)))],` — `sibling_folder_names` is implemented in Task 14; leave a `todo!("Task 14")` marker here only if implementing out of order, otherwise implement Task 14 first |
| `open_content_rename_prompt`, file arm, `src/tui.rs:1764` | `lines: vec![basename],` | `lines: vec![TextField::new(basename)],` |
| `open_rate_limit_prompt`, `src/tui.rs:1795` | `lines: vec![dl_str, ul_str],` | `lines: vec![TextField::new(dl_str), TextField::new(ul_str)],` |
| `open_add_prompt`, `src/tui.rs:1808` | `lines: vec![prefill],` | `lines: vec![TextField::with_completion(prefill, CompletionSource::Filesystem)],` |
| `open_add_tracker_prompt`, `src/tui.rs:1862` | `lines: vec![String::new()],` | `lines: vec![TextField::new(String::new())],` |
| add-feed prompt (feeds page key handler), `src/tui.rs:2904` | `lines: vec![String::new()],` | `lines: vec![TextField::new(String::new())],` |

Note: this plan sequences Task 7 after Task 6 but the sibling-folder row in the table above has
a forward dependency on Task 14's `sibling_folder_names` helper. Do Task 14 first if executing
strictly in this order matters to you, or land this one site with a temporary
`CompletionSource::None` and revisit it in Task 14 — either order produces the same end state.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: no more errors about `Prompt.lines` type mismatches.

- [ ] **Step 3: Manual smoke test**

Run: `cargo run` (or however this project is normally launched locally — check for a `run`
script/skill first), open the add-torrent prompt (`n`), type a local path, confirm arrow
keys/Home/End/Tab-completion all work, confirm Ctrl+V pastes clipboard text at the cursor.

- [ ] **Step 4: Commit**

```bash
git add src/tui.rs
git commit -m "tui: assign CompletionSource at every Prompt construction site"
```

---

## Task 8: convert `AddOptionsForm.edit_buffer`

**Files:**
- Modify: `src/tui.rs:1032-1042` (`AddOptionsForm` struct)
- Modify: `src/tui.rs:2211-2229` (`handle_add_options_key`'s edit-mode branch)
- Modify: `src/tui.rs:2278` (`activate_add_options_field`, field 4)
- Modify: `src/tui.rs:3581-3587` (`draw_add_options_form`'s save_path rendering)
- Modify: `src/tui.rs:2136` (`PromptAction::Add` handler's `AddOptionsForm { edit_buffer: None, ... }`)

- [ ] **Step 1: Convert the struct field**

In `AddOptionsForm` (`src/tui.rs:1032-1042`), change:
```rust
    edit_buffer: Option<String>,
```
to:
```rust
    edit_buffer: Option<TextField>,
```

- [ ] **Step 2: Rewrite the edit-mode key handler**

Replace `src/tui.rs:2211-2229` (`if (form.edit_buffer.is_some()) { ... }` block header through
its closing `}`) — this is the block right after
`fn handle_add_options_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {`
— with:

```rust
    if (form.edit_buffer.is_some()) {
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Esc, _) => form.edit_buffer = None,
            (KeyCode::Enter, _) => {
                let buffer = form.edit_buffer.take().map(|field| field.buffer().to_string()).unwrap_or_default();
                if let Some(options) = form.options.get_mut(form.current) {
                    options.save_path = buffer;
                }
            }
            (KeyCode::Left, KeyModifiers::CONTROL) | (KeyCode::Left, KeyModifiers::ALT) => {
                if let Some(field) = form.edit_buffer.as_mut() { field.move_word_left(); }
            }
            (KeyCode::Right, KeyModifiers::CONTROL) | (KeyCode::Right, KeyModifiers::ALT) => {
                if let Some(field) = form.edit_buffer.as_mut() { field.move_word_right(); }
            }
            (KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Backspace, KeyModifiers::ALT) => {
                if let Some(field) = form.edit_buffer.as_mut() { field.delete_word_backward(); }
            }
            (KeyCode::Delete, KeyModifiers::CONTROL) | (KeyCode::Delete, KeyModifiers::ALT) => {
                if let Some(field) = form.edit_buffer.as_mut() { field.delete_word_forward(); }
            }
            (KeyCode::Left, _) => { if let Some(field) = form.edit_buffer.as_mut() { field.move_left(); } }
            (KeyCode::Right, _) => { if let Some(field) = form.edit_buffer.as_mut() { field.move_right(); } }
            (KeyCode::Home, _) => { if let Some(field) = form.edit_buffer.as_mut() { field.move_home(); } }
            (KeyCode::End, _) => { if let Some(field) = form.edit_buffer.as_mut() { field.move_end(); } }
            (KeyCode::Delete, _) => { if let Some(field) = form.edit_buffer.as_mut() { field.delete_forward(); } }
            (KeyCode::Backspace, _) => {
                if let Some(field) = form.edit_buffer.as_mut() { field.backspace(); }
            }
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                if let (Ok(mut clipboard), Some(field)) = (arboard::Clipboard::new(), form.edit_buffer.as_mut()) {
                    if let Ok(text) = clipboard.get_text() { field.paste(&text); }
                }
            }
            (KeyCode::Tab, _) => {
                if let Some(field) = form.edit_buffer.as_mut() { field.tab_complete(); }
            }
            (KeyCode::Char(character), modifiers)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(field) = form.edit_buffer.as_mut() { field.insert_char(character); }
            }
            _ => {}
        }
        return false;
    }
```

- [ ] **Step 3: Fix the two remaining `edit_buffer` sites**

At `src/tui.rs:2278` (`activate_add_options_field`, field 4), change:
```rust
        4 => form.edit_buffer = Some(options.save_path.clone()),
```
to:
```rust
        4 => form.edit_buffer = Some(TextField::with_completion(options.save_path.clone(), CompletionSource::Filesystem)),
```

At `src/tui.rs:2136` (inside `PromptAction::Add` in `submit_prompt`), `edit_buffer: None,` is
already correct as-is (`Option<TextField>`'s `None` variant needs no change).

- [ ] **Step 4: Fix rendering**

`src/tui.rs:3580-3625` builds a `rows: Vec<(&str, String)>` (label, value) and maps it into
`Line`s with per-row styling; row index 4 is the save_path row. Replace the whole block
(`let options = ...` through the `.collect();` that produces `lines`) with:

```rust
    let options = form.options.get(form.current).cloned().unwrap_or_default();
    let button_label = if (form.current + 1 < form.entries.len()) { "[ next → ]" } else { "[ add ]" };
    let rows: Vec<(&str, String)> = vec![
        ("start",          format_bool(options.start)),
        ("sequential",     format_bool(options.sequential)),
        ("first/last",     format_bool(options.first_last).to_string()),
        ("create subfolder", options.content_layout.label().to_string()),
        ("download path",  String::new()), // row 4 is rendered specially below; this value is unused
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
        let value_style = if (index == 5 && is_focused) {
            Style::default().fg(Color::Green)
        } else if (is_focused) {
            Style::default().fg(Color::Cyan)
        } else if (index == 5) {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Gray)
        };
        let mut spans = vec![
            Span::styled(marker, Style::default().fg(Color::Cyan)),
            Span::styled(format!("{:18}", label), label_style),
            Span::raw("  "),
        ];
        if (index == 4) {
            if let Some(field) = &form.edit_buffer {
                spans.push(Span::raw("[ "));
                spans.extend(render_field_with_cursor(field));
                spans.push(Span::raw(" ]"));
            } else {
                let display = if (options.save_path.is_empty()) {
                    "(default — daemon's default_save_path)".to_string()
                } else {
                    options.save_path.clone()
                };
                spans.push(Span::styled(display, value_style));
            }
        } else {
            spans.push(Span::styled(value.clone(), value_style));
        }
        Line::from(spans)
    }).collect();
    frame.render_widget(Paragraph::new(lines), layout[3]);
```

(This drops the old `editing_path` variable — row 4's special case now checks
`form.edit_buffer` directly, which is equivalent since `edit_buffer` is only ever `Some` while
that exact field is being edited. The old highlight-the-whole-value-while-editing behavior is
superseded by the real per-character cursor from `render_field_with_cursor`.)

- [ ] **Step 5: Verify it compiles**

Run: `cargo check`
Expected: PASS.

- [ ] **Step 6: Manual smoke test**

Add a torrent, in the add-options form activate the save_path field (`enter`/`space` on that
row), confirm typing + arrow keys + Tab-completion work, confirm Esc cancels back to the
previous value.

- [ ] **Step 7: Commit**

```bash
git add src/tui.rs
git commit -m "tui: convert AddOptionsForm.edit_buffer to TextField"
```

---

## Task 9: convert `TextInput.buffer` (list-filter / content-filter)

**Files:**
- Modify: `src/tui.rs:901-914` (`TextInput` struct)
- Modify: `src/tui.rs:2496-2564` (`handle_active_input_key`)
- Modify: wherever `TextInput` is drawn (search `state.active_input` in the draw functions —
  likely the status/input bar near the bottom of `draw_main`)

- [ ] **Step 1: Convert the struct**

```rust
struct TextInput {
    purpose: InputPurpose,
    field: TextField,
}
```
(renamed from `buffer: String` to `field: TextField` — grep `active_input.*buffer\|input\.buffer`
in `src/tui.rs` afterward to catch every read site, since the field name changed, not just its type.)

- [ ] **Step 2: Update every construction site**

Search `TextInput {` in `src/tui.rs` (likely 2 sites: opening the `/` list filter and the
content-tab filter). Each currently does something like
`TextInput { purpose: InputPurpose::ListFilter, buffer: state.name_filter.clone() }` — change to
`TextInput { purpose: InputPurpose::ListFilter, field: TextField::new(state.name_filter.clone()) }`
(same pattern for `ContentFilter`, both with no explicit `CompletionSource` — `TextField::new`
already defaults to `CompletionSource::None`, which is correct here: these are filters over
already-known data, not path entry).

- [ ] **Step 3: Rewrite `handle_active_input_key`**

Replace `src/tui.rs:2496-2564` in full:

```rust
fn handle_active_input_key(code: KeyCode, modifiers: KeyModifiers, state: &mut AppState) -> bool {
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Tab, _) => {
            let purpose = state.active_input.as_ref().map(|input| input.purpose);
            if (purpose == Some(InputPurpose::ContentFilter)) {
                tab_complete_content_filter(state);
            }
        }
        (KeyCode::Esc, _) => { state.active_input = None; }
        (KeyCode::Enter, _) => { state.active_input = None; }
        (KeyCode::Left, KeyModifiers::CONTROL) | (KeyCode::Left, KeyModifiers::ALT) => {
            if let Some(input) = state.active_input.as_mut() { input.field.move_word_left(); }
        }
        (KeyCode::Right, KeyModifiers::CONTROL) | (KeyCode::Right, KeyModifiers::ALT) => {
            if let Some(input) = state.active_input.as_mut() { input.field.move_word_right(); }
        }
        (KeyCode::Left, _) => { if let Some(input) = state.active_input.as_mut() { input.field.move_left(); } }
        (KeyCode::Right, _) => { if let Some(input) = state.active_input.as_mut() { input.field.move_right(); } }
        (KeyCode::Home, _) => { if let Some(input) = state.active_input.as_mut() { input.field.move_home(); } }
        (KeyCode::End, _) => { if let Some(input) = state.active_input.as_mut() { input.field.move_end(); } }
        (KeyCode::Delete, _) => { if let Some(input) = state.active_input.as_mut() { input.field.delete_forward(); } }
        (KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Backspace, KeyModifiers::ALT) => {
            apply_active_input_edit(state, |field| field.delete_word_backward());
        }
        (KeyCode::Backspace, _) => {
            apply_active_input_edit(state, |field| field.backspace());
        }
        (KeyCode::Char(character), modifiers)
            if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
        {
            apply_active_input_edit(state, move |field| field.insert_char(character));
        }
        _ => {}
    }
    false
}

/// mutate the active input's TextField, then propagate the new buffer into
/// whichever live filter it drives. both edit operations (typed char,
/// backspace, word-delete) need this same propagate-after-mutate step, so it
/// lives in one place instead of being copy-pasted per key.
fn apply_active_input_edit(state: &mut AppState, mutate: impl FnOnce(&mut TextField)) {
    let Some(input) = state.active_input.as_mut() else { return; };
    mutate(&mut input.field);
    let buffer = input.field.buffer().to_string();
    match input.purpose {
        InputPurpose::ListFilter => {
            state.name_filter = buffer;
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
            state.content_filter = buffer;
            state.content_filter_lc = state.content_filter.to_lowercase();
            rebuild_content_matches(state);
            state.detail_files_state.select(Some(0));
        }
    }
}
```

Note this drops the old `Esc` special-case comment ("nothing to revert...") since it was a no-op
already — behavior is unchanged, just no dead branch.

- [ ] **Step 4: Fix `tab_complete_content_filter`**

At `src/tui.rs:3308` (`if let Some(input) = state.active_input.as_mut() { input.buffer = state.content_filter.clone(); }`),
change to:
```rust
        if let Some(input) = state.active_input.as_mut() {
            input.field = TextField::new(state.content_filter.clone());
        }
```

- [ ] **Step 5: Fix the draw site**

Find the render call for the input bar (search `active_input` in draw functions). Replace its
`Span::raw(&input.buffer)`-style usage with `render_field_with_cursor(&input.field)`, following
the same pattern as Task 6 Step 4.

- [ ] **Step 6: Verify and test**

Run: `cargo check` — expect PASS.
Manual: open the `/` filter, type, use arrow keys/Home/End, confirm the torrent list still
filters live on every keystroke exactly as before.

- [ ] **Step 7: Commit**

```bash
git add src/tui.rs
git commit -m "tui: convert TextInput to use TextField"
```

---

## Task 10: convert `SettingsState.edit_buffer`

**Files:**
- Modify: `src/tui.rs` (`SettingsState` struct's `edit_buffer` field)
- Modify: `src/tui.rs:4997-5017` (settings edit-mode key handler)
- Modify: `src/tui.rs:5109-5130` (`activate_field`)
- Modify: `src/tui.rs:5094-5099` (`handle_interface_picker_key`'s `__specific__` branch)
- Modify: settings value-rendering site (search `settings.edit_buffer` in draw functions,
  around `src/tui.rs:5361-5408`)

- [ ] **Step 1: Convert the struct field**

Change `edit_buffer: Option<String>` to `edit_buffer: Option<TextField>` on `SettingsState`.

- [ ] **Step 2: Rewrite the edit-mode key handler**

Replace `src/tui.rs:4997-5017`:

```rust
    if (settings.edit_buffer.is_some()) {
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Esc, _) => settings.edit_buffer = None,
            (KeyCode::Enter, _) => {
                let buffer = settings.edit_buffer.take().map(|field| field.buffer().to_string()).unwrap_or_default();
                commit_edit(settings, &buffer);
            }
            (KeyCode::Left, KeyModifiers::CONTROL) | (KeyCode::Left, KeyModifiers::ALT) => {
                if let Some(field) = settings.edit_buffer.as_mut() { field.move_word_left(); }
            }
            (KeyCode::Right, KeyModifiers::CONTROL) | (KeyCode::Right, KeyModifiers::ALT) => {
                if let Some(field) = settings.edit_buffer.as_mut() { field.move_word_right(); }
            }
            (KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Backspace, KeyModifiers::ALT) => {
                if let Some(field) = settings.edit_buffer.as_mut() { field.delete_word_backward(); }
            }
            (KeyCode::Delete, KeyModifiers::CONTROL) | (KeyCode::Delete, KeyModifiers::ALT) => {
                if let Some(field) = settings.edit_buffer.as_mut() { field.delete_word_forward(); }
            }
            (KeyCode::Left, _) => { if let Some(field) = settings.edit_buffer.as_mut() { field.move_left(); } }
            (KeyCode::Right, _) => { if let Some(field) = settings.edit_buffer.as_mut() { field.move_right(); } }
            (KeyCode::Home, _) => { if let Some(field) = settings.edit_buffer.as_mut() { field.move_home(); } }
            (KeyCode::End, _) => { if let Some(field) = settings.edit_buffer.as_mut() { field.move_end(); } }
            (KeyCode::Delete, _) => { if let Some(field) = settings.edit_buffer.as_mut() { field.delete_forward(); } }
            (KeyCode::Backspace, _) => {
                if let Some(field) = settings.edit_buffer.as_mut() { field.backspace(); }
            }
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                if let (Ok(mut clipboard), Some(field)) = (arboard::Clipboard::new(), settings.edit_buffer.as_mut()) {
                    if let Ok(text) = clipboard.get_text() { field.paste(&text); }
                }
            }
            (KeyCode::Tab, _) => {
                if let Some(field) = settings.edit_buffer.as_mut() { field.tab_complete(); }
            }
            (KeyCode::Char(character), modifiers)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(field) = settings.edit_buffer.as_mut() { field.insert_char(character); }
            }
            _ => {}
        }
        return false;
    }
```

- [ ] **Step 3: Set `CompletionSource::Filesystem` only for `default_save_path`**

In `activate_field` (`src/tui.rs:5109-5130`), the `FieldKind::Text | FieldKind::Integer | ...`
arm currently does `settings.edit_buffer = Some(current);`. Change to:
```rust
        FieldKind::Integer
        | FieldKind::IntegerUnlimited
        | FieldKind::Float
        | FieldKind::Text => {
            let completion = if (field.key == "default_save_path") {
                CompletionSource::Filesystem
            } else {
                CompletionSource::None
            };
            settings.edit_buffer = Some(TextField::with_completion(current, completion));
        }
```

Note: as of this codebase's current state, `default_save_path` is the *only* `FieldKind::Text`
settings field that is an actual filesystem path — `ip_filter_path`, `network_cert_path`, and
`network_key_path` (mentioned in the design spec) do not currently have `SettingField` entries
in the TUI settings overlay at all, so there is nothing to wire for them yet. If a future task
adds `SettingField` entries for those three keys, add their keys to the check above at that time.

- [ ] **Step 4: Fix `handle_interface_picker_key`'s `__specific__` branch**

At `src/tui.rs:5097-5099`:
```rust
            if (value == "__specific__") {
                settings.edit_buffer = Some(config_value_string(&settings.config, key));
            }
```
change to:
```rust
            if (value == "__specific__") {
                settings.edit_buffer = Some(TextField::new(config_value_string(&settings.config, key)));
            }
```

- [ ] **Step 5: Fix rendering**

At the settings value-render site (`src/tui.rs:5407-5408` area — `if (settings.edit_buffer.is_some() && index == settings.selected) { let buffer = settings.edit_buffer.as_deref().unwrap_or(""); ... }`),
change `settings.edit_buffer.as_deref().unwrap_or("")` (a `&str`) into a call to
`render_field_with_cursor` producing spans, following the exact pattern from Task 6 Step 4 —
read the ~10 lines around that site to fit it into the existing `Line`/`Span` construction there.

- [ ] **Step 6: Verify and test**

Run: `cargo check` — PASS.
Manual: open settings (`,`), edit `default_save_path`, confirm Tab-completion works there;
edit a non-path text field (e.g. a rate limit) and confirm Tab does nothing (no panic, no
unexpected completion).

- [ ] **Step 7: Commit**

```bash
git add src/tui.rs
git commit -m "tui: convert SettingsState.edit_buffer to TextField"
```

---

## Task 11: convert `SettingsState.watch_dir_buffer`

**Files:**
- Modify: `src/tui.rs` (`SettingsState` struct's `watch_dir_buffer` field)
- Modify: `src/tui.rs:4882-4913` (watch-dir inline editor key handler)
- Modify: watch-dir list rendering (around `src/tui.rs:5324` where `watch_dir_list` rows are drawn)

- [ ] **Step 1: Convert the struct field**

Change `watch_dir_buffer: String` to `watch_dir_buffer: TextField`.

- [ ] **Step 2: Rewrite the inline editor key handler**

Replace `src/tui.rs:4882-4913`:

```rust
    if (settings.watch_dir_editing) {
        match (code, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Esc, _) => {
                settings.watch_dir_editing = false;
                settings.watch_dir_buffer = TextField::new(String::new());
            }
            (KeyCode::Enter, _) => {
                let value = settings.watch_dir_buffer.buffer().trim().to_string();
                settings.watch_dir_editing = false;
                settings.watch_dir_buffer = TextField::new(String::new());
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
            (KeyCode::Left, KeyModifiers::CONTROL) | (KeyCode::Left, KeyModifiers::ALT) => {
                settings.watch_dir_buffer.move_word_left();
            }
            (KeyCode::Right, KeyModifiers::CONTROL) | (KeyCode::Right, KeyModifiers::ALT) => {
                settings.watch_dir_buffer.move_word_right();
            }
            (KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Backspace, KeyModifiers::ALT) => {
                settings.watch_dir_buffer.delete_word_backward();
            }
            (KeyCode::Delete, KeyModifiers::CONTROL) | (KeyCode::Delete, KeyModifiers::ALT) => {
                settings.watch_dir_buffer.delete_word_forward();
            }
            (KeyCode::Left, _) => settings.watch_dir_buffer.move_left(),
            (KeyCode::Right, _) => settings.watch_dir_buffer.move_right(),
            (KeyCode::Home, _) => settings.watch_dir_buffer.move_home(),
            (KeyCode::End, _) => settings.watch_dir_buffer.move_end(),
            (KeyCode::Delete, _) => settings.watch_dir_buffer.delete_forward(),
            (KeyCode::Backspace, _) => settings.watch_dir_buffer.backspace(),
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    if let Ok(text) = clipboard.get_text() { settings.watch_dir_buffer.paste(&text); }
                }
            }
            (KeyCode::Tab, _) => settings.watch_dir_buffer.tab_complete(),
            (KeyCode::Char(character), modifiers)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                settings.watch_dir_buffer.insert_char(character);
            }
            _ => {}
        }
        return false;
    }
```

- [ ] **Step 3: Set `CompletionSource::Filesystem` when entering edit mode**

Find where `watch_dir_editing = true` is set (the `'a'` add-entry key, `src/tui.rs:4945-4949`,
and wherever the "edit selected entry" key is bound — search `watch_dir_editing = true` for
the second site) and ensure `watch_dir_buffer` is initialized with
`TextField::with_completion(initial, CompletionSource::Filesystem)` (empty string for "add
new", the existing entry's value for "edit existing" — the existing code already computes an
`initial` value per `src/tui.rs:4964`, `let initial = settings.watch_dir_list.get(index).cloned().unwrap_or_default();`
— use that as the constructor argument instead of assigning to a raw `String` field).

- [ ] **Step 4: Fix the `SettingsState` constructor**

Wherever `SettingsState` is constructed (its `::load()` or `Default` implementation),
`watch_dir_buffer: String::new()` becomes `watch_dir_buffer: TextField::new(String::new())`.

- [ ] **Step 5: Fix rendering**

At the watch-dir row rendering (around `src/tui.rs:5324-5348`), the currently-editing row needs
`render_field_with_cursor(&settings.watch_dir_buffer)` in place of whatever raw-string
formatting it does today — follow the Task 6 Step 4 pattern.

- [ ] **Step 6: Verify and test**

Run: `cargo check` — PASS.
Manual: settings → paths tab → watch directories, add a new entry, confirm typing +
Tab-completion + arrow keys work, confirm it still saves via `submit_watch_dirs`.

- [ ] **Step 7: Commit**

```bash
git add src/tui.rs
git commit -m "tui: convert SettingsState.watch_dir_buffer to TextField"
```

At this point every text-entry surface in the app shares one `TextField` implementation —
Phase A/B of the spec is complete.

---

## Task 12: fix the priority-key mapping (independent — can run any time before Task 13)

**Files:**
- Modify: `src/tui.rs:1649-1662` (live Content tab priority keybind)
- Modify: `src/tui.rs:3178-3186` (`PriorityStep`'s priority keybind, pre-unification — this
  exact line range disappears in Task 13, so land this fix first or the diff won't apply)

- [ ] **Step 1: Write a unit test for the mapping**

There's no existing pure function to test (today's mapping is inlined in the match arm). Add
one so the mapping is verified independent of TUI event plumbing. In `src/tui.rs`, add near the
other small free functions (e.g. next to `collapse_focused`):

```rust
/// libtorrent's file priority is 0..=7 (0 = don't download, 4 = default,
/// 7 = maximum) — see `lt::download_priority_t` in `src/bridge.cpp`. keys 0-7
/// map 1:1 onto that range; there is no 8th or 9th level to bind further keys to.
fn priority_key_to_value(character: char) -> Option<u8> {
    character.to_digit(10).filter(|&digit| digit <= 7).map(|digit| digit as u8)
}

#[cfg(test)]
mod priority_key_tests {
    use super::priority_key_to_value;

    #[test]
    fn maps_zero_through_seven_one_to_one() {
        for digit in 0..=7u8 {
            let character = char::from_digit(digit as u32, 10).unwrap();
            assert_eq!(priority_key_to_value(character), Some(digit));
        }
    }

    #[test]
    fn eight_and_nine_are_not_mapped() {
        assert_eq!(priority_key_to_value('8'), None);
        assert_eq!(priority_key_to_value('9'), None);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test priority_key_tests`
Expected: FAIL — `priority_key_to_value` doesn't exist yet (write the test before the function
per TDD; if you added both in Step 1, instead run this after deleting the function body
temporarily, or simply proceed — the important verification is Step 4 below).

- [ ] **Step 3: Replace both match arms**

At `src/tui.rs:1649-1662` (live Content tab), replace:
```rust
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
```
with:
```rust
        (KeyCode::Char(character), KeyModifiers::NONE)
            if (state.focus == Pane::Detail
                && state.detail_tab == DetailTab::Content
                && priority_key_to_value(character).is_some()) =>
        {
            set_focused_priority(state, priority_key_to_value(character).unwrap());
        }
```

At `src/tui.rs:3178-3186` (`PriorityStep`'s keybind, pre-unification), replace:
```rust
        (KeyCode::Char(character), KeyModifiers::NONE)
            if matches!(character, '0' | '1' | '2' | '3' | '4') =>
        {
            let priority = match character {
                '0' => 0u8, '1' => 1u8, '2' => 4u8, '3' => 6u8, '4' => 7u8,
                _ => unreachable!(),
            };
            set_step_priority(step, priority);
        }
```
with:
```rust
        (KeyCode::Char(character), KeyModifiers::NONE) if priority_key_to_value(character).is_some() => {
            set_step_priority(step, priority_key_to_value(character).unwrap());
        }
```

- [ ] **Step 4: Run tests and verify compilation**

Run: `cargo test priority_key_tests && cargo check`
Expected: PASS.

- [ ] **Step 5: Update the help overlay / hint text**

Search for any help text listing `0-4` as the priority keys (likely in `draw_help_overlay` or a
hint line near the content tab) and update it to `0-7`.

- [ ] **Step 6: Commit**

```bash
git add src/tui.rs
git commit -m "tui: map priority keys 0-7 1:1 to libtorrent's actual range"
```

---

## Task 13: unify `PriorityStep` rename with the Content tab's `Prompt`-based rename

This is the fix for the confirmed bug: `commit_priority_step_rename` sends `RenameFolder`/`Move`
requests but never checks for `Response::RenameConfirmation`, so a merge that needs confirmation
silently does nothing. The fix is to delete `PriorityStep`'s own rename machinery and have it
open the exact same `Prompt` the live Content tab uses.

**Files:**
- Modify: `src/tui.rs` (`PriorityStep` struct: remove `rename_buffer`, `rename_target`)
- Modify: `src/tui.rs:3074-3096` (`handle_priority_step_key`'s rename-mode branch — removed)
- Modify: `src/tui.rs:3133-3140` (the `r`/`t` key bindings inside `handle_priority_step_key`)
- Delete: `open_priority_step_rename`, `open_priority_step_torrent_rename`,
  `commit_priority_step_rename` (`src/tui.rs:3218-3267`)
- Modify: `open_content_rename_prompt` (`src/tui.rs:1728-1769`) — generalized to work against
  either the live Content tab or a `PriorityStep`
- Modify: `open_rename_prompt` — already works against `state.torrents`/`selected_torrent_index`,
  needs a `PriorityStep`-aware variant for the `t` key inside the step

- [ ] **Step 1: Generalize the row source**

`open_content_rename_prompt` currently reads `state.detail`, `state.content_filter`,
`state.content_filter_matches`, and `state.detail_files_state` directly. `PriorityStep` has its
own `detail`, `filter`, `filter_matches`, and `files_state` fields holding the equivalent data
(see `PriorityStep::current_rows()`, already defined). Extract the row-lookup into a function
parameterized over those four pieces instead of `&AppState`:

```rust
/// resolve which TreeRow is currently focused, given the raw pieces both the
/// live Content tab (via `AppState`) and the add-time organize step (via
/// `PriorityStep`) hold. this is the one place row lookup happens so the two
/// surfaces cannot diverge on which row "focused" means.
fn focused_tree_row(
    detail: &TorrentDetail,
    collapsed_folders: &std::collections::BTreeSet<String>,
    filter_matches: &[usize],
    filter_active: bool,
    files_state: &TableState,
) -> Option<TreeRow> {
    let rows = if (filter_active) {
        filter_content_rows(detail, filter_matches)
    } else {
        build_tree_rows(detail, collapsed_folders)
    };
    files_state.selected().and_then(|index| rows.get(index)).cloned()
}
```

This requires `TreeRow` to derive `Clone` — check its definition (`src/tui.rs:4416-4431`) and
add `#[derive(Clone)]` if not already present (it is not, based on the struct as read during
planning — add it).

- [ ] **Step 2: Rewrite `open_content_rename_prompt` to build the prompt from a resolved row**

Split the function into row-resolution (call site's job) and prompt-construction (shared).
Replace `src/tui.rs:1728-1769` with:

```rust
fn open_content_rename_prompt(state: &mut AppState) {
    let Some(torrent_index) = state.selected_torrent_index() else {
        state.error = Some("no torrent selected".to_string());
        return;
    };
    let Some(detail) = &state.detail else {
        state.error = Some("file list not loaded".to_string());
        return;
    };
    let Some(row) = focused_tree_row(
        detail,
        &state.collapsed_folders,
        &state.content_filter_matches,
        !state.content_filter.is_empty(),
        &state.detail_files_state,
    ) else {
        state.error = Some("no file selected".to_string());
        return;
    };
    state.prompt = Some(build_rename_prompt(detail, torrent_index, &row));
}

/// shared by the live Content tab and the add-time organize step — this is
/// the ONLY place a rename Prompt for a file/folder row gets constructed, so
/// the two surfaces cannot drift on completion source, helper text, or the
/// PromptAction they dispatch.
fn build_rename_prompt(detail: &TorrentDetail, torrent_index: usize, row: &TreeRow) -> Prompt {
    if (row.is_folder) {
        let basename = row.full_path.rsplit('/').next().unwrap_or(&row.full_path).to_string();
        let siblings = sibling_folder_names(detail, &row.full_path);
        Prompt {
            title: format!("rename folder \"{}\"", row.full_path),
            helper: "new name (relative to this folder's parent). use ../ to move up; cannot leave the torrent root. merging into an existing folder warns; file collisions are rejected. tab completes to an existing sibling folder to merge into it.".to_string(),
            lines: vec![TextField::with_completion(basename, CompletionSource::SiblingFolders(siblings))],
            cursor_line: 0,
            action: PromptAction::RenameFolder { old_prefix: row.full_path.clone() },
            torrent_index,
            allow_multiline: false,
        }
    } else {
        let file_index = row.file_index.unwrap_or(0);
        let basename = row.full_path.rsplit('/').next().unwrap_or(&row.full_path).to_string();
        Prompt {
            title: format!("rename file \"{}\"", row.label),
            helper: "new name (relative to this file's folder). use ../ to move up; cannot leave the torrent root. collisions with existing files are rejected.".to_string(),
            lines: vec![TextField::new(basename)],
            cursor_line: 0,
            action: PromptAction::RenameFile { file_index },
            torrent_index,
            allow_multiline: false,
        }
    }
}
```

(`sibling_folder_names` is implemented in Task 14 — implement Task 14 alongside this task, they
are tightly coupled; this task's row-lookup extraction and Task 14's candidate computation both
land inside `build_rename_prompt`.)

- [ ] **Step 3: Add a `PriorityStep`-aware entry point**

```rust
fn open_priority_step_content_rename(state: &mut AppState) {
    let Some(step) = state.priority_step.as_mut() else { return; };
    let Some(torrent_index) = step.torrent_index() else { return; };
    let Some(detail) = &step.detail else {
        state.error = Some("file list not loaded".to_string());
        return;
    };
    let Some(row) = focused_tree_row(
        detail,
        &step.collapsed_folders,
        &step.filter_matches,
        !step.filter.is_empty(),
        &step.files_state,
    ) else {
        state.error = Some("no file selected".to_string());
        return;
    };
    state.prompt = Some(build_rename_prompt(detail, torrent_index, &row));
}
```

Note this takes `&mut AppState` (not `&mut PriorityStep`) because it writes to `state.prompt` —
opening the shared `Prompt` overlay necessarily suspends the `PriorityStep` UI underneath it,
exactly like it already suspends the live Content tab. `submit_prompt`'s existing
`PromptAction::RenameFile`/`RenameFolder` arms (`src/tui.rs:2025-2071`, unchanged by this task)
already send the request and handle `Response::RenameConfirmation` correctly — this is what
fixes the bug, by routing `PriorityStep`'s renames through code that was never broken.

- [ ] **Step 4: Remove `PriorityStep`'s own rename fields and functions**

Delete from the `PriorityStep` struct: `rename_buffer: Option<String>`, `rename_target: Option<PriorityRenameTarget>`.
Delete from `PriorityStep::new`'s initializer: `rename_buffer: None, rename_target: None,`.
Delete the `PriorityRenameTarget` enum entirely (`src/tui.rs:1044-1048`) — no longer referenced.
Delete `open_priority_step_rename`, `open_priority_step_torrent_rename`, and
`commit_priority_step_rename` (`src/tui.rs:3218-3267`) in full.

- [ ] **Step 5: Update `handle_priority_step_key`**

Delete the rename-input-mode block (`src/tui.rs:3074-3096`, the
`if step.rename_buffer.is_some() { ... }` block).

Replace the `r`/`t` key bindings (`src/tui.rs:3133-3140`):
```rust
        (KeyCode::Char('r'), KeyModifiers::NONE) | (KeyCode::F(2), _) => {
            open_priority_step_content_rename(state);
            return false;
        }
        (KeyCode::Char('t'), KeyModifiers::NONE) => {
            open_rename_prompt(state);
            return false;
        }
```
(`open_rename_prompt` already reads `state.selected_torrent_index()`/`state.torrents` for the
torrent-display-name rename, which works unchanged here since `PriorityStep::torrent_index()`
returns the same real daemon index — no `PriorityStep`-specific torrent-rename variant is
needed.)

This function must now also route through the main `handle_prompt_key` when `state.prompt` is
`Some(_)`, same as the live view does. Find where the top-level key-dispatch function decides
between `handle_priority_step_key` and `handle_prompt_key` (likely near
`src/tui.rs:1499-1532`, which already checks `state.priority_step.is_some()`) and make sure a
non-`None` `state.prompt` takes priority over `state.priority_step` in that dispatch, mirroring
how it already prioritizes `state.rename_confirm` over other modes at `src/tui.rs:1531-1532`.

- [ ] **Step 6: Verify it compiles**

Run: `cargo check`
Expected: PASS. If `PriorityRenameTarget` or the removed fields are still referenced anywhere,
the compiler will point at the exact remaining site.

- [ ] **Step 7: Manual regression test**

Add a torrent with multiple files/folders, in the (still paused-only, until Task 15) organize
step press `r` on a folder, rename it to merge into an existing sibling folder, and confirm the
merge-confirmation prompt now actually appears (this is the bug fix — previously nothing
happened). Repeat the same rename from the live Content tab and confirm identical behavior.

- [ ] **Step 8: Merge `set_focused_priority` and `set_step_priority` into one function**

The spec (section C2) requires one shared priority-setting function, not two independently
maintained copies — `set_step_priority` is already explicitly commented in the current codebase
as "same cascading folder logic as `set_focused_priority`, but uses the step's own state," which
is exactly the kind of duplication this whole plan exists to eliminate. Replace both
(`set_focused_priority` at `src/tui.rs:2435-2473`, `set_step_priority` at `src/tui.rs:3194-3216`)
with one function taking the same resolved-row inputs `focused_tree_row` (Step 1) already
established:

```rust
/// set the priority of the currently-focused row, given the torrent index
/// and the resolved row. shared by the live Content tab and the add-time
/// organize step so their priority-setting logic (folder rows cascade to
/// every descendant file) cannot diverge, the way `set_step_priority` had
/// already diverged from `set_focused_priority` before this task.
fn apply_priority_to_row(torrent_index: usize, detail: &TorrentDetail, row: &TreeRow, priority: u8) -> Result<usize, String> {
    let targets: Vec<usize> = if (row.is_folder) {
        let prefix = format!("{}/", row.full_path);
        detail.files.iter().enumerate()
            .filter(|(_, file)| file.path == row.full_path || file.path.starts_with(&prefix))
            .map(|(file_index, _)| file_index)
            .collect()
    } else if let Some(file_index) = row.file_index {
        vec![file_index]
    } else {
        Vec::new()
    };
    if (targets.is_empty()) { return Ok(0); }
    let priorities: Vec<(usize, u8)> = targets.iter().map(|&file_index| (file_index, priority)).collect();
    let count = priorities.len();
    match client::send(Request::SetFilePrioritiesBatch { index: torrent_index, priorities }) {
        Ok(Response::Ok) => Ok(count),
        Ok(Response::Err(message)) => Err(message),
        Ok(_) => Err("unexpected response to batch priority".to_string()),
        Err(error) => Err(error.to_string()),
    }
}
```

Replace the live Content tab's call site (`set_focused_priority(state, priority)` at
`src/tui.rs:1662`, from Task 12) with a wrapper that resolves the row from `AppState` and
reports through `state.error`, matching the original function's user-facing messages:
```rust
fn set_focused_priority(state: &mut AppState, priority: u8) {
    let Some(torrent_index) = state.selected_torrent_index() else { return; };
    let Some(detail) = state.detail.clone() else { return; };
    let Some(row) = focused_tree_row(
        &detail,
        &state.collapsed_folders,
        &state.content_filter_matches,
        !state.content_filter.is_empty(),
        &state.detail_files_state,
    ) else { return; };
    match apply_priority_to_row(torrent_index, &detail, &row, priority) {
        Ok(0) => {}
        Ok(count) => state.error = Some(format!("priority {} set on {} file(s)", priority, count)),
        Err(message) => state.error = Some(format!("priority: {}", message)),
    }
    state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
}
```
(`TorrentDetail` needs `#[derive(Clone)]` for this — check `src/ipc.rs` and add it if not
already present, since `state.detail` is borrowed elsewhere in the same function otherwise.)

Replace `set_step_priority`'s call site (from Task 12) similarly:
```rust
fn set_step_priority(step: &mut PriorityStep, priority: u8) {
    let Some(torrent_index) = step.torrent_index() else { return; };
    let Some(detail) = step.detail.clone() else { return; };
    let Some(row) = focused_tree_row(
        &detail,
        &step.collapsed_folders,
        &step.filter_matches,
        !step.filter.is_empty(),
        &step.files_state,
    ) else { return; };
    if let Ok(count) = apply_priority_to_row(torrent_index, &detail, &row, priority) {
        if (count > 0) { step.last_poll = Instant::now() - DETAIL_POLL_INTERVAL; }
    }
}
```

- [ ] **Step 9: Verify and re-test**

Run: `cargo check` — PASS. Re-run the Step 7 manual test, plus: set a priority from both the
live Content tab and the organize step on a folder row, and confirm every descendant file's
priority updates identically from both surfaces.

- [ ] **Step 10: Commit**

```bash
git add src/tui.rs
git commit -m "tui: unify PriorityStep with the Content tab (rename + priority-setting)

fixes a bug where renaming a folder inside the add-time organize step to
merge it into an existing folder silently did nothing whenever the server
needed a merge confirmation — commit_priority_step_rename discarded
Response::RenameConfirmation instead of acting on it. also merges
set_focused_priority/set_step_priority, which had already diverged into two
copies of the same cascading-folder logic, into one shared function."
```

---

## Task 14: sibling-folder candidates for rename-folder completion

**Files:**
- Modify: `src/tui.rs` (new free function, used by Task 13's `build_rename_prompt`)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod sibling_folder_tests {
    use super::*;

    fn file(path: &str) -> crate::ipc::FileInfo {
        crate::ipc::FileInfo { index: 0, path: path.to_string(), size: 0, progress: 0.0, priority: 4 }
    }

    #[test]
    fn finds_siblings_under_the_same_parent_excluding_self() {
        let detail = TorrentDetail {
            info: Default::default(),
            peers: Vec::new(),
            trackers: Vec::new(),
            files: vec![
                file("Show/Season 1/e01.mkv"),
                file("Show/Season 2/e01.mkv"),
                file("Show/Extras/behind.mkv"),
            ],
        };
        let mut siblings = sibling_folder_names(&detail, "Show/Season 1");
        siblings.sort();
        assert_eq!(siblings, vec!["Extras".to_string(), "Season 2".to_string()]);
    }

    #[test]
    fn top_level_folder_gets_top_level_siblings() {
        let detail = TorrentDetail {
            info: Default::default(),
            peers: Vec::new(),
            trackers: Vec::new(),
            files: vec![file("A/x.mkv"), file("B/y.mkv")],
        };
        let mut siblings = sibling_folder_names(&detail, "A");
        siblings.sort();
        assert_eq!(siblings, vec!["B".to_string()]);
    }
}
```

Check `TorrentInfo`/`TorrentDetail`/`FileInfo`'s exact field lists in `src/ipc.rs` before
running this — adjust the test's struct-literal fields to match exactly (the fields shown above
are based on the `FileInfo` fields already seen in this codebase's `server.rs` construction
sites; `TorrentInfo`'s `Default` may not derive automatically — if it doesn't, construct a
minimal real value instead of `Default::default()`).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test sibling_folder_tests`
Expected: FAIL — `sibling_folder_names` not found.

- [ ] **Step 3: Implement**

```rust
/// every other top-level folder name that shares `target_path`'s parent
/// directory, derived from the torrent's own file list (not the real
/// filesystem — folders that only exist in the torrent's metadata, not yet
/// downloaded, still count). used to let rename-folder's Tab key complete to
/// an existing sibling for a deliberate merge.
fn sibling_folder_names(detail: &TorrentDetail, target_path: &str) -> Vec<String> {
    let parent_prefix = match target_path.rfind('/') {
        Some(index) => &target_path[..index],
        None => "",
    };
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for file in &detail.files {
        let relative = if (parent_prefix.is_empty()) {
            file.path.as_str()
        } else {
            let prefix = format!("{}/", parent_prefix);
            match file.path.strip_prefix(prefix.as_str()) {
                Some(rest) => rest,
                None => continue,
            }
        };
        let Some(slash_index) = relative.find('/') else { continue; };
        let folder_name = &relative[..slash_index];
        let full_folder_path = if (parent_prefix.is_empty()) {
            folder_name.to_string()
        } else {
            format!("{}/{}", parent_prefix, folder_name)
        };
        if (full_folder_path != target_path) {
            names.insert(folder_name.to_string());
        }
    }
    names.into_iter().collect()
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test sibling_folder_tests`
Expected: PASS.

- [ ] **Step 5: Wire it into `build_rename_prompt`**

Already shown in Task 13 Step 2 (`let siblings = sibling_folder_names(&detail, &row.full_path);`)
— if Task 13 was implemented first with a placeholder, replace that placeholder now.

- [ ] **Step 6: Commit**

```bash
git add src/tui.rs
git commit -m "tui: derive sibling-folder candidates for rename-folder tab-completion"
```

---

## Task 15: always-organize-before-start

**Files:**
- Modify: `src/tui.rs:2245-2299` (`dispatch_add_options`)
- Modify: `src/tui.rs` (`AddOptions` struct — no change needed, `start: bool` already exists
  and is exactly the "resume after organizing?" intent)
- Modify: `PriorityStep` construction site and `advance_priority_step`

- [ ] **Step 1: Change `dispatch_add_options` to always add paused**

Replace `src/tui.rs:2245-2299` in full:

```rust
fn dispatch_add_options(form: AddOptionsForm, state: &mut AppState) {
    let mut succeeded: usize = 0;
    let mut failures: Vec<String> = Vec::new();
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
        }) {
            Ok(Response::Added { id }) => Some(id),
            Ok(Response::Err(message)) => { failures.push(format!("{}: {}", uri, message)); None }
            Ok(_) => { failures.push(format!("{}: unexpected response", uri)); None }
            Err(error) => { failures.push(format!("{}: {}", uri, error)); None }
        };
        if (added_id.is_none()) { continue; }
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
    if (!organize_indices.is_empty()) {
        state.priority_step = Some(Box::new(PriorityStep::new(organize_entries, organize_indices, organize_resume)));
    }
}
```

This drops the `!options.start` gate entirely — every successful add now enters
`organize_indices`/the organize step, and `organize_resume` carries each entry's original
start intent through to Task 15 Step 3 below. It also removes the now-dead `if
(!matches!(options.subfolder, SubfolderMode::Default)) { todo!(...) }` line if it's still
present in your checkout — the already-landed content-layout overhaul replaced `subfolder`
with `content_layout` and resolved that `todo!()` before this plan started (see the
`project_git_remotes` context in the design spec's Overview), so if `cargo check` reports no
such line exists, there is nothing to remove here.

- [ ] **Step 2: Add a resume flag to `PriorityStep`**

In the `PriorityStep` struct, add:
```rust
    /// per-entry: resume the torrent (matches the originally-requested
    /// `start` option) once its organize step concludes.
    resume_on_finish: Vec<bool>,
```

Update `PriorityStep::new`:
```rust
    fn new(entries: Vec<String>, indices: Vec<usize>, resume_on_finish: Vec<bool>) -> Self {
        let mut files_state = TableState::default();
        files_state.select(Some(0));
        Self {
            entries,
            indices,
            resume_on_finish,
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
        }
    }
```
(`rename_buffer`/`rename_target` were already removed in Task 13.)

- [ ] **Step 3: Resume on conclusion in `advance_priority_step`**

Find `advance_priority_step` (referenced at `src/tui.rs:3129-3131`'s `Tab`/`Enter`/`Esc` arm;
its definition is further down the file). It currently moves `current` forward or clears
`state.priority_step` when the last entry finishes. Add a resume call for the entry being left:

```rust
fn advance_priority_step(state: &mut AppState) {
    let Some(step) = state.priority_step.as_mut() else { return; };
    let leaving = step.current;
    if let (Some(&torrent_index), Some(&should_resume)) =
        (step.indices.get(leaving), step.resume_on_finish.get(leaving))
    {
        if (should_resume) {
            let _ = client::send(Request::Resume { index: torrent_index });
        }
    }
    if (step.current + 1 < step.entries.len()) {
        step.current += 1;
        step.files_state.select(Some(0));
        return;
    }
    state.priority_step = None;
}
```

Adjust field/method names above to match whatever `advance_priority_step` actually does today
beyond the two branches shown (re-read its current body before replacing it — it may reset
additional per-entry fields like `detail`/`filter` between entries; preserve that behavior,
only adding the resume call and the `resume_on_finish` read).

- [ ] **Step 4: Verify and test**

Run: `cargo check` — PASS.
Manual: add a torrent with "start" checked in the add-options form. Confirm it does **not**
start downloading immediately — it enters the organize step first — and confirm it resumes
automatically once you advance past it (Tab/Enter) or skip it (Esc). Add a second torrent with
"start" unchecked and confirm it stays paused after the same step.

- [ ] **Step 5: Update the design's changelog-worthy note**

This is a user-visible behavior change (adds with "start" checked no longer begin downloading
immediately). If this project keeps a CHANGELOG or release-notes file, add a line there; if it
doesn't (check `git log --oneline -- CHANGELOG*` first), skip this step.

- [ ] **Step 6: Commit**

```bash
git add src/tui.rs
git commit -m "tui: every add goes through the organize step before starting

torrents are now always added paused internally so the organize step (file
tree browsing, rename/merge, priority-setting) runs before any data
downloads; the originally-requested start/pause choice is honored once the
step concludes for that entry."
```

---

## Task 16: `default_layout` field and persistence

**Files:**
- Modify: `src/server.rs:23-60` (`ManagedTorrent`, `TorrentRecord`)
- Modify: `src/server.rs` wherever `ManagedTorrent`/`TorrentRecord` are constructed and
  round-tripped (the `persist_torrent_list`/load-on-restart code, near `src/server.rs:133-190`)

- [ ] **Step 1: Add the field to both structs**

In `ManagedTorrent` (`src/server.rs:23-41`), add after `pending_layout`:
```rust
    /// snapshot of every file's path, taken once when the add-time organize
    /// step concludes (see `Request::FinalizeAdd`). `None` until that
    /// happens. used by `Request::RevertToDefaultLayout` to undo any renames
    /// made after that point.
    default_layout: Option<Vec<String>>,
```

In `TorrentRecord` (`src/server.rs:44-60`), add after `pending_layout`:
```rust
    #[serde(default)]
    default_layout: Option<Vec<String>>,
```

- [ ] **Step 2: Fix every construction site**

Run: `cargo check` and follow each "missing field `default_layout`" error — this project's
existing pattern (visible for `pending_layout`/`display_name`) is: `None` for a freshly-added
torrent (the two `add_magnet`/`add_file`-style constructors, `src/server.rs` around lines
242-254 and 289-301), and the loaded value (`record.default_layout`) when reconstructing from
`TorrentRecord` on daemon restart (`src/server.rs` around line 165-167, alongside
`display_name`/`pending_layout`).

- [ ] **Step 3: Include it in the persisted round-trip**

At the `TorrentRecord` construction inside `persist_torrent_list` (`src/server.rs` around line
185-188, alongside `display_name`/`pending_layout`), add:
```rust
            default_layout: torrent.default_layout.clone(),
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server.rs
git commit -m "server: add default_layout field to ManagedTorrent/TorrentRecord"
```

---

## Task 17: `FinalizeAdd` and `RevertToDefaultLayout` IPC requests

**Files:**
- Modify: `src/ipc.rs` (`Request` enum)

- [ ] **Step 1: Add the two request variants**

In the `Request` enum (`src/ipc.rs`, alongside `RenameFolder`/`Move`), add:

```rust
    /// conclude a torrent's add-time organize step: snapshot its current file
    /// paths as the "default layout" (only ever written once, here), then
    /// resume it if `resume` is true. `resume` reflects the originally-
    /// requested start/pause option, carried through from the add-options form.
    FinalizeAdd { index: usize, resume: bool },
    /// undo every rename made since the torrent's default_layout snapshot was
    /// taken, restoring its original file structure. can touch many files at
    /// once; the whole operation is atomic (any single hard-conflicting file
    /// rejects the entire revert). same two-phase shape as RenameFolder/Move:
    /// absent `decisions` on the first call may get back
    /// `Response::RenameConfirmation`; resend with `decisions` filled in to commit.
    RevertToDefaultLayout { index: usize, decisions: Option<RenameDecisions> },
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: `src/ipc.rs` compiles on its own; `src/server.rs` will now fail to compile because
`handle_request`'s match isn't exhaustive — that's expected and fixed in Task 18.

- [ ] **Step 3: Commit**

```bash
git add src/ipc.rs
git commit -m "ipc: add FinalizeAdd and RevertToDefaultLayout requests"
```

---

## Task 18: server-side `FinalizeAdd`/`RevertToDefaultLayout` handlers

This extracts the collision/merge analysis already inside `rename_folder` into a reusable
function, since `RevertToDefaultLayout` needs the exact same analysis applied to a
diff-computed rename plan instead of a prefix-rewrite plan — this is what the spec's risk note
means by "reuses the same underlying collision-detection primitives" without an existing
implementation to lift wholesale.

**Files:**
- Modify: `src/server.rs:718-` (`rename_folder`) — extract the shared analyzer
- Modify: `src/server.rs` (`handle_request`'s match, `apply_pending_layouts` neighborhood)
- Modify: `src/layout.rs` (new pure diff function)

- [ ] **Step 1: Write the diff function in `src/layout.rs` (pure, unit-testable)**

```rust
/// compute the rename plan needed to restore `current` paths back to
/// `default_layout`, indexed by file_index (both vecs are parallel to the
/// torrent's file list). only files whose current path differs from the
/// stored default appear in the plan.
pub fn compute_revert_plan(current: &[String], default_layout: &[String]) -> Vec<(usize, String)> {
    current.iter().enumerate()
        .filter_map(|(file_index, current_path)| {
            let target = default_layout.get(file_index)?;
            if (target == current_path) { None } else { Some((file_index, target.clone())) }
        })
        .collect()
}

#[cfg(test)]
mod revert_plan_tests {
    use super::*;

    #[test]
    fn only_changed_files_appear_in_the_plan() {
        let current = vec!["Show/a.mkv".to_string(), "Show/b.mkv".to_string()];
        let default_layout = vec!["Show/a.mkv".to_string(), "Show/Renamed/b.mkv".to_string()];
        let plan = compute_revert_plan(&current, &default_layout);
        assert_eq!(plan, vec![(1, "Show/Renamed/b.mkv".to_string())]);
    }

    #[test]
    fn no_changes_yields_empty_plan() {
        let current = vec!["a.mkv".to_string()];
        assert!(compute_revert_plan(&current, &current).is_empty());
    }

    #[test]
    fn mismatched_lengths_only_diff_the_overlapping_indices() {
        let current = vec!["a.mkv".to_string(), "b.mkv".to_string()];
        let default_layout = vec!["a.mkv".to_string()];
        assert!(compute_revert_plan(&current, &default_layout).is_empty());
    }
}
```

Run: `cargo test --lib layout::revert_plan_tests` and confirm PASS before moving on.

- [ ] **Step 2: Extract the shared collision/merge analyzer out of `rename_folder`**

`rename_folder` (`src/server.rs:718-` onward) already builds a `plan: Vec<(usize, String)>`
(matched files → target path) at lines 739-764 before running collision/merge analysis on it
(lines 770-861) and finally committing (the code after line 861, not shown in this plan —
re-read it before this step). Extract everything from line 770 through the commit into a new
method taking the plan directly, so both the prefix-rewrite case (`rename_folder`) and the
diff case (`revert_to_default_layout`, Step 3 below) share one analysis+commit path:

```rust
    /// given an already-computed rename plan (file_index -> new path) and the
    /// torrent's current files, run the full collision/merge/untracked
    /// analysis and, if nothing needs a decision, commit the renames. shared
    /// by `rename_folder` (plan = prefix rewrite) and
    /// `revert_to_default_layout` (plan = diff against the stored snapshot)
    /// so the two can never diverge on what counts as a conflict.
    fn analyze_and_commit_rename_plan(
        &mut self,
        index: usize,
        plan: Vec<(usize, String)>,
        decisions: Option<crate::ipc::RenameDecisions>,
    ) -> Result<crate::ipc::Response> {
        use crate::ipc::Response;
        // move lines 766-861 of the current rename_folder here verbatim,
        // replacing every read of the locally-computed `plan`/`rejected`
        // variables with this function's `plan` parameter, and every
        // `self.torrents.get(index)` with the same lookup this function
        // already needs at its top (add `let torrent = self.torrents.get(index)
        // .ok_or_else(|| anyhow::anyhow!("invalid index: {}", index))?;` as the
        // first line, since the extracted block relied on `torrent`/`files`
        // being in scope from rename_folder's own earlier lines).
    }
```

Re-derive the exact extracted body directly from your checkout's current `rename_folder`
(lines 766 through wherever it returns `Ok(Response::RenameResult { ... })` on success) rather
than retyping it here — the plan-writing pass that produced this document read it in full but
the exact line numbers may have shifted by the time you implement this if earlier tasks in this
plan touched `server.rs` (none do, so line numbers should match `src/server.rs:718-` as
originally read, but verify before extracting).

Then shrink `rename_folder` itself to just the prefix-rewrite plan computation (lines 718-764,
unchanged) followed by a call to the extracted method:
```rust
        self.analyze_and_commit_rename_plan(index, plan, decisions)
```

- [ ] **Step 3: Implement `revert_to_default_layout`**

```rust
    fn revert_to_default_layout(
        &mut self,
        index: usize,
        decisions: Option<crate::ipc::RenameDecisions>,
    ) -> Result<crate::ipc::Response> {
        let torrent = self.torrents.get(index)
            .ok_or_else(|| anyhow::anyhow!("invalid index: {}", index))?;
        let Some(default_layout) = torrent.default_layout.clone() else {
            return Err(anyhow::anyhow!("no default layout recorded for this torrent"));
        };
        let current: Vec<String> = torrent.handle.files().iter().map(|file| file.path.clone()).collect();
        let plan = crate::layout::compute_revert_plan(&current, &default_layout);
        if (plan.is_empty()) {
            return Ok(crate::ipc::Response::Ok);
        }
        self.analyze_and_commit_rename_plan(index, plan, decisions)
    }
```

- [ ] **Step 4: Implement `FinalizeAdd`**

Add a new method (near `apply_pending_layouts`, since it has the same "only once metadata/
verification is settled" character, though `FinalizeAdd` is client-triggered rather than
alert-driven, so it doesn't need the polling-loop latch `pending_layout` uses):

```rust
    fn finalize_add(&mut self, index: usize, resume: bool) -> Result<()> {
        let torrent = self.torrents.get_mut(index)
            .ok_or_else(|| anyhow::anyhow!("invalid index: {}", index))?;
        if (torrent.default_layout.is_none()) {
            let paths: Vec<String> = torrent.handle.files().iter().map(|file| file.path.clone()).collect();
            torrent.default_layout = Some(paths);
        }
        if (resume) {
            self.seed_limit_acted.remove(&torrent.info_hash.clone());
            torrent.handle.resume();
            torrent.handle.submit_save_resume_data();
        }
        self.persist_torrent_list();
        Ok(())
    }
```

(The `torrent.default_layout.is_none()` guard matches the spec's "only ever written once"
requirement even if `FinalizeAdd` were somehow sent twice for the same torrent — re-sends are a
no-op for the snapshot, though `resume` still applies each time, which is harmless since
`Request::Resume` is itself idempotent per the existing `Resume` handler's pattern of clearing
`seed_limit_acted` and calling `torrent.handle.resume()` unconditionally, already visible at
`src/server.rs`'s existing `Request::Resume` arm.)

- [ ] **Step 5: Wire both into `handle_request`**

In the `match request { ... }` inside `handle_request`, alongside the existing
`Request::RenameFolder`/`Request::Move` arms:

```rust
            Request::FinalizeAdd { index, resume } => match self.finalize_add(index, resume) {
                Ok(_) => Response::Ok,
                Err(error) => Response::Err(error.to_string()),
            },
            Request::RevertToDefaultLayout { index, decisions } =>
                match self.revert_to_default_layout(index, decisions) {
                    Ok(response) => response,
                    Err(error) => Response::Err(error.to_string()),
                },
```

- [ ] **Step 6: Write a server-side integration test for the atomic-reject case**

Find this project's existing test pattern for `rename_folder`/collision detection (search
`#[cfg(test)]` in `src/server.rs` — there is at least one, given `folder_merge_same` has tests
at `src/server.rs:2079-2081`). Add a test alongside it that builds a minimal `App`/`ManagedTorrent`
fixture (match whatever fixture helper the existing rename tests use — do not invent a new one),
sets a `default_layout` that would collide with a currently-renamed-elsewhere file, calls
`revert_to_default_layout`, and asserts the whole plan is rejected (no partial renames
committed) — mirroring the existing "file-on-file conflict is never auto-merged" assertion
style already used for `rename_folder`.

- [ ] **Step 7: Verify and test**

Run: `cargo test --lib` (full suite) and `cargo check`.
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/server.rs src/layout.rs
git commit -m "server: implement FinalizeAdd and RevertToDefaultLayout

extracts the collision/merge/untracked analysis rename_folder already did
into a shared analyze_and_commit_rename_plan, so reverting to the stored
default layout gets the same atomic all-or-nothing collision guarantee
without a second hand-written implementation of that logic."
```

---

## Task 19: client-side wiring for `FinalizeAdd` and revert-to-default

**Files:**
- Modify: `src/tui.rs` (`advance_priority_step`, from Task 15 — send `FinalizeAdd` instead of
  just `Request::Resume`)
- Modify: `src/tui.rs` (Content tab: new keybind + confirmation prompt for revert)

- [ ] **Step 1: Replace the bare `Request::Resume` call from Task 15 with `FinalizeAdd`**

In `advance_priority_step` (written in Task 15, Step 3), replace:
```rust
        if (should_resume) {
            let _ = client::send(Request::Resume { index: torrent_index });
        }
```
with:
```rust
        let _ = client::send(Request::FinalizeAdd { index: torrent_index, resume: should_resume });
```
This is the one call site where `FinalizeAdd` is sent — it now both snapshots the baseline and
resumes (or doesn't) in one round trip, replacing the plain resume call.

- [ ] **Step 2: Add a `ConfirmRevertLayout` state, mirroring `ConfirmDelete`**

Find `ConfirmDelete` (`src/tui.rs:979-984`) as the pattern to mirror. Add:
```rust
struct ConfirmRevertLayout {
    torrent_index: usize,
    torrent_name: String,
}
```
Add `confirm_revert_layout: Option<ConfirmRevertLayout>` to `AppState`, initialized to `None`
wherever `AppState`'s other `confirm_*`/`Option` fields are initialized.

- [ ] **Step 3: Add the keybind (Content tab, live view only — not inside the organize step)**

In the main key handler, alongside the existing content-tab-only bindings (e.g. near the `r`
rename binding at `src/tui.rs:1623-1630`), add:
```rust
        (KeyCode::Char('R'), KeyModifiers::SHIFT) => {
            if (state.focus == Pane::Detail && state.detail_tab == DetailTab::Content) {
                open_revert_layout_confirm(state);
            }
        }
```

```rust
fn open_revert_layout_confirm(state: &mut AppState) {
    let Some(index) = state.selected_torrent_index() else {
        state.error = Some("no torrent selected".to_string());
        return;
    };
    let name = state.torrents.get(index).map(|torrent| torrent.name.clone()).unwrap_or_default();
    state.confirm_revert_layout = Some(ConfirmRevertLayout { torrent_index: index, torrent_name: name });
}
```

- [ ] **Step 4: Extend `RenameConfirmKind` with a third variant**

`RenameConfirm` (`src/tui.rs:967-971`) is `{ kind: RenameConfirmKind, concerns: VecDeque<RenameConcern>, decisions: RenameDecisions }`,
and `RenameConfirmKind` (`src/tui.rs:973-976`) currently has two variants, `Folder { index,
old_prefix, new_prefix }` and `Move { index, new_save_path }`. Add a third:
```rust
enum RenameConfirmKind {
    Folder { index: usize, old_prefix: String, new_prefix: String },
    Move { index: usize, new_save_path: String },
    RevertToDefaultLayout { index: usize },
}
```

Update `resend_rename_confirm`'s match (`src/tui.rs:3988-3993`) to add the third arm:
```rust
    let response = match confirm.kind {
        RenameConfirmKind::Folder { index, old_prefix, new_prefix } =>
            client::send(Request::RenameFolder { index, old_prefix, new_prefix, decisions }),
        RenameConfirmKind::Move { index, new_save_path } =>
            client::send(Request::Move { index, new_save_path, decisions }),
        RenameConfirmKind::RevertToDefaultLayout { index } =>
            client::send(Request::RevertToDefaultLayout { index, decisions }),
    };
```

- [ ] **Step 5: Handle the confirmation and dispatch**

Add a handler function, following the exact same shape as the existing
`PromptAction::RenameFolder`/`PromptAction::Move` arms in `submit_prompt`
(`src/tui.rs:2053-2117`, read above during planning) so the three cases stay structurally
identical:

```rust
fn handle_revert_layout_confirm_key(code: KeyCode, state: &mut AppState) {
    let Some(confirm) = state.confirm_revert_layout.take() else { return; };
    match code {
        KeyCode::Char('y') | KeyCode::Enter => {
            match client::send(Request::RevertToDefaultLayout { index: confirm.torrent_index, decisions: None }) {
                Ok(Response::Ok) => {
                    state.error = Some(format!("reverted \"{}\" to its default layout", confirm.torrent_name));
                    state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
                }
                Ok(Response::RenameResult { renamed, rejected }) if rejected.is_empty() => {
                    state.error = Some(format!("reverted {} file(s)", renamed.len()));
                    state.last_detail_poll = Instant::now() - DETAIL_POLL_INTERVAL;
                }
                Ok(Response::RenameResult { rejected, .. }) => {
                    state.error = rejected.first().map(|(_, reason)| format!("revert rejected: {}", reason));
                }
                Ok(Response::RenameConfirmation { concerns }) => {
                    state.rename_confirm = Some(RenameConfirm {
                        kind: RenameConfirmKind::RevertToDefaultLayout { index: confirm.torrent_index },
                        concerns: concerns.into_iter().collect(),
                        decisions: crate::ipc::RenameDecisions {
                            merge_same: true,
                            merge_unrelated: true,
                            untracked: crate::ipc::UntrackedChoice::Leave,
                        },
                    });
                }
                Ok(Response::Err(message)) => state.error = Some(format!("revert: {}", message)),
                Ok(_) => state.error = Some("unexpected response to revert".to_string()),
                Err(error) => state.error = Some(format!("revert: {}", error)),
            }
        }
        _ => {}
    }
}
```

(The `decisions` seeded here — `merge_same`/`merge_unrelated` defaulting to `true`,
`untracked` to `Leave` — matches the exact default the two existing call sites use; each is
overwritten per-concern as the user answers `handle_rename_confirm_key`'s prompts, same as
today.)

- [ ] **Step 6: Draw the confirmation prompt**

Mirror `draw_delete_confirm`'s rendering, titled e.g. `"revert \"{name}\" to its layout as of
adding it?"`, with a warning that this can undo a significant amount of manual reorganization,
and `y`/`enter` to confirm, any other key to cancel (matching Step 5's key handling).

- [ ] **Step 7: Wire the confirm state into the main draw/dispatch loop**

Add `if (state.confirm_revert_layout.is_some()) { draw_revert_layout_confirm(frame, state); }`
to the `draw` function (alongside the existing `state.confirm_delete.is_some()` branch) and the
equivalent key-dispatch check alongside wherever `state.confirm_delete.is_some()` is checked in
the top-level key handler.

- [ ] **Step 8: Verify and test**

Run: `cargo check` — PASS.
Manual: add a torrent, let it finish organizing (Task 15's flow), rename a folder from the live
Content tab, then press Shift+R, confirm, and verify the structure reverts to what it was right
after adding. Also verify pressing any key other than `y`/Enter at the confirmation cancels
without reverting.

- [ ] **Step 9: Commit**

```bash
git add src/tui.rs
git commit -m "tui: wire FinalizeAdd snapshot and revert-to-default-layout action"
```

---

## Task 20: final verification pass

**Files:** none (verification only)

- [ ] **Step 1: Full build**

Run: `cargo build`
Expected: PASS, no warnings about unused `TextField`/`CompletionSource` methods (everything
from Tasks 1-4 should be in active use by now).

- [ ] **Step 2: Full test suite**

Run: `cargo test`
Expected: PASS — every test added in Tasks 1, 2, 3, 4, 12, 14, 16 (implicitly), 18 passes.

- [ ] **Step 3: Clippy**

Run: `cargo clippy --all-targets -- -D warnings` (check `Cargo.toml`/CI config first for the
project's actual clippy invocation and lint level, and match it rather than assuming `-D
warnings` — if CI uses a looser setting, use that instead)
Expected: PASS.

- [ ] **Step 4: Full manual pass through the spec's testing checklist**

Re-run every manual check listed in the design spec's "Testing" section
(`docs/superpowers/specs/2026-06-30-universal-input-and-content-organization-design.md`) in one
sitting, end to end, rather than only the per-task spot checks already done — in particular the
cross-cutting ones no single task fully covers alone: renaming a folder to merge produces
*identical* behavior from the live Content tab and from inside the organize step; every add
(paused or not) goes through the organize step; priority keys 0-7 set the expected libtorrent
value from both surfaces; revert-to-default is rejected atomically when it would collide.

- [ ] **Step 5: Commit (only if Step 3 required fixes; otherwise nothing to commit)**

```bash
git add -A
git commit -m "fix clippy warnings from the TextField/organize-step overhaul"
```
