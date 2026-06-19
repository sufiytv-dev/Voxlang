// src/shell/editor.rs – Notepad component (lines, cursor, file I/O)

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Simple text editor – stores lines, cursor position, and current file path.
pub struct Editor {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    scroll_top: usize,
    file_path: Option<PathBuf>,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            scroll_top: 0,
            file_path: None,
        }
    }

    pub fn load_file(&mut self, path: &Path) -> io::Result<()> {
        let content = fs::read_to_string(path)?;
        self.lines = content.lines().map(|l| l.to_string()).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.scroll_top = 0;
        self.file_path = Some(path.to_path_buf());
        Ok(())
    }

    pub fn save(&self) -> io::Result<()> {
        if let Some(path) = &self.file_path {
            let content = self.lines.join("\n");
            fs::write(path, content)?;
        }
        Ok(())
    }

    pub fn insert_char(&mut self, ch: char) {
        let line = &mut self.lines[self.cursor_row];
        line.insert(self.cursor_col, ch);
        self.cursor_col += 1;
    }

    pub fn delete_char(&mut self) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        if col < self.lines[row].len() {
            self.lines[row].remove(col);
        } else if col == self.lines[row].len() && row + 1 < self.lines.len() {
            let next_line = self.lines.remove(row + 1);
            self.lines[row].push_str(&next_line);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            line.remove(self.cursor_col - 1);
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            let prev_len = self.lines[self.cursor_row - 1].len();
            let line = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = prev_len;
            self.lines[self.cursor_row].push_str(&line);
        }
    }

    pub fn newline(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let right = line.split_off(self.cursor_col);
        self.lines.insert(self.cursor_row + 1, right);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
    }

    pub fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col < line_len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    pub fn home(&mut self) {
        self.cursor_col = 0;
    }

    pub fn end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].len();
    }

    /// Returns the current line (as a string slice) – used for command parsing.
    pub fn current_line(&self) -> &str {
        &self.lines[self.cursor_row]
    }

    /// Clears the current line and resets cursor column to 0.
    pub fn clear_line(&mut self) {
        self.lines[self.cursor_row].clear();
        self.cursor_col = 0;
    }

    /// Returns a reference to the current file path, if any.
    pub fn file_path(&self) -> Option<&PathBuf> {
        self.file_path.as_ref()
    }

    /// Returns a mutable reference to the lines – used only for LSP notifications.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Render the editor pane into an ANSI‑formatted string.
    /// `height` = number of rows to draw, `width` = number of columns.
    pub fn render(&self, height: usize, width: usize) -> String {
        let mut out = String::new();
        let line_start = self.scroll_top;
        let line_end = (self.scroll_top + height).min(self.lines.len());

        for i in line_start..line_end {
            let line = &self.lines[i];
            let display = if line.len() > width {
                &line[..width]
            } else {
                line.as_str()
            };
            out.push_str(&format!("{}\r\n", display));
        }
        // Fill remaining lines with `~` (empty lines indicator)
        for _ in line_end..self.scroll_top + height {
            out.push_str("~\r\n");
        }
        // Move cursor to its screen position (row/col 1‑based)
        let screen_row = self.cursor_row - self.scroll_top;
        out.push_str(&format!("\x1b[{};{}H", screen_row + 1, self.cursor_col + 1));
        out
    }
}
