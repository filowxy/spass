//! Tracks source locations for tokens and AST nodes.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: String,
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub col: u32,
}

impl Span {
    pub fn new(file: impl Into<String>, start: usize, end: usize, line: u32, col: u32) -> Self {
        Self { file: file.into(), start, end, line, col }
    }

    /// Joins two spans (e.g. for binary expressions: result covers operands).
    pub fn join(self, other: Span) -> Span {
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            line: self.line.min(other.line),
            col: if self.line <= other.line { self.col } else { other.col },
        }
    }

    pub fn dummy() -> Self {
        Span::new("<dummy>", 0, 0, 0, 0)
    }
}

impl Default for Span {
    fn default() -> Self {
        Self::dummy()
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}
