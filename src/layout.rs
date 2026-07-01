//! pure path logic shared by the tui (rename input resolution) and the daemon
//! (content layout + folder-rename planning). no libtorrent or filesystem
//! access — just string work over `/`-separated torrent paths.

use crate::ipc::ContentLayout;

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
}
