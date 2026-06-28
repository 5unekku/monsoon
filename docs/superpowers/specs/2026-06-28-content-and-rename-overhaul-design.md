# Content layout, rename overhaul, and TUI bug fixes — design

**Date:** 2026-06-28
**Status:** approved for planning

## Overview

Four mostly-independent pieces of work, shipped as one design / one plan, implemented
bugs-first:

- **A. Two TUI bugs** — clicking a torrent selects the wrong row; greyed text becomes
  invisible when the row is selected.
- **B. Subfolder (content layout) mode** — finish the `todo!()` at `src/tui.rs:2271`:
  a four-option picker plus a preference, with the layout applied daemon-side.
- **C. Rename & move overhaul** — plain-name input with `../` ascent, root-escape
  prevention, merge warnings, hard-rejection of file conflicts, and move/leave handling
  for untracked files; the same merge/conflict rules extended to the torrent **move**
  (save-path relocation) operation.
- **D. Doc cleanup** — fix the dangling `plans/TODO-tui.md` reference at `src/tui.rs:339`.

### Non-goals

- No change to the `src/bridge.cpp:578` libtorrent-2.1 note; it is a deliberate
  upgrade marker and stays.
- No move/leave handling of untracked files for the **root move** operation — `move_storage`
  only relocates the torrent's own files; untracked files at the root are out of scope for
  move (they are only in scope for in-torrent folder rename, area C3).
- No auto-overwrite / merge-with-replace for file-on-file conflicts anywhere — those are
  always hard-rejected.

---

## A. TUI bug fixes

### A1. Click selects the wrong row

**Symptom (reported):** clicking a torrent row selects the row above or below it.

**Code:** `mouse_left_down` (`src/tui.rs`) maps a click in the list to
`target = row - (list_rect.y + 2)` and calls `state.table_state.select(Some(target))`
where `target` indexes `filtered_indices()` directly.

**Two suspected causes, both to be confirmed by reproduction (systematic-debugging):**

1. **Header offset mismatch.** The `+2` assumes a 1-row border followed by a 1-row header.
   If `draw_torrent_list` renders the table without a header row (or with a different
   layout), every click lands one row off — consistent with "selects the one below".
2. **Ignored vertical scroll offset.** `target` is used as an absolute index into
   `filtered_indices()` but is computed purely from the on-screen row. Once the list is
   scrolled, the click is off by the table's scroll offset.

**Fix:** reproduce first, then correct the hit-test to (a) match the actual rendered
header/border geometry and (b) add the table's current vertical scroll offset before
indexing. Verify the selected row equals the clicked row at the top of the list and after
scrolling down.

### A2. Greyed text invisible when selected

**Symptom:** a file with priority 0 (skip), or a stopped torrent, renders in grey; when that
row is selected the grey text disappears.

**Cause:** several tables set `row_highlight_style(Style::default().bg(Color::DarkGray) …)`
with no foreground (`src/tui.rs:3413`, `4120`, `4439`, `4489`, `4547`), while greyed rows use
`fg(Color::DarkGray)` (e.g. priority-0 rows at `src/tui.rs:3393`). ratatui patches the
highlight style over the row's base style, so the row keeps `fg(DarkGray)` on a `DarkGray`
background — invisible.

**Fix:** give the selected-row highlight an explicit, contrasting style — a readable
foreground plus a background distinct from `DarkGray` (e.g. reversed video, or a brighter
background with an explicit light foreground), applied consistently across the affected
tables. The main torrent list already uses `fg(Black).bg(Cyan)` (`src/tui.rs:2933`) and is
fine; align the file/tracker/priority tables with an equally legible scheme. Verify that
priority-0 / skipped / stopped rows stay readable when selected.

---

## B. Subfolder (content layout) mode

### B1. Picker

Replace `enum SubfolderMode { Default, Yes, No }` (`src/tui.rs:998`) with four variants:

| Variant      | Label                | Meaning                                                       |
|--------------|----------------------|--------------------------------------------------------------|
| `Default`    | `default`            | resolve to the `default_content_layout` preference at apply  |
| `Always`     | `always`             | wrap content in a folder named after the torrent             |
| `Never`      | `never`              | strip the root folder; files go straight in the save path    |
| `IfMultiple` | `if multiple files`  | folder when >1 file, bare file when single-file              |

Update `label()`, `cycle()` (now four-way), the activation handler
(`activate_add_options_field`, `src/tui.rs:2213`), and the add-options form summary
(`src/tui.rs:3526`).

### B2. Preference

New config field **`default_content_layout: String`** in `src/config.rs`, values
`always` | `never` | `if_multiple`, **default `if_multiple`** (the natural behavior). Add it
to the `apply_config_change` key match in `src/server.rs` and to the TUI settings overlay so
it is editable. `SubfolderMode::Default` resolves to this value when the layout is applied.

### B3. Apply mechanism (daemon-side, after verification)

`Request::Add` gains `content_layout: ContentLayout` (the four-way enum). `Default` is
resolved against the `default_content_layout` preference **server-side**, at apply time, since
the preference lives in the daemon config. The daemon stores the desired layout on the torrent
record (`pending_layout: Option<ContentLayout>`).

The layout is applied once the torrent reaches a **stable, verified state** — metadata present
and the initial file check (`checking_resume_data` / `checking_files`) complete. This makes one
code path work for both `.torrent` adds (metadata immediate) and magnets (metadata deferred).
Practically, metadata delivers structure and the file list together, so for a fresh add no data
exists yet and applying the layout is a cheap path rewrite. If the layout is **changed later**,
it is re-applied (honoring the new choice even if that means libtorrent physically moves
already-downloaded files).

**Path-rewrite computation** (given the file list and the torrent name):

- Determine the common root folder, if any: the first path component shared by every file.
  A multi-file torrent normally has one (its name); a single-file torrent has none.
- **Always:** ensure a root folder named after the torrent. If a common root already exists,
  no-op; if not (single-file, or flat multi-file), prepend `"<torrent name>/"` to every path.
- **Never:** if a common root exists, strip it from every path; else no-op.
- **IfMultiple:** files > 1 → behave as **Always**; files == 1 → behave as **Never**.

Apply by issuing `handle.rename_file(i, new_path)` for each file whose path changes, then clear
`pending_layout`. The existing alert loop already logs `file_renamed_alert` /
`file_rename_failed_alert`.

This deletes the `todo!()` at `src/tui.rs:2271`; the dispatch loop simply includes the resolved
layout in each `Request::Add`.

---

## C. Rename & move overhaul

Three operations share one set of merge/conflict rules:

- **rename file** — `PromptAction::RenameFile` → `Request::RenameFile`
- **rename folder** — `PromptAction::RenameFolder` → `Request::RenameFolder`
- **move** — relocate the torrent root / save path → `Request::Move` (`move_storage`)

### C1. Plain-name input with `../` ascent, no root escape

Today the folder prompt seeds the buffer with `row.full_path` (`src/tui.rs:1739`) and the file
prompt with `file.path` (`src/tui.rs:1750`) — both full paths from the torrent root.

**New behavior:**

- Seed the buffer with the item's **own name** — the last path component of `full_path`
  (folder) or `file.path` (file).
- Interpret the typed input **relative to the item's parent directory**. Compute
  `new_full = normalize(parent + "/" + input)`.
- Support `../` to ascend one level. `normalize` collapses `.` and `..` segments.
- **Reject** if the normalized path still contains a leading `..` (would escape the torrent
  root) or is empty / resolves to the root itself.
- Resolution and the escape-check happen TUI-side (it knows the parent). The already-resolved,
  root-relative path is sent to the server. The server keeps `validate_rename_name`
  (`src/server.rs:1277`, which rejects raw `..`, absolute paths, null bytes) as
  defense-in-depth — the resolved path contains no `..`, so it passes.
- Update the prompt helper text to describe plain-name + `../` semantics.

### C2. Merge warnings and file-conflict rejection

For each operation the daemon inspects the outcome before committing and classifies it:

1. **No conflict** → proceed silently, no prompt.
2. **Folder merge** → the destination already holds files (this torrent's and/or unrelated
   on-disk files). **Allowed, but warned** (Always / Yes / No), subject to preferences (C4).
3. **File-on-file conflict** → a renamed/moved file would land exactly on an existing file
   path. **Hard-reject the entire operation**, atomically (nothing is submitted). The error
   message instructs the user to rename the conflicting file manually, then retry the merge.
   This is never a preference and never prompted.

Two merge situations are distinguished (each with its own preference):

- **Merge into a folder that already holds this torrent's files** — detectable from the file
  list alone: a non-renamed torrent file already lives under the destination prefix. The
  server's `rename_folder` already computes this (`static_files`, `src/server.rs:664`).
- **Merge into an on-disk folder that also holds unrelated files** — the real destination
  directory (`save_path/<dest>`) contains files not in the torrent's file list. Requires a
  server-side filesystem scan. Surfaced with an extra "unrelated files present" warning line.

File-conflict detection already exists for single-file rename (`check_rename_collision`,
`src/server.rs:1468`) and folder rename (`src/server.rs:686`); both currently return a rejected
result. This work routes those into the hard-reject path with the "rename manually" message and
extends the same check to the move operation.

### C3. Untracked files inside a renamed folder — move or leave

When **renaming a folder**, the real source directory (`save_path/<old_prefix>`) may contain
files that are not part of the torrent. libtorrent only moves its own files, leaving these
behind. The user chooses **Move** (relocate them alongside the rename) or **Leave** (leave them
at the old location), subject to a preference (C4). "Move" also sweeps up an otherwise-empty
leftover directory (i.e. cleans up / relocates the now-empty folder). This applies only to
folder rename, not to file rename or the root move.

When "Move" is chosen, the server performs the libtorrent renames for tracked files first
(libtorrent creates the destination directory), then physically relocates the untracked
files/empty dirs into the destination via `std::fs`.

### C4. Preferences (three)

New config fields in `src/config.rs`, all consulted server-side, all editable in the TUI
settings overlay and via `SetConfig`:

| Field                      | Values                                   | Default | Governs                                                  |
|----------------------------|------------------------------------------|---------|----------------------------------------------------------|
| `rename_merge_same`        | `always` \| `ask`                        | `ask`   | merge into a folder already holding this torrent's files |
| `rename_merge_unrelated`   | `always` \| `ask`                        | `ask`   | merge into a folder that also holds unrelated files      |
| `rename_untracked_files`   | `always_move` \| `always_leave` \| `ask` | `ask`   | untracked files inside a renamed folder                  |

`always` / `always_move` / `always_leave` skip the prompt and act accordingly; `ask` prompts.

### C5. Confirmation flow (server-driven, two-phase)

Because the daemon may run on a different machine, **all filesystem inspection is server-side**.

1. The TUI sends the rename/move request with an optional `decisions` payload (initially empty).
2. The server analyzes, applying preferences:
   - any **file-on-file conflict** → return an error immediately (hard reject), regardless of
     decisions.
   - each applicable **merge** / **untracked** concern whose preference is `always*` → resolved
     automatically.
   - each applicable concern whose preference is `ask` and which has no decision yet → collected.
3. If any concern still needs a decision, the server returns a new
   `Response::RenameConfirmation { concerns: Vec<RenameConcern> }` describing what applies
   (including, for the untracked case, how many files would move).
4. The TUI shows the applicable prompts **sequentially**, in a fixed order (merge-same →
   merge-unrelated → untracked), only for concerns the server flagged. Prompt buttons:
   - merges: **Always / Yes / No**
   - untracked: **Always move / Always leave / Move / Leave**
   - **No** cancels the whole operation.
5. For any "Always…" choice the TUI also sends a `SetConfig` to persist that preference.
6. The TUI re-sends the original request with explicit `decisions`. The server commits: tracked
   renames/moves first, then untracked `std::fs` moves for any "move" decision.

### C6. Move (root relocation) brought into scope

`move_storage` (`src/server.rs:536`) currently submits to libtorrent with no pre-checking. It
gains the same pre-commit analysis: scan the destination, **warn on folder merge**, **hard-reject
on file conflict**, then proceed. Reuse the two-phase `decisions` flow. (libtorrent's
`move_storage` flags may additionally be set to a non-overwriting mode for belt-and-suspenders,
but the authoritative guard is the server-side pre-check.)

---

## D. Doc cleanup

`src/tui.rs:339` references `plans/TODO-tui.md`, which does not exist. Update the comment to
drop the dead path while keeping the `security-anonymity-priorities` memory pointer.

---

## IPC / data-model summary

**New / changed requests (`src/ipc.rs`):**

- `Add` gains `content_layout: ContentLayout`.
- `RenameFile`, `RenameFolder`, `Move` each gain an optional `decisions: RenameDecisions`
  (merge approval flags + untracked move/leave choice).

**New responses:**

- `Response::RenameConfirmation { concerns: Vec<RenameConcern> }` — returned when one or more
  `ask` concerns need a decision. `RenameConcern` enumerates: `MergeSame`,
  `MergeUnrelated { unrelated_count }`, `UntrackedFiles { count }`.

**New enums (shared):**

- `ContentLayout { Default, Always, Never, IfMultiple }`.

**New config fields (`src/config.rs`):** `default_content_layout`, `rename_merge_same`,
`rename_merge_unrelated`, `rename_untracked_files` (see tables above), each wired into
`apply_config_change` and the TUI settings overlay.

---

## Testing

- **Unit (Rust):**
  - path resolution / normalization for C1, including `../` ascent and root-escape rejection.
  - content-layout path rewrites (B3) for single-file and multi-file inputs across
    Always / Never / IfMultiple.
  - concern classification (C2/C3): given a synthetic file list plus temp directories, assert
    merge-same vs merge-unrelated vs untracked detection and file-conflict hard-reject.
- **Manual (TUI):**
  - A1: click selects the clicked row at top and after scrolling.
  - A2: priority-0 / stopped rows stay legible when selected.
  - B: each picker option produces the expected on-disk layout for a `.torrent` and a magnet.
  - C: merge warning appears and is approvable; file conflict is rejected with the guidance
    message; untracked move/leave behaves; `Always…` choices persist to config; move honors the
    same rules.

## Risks / notes

- The verified-state apply (B3) needs a reliable signal that the initial check is done; if the
  chosen signal is noisy, guard against applying the layout more than once per torrent
  (the `pending_layout` flag is cleared on apply).
- Untracked-file `std::fs` moves (C3) must order after the libtorrent renames and handle the
  source directory being emptied; failures there should surface to the user without corrupting
  the tracked rename that already succeeded.
