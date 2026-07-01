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
        // never let a completion shrink what the user typed (multi-char
        // lowercase expansions can make the lcp shorter than the input)
        if (completed.chars().count() < before.chars().count()) { return; }
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
}

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

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
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
        field.delete_forward();
        assert_eq!(field.buffer(), "abc");
        assert_eq!(field.cursor(), 3);
    }

    #[test]
    fn cursor_movement_clamps_at_both_ends() {
        let mut field = TextField::new("ab");
        field.move_right();
        assert_eq!(field.cursor(), 2);
        field.set_cursor(0);
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

    #[test]
    fn move_word_left_skips_separators_then_word() {
        let mut field = TextField::new("foo/bar baz");
        field.set_cursor(11);
        field.move_word_left();
        assert_eq!(field.cursor(), 8);
        field.move_word_left();
        assert_eq!(field.cursor(), 4);
        field.move_word_left();
        assert_eq!(field.cursor(), 0);
    }

    #[test]
    fn move_word_right_skips_word_then_separators() {
        let mut field = TextField::new("foo/bar baz");
        field.set_cursor(0);
        field.move_word_right();
        assert_eq!(field.cursor(), 3);
        field.move_word_right();
        assert_eq!(field.cursor(), 7);
        field.move_word_right();
        assert_eq!(field.cursor(), 11);
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
        let prefix_char_len = format!("{}/down", temp_dir.display()).chars().count();
        field.set_cursor(prefix_char_len);
        field.tab_complete();
        assert!(field.buffer().starts_with(&format!("{}/downloads/", temp_dir.display())));
        assert!(field.buffer().ends_with(" TAIL"));
        // cursor must sit at the end of the completed prefix, before the tail
        let completed_prefix_len = format!("{}/downloads/", temp_dir.display()).chars().count();
        assert_eq!(field.cursor(), completed_prefix_len);
        std::fs::remove_dir_all(&temp_dir).ok();
    }

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

    #[test]
    fn empty_buffer_operations_do_not_panic() {
        let mut field = TextField::new("");
        field.backspace();
        field.delete_forward();
        field.delete_word_backward();
        field.delete_word_forward();
        field.move_word_left();
        field.move_word_right();
        assert_eq!(field.buffer(), "");
        assert_eq!(field.cursor(), 0);
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
}
