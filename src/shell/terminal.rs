// src/shell/terminal.rs – Terminal output buffer (ring buffer)
//
// Uses a fixed‑capacity ring buffer to keep the most recent lines.
// The `take_all()` method provides a lock‑free‑ready way to steal
// all pending lines in logical order, resetting the buffer to empty.

/// Bounded ring buffer for terminal output.
/// Prevents memory leaks and maintains a fixed capacity.
#[derive(Clone)]
pub struct TerminalBuffer {
    lines: Vec<String>,
    cap: usize,
    head: usize,
}

impl TerminalBuffer {
    /// Default capacity (2000 lines).
    pub const DEFAULT_CAP: usize = 2000;

    /// Creates a new empty terminal buffer with the default capacity.
    pub fn new() -> Self {
        Self {
            lines: Vec::with_capacity(Self::DEFAULT_CAP),
            cap: Self::DEFAULT_CAP,
            head: 0,
        }
    }

    /// Creates a new terminal buffer with a custom capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            lines: Vec::with_capacity(capacity),
            cap: capacity,
            head: 0,
        }
    }

    /// Appends a line to the buffer. If the buffer is full, the oldest line is overwritten.
    pub fn push(&mut self, line: String) {
        if self.lines.len() < self.cap {
            self.lines.push(line);
        } else {
            self.lines[self.head] = line;
            self.head = (self.head + 1) % self.cap;
        }
    }

    /// Clears all lines from the buffer.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.head = 0;
    }

    /// Steals all pending lines in logical order, leaving the buffer empty.
    /// This is an O(n) operation, but the lock is held only for the time
    /// it takes to collect and clone the lines. After this call, the buffer
    /// is empty and can be reused.
    ///
    /// The returned `Vec<String>` contains lines from oldest to newest.
    pub fn take_all(&mut self) -> Vec<String> {
        let len = self.lines.len();
        if len == 0 {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(len);
        if len == self.cap {
            // Full ring: logical order starts at `head`
            for i in 0..self.cap {
                let idx = (self.head + i) % self.cap;
                result.push(self.lines[idx].clone());
            }
        } else {
            // Not full: logical order is 0..len
            for i in 0..len {
                result.push(self.lines[i].clone());
            }
        }

        // Reset buffer to empty
        self.lines.clear();
        self.head = 0;

        result
    }

    /// Returns the current number of lines in the buffer.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Returns `true` if the buffer contains no lines.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Returns a reference to the line at the given logical index, if it exists.
    ///
    /// The buffer maintains logical order even when the ring wraps.
    /// For a full buffer, index 0 corresponds to the oldest line (at `head`),
    /// and indices increase in the order they were added.
    pub fn get(&self, index: usize) -> Option<&str> {
        if index >= self.lines.len() {
            return None;
        }
        let vec_index = if self.lines.len() == self.cap {
            // When full, the logical order starts at `head`
            (self.head + index) % self.cap
        } else {
            // When not full, logical order is the same as vector order
            index
        };
        Some(&self.lines[vec_index])
    }

    /// Renders the last `height` lines, truncating each line to `width` characters.
    /// Returns a string with `\r\n` line endings, suitable for direct printing.
    pub fn render(&self, height: usize, width: usize) -> String {
        let mut out = String::new();
        let total = self.lines.len();
        let start = if total > height { total - height } else { 0 };
        for i in start..total {
            if let Some(line) = self.get(i) {
                let display = if line.len() > width {
                    &line[..width]
                } else {
                    line
                };
                out.push_str(&format!("{}\r\n", display));
            }
        }
        // Fill remaining lines with empty strings (clear to end of pane)
        for _ in total..start + height {
            out.push_str("\r\n");
        }
        out
    }
}

impl Default for TerminalBuffer {
    fn default() -> Self {
        Self::new()
    }
}
