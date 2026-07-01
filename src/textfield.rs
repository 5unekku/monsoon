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
}
