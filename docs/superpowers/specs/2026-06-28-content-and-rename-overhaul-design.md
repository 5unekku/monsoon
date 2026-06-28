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

**Fix:** give the selected-row highlight an **explicit foreground and background** (not the
`REVERSED` modifier — reversing a row that already carries `fg(DarkGray)` just swaps it to a
`DarkGray` background and can stay low-contrast). Setting an explicit `fg` deterministically
overrides the row's greyed foreground. Apply one legible scheme consistently across the affected
tables; the main torrent list already uses `fg(Black).bg(Cyan)` (`src/tui.rs:2933`) and is a good
template. Verify that priority-0 / skipped / stopped rows stay readable when selected.

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

The layout is applied when the torrent first reaches a verified state — triggered by
libtorrent's **`torrent_checked_alert`**, which fires after metadata is present and the initial
file check completes, so one path covers both `.torrent` and magnet adds. The daemon keeps a
per-torrent latch and clears `pending_layout` on apply, so the renames it issues — which
themselves cause a re-check — do not re-trigger application. If the layout is **changed later**,
`pending_layout` is set again and re-applied (honoring the new choice even if libtorrent must
physically move already-downloaded files). `Default` is resolved against `default_content_layout`
once, at apply time; changing the preference afterward does **not** retroactively re-lay-out
torrents already processed.

**Implementation dependency:** confirm `torrent_checked_alert` is surfaced by
`bridge_pop_alerts` (the bridge currently maps `file_renamed_alert`, `storage_moved_alert`,
etc.); add it to the mapping if missing. No other new bridge field is needed — `state` and
`files()` already expose what's required.

**Path-rewrite computation.** libtorrent always presents a multi-file torrent's files under its
name as the first path component (`"<name>/…"`), and a single-file torrent as a bare file whose
path equals the name. So only two transforms are ever non-trivial:

- **Never** — strip a leading `"<name>/"` from every file path. Affects multi-file torrents
  (files move directly into the save path, inner structure preserved); single-file paths have no
  such prefix, so it is a no-op.
- **Always** — ensure the content sits in a folder. Multi-file torrents already have their root
  folder, so this is a **no-op** (Always adds a folder when one is absent; it does not rename an
  existing root folder). A single-file torrent's lone file is renamed from `<filename>` to
  `"<torrent name>/<filename>"`, where **torrent name** is the effective display name
  (`display_name` if set via `RenameTorrent`, else libtorrent's metadata `status.name`; see
  `src/server.rs:1229`), sanitized to a single safe path component, and **filename** is the
  basename of the file. The two can differ (a renamed torrent, or a magnet `dn`), so the result
  is `name/filename` — **not** necessarily `name/name`.
- **IfMultiple** — the torrent's natural layout already satisfies this (multi-file has a root
  folder, single-file does not), so it is a **no-op in all cases**. It exists as the safe default
  and is the value `Default` usually resolves to.

**Concrete examples** (file shown as `movie.mkv`, torrent name as `Name`):

| Torrent                          | Default / IfMultiple | Always            | Never        |
|----------------------------------|----------------------|-------------------|--------------|
| single-file, file `movie.mkv`    | `movie.mkv`          | `Name/movie.mkv`  | `movie.mkv`  |
| multi-file under `Name/`         | `Name/movie.mkv`     | `Name/movie.mkv`  | `movie.mkv`  |

(`Never` on a single-file torrent and `Always` on a multi-file torrent are both no-ops — the file
is already where that layout would put it.)

Apply by issuing `handle.rename_file(i, new_path)` for each file whose path changes, then clear
`pending_layout`. The existing alert loop already logs `file_renamed_alert` /
`file_rename_failed_alert`.

This deletes the `todo!()` at `src/tui.rs:2271`; the dispatch loop simply includes the resolved
layout in each `Request::Add`. **Internal add paths** (RSS feeds, watch directories — which call
`add_magnet` / `add_file` directly) pass `ContentLayout::Default`, so they honor
`default_content_layout` too.

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
- Support `../` to ascend one level. `normalize` is **purely lexical** (string-only, no
  filesystem access — these are torrent-internal `/`-separated paths, not real on-disk paths):
  it collapses `.` and resolves `..` against preceding segments.
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

**Detection is part logical, part on-disk:** *merge-same* is computed from the torrent's file
list (logical — independent of what has downloaded); *merge-unrelated* and *untracked* (C3)
require scanning the real directory and therefore only see files that have actually been
materialized on disk. **Applicability by operation:** a **file rename** can only ever trigger the
file-conflict hard-reject — it cannot merge a folder and has no untracked-files concern. **Folder
rename** can trigger all three concerns. **Move** can trigger merge + file-conflict (no untracked
handling — see non-goals).

### C3. Untracked files inside a renamed folder — move or leave

When **renaming a folder**, the real source directory (`save_path/<old_prefix>`) may contain
files that are not part of the torrent. libtorrent only moves its own files, leaving these
behind. The user chooses **Move** (relocate them alongside the rename) or **Leave** (leave them
at the old location), subject to a preference (C4). "Move" also sweeps up an otherwise-empty
leftover directory (i.e. cleans up / relocates the now-empty folder). This applies only to
folder rename, not to file rename or the root move.

When "Move" is chosen: libtorrent's `rename_file` calls are **asynchronous** (they complete
later via `file_renamed_alert`), so we cannot sequence the untracked moves strictly "after" the
tracked ones. We don't need to — they operate on **different files**. The server therefore:
(1) ensures the destination directory exists (`std::fs::create_dir_all`); (2) `std::fs::rename`s
the untracked files into it — independent of libtorrent's timing; (3) submits the libtorrent
renames for the tracked files. The only genuinely order-sensitive step is **removing the now-empty
source directory** (since the tracked files leave it asynchronously): this is done **best-effort
and deferred** — attempt a non-recursive `remove_dir` after the batch's `file_renamed_alert`s
arrive, and let a later pass retry if it's not yet empty. A failed untracked `std::fs` move is
surfaced to the user without rolling back tracked renames that already succeeded.

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
6. The TUI re-sends the original request with explicit `decisions`. The server commits per C3:
   ensure the destination dir, `std::fs`-move any untracked files for a "move" decision, submit
   the libtorrent renames, and defer the best-effort empty-source-dir cleanup.

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
- `RenameFile`, `RenameFolder`, `Move` each gain an optional `decisions: Option<RenameDecisions>`
  (absent on the first call → server analyzes; present on the re-send → server commits).
  `RenameDecisions { merge_same: bool, merge_unrelated: bool, untracked: UntrackedChoice }`
  where `UntrackedChoice { Move, Leave }`. The `bool`s mean "user approved this merge".

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

- The folder name for single-file `Always` comes from the torrent's display name, which is
  user-controllable (`RenameTorrent`) and from magnet `dn`; it **must be sanitized** to a single
  safe path component (reject/strip `/`, `..`, null, leading/trailing dots-or-spaces) before being
  used as an on-disk directory.

- The B3 trigger relies on `torrent_checked_alert`; if the bridge doesn't surface it, that's a
  small bridge addition (see B3 implementation dependency). The per-torrent latch
  (`pending_layout` cleared on apply) prevents double-application when the layout renames cause a
  re-check.
- C3's only async-sensitive step is empty-source-dir cleanup; it is best-effort/deferred (see
  C3). Untracked `std::fs` move failures surface to the user without rolling back tracked renames
  that already succeeded.
- Two-phase rename (C5) has a small TOCTOU window between analysis and the decision re-send; for a
  single-user daemon this is acceptable, and the commit step's own checks remain authoritative.
