//! Source locations.

/// Identifies a source file inside a [`crate::SourceProvider`].
pub type FileId = u32;

/// A byte range inside one file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const fn new(file: FileId, start: u32, end: u32) -> Span {
        Span { file, start, end }
    }

    /// True when `offset` is inside the span; the end is inclusive so that a cursor placed
    /// right after an identifier still hits it.
    pub fn contains(&self, offset: u32) -> bool {
        self.start <= offset && offset <= self.end
    }

    /// The smallest span covering `self` and `other` when both are in the same file,
    /// otherwise `self`.
    pub fn cover(self, other: Span) -> Span {
        if self.file != other.file {
            return self;
        }
        Span::new(self.file, self.start.min(other.start), self.end.max(other.end))
    }

    pub fn len(&self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Converts byte offsets to (line, UTF-16 column) and back.
#[derive(Clone, Debug)]
pub struct LineIndex {
    line_starts: Vec<u32>,
    /// The lines holding a non-ASCII character, in order: only on those do columns need
    /// counting (one `é` in a comment does not make every long line slow).
    non_ascii: Vec<u32>,
}

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let mut line_starts = vec![0u32];
        let mut non_ascii = Vec::new();
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            } else if !b.is_ascii() && non_ascii.last() != Some(&(line_starts.len() as u32 - 1)) {
                non_ascii.push(line_starts.len() as u32 - 1);
            }
        }
        LineIndex { line_starts, non_ascii }
    }

    fn ascii_line(&self, line: u32) -> bool {
        self.non_ascii.binary_search(&line).is_err()
    }

    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    pub fn line_start(&self, line: u32) -> u32 {
        self.line_starts.get(line as usize).copied().unwrap_or(*self.line_starts.last().unwrap())
    }

    /// Zero-based line of `offset`.
    pub fn line_of(&self, offset: u32) -> u32 {
        match self.line_starts.binary_search(&offset) {
            Ok(l) => l as u32,
            Err(l) => l as u32 - 1,
        }
    }

    /// Zero-based line and UTF-16 column of `offset`.
    pub fn position(&self, text: &str, offset: u32) -> (u32, u32) {
        let offset = offset.min(text.len() as u32);
        let line = match self.line_starts.binary_search(&offset) {
            Ok(l) => l,
            Err(l) => l - 1,
        };
        let start = self.line_starts[line];
        let col = if self.ascii_line(line as u32) {
            offset - start
        } else {
            let mut s = start as usize;
            let e = offset as usize;
            while !text.is_char_boundary(s) && s < e {
                s += 1;
            }
            text[s..floor_char_boundary(text, e)].encode_utf16().count() as u32
        };
        (line as u32, col)
    }

    /// Byte offset of a zero-based line and UTF-16 column, clamped to the line.
    pub fn offset(&self, text: &str, line: u32, col: u32) -> u32 {
        if line as usize >= self.line_starts.len() {
            return text.len() as u32;
        }
        let start = self.line_starts[line as usize];
        let end = self
            .line_starts
            .get(line as usize + 1)
            .map(|e| e - 1)
            .unwrap_or(text.len() as u32);
        if self.ascii_line(line) {
            return start.saturating_add(col).min(end);
        }
        let mut units = 0u32;
        for (i, ch) in text[start as usize..end as usize].char_indices() {
            if units >= col {
                return start + i as u32;
            }
            units += ch.len_utf16() as u32;
        }
        end
    }
}

fn floor_char_boundary(text: &str, mut i: usize) -> usize {
    if i >= text.len() {
        return text.len();
    }
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_of_line_does_not_overflow() {
        let text = "ab\ncdef\n";
        let li = LineIndex::new(text);
        assert_eq!(li.offset(text, 1, u32::MAX), 7);
    }

    #[test]
    fn positions_round_trip() {
        let text = "ab\ncdé\nx";
        let li = LineIndex::new(text);
        assert_eq!(li.position(text, 0), (0, 0));
        assert_eq!(li.position(text, 4), (1, 1));
        assert_eq!(li.position(text, 7), (1, 3));
        assert_eq!(li.position(text, 8), (2, 0));
        assert_eq!(li.offset(text, 1, 3), 7);
        assert_eq!(li.offset(text, 2, 0), 8);
    }

    #[test]
    fn only_lines_with_non_ascii_text_count_units() {
        // 😀 is 4 bytes and 2 UTF-16 units; the lines around it are ASCII.
        let text = "// é\nmodule m; a😀b = 1;\nint x;\n";
        let li = LineIndex::new(text);
        let b = text.find('b').unwrap() as u32;
        assert_eq!(li.position(text, b), (1, 13));
        assert_eq!(li.offset(text, 1, 13), b);
        let x = text.find('x').unwrap() as u32;
        assert_eq!(li.position(text, x), (2, 4));
        assert_eq!(li.offset(text, 2, 4), x);
        assert_eq!(li.position(text, 5), (0, 4));
    }
}
