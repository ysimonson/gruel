//! Position conversion between LSP Position (UTF-16 code units) and Gruel
//! byte offsets (ADR-0091).
//!
//! LSP defaults to UTF-16; we also support UTF-8 (LSP 3.17 `positionEncoding`)
//! to skip the conversion entirely for capable clients.

use gruel_util::span::Span;
use lsp_types::{Position, Range};

/// Position encoding negotiated with the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

impl Default for PositionEncoding {
    fn default() -> Self {
        PositionEncoding::Utf16
    }
}

/// Cached line start byte offsets for a source string.
///
/// `line_starts[i]` is the byte offset of the start of line `i` (0-indexed
/// for LSP). Always begins with `0`.
#[derive(Debug, Clone)]
pub struct LineMap {
    line_starts: Vec<u32>,
    source_len: u32,
}

impl LineMap {
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0u32];
        for (i, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        Self {
            line_starts,
            source_len: source.len() as u32,
        }
    }

    /// Number of lines (always at least 1).
    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// Get the 0-based line index containing `byte`.
    pub fn line_for_byte(&self, byte: u32) -> u32 {
        let byte = byte.min(self.source_len);
        // Largest index where line_starts[i] <= byte.
        let pp = self.line_starts.partition_point(|&s| s <= byte);
        pp.saturating_sub(1) as u32
    }

    /// Byte offset where the given 0-based line starts. Returns
    /// `source_len` if `line` is past the end.
    pub fn line_start(&self, line: u32) -> u32 {
        let idx = line as usize;
        if idx >= self.line_starts.len() {
            self.source_len
        } else {
            self.line_starts[idx]
        }
    }

    /// Byte offset just after the last character of the given 0-based line,
    /// excluding the trailing newline (if any).
    pub fn line_end(&self, source: &str, line: u32) -> u32 {
        let next = line.saturating_add(1) as usize;
        let bytes = source.as_bytes();
        if next >= self.line_starts.len() {
            return self.source_len;
        }
        let next_start = self.line_starts[next];
        if next_start > 0 && bytes.get((next_start - 1) as usize) == Some(&b'\n') {
            next_start - 1
        } else {
            next_start
        }
    }
}

/// Convert a byte offset within `source` to an LSP `Position`.
pub fn byte_to_position(
    line_map: &LineMap,
    source: &str,
    byte: u32,
    encoding: PositionEncoding,
) -> Position {
    let byte = byte.min(source.len() as u32);
    let line = line_map.line_for_byte(byte);
    let line_start = line_map.line_start(line) as usize;
    let prefix = &source[line_start..byte as usize];
    let character = match encoding {
        PositionEncoding::Utf8 => prefix.len() as u32,
        PositionEncoding::Utf16 => prefix.encode_utf16().count() as u32,
    };
    Position { line, character }
}

/// Convert an LSP `Position` to a byte offset within `source`.
pub fn position_to_byte(
    line_map: &LineMap,
    source: &str,
    pos: Position,
    encoding: PositionEncoding,
) -> u32 {
    let line_start = line_map.line_start(pos.line) as usize;
    let line_end = line_map.line_end(source, pos.line) as usize;
    let line_text = &source[line_start..line_end];
    let column_bytes = match encoding {
        PositionEncoding::Utf8 => (pos.character as usize).min(line_text.len()),
        PositionEncoding::Utf16 => {
            let mut utf16_count = 0u32;
            let mut byte_off = 0usize;
            for c in line_text.chars() {
                if utf16_count >= pos.character {
                    break;
                }
                let unit_len = c.len_utf16() as u32;
                utf16_count += unit_len;
                byte_off += c.len_utf8();
            }
            byte_off.min(line_text.len())
        }
    };
    (line_start + column_bytes) as u32
}

/// Convert a Gruel `Span` (within `source`) to an LSP `Range`.
pub fn span_to_range(
    line_map: &LineMap,
    source: &str,
    span: Span,
    encoding: PositionEncoding,
) -> Range {
    Range {
        start: byte_to_position(line_map, source, span.start, encoding),
        end: byte_to_position(line_map, source, span.end, encoding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_to_position_utf8() {
        let s = "hello\nworld";
        let li = LineMap::new(s);
        assert_eq!(
            byte_to_position(&li, s, 0, PositionEncoding::Utf8),
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            byte_to_position(&li, s, 5, PositionEncoding::Utf8),
            Position {
                line: 0,
                character: 5
            }
        );
        assert_eq!(
            byte_to_position(&li, s, 6, PositionEncoding::Utf8),
            Position {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            byte_to_position(&li, s, 11, PositionEncoding::Utf8),
            Position {
                line: 1,
                character: 5
            }
        );
    }

    #[test]
    fn position_to_byte_utf8_roundtrip() {
        let s = "foo\nbar\nbaz";
        let li = LineMap::new(s);
        for (line, ch, expected) in [
            (0u32, 0u32, 0u32),
            (0, 3, 3),
            (1, 0, 4),
            (1, 2, 6),
            (2, 3, 11),
        ] {
            let pos = Position {
                line,
                character: ch,
            };
            assert_eq!(
                position_to_byte(&li, s, pos, PositionEncoding::Utf8),
                expected
            );
        }
    }

    #[test]
    fn utf16_handles_surrogate_pairs() {
        // 🦀 is one Unicode scalar (4 UTF-8 bytes, 2 UTF-16 code units).
        let s = "ab🦀c";
        let li = LineMap::new(s);
        let pos_a = byte_to_position(&li, s, 0, PositionEncoding::Utf16);
        let pos_b = byte_to_position(&li, s, 1, PositionEncoding::Utf16);
        let pos_crab = byte_to_position(&li, s, 2, PositionEncoding::Utf16);
        let pos_after_crab = byte_to_position(&li, s, 6, PositionEncoding::Utf16);
        assert_eq!(pos_a.character, 0);
        assert_eq!(pos_b.character, 1);
        assert_eq!(pos_crab.character, 2);
        assert_eq!(pos_after_crab.character, 4);

        // Round-trip
        assert_eq!(position_to_byte(&li, s, pos_a, PositionEncoding::Utf16), 0);
        assert_eq!(
            position_to_byte(&li, s, pos_after_crab, PositionEncoding::Utf16),
            6
        );
    }

    #[test]
    fn span_to_range_basic() {
        let s = "let x = 42;";
        let li = LineMap::new(s);
        let span = Span::with_file(gruel_util::span::FileId::DEFAULT, 4, 5);
        let range = span_to_range(&li, s, span, PositionEncoding::Utf8);
        assert_eq!(
            range.start,
            Position {
                line: 0,
                character: 4
            }
        );
        assert_eq!(
            range.end,
            Position {
                line: 0,
                character: 5
            }
        );
    }

    #[test]
    fn line_map_empty_source() {
        let li = LineMap::new("");
        assert_eq!(li.line_count(), 1);
        assert_eq!(li.line_for_byte(0), 0);
        assert_eq!(li.line_start(0), 0);
    }
}
