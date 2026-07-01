# Glob expansion, live add validation, and add-result review: design

**Date:** 2026-07-01
**Status:** approved for planning

## Overview

This is sub-project #3 of the five-part filename/path effort (universal input, recent
paths, add-time validation/globbing/review, subfolder mode, remote auth). Sub-project #1
landed the shared `TextField` widget, `CompletionSource`, the unified organize step, and
`FinalizeAdd`/`RevertToDefaultLayout`; this spec builds directly on that work and on the
established two-phase confirm pattern (`Response::RenameConfirmation` plus the
"always"/"ask" string preferences in `src/config.rs`).

Three user-facing pieces, all centered on the add-torrent prompt:

- **A.** Shell-style glob expansion for local file paths in the add prompt. Globbing
  applies only where a filesystem exists to glob against: local paths. Magnets and URLs
  never glob.
- **B.** Instant per-line validity feedback while typing in the add prompt: each line
  shows what it will be treated as (magnet, url, existing file, glob with N matches, or
  not found), recomputed as that line is edited.
- **C.** A post-dispatch results overlay listing every expanded entry in order with its
  ok/fail outcome. Entries are dismissable one by one, all at once, or permanently via a
  persisted `add_result_review = "never"` preference.

The grounding principle carries over from sub-project #1: exactly one implementation of
each behavior. The live indicator, the submit-time expansion, and the daemon's
`sources::resolve()` all consume one shared classify function, so the indicator can never
predict something different from what resolve actually does.

### Non-goals

- No globbing for magnets, URLs, or any other non-local source, ever. There is no remote
  filesystem to expand against; this is a requirement, not a deferral.
- No glob support in other path fields (`save_path`, move prompt, watch directories,
  settings paths). Only the add prompt expands.
- No daemon-side expansion. The TUI owns the local filesystem view; if a remote-daemon
  mode ever lands, expansion stays client-side by design, since the paths the user types
  name files on the client's disk.
- No bulk "apply these options to all glob matches" step in the add-options form. Each
  expanded entry gets its own options pass, same as multi-line adds today. A glob matching
  many files means many passes; that ceiling is accepted here and a bulk-apply is a
  natural later addition if it hurts in practice.
- No debounced or async classification. Classification of the edited line runs inline per
  keystroke (one stat or one glob walk); see risks for the upgrade path.
- No pre-fetch verification of remote sources. Invalid or unverifiable magnets/URLs pass
  through to the daemon's fetch and surface as failures in the results overlay.
- Recent-paths list (sub-project #2) and remote auth (#5) stay out of scope.

---

## A. Shared classification + glob expansion

### A1. One classify function

`src/sources.rs` currently classifies inline inside `resolve()`: magnet prefix, then
`is_url()`, then fall through to tilde-expanded local path with an `exists()` check. A
glob pattern today just fails that check ("file not found: ...*.torrent"). The
classification logic gets extracted so the TUI and the daemon share it:

```rust
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

/// classify one trimmed input line. returns None for empty input.
pub fn classify(input: &str) -> Option<SourceKind>
```

`resolve()` is refactored to call `classify()` first and dispatch on the result; the
magnet/url/tilde logic moves, it is not duplicated. Since daemon and TUI live in one
binary, sharing is free. `resolve()` never receives a glob in practice (the TUI expands
before dispatch), but if one reaches it anyway (e.g. a future caller), `LocalGlob` fails
with a clear "glob patterns must be expanded by the client" error rather than the current
misleading "file not found".

Classification order for a non-magnet, non-url line, after tilde expansion and path
normalisation:

1. if the literal path exists on disk, it is `LocalPath`, even when it contains glob
   metacharacters. Torrent names routinely contain brackets ("[Group] Show.torrent");
   a pasted real path must never be reinterpreted as a character class.
2. otherwise, if it contains any of `*`, `?`, `[`, it is `LocalGlob`.
3. otherwise it is `LocalPath` (nonexistent, which the indicator and submit both surface).

### A2. The matcher

The `glob` crate (rust-lang owned, pure safe rust, zero transitive dependencies) is added
to `Cargo.toml` as the matcher. It supports `*`, `?`, `[...]`, and `**`; recursive `**`
comes free and is allowed, not blocked.

```rust
/// expand a glob pattern against the local filesystem. matches come back
/// in the crate's deterministic per-directory alphabetical order.
pub fn expand_glob(pattern: &str) -> Result<Vec<PathBuf>, String>
```

Unreadable entries yielded by the walk are skipped (they would fail at add anyway).
Directory matches are not filtered out: a directory reaching `Request::Add` fails at the
daemon with a real error, and the results overlay exists precisely to show that. No
special case in the expansion code.

### A3. Expansion point: prompt submit, TUI-side

Expansion happens in `submit_prompt`'s `PromptAction::Add` arm (`src/tui.rs`), before the
`AddOptionsForm` is built. Each line is classified; `LocalGlob` lines are replaced by
their matches, in line order, matches in `expand_glob` order. The flattened list becomes
`AddOptionsForm.entries`, so everything downstream (per-entry options, dispatch, the
organize step, the results overlay) already operates on expanded entries with no further
changes to its shape. Dispatch order therefore equals "order of adding plus glob
expansion" by construction.

Submit-time rejection, keeping the prompt open via the existing error-keeps-prompt path in
`handle_prompt_key`:

- a `LocalGlob` line with zero matches ("no matches: <pattern>")
- a `LocalPath` line that does not exist ("file not found: <path>")

Rationale: the TUI can verify local paths authoritatively and the live indicator already
flagged the problem, so there is nothing sensible to dispatch. Remote sources
(magnet/url) always pass through unverified, per requirements. A file vanishing between
submit and dispatch is still caught by the daemon's `resolve()` and lands in the results
overlay; the submit-time check is a courtesy, not the safety net.

---

## B. Live per-line validation in the add prompt

### B1. State

`Prompt` (`src/tui.rs`) gains a cached indicator per line, parallel to `lines`:

```rust
enum LineIndicator {
    Empty,          // blank line, draw nothing
    Magnet,         // "magnet"
    Url,            // "url"
    FileOk,         // "file ok"
    Glob(usize),    // "glob: N matches"
    NotFound,       // "not found"
}
```

Indicators are derived from `classify()` plus, for `LocalPath`, its embedded existence
knowledge, and for `LocalGlob`, a match count from `expand_glob`. There is no second
parser: the indicator is a thin projection of the same function submit uses, which is
what makes the feedback trustworthy.

Only `PromptAction::Add` prompts compute indicators; every other prompt keeps an empty
vec and draws nothing. Recomputation is scoped to the edited line only: any mutating key
on the current line (insert, backspace, delete, word ops, tab-complete) reclassifies just
`cursor_line`. Paste reclassifies each line it created; shift+enter inserts an `Empty`
indicator alongside the new line; cursor movement between lines recomputes nothing.

### B2. Rendering

`draw_prompt` appends one dim suffix span per line after the buffer content, e.g.
`  [glob: 4 matches]`. Green for `Magnet`/`Url`/`FileOk`/`Glob(n >= 1)`, red for
`NotFound`/`Glob(0)`, nothing for `Empty`. No layout changes; the suffix rides on the
existing per-line `Line` construction in `draw_prompt`'s body loop.

Note the deliberate semantics: a line like `asdfgh` is neither magnet nor url, so it
classifies as a local path and shows red "not found". That is exactly what `resolve()`
would conclude, so the indicator predicting it is correct, not overeager.

---

## C. Add-result review overlay

### C1. Collecting outcomes

`dispatch_add_options` (`src/tui.rs`) currently collapses everything into one joined
status string in `state.error`. It now also collects a per-entry outcome in dispatch
order (entries are already glob-expanded, see A3):

```rust
struct AddResultEntry {
    source: String,                 // the uri/path as dispatched
    outcome: Result<(), String>,    // Ok, or the failure reason
}

struct AddResultsReview {
    entries: Vec<AddResultEntry>,   // dispatch order == add + expansion order
    focused: usize,
}
```

The existing one-line summary in `state.error` is kept in both modes (it is the status
line and costs nothing); the overlay adds per-entry detail on top when enabled.

### C2. The overlay

`AppState` gains `add_results: Option<AddResultsReview>`. It renders with the same modal
recipe as `draw_rename_confirm` and `draw_help_overlay`: centered `Rect`, `Clear`,
rounded yellow border, dim hint line at the bottom. One row per entry: ok/fail marker,
source (middle-truncated to fit), and the failure reason for failed entries. The list
scrolls when longer than the modal body, keeping `focused` visible.

Keys, shown in the hint line:

| Key | Action |
|---|---|
| w/s, Up/Down | move focus (wasd plus arrows, matching the rest of the tui) |
| Enter or d | dismiss the focused entry; removing the last one closes the overlay |
| Shift+D | dismiss all, close the overlay |
| Ctrl+D | dismiss all, close, and persist `add_result_review = "never"` |
| Esc | same as Shift+D (every modal in the app exits on esc) |
| Ctrl+C | quit, same as everywhere else |

Input-routing ladder position: `add_results` slots directly after `state.prompt` and
before `state.priority_step` in the ladder in `run()`. Dispatch creates both the review
and the organize step in the same call; the review captures input first, and dismissing
it reveals the organize step already queued underneath. `draw` ordering mirrors the
ladder so the overlay also renders on top.

### C3. The persisted preference

`Config` (`src/config.rs`) gains, next to the `rename_*` preferences:

```rust
/// show the per-entry results overlay after adding torrents: always | never
#[serde(default = "default_always")]
pub add_result_review: String,
```

with a `sanitize()` arm resetting anything outside `"always" | "never"` to the default,
and an `apply_config_change` arm in `src/server.rs` validating the same set (mirroring
`rename_merge_same`). Default `"always"`: the overlay shows. `"never"`: dispatch skips
building `AddResultsReview` entirely and the current one-line summary is all the user
sees, exactly today's behavior.

Persistence path for Ctrl+D follows the established persist-a-choice pattern from
`handle_rename_confirm_key` (`submit_set("rename_merge_same", "always")`): send
`Request::SetConfig { key: "add_result_review", value: "never" }` so the daemon's
in-memory config and the on-disk file both update. The TUI reads the preference once at
startup from the existing `Config::load()` in `AppState` init, keeps it as an `AppState`
field, and flips that field in memory when Ctrl+D fires, so the very next add in the same
session already skips the overlay. Re-enabling is an edit via the settings page or
config.toml, symmetric with how `rename_merge_same = "always"` is undone today.

---

## IPC / data-model summary

**New IPC: none.** Adds already go through per-entry `Request::Add`; outcomes are
collected client-side from the responses the TUI already receives. Preference persistence
reuses `Request::SetConfig`.

**New config field:** `add_result_review: String` ("always" | "never", default "always"),
with `sanitize()` and `apply_config_change` validation arms.

**New dependency:** `glob` crate in `Cargo.toml`.

**New shared code:** `SourceKind` + `classify()` + `expand_glob()` in `src/sources.rs`;
`resolve()` refactored on top of `classify()`.

**New TUI state:** per-line indicator cache on `Prompt`; `AddResultsReview` on
`AppState`; `add_result_review` preference mirror on `AppState`.

---

## Decisions

Every open choice, decided here so implementation needs no further input:

1. **`glob` crate as the matcher.** rust-lang owned, safe rust, zero transitive deps;
   hand-rolling glob semantics is exactly the wheel not worth reinventing.
2. **Expansion at prompt submit, TUI-side, before the options form.** Everything
   downstream then operates on expanded entries unchanged, and result order equals
   expansion order by construction.
3. **One shared `classify()` consumed by indicator, submit, and `resolve()`.** The
   indicator can never disagree with what dispatch actually does.
4. **Literal-path-exists beats glob interpretation.** Bracket-laden torrent filenames are
   common; a pasted real path must always win over character-class parsing.
5. **Zero-match globs and nonexistent local paths block submit (prompt stays open);
   remote sources always pass through.** Local verification is authoritative; remote
   verification before fetch is impossible.
6. **Directory glob matches are not filtered.** The daemon rejects them with a real
   error and the review overlay surfaces it; no special case needed.
7. **Recursive `**` is allowed.** It comes free with the crate; blocking it would be
   added code for less capability.
8. **Indicator recomputes only the edited line, inline, no debounce.** One stat or glob
   walk per keystroke on one line; see risks for the upgrade path.
9. **Per-match options passes in the add-options form, no bulk-apply.** Consistent with
   multi-line adds today; bulk-apply is a clean later addition if globs make it painful.
10. **Overlay keys: enter/d, shift+d, ctrl+d, esc as dismiss-all alias.** Esc exits every
    other modal; omitting it here would be the surprising choice.
11. **`add_result_review` persisted via `Request::SetConfig`.** Matches the
    rename-preference persist-a-choice pattern and keeps the daemon's in-memory config
    coherent, unlike a direct client-side `Config::save()`.
12. **Status-line summary kept in both modes.** It is one existing code path and keeps
    the status bar meaningful after the overlay closes.
13. **Overlay outranks the organize step in the input ladder.** Review what happened
    first, then organize what succeeded.

---

## Testing

- **Unit (Rust):** `classify()` table covering magnet, each url scheme, tilde paths,
  windows drive paths, metacharacter inputs where the literal path exists (tempdir) vs
  not, and empty input; `expand_glob` ordering, zero-match, and `**` against a tempdir
  fixture tree; `LineIndicator` derivation for each `SourceKind` variant including
  `Glob(0)`; `sanitize()` resetting an invalid `add_result_review` value.
- **Manual (TUI):** indicator updates live per line and only for the edited line; paste
  of multiple lines classifies each; submit with a zero-match glob or missing file keeps
  the prompt open with the error; a glob line expands into per-entry options passes and
  dispatches in order; results overlay lists entries in add + expansion order with
  correct ok/fail reasons; enter/d, shift+d, ctrl+d, esc behave per the table; ctrl+d
  writes `add_result_review = "never"` to config.toml and the next add in the same
  session skips the overlay, falling back to the one-line summary; dismissing the
  overlay reveals the organize step for the successful adds.

## Risks / notes

- Re-globbing on every keystroke can lag in enormous directories (the walk touches every
  candidate in the wildcard components). Accepted for now since it scans only on edits to
  a glob line; the upgrade path is debouncing classification to a short idle tick, purely
  a TUI-side change.
- A nonexistent literal path containing brackets classifies as a glob and shows
  "glob: 0 matches" rather than "not found". Mildly misleading but visible and honest;
  the literal-exists rule already covers the case that matters (real bracket-laden
  files).
- `AddOptionsForm.entries` growing large via glob makes the one-at-a-time options walk
  tedious. Known ceiling, called out in non-goals; bulk-apply is the future fix and
  nothing in this design blocks it.
- The dispatch loop's `Request::List` len-minus-one indexing for post-add tweaks is
  untouched by this design; its raciness (if any) predates and is orthogonal to this
  sub-project.
