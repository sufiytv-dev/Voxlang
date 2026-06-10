// span.rs – Source code span tracking
//
// Represents a contiguous byte range in a source file with line/column info.

/// A contiguous region in a source file.
/// `start` and `end` are byte offsets (0‑based). `line` and `col` are 0‑based.
/// Invariant: `line`/`col` correspond to the `start` offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub col: usize,
}

impl Span {
    /// Creates a new span.
    pub fn new(start: usize, end: usize, line: usize, col: usize) -> Self {
        Self {
            start,
            end,
            line,
            col,
        }
    }

    /// Returns a dummy span (0,0) for tokens without location info.
    pub fn dummy() -> Self {
        Self::new(0, 0, 0, 0)
    }

    /// Checks if the span is empty (start == end).
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Validates internal invariants (for debugging). Called by debug builds when span is created.
    pub fn assert_valid(&self) {
        debug_assert!(self.start <= self.end, "Span start > end");
        // Additional checks could be added here (e.g., line/col consistency).
    }
}
