//! Single-line text editing for the comment input.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LineInput {
    pub text: String,
    /// Cursor position in chars.
    pub cursor: usize,
}

/// What a key did to the input.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Edited,
    Submit,
    Cancel,
}

impl LineInput {
    pub fn new(text: &str) -> Self {
        LineInput {
            text: text.to_string(),
            cursor: text.chars().count(),
        }
    }

    /// Byte offset of the cursor in `text`.
    pub fn cursor_byte(&self) -> usize {
        self.byte(self.cursor)
    }

    fn byte(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn insert(&mut self, s: &str) {
        let at = self.byte(self.cursor);
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    /// Inserts pasted text on the single line: line breaks become spaces, tabs are expanded
    /// (as in diffs) and other control characters dropped.
    pub fn paste(&mut self, s: &str) {
        let mut clean = String::with_capacity(s.len());
        for c in s.replace("\r\n", "\n").chars() {
            match c {
                '\n' | '\r' => clean.push(' '),
                '\t' => clean.push_str("    "),
                c if c.is_control() => {}
                c => clean.push(c),
            }
        }
        self.insert(&clean);
    }

    /// Removes the chars in `from..to`, leaving the cursor at `from`.
    fn delete(&mut self, from: usize, to: usize) {
        let (a, b) = (self.byte(from), self.byte(to));
        self.text.replace_range(a..b, "");
        self.cursor = from;
    }

    fn delete_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.delete(i, self.cursor);
    }

    pub fn on_key(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return Outcome::Submit,
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('c') if ctrl => return Outcome::Cancel,
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.len(),
            KeyCode::Char('u') if ctrl => self.delete(0, self.cursor),
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Char(_) if ctrl => {}
            KeyCode::Char(c) => self.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Backspace if self.cursor > 0 => self.delete(self.cursor - 1, self.cursor),
            KeyCode::Delete if self.cursor < self.len() => {
                self.delete(self.cursor, self.cursor + 1)
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.len(),
            _ => {}
        }
        Outcome::Edited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn edits_text() {
        let mut i = LineInput::new("hé");
        assert_eq!(i.cursor, 2);
        i.on_key(&key(KeyCode::Char('ü')));
        assert_eq!(i.text, "héü");
        i.on_key(&key(KeyCode::Left));
        i.on_key(&key(KeyCode::Backspace));
        assert_eq!((i.text.as_str(), i.cursor), ("hü", 1));
        i.on_key(&key(KeyCode::Delete));
        assert_eq!(i.text, "h");
        i.insert(" foo  bar ");
        i.on_key(&ctrl('w'));
        assert_eq!(i.text, "h foo  ");
        i.on_key(&ctrl('w'));
        assert_eq!(i.text, "h ");
        i.on_key(&ctrl('a'));
        i.on_key(&key(KeyCode::Char('>')));
        i.on_key(&ctrl('e'));
        assert_eq!((i.text.as_str(), i.cursor), (">h ", 3));
        i.on_key(&key(KeyCode::Left));
        i.on_key(&ctrl('u'));
        assert_eq!((i.text.as_str(), i.cursor), (" ", 0));
        assert_eq!(i.on_key(&key(KeyCode::Enter)), Outcome::Submit);
        let mut p = LineInput::new("ä");
        p.paste("a\r\nb\tc\u{7}d\n");
        assert_eq!(p.text, "äa b    cd ");
        assert_eq!(p.cursor_byte(), p.text.len());
        p.cursor = 1;
        assert_eq!(p.cursor_byte(), 2);
        assert_eq!(i.on_key(&key(KeyCode::Esc)), Outcome::Cancel);
    }
}
