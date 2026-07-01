# Universal input widget and torrent content organization — design

**Date:** 2026-06-30
**Status:** approved for planning

## Overview

This is sub-project #1 of a five-part effort to make filename/path handling consistent
across the app (universal input, recent-paths list, add-time validation/globbing/ignore
flow, subfolder mode, remote auth). Sub-project #4 (subfolder mode + outstanding TODOs)
turned out to already be complete, merged in from `origin/master`'s
`2026-06-28-content-and-rename-overhaul` work (rebased onto local master on 2026-06-30 —
see the `project_git_remotes` memory). Sub-projects #2, #3, and #5 get their own specs
later, once this one lands.

What started as "add cursor movement to text boxes" grew, over investigation, into four
related pieces, all serving one goal: **there must be exactly one implementation of each
piece of behavior** (typed input, path completion, file-tree browsing, rename/merge,
priority-setting), so it cannot silently drift between the places that need it.

- **A.** A shared `TextField` widget — cursor position, word movement/deletion, paste —
  used by every text-entry surface in the app.
- **B.** A generalized completion source (filesystem / sibling-folders / none) so
  rename-folder can autocomplete to an existing folder name for a deliberate merge,
  without giving files or non-path fields a completion behavior that doesn't apply to them.
- **C.** Unifying the add-time `PriorityStep` with the live Content-tab tree view into one
  component, fixing a confirmed bug where the two had silently diverged, and making the
  organize step run before every torrent starts (not just paused adds).
- **D.** A persisted "default layout" snapshot per torrent, captured once the organize step
  concludes, with a revert-to-default action usable at any later point.

### Non-goals

- Recent-paths list (sub-project #2), glob expansion / pre-add validation review flow
  (sub-project #3), and remote auth (sub-project #5) are out of scope here.
- `default_layout` snapshots file **paths** only, not priorities. Reverting restores
  structure, not per-file priority choices.
- No selection/clipboard-region editing in `TextField` (no shift+arrow text selection) —
  cursor movement, word operations, and whole-buffer paste cover the app's actual needs
  without the added complexity of a selection model.
- `RenameFile`'s completion source stays `None`: a file rename can only ever hard-reject on
  collision, never merge, so there is nothing sensible to autocomplete to.

---

## A. `TextField` widget

Replaces every hand-rolled text buffer in `src/tui.rs` (`Prompt.lines: Vec<String>`,
`AddOptionsForm.edit_buffer: Option<String>`, `SettingsState.edit_buffer: Option<String>`,
`TextInput.buffer: String`) with one shared type:

```rust
struct TextField {
    buffer: String,
    /// char index, not byte index — unicode paths must not panic on split.
    cursor: usize,
    completion: CompletionSource,
}
```

Methods: `insert_char`, `backspace`, `delete_forward`, `move_left`/`move_right`,
`move_home`/`move_end`, `delete_word_backward`/`delete_word_forward`,
`move_word_left`/`move_word_right`, `paste(text: &str)`, `tab_complete()` (dispatches on
`completion`, a no-op when `CompletionSource::None`).

Multi-line prompts (the add-torrent prompt) become `Vec<TextField>`; `cursor_line` keeps
selecting which line is active, same as today. Moving Up/Down between lines preserves and
clamps the column instead of resetting it, since each line now carries its own cursor.

**Key bindings, universal to every `TextField`:**

| Key | Action |
|---|---|
| Left / Right | move cursor one char |
| Home / End | jump to line start / end |
| Backspace / Delete | remove char before / after cursor |
| Ctrl+Left / Ctrl+Right, Alt+Left / Alt+Right | move by word (both modifiers bound to the same action — terminals vary in which they send) |
| Ctrl+Backspace, Alt+Backspace | delete previous word |
| Ctrl+Delete, Alt+Delete | delete next word |
| Ctrl+V | paste clipboard at cursor (via the existing `arboard` dependency). In a multi-line-capable prompt, embedded newlines split into new lines; in a single-line field, embedded newlines are stripped. |
| Tab | `tab_complete()` — no-op unless `completion != None` |

Insertion always happens at the cursor position (today's buffers only append at line end);
this is the behavior change that makes arrow-key movement meaningful at all.

**Unicode note:** all indexing is by `char`, not `u8`. `cursor` is a char count; converting
to/from byte offsets for `String` slicing goes through `char_indices()`, never raw byte
slicing, so multi-byte path components (accented filenames, etc.) can't panic.

---

## B. `CompletionSource`

```rust
enum CompletionSource {
    None,
    /// tab-complete against the real filesystem (today's `tab_complete_path`,
    /// generalized to complete only the substring before the cursor and splice
    /// the result back in front of whatever was after it).
    Filesystem,
    /// tab-complete against the torrent's own existing folder names at the same
    /// level as the item's parent — lets you type a partial name and complete to
    /// an existing folder to deliberately merge into it.
    SiblingFolders(Vec<String>),
}
```

**Assignment per field:**

| Field | `CompletionSource` |
|---|---|
| Add-torrent prompt lines | `Filesystem` |
| `save_path` (add-options form) | `Filesystem` |
| Move prompt | `Filesystem` |
| Rename-folder prompt | `SiblingFolders`, populated from the folder's siblings under the same parent (derived client-side from `TorrentDetail.files`, no new IPC needed) |
| Rename-file prompt | `None` — a file rename can only hard-reject on collision, never merge, so there's nothing to complete to |
| Add-tracker / add-feed URL | `None` |
| Settings: `default_save_path`, `ip_filter_path`, `watch_directories`, `network_cert_path`, `network_key_path` | `Filesystem` |
| Other settings (ratios, ports, proxy credentials, etc.) | `None` |
| List-filter / content-filter | `None` (still a `TextField`, for cursor movement only) |

`Filesystem` completion generalizes `tab_complete_path` to operate on the substring before
the cursor rather than assuming the whole buffer is the path, so it works correctly mid-line.

The existing hard-reject-on-file-collision check (already landed: `check_rename_collision`,
folder-rename's static-file check) is unchanged and stays authoritative — autocomplete only
makes it easier to *type* a merge target, it doesn't weaken collision detection.

---

## C. Unify `PriorityStep` and the Content tab

### C1. The bug this fixes

`commit_priority_step_rename` (`src/tui.rs:3247`) sends `RenameFolder`/`Move` with
`decisions: None` and never checks the response for `Response::RenameConfirmation` — unlike
`submit_prompt`, which correctly routes that response into `state.rename_confirm` and drives
the merge-confirmation UI. Today, renaming a folder inside the add-time step to merge it into
an existing one **silently does nothing** whenever the server needs a merge decision. This is
a direct consequence of `PriorityStep` reimplementing rename instead of sharing the main
flow's implementation.

### C2. The unification

One shared component (kept under the name `PriorityStep` for now — it now does more than
priorities, but renaming the type is a mechanical detail for the plan, not a design
decision) replaces the current pair of parallel implementations:

- **Tree browsing:** one `build_tree_rows` / `TreeRow` / collapse-expand implementation,
  used by both the live Content tab and the add-time step (today: `build_tree_rows` +
  `filter_content_rows` for the live tab, `current_rows()` reimplementing the equivalent
  logic against the step's own state).
- **Rename:** both surfaces open the same `Prompt` with the same `TextField` +
  `CompletionSource::SiblingFolders`/`None`, and both route the response through the same
  `submit_prompt` → `state.rename_confirm` two-phase flow. `PriorityStep`'s own
  `rename_buffer`/`rename_target`/`commit_priority_step_rename` are deleted, not
  parallel-maintained.
- **Priority-setting:** one `set_focused_priority`-equivalent function, called from both
  surfaces (today: `set_focused_priority` for the live tab, `set_step_priority` — explicitly
  commented as a duplicate — for the add-time step). Keys **0 through 7** map 1:1 to
  libtorrent's actual `download_priority_t` range (0=don't download, 1-3=low, 4=normal,
  5-6=high, 7=max), replacing both surfaces' current 5-key remap table
  (`'0'|'1'|'2'|'3'|'4'` → `0,1,4,6,7`). Keys 8 and 9 stay unused; there is no ninth or
  tenth priority level in libtorrent to bind them to.

### C3. Always-organize-before-start

Every add now goes through the organize step, not just paused adds. Concretely:
`dispatch_add_options` always adds with `start_paused: true` regardless of the user's chosen
`start` option, remembers the originally-requested start intent per entry, runs the (unified)
organize step for every entry, and on that entry's step concluding — by advancing normally or
by Esc-skipping — issues `Request::FinalizeAdd { index, resume }` where `resume` reflects the
original intent (see D2). This guarantees the chance to reorganize happens before any data
downloads, for every add, matching "modify the structure before starting it."

---

## D. Persisted default layout + revert

### D1. Storage

`ManagedTorrent` and `TorrentRecord` (`src/server.rs:23`/`45`) each gain, following the exact
pattern already used for `display_name`/`pending_layout`:

```rust
default_layout: Option<Vec<String>>,  // file_index → baseline path, snapshotted once
```

### D2. Capture point

`Request::FinalizeAdd { index: usize, resume: bool }` (new): the server snapshots every
file's current path into `default_layout` (this is the *only* time it's written — later
reverts don't overwrite it), then resumes the torrent if `resume` is true. This fires exactly
once per torrent, when its organize step concludes — after any content-layout renames (B3 of
the already-landed overhaul) and any manual reorganization during the step, so the baseline
reflects the user's deliberate setup, not just the raw torrent metadata.

### D3. Revert

`Request::RevertToDefaultLayout { index: usize, decisions: Option<RenameDecisions> }` (new,
same two-phase shape as the existing `RenameFolder`/`Move`): diffs current file paths against
the stored `default_layout` and computes the renames needed to restore it. This can touch many
files at once (not just one folder prefix), so the merge/conflict analysis runs across the
whole batch atomically — if any single file in the revert set would hard-conflict, the entire
revert is rejected, consistent with the existing all-or-nothing rule for folder rename. Reuses
`Response::RenameConfirmation` for any merge warnings the same way regular rename does.

Available from the Content tab at any later point (not just at add-time) via a new keybind,
gated behind a yes/no confirmation prompt (mirroring the existing `ConfirmDelete` pattern)
since it can undo a large amount of accumulated manual reorganization in one action.

---

## IPC / data-model summary

**New requests (`src/ipc.rs`):**
- `FinalizeAdd { index, resume }`
- `RevertToDefaultLayout { index, decisions: Option<RenameDecisions> }` (reuses the existing
  `RenameDecisions`/`RenameConcern` types from the rename-overhaul work)

**New server-side fields:** `default_layout: Option<Vec<String>>` on `ManagedTorrent` and
`TorrentRecord`.

**No new fields needed for:** rename-folder merge-autocomplete (computed client-side from
already-fetched `TorrentDetail.files`), or the TextField widget itself (pure client-side UI).

---

## Testing

- **Unit (Rust):** `TextField` cursor/word-boundary math over unicode strings (multi-byte
  chars at word boundaries, empty buffer, cursor at 0/end); `CompletionSource::SiblingFolders`
  candidate derivation from a synthetic file list; `default_layout` diff-and-revert
  computation including the atomic all-or-nothing collision case.
- **Manual (TUI):** arrow/home/end/word-delete/paste in each of the boxes listed in section B;
  tab-completion in each `Filesystem` and `SiblingFolders` box, including mid-line completion;
  renaming a folder to merge from *both* the live Content tab and the add-time step, confirming
  identical behavior (including the merge-confirmation prompt actually appearing); every add
  (paused or not) goes through the organize step; priority keys 0-7 set the expected libtorrent
  value; revert-to-default restores structure after a manual rename, and is rejected atomically
  when a revert would collide.

## Risks / notes

- `TextField` touches every text-entry call site in `tui.rs` — the largest mechanical part of
  the plan, but low risk per-site since the behavior is additive (nothing currently relies on
  append-only buffers).
- `RevertToDefaultLayout`'s batch collision analysis is new: existing rename/move handle one
  folder prefix or one file at a time, so the "diff N files at once, atomically" logic doesn't
  have an existing implementation to lift as-is, even though it reuses the same underlying
  collision-detection primitives.
- Always-organize-before-start changes existing behavior for `start: true` adds (they no longer
  begin downloading immediately) — worth calling out clearly in the plan/changelog since it's a
  user-visible workflow change, not just an internal refactor.
