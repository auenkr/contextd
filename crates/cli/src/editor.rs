use contextd_core::config::EditorMode;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub enum Action {
    Continue,
    Save(bool),
    Cancel,
}

pub struct Editor {
    pub id: String,
    pub original: String,
    pub lines: Vec<String>,
    pub row: usize,
    pub column: usize,
    pub insert: bool,
    pub mode: EditorMode,
    pub scroll: usize,
    pub width: usize,
    preferred_column: Option<usize>,
    pub command: Option<String>,
    pending_g: bool,
}

impl Editor {
    pub fn new(id: String, text: String, mode: EditorMode) -> Self {
        Self {
            id,
            lines: text.split('\n').map(str::to_owned).collect(),
            original: text,
            row: 0,
            column: 0,
            insert: mode == EditorMode::Normal,
            mode,
            scroll: 0,
            width: 80,
            preferred_column: None,
            command: None,
            pending_g: false,
        }
    }
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
    pub fn dirty(&self) -> bool {
        self.text() != self.original
    }
    fn length(&self) -> usize {
        self.lines[self.row].graphemes(true).count()
    }
    fn byte(&self, column: usize) -> usize {
        self.lines[self.row]
            .grapheme_indices(true)
            .nth(column)
            .map(|(i, _)| i)
            .unwrap_or(self.lines[self.row].len())
    }
    fn move_row(&mut self, delta: isize) {
        let width = self.width.max(1);
        let (lines, current_row, current_column) = self.visual_lines(width);
        let desired = *self.preferred_column.get_or_insert(current_column);
        let target = current_row
            .saturating_add_signed(delta)
            .min(lines.len() - 1);
        let mut visual_row = 0;
        let mut closest = None;
        for (row, line) in self.lines.iter().enumerate() {
            let mut column = 0;
            for (index, grapheme) in line.graphemes(true).enumerate() {
                let size = grapheme.width();
                if column + size > width || column == width {
                    visual_row += 1;
                    column = 0;
                }
                if visual_row == target {
                    let distance = column.abs_diff(desired);
                    if closest.is_none_or(|(_, _, best)| distance < best) {
                        closest = Some((row, index, distance));
                    }
                }
                column += size;
            }
            if column == width {
                visual_row += 1;
                column = 0;
            }
            if visual_row == target {
                let distance = column.abs_diff(desired);
                if closest.is_none_or(|(_, _, best)| distance < best) {
                    closest = Some((row, line.graphemes(true).count(), distance));
                }
            }
            visual_row += 1;
        }
        if let Some((row, column, _)) = closest {
            self.row = row;
            self.column = column;
        }
    }
    fn move_word(&mut self, forward: bool) {
        let text = &self.lines[self.row];
        let starts: Vec<_> = text
            .split_word_bound_indices()
            .filter(|(_, word)| word.chars().any(char::is_alphanumeric))
            .map(|(byte, _)| text[..byte].graphemes(true).count())
            .collect();
        if forward {
            if let Some(column) = starts.into_iter().find(|column| *column > self.column) {
                self.column = column;
            } else if self.row + 1 < self.lines.len() {
                self.row += 1;
                self.column = 0;
            } else {
                self.column = self.length();
            }
        } else if let Some(column) = starts
            .into_iter()
            .rev()
            .find(|column| *column < self.column)
        {
            self.column = column;
        } else if self.row > 0 {
            self.row -= 1;
            self.column = self.length();
        }
    }
    fn delete(&mut self) {
        if self.column < self.length() {
            let range = self.byte(self.column)..self.byte(self.column + 1);
            self.lines[self.row].replace_range(range, "");
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }
    fn backspace(&mut self) {
        if self.column > 0 {
            self.column -= 1;
            self.delete();
        } else if self.row > 0 {
            let current = self.lines.remove(self.row);
            self.row -= 1;
            self.column = self.length();
            self.lines[self.row].push_str(&current);
        }
    }
    fn newline(&mut self) {
        let byte = self.byte(self.column);
        let next = self.lines[self.row].split_off(byte);
        self.row += 1;
        self.column = 0;
        self.lines.insert(self.row, next);
    }
    fn character(&mut self, c: char) {
        let byte = self.byte(self.column);
        self.lines[self.row].insert(byte, c);
        self.column = self.lines[self.row][..byte + c.len_utf8()]
            .graphemes(true)
            .count();
    }
    pub fn key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('s') => Action::Save(true),
                KeyCode::Char('c') => Action::Cancel,
                _ => Action::Continue,
            };
        }
        if let Some(command) = &mut self.command {
            match key.code {
                KeyCode::Esc => self.command = None,
                KeyCode::Backspace => {
                    command.pop();
                }
                KeyCode::Char(c) => command.push(c),
                KeyCode::Enter => {
                    let command = self.command.take().unwrap();
                    return match command.as_str() {
                        "w" => Action::Save(false),
                        "wq" => Action::Save(true),
                        "q" | "q!" => Action::Cancel,
                        _ => Action::Continue,
                    };
                }
                _ => {}
            }
            return Action::Continue;
        }
        if !matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        ) && !(self.mode == EditorMode::Vim
            && !self.insert
            && matches!(key.code, KeyCode::Char('j' | 'k')))
        {
            self.preferred_column = None;
        }
        match key.code {
            KeyCode::Left => self.column = self.column.saturating_sub(1),
            KeyCode::Right => self.column = (self.column + 1).min(self.length()),
            KeyCode::Up => self.move_row(-1),
            KeyCode::Down => self.move_row(1),
            KeyCode::PageUp => self.move_row(-10),
            KeyCode::PageDown => self.move_row(10),
            KeyCode::Home => self.column = 0,
            KeyCode::End => self.column = self.length(),
            KeyCode::Esc if self.mode == EditorMode::Vim && self.insert => self.insert = false,
            KeyCode::Esc => return Action::Cancel,
            KeyCode::Backspace if self.insert => self.backspace(),
            KeyCode::Delete if self.insert => self.delete(),
            KeyCode::Enter if self.insert => self.newline(),
            KeyCode::Char(c) if self.insert => self.character(c),
            KeyCode::Char(c) => {
                match c {
                    'h' => self.column = self.column.saturating_sub(1),
                    'l' => self.column = (self.column + 1).min(self.length()),
                    'j' => self.move_row(1),
                    'k' => self.move_row(-1),
                    'w' => self.move_word(true),
                    'b' => self.move_word(false),
                    '0' => self.column = 0,
                    '$' => self.column = self.length(),
                    'g' if self.pending_g => {
                        self.row = 0;
                        self.column = 0;
                    }
                    'g' => {
                        self.pending_g = true;
                        return Action::Continue;
                    }
                    'G' => {
                        self.row = self.lines.len() - 1;
                        self.column = self.length();
                    }
                    'i' => self.insert = true,
                    'a' => {
                        self.column = (self.column + 1).min(self.length());
                        self.insert = true;
                    }
                    'A' => {
                        self.column = self.length();
                        self.insert = true;
                    }
                    'o' => {
                        self.column = self.length();
                        self.newline();
                        self.insert = true;
                    }
                    'x' => self.delete(),
                    ':' => self.command = Some(String::new()),
                    'q' => return Action::Cancel,
                    _ => {}
                }
                self.pending_g = false;
            }
            _ => {}
        }
        Action::Continue
    }
    /// Hard wrap for editing so the terminal caret matches the stored text,
    /// including Hindi grapheme clusters. Reading uses ordinary word wrapping.
    pub fn visual_lines(&self, width: usize) -> (Vec<String>, usize, usize) {
        let width = width.max(1);
        let mut output = Vec::new();
        let mut caret = (0, 0);
        for (row, line) in self.lines.iter().enumerate() {
            let mut visual = String::new();
            let mut column = 0;
            for (index, grapheme) in line.graphemes(true).enumerate() {
                let size = grapheme.width();
                if column + size > width || column == width {
                    output.push(std::mem::take(&mut visual));
                    column = 0;
                }
                if row == self.row && index == self.column {
                    caret = (output.len(), column);
                }
                visual.push_str(grapheme);
                column += size;
            }
            if column == width {
                output.push(std::mem::take(&mut visual));
                column = 0;
            }
            if row == self.row && self.column == line.graphemes(true).count() {
                caret = (output.len(), column);
            }
            output.push(visual);
        }
        (output, caret.0, caret.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn arrow_movement_reaches_wrapped_lines_without_inserting_newlines() {
        let mut editor = Editor::new("note".into(), "abcdefghijkl".into(), EditorMode::Normal);
        editor.width = 5;
        editor.column = 2;
        editor.key(key(KeyCode::Down));
        assert_eq!((editor.row, editor.column), (0, 7));
        editor.key(key(KeyCode::Down));
        assert_eq!(editor.column, 12);
        editor.key(key(KeyCode::Up));
        assert_eq!(editor.column, 7);
        editor.key(key(KeyCode::Up));
        assert_eq!(editor.column, 2);
        assert_eq!(editor.text(), "abcdefghijkl");
    }

    #[test]
    fn normal_editing_handles_hindi_graphemes_and_line_boundaries() {
        let mut editor = Editor::new("note".into(), "हिंदी\nEnglish".into(), EditorMode::Normal);
        editor.key(key(KeyCode::End));
        editor.key(key(KeyCode::Backspace));
        assert_eq!(editor.lines[0], "हिं");
        editor.key(key(KeyCode::Enter));
        editor.key(key(KeyCode::Char('X')));
        assert_eq!(editor.text(), "हिं\nX\nEnglish");
        editor.key(key(KeyCode::Home));
        editor.key(key(KeyCode::Backspace));
        assert_eq!(editor.text(), "हिंX\nEnglish");
        assert!(editor.dirty());
    }
    #[test]
    fn vim_modes_and_wrapped_caret_preserve_text() {
        let mut editor = Editor::new("note".into(), "abcdef".into(), EditorMode::Vim);
        editor.key(key(KeyCode::Char('j')));
        assert_eq!(editor.text(), "abcdef");
        editor.key(key(KeyCode::Char('A')));
        editor.key(key(KeyCode::Char('!')));
        editor.key(key(KeyCode::Esc));
        assert!(!editor.insert);
        let (lines, row, col) = editor.visual_lines(3);
        assert_eq!(lines, vec!["abc", "def", "!"]);
        assert_eq!((row, col), (2, 1));
        editor.key(key(KeyCode::Char(':')));
        editor.key(key(KeyCode::Char('w')));
        assert!(matches!(
            editor.key(key(KeyCode::Enter)),
            Action::Save(false)
        ));
    }
}
