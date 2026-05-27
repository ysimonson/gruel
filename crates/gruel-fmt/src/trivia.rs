//! Trivia scanner (ADR-0093 Phase 4).
//!
//! Walks the raw source once and records `//` line comments and blank-line
//! runs as a sorted vector of [`TriviaEntry`] values. The emitter consults
//! this table to weave trivia back into the canonical output at the right
//! position.
//!
//! `///` doc comments are *not* trivia — they are parsed onto AST nodes and
//! emitted directly. `////+` runs are also ignored (lexer-skipped).

/// One scanned trivia run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriviaEntry {
    pub kind: TriviaKind,
    /// Inclusive start byte of the trivia in source.
    pub start: u32,
    /// Exclusive end byte of the trivia in source.
    pub end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriviaKind {
    /// `// ...` line comment (excludes `///` doc comments and `////+` runs).
    Comment,
    /// Run of one or more blank lines, collapsed to one in output.
    Blank,
}

/// Sorted-by-`start` trivia entries for `src`.
#[derive(Debug, Clone)]
pub struct TriviaTable {
    pub entries: Vec<TriviaEntry>,
}

impl TriviaTable {
    /// Walk `src` once and collect trivia entries.
    pub fn scan(src: &str) -> Self {
        let bytes = src.as_bytes();
        let mut entries = Vec::new();
        let mut i = 0;
        // Count consecutive `\n` bytes; 2+ in a row means at least one blank line.
        let mut blank_run_start: Option<u32> = None;
        let mut newlines_in_run: u32 = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                // Distinguish `//`, `///` (doc), `////+` (skipped) by counting slashes.
                let mut slash_count = 0;
                let mut j = i;
                while j < bytes.len() && bytes[j] == b'/' {
                    slash_count += 1;
                    j += 1;
                }
                // ADR-0089: `///` is a doc comment (already on AST); `////+` is
                // lexer-skipped and not authored as trivia worth preserving.
                let is_plain_comment = slash_count == 2;
                // Walk to end of line.
                let start = i as u32;
                while j < bytes.len() && bytes[j] != b'\n' {
                    j += 1;
                }
                let end = j as u32;
                if is_plain_comment {
                    Self::flush_blank(&mut entries, blank_run_start, newlines_in_run);
                    blank_run_start = None;
                    newlines_in_run = 0;
                    entries.push(TriviaEntry {
                        kind: TriviaKind::Comment,
                        start,
                        end,
                    });
                }
                i = j;
                continue;
            }
            if b == b'\n' {
                if newlines_in_run == 0 {
                    blank_run_start = Some(i as u32);
                }
                newlines_in_run += 1;
                i += 1;
                continue;
            }
            // Whitespace (other than newlines) doesn't break a newline run.
            if b == b' ' || b == b'\t' || b == b'\r' {
                i += 1;
                continue;
            }
            // Any other byte starts non-trivia content — flush any pending
            // blank-line run.
            Self::flush_blank(&mut entries, blank_run_start, newlines_in_run);
            blank_run_start = None;
            newlines_in_run = 0;
            // Skip a string literal so its embedded `//` doesn't trip the scanner.
            if b == b'"' {
                i = skip_string(bytes, i);
                continue;
            }
            // Skip a char literal so embedded `'/'`, `'\''` don't confuse us.
            if b == b'\'' {
                i = skip_char(bytes, i);
                continue;
            }
            i += 1;
        }
        // EOF flush.
        Self::flush_blank(&mut entries, blank_run_start, newlines_in_run);
        TriviaTable { entries }
    }

    fn flush_blank(entries: &mut Vec<TriviaEntry>, start: Option<u32>, newlines: u32) {
        // 2+ consecutive newlines == at least one blank line.
        if newlines >= 2
            && let Some(s) = start
        {
            entries.push(TriviaEntry {
                kind: TriviaKind::Blank,
                start: s,
                end: s + newlines,
            });
        }
    }
}

fn skip_string(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return i + 1,
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'\n' => return i, // unterminated — let lexer error
            _ => i += 1,
        }
    }
    i
}

fn skip_char(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => return i + 1,
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'\n' => return i,
            _ => i += 1,
        }
    }
    i
}

/// Map byte offset → 0-based line number. Built once per source.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// `line_starts[i]` is the byte offset of the first character on line `i`.
    line_starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(src: &str) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        Self { line_starts }
    }

    /// 0-based line number containing `byte`. Saturates at the last line.
    pub fn line_of(&self, byte: u32) -> u32 {
        match self.line_starts.binary_search(&byte) {
            Ok(idx) => idx as u32,
            Err(idx) => idx.saturating_sub(1) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_no_comments() {
        let t = TriviaTable::scan("fn main() -> i32 { 0 }");
        assert!(t.entries.is_empty());
    }

    #[test]
    fn scan_single_line_comment() {
        let t = TriviaTable::scan("// hello\nfn main() -> i32 { 0 }");
        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].kind, TriviaKind::Comment);
        assert_eq!(t.entries[0].start, 0);
        assert_eq!(t.entries[0].end, 8); // up to but not including \n
    }

    #[test]
    fn scan_doc_comment_is_not_trivia() {
        let t = TriviaTable::scan("/// doc\nfn main() -> i32 { 0 }");
        assert!(t.entries.is_empty());
    }

    #[test]
    fn scan_quadruple_slash_is_not_trivia() {
        let t = TriviaTable::scan("//// skipped\nfn main() -> i32 { 0 }");
        assert!(t.entries.is_empty());
    }

    #[test]
    fn scan_blank_line() {
        let t = TriviaTable::scan("fn a() -> i32 { 0 }\n\nfn b() -> i32 { 0 }");
        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].kind, TriviaKind::Blank);
    }

    #[test]
    fn scan_comment_in_string_literal_ignored() {
        let t = TriviaTable::scan(r#"fn main() -> i32 { let s = "// not a comment"; 0 }"#);
        assert!(t.entries.is_empty());
    }

    #[test]
    fn line_of() {
        let li = LineIndex::new("abc\ndef\nghi");
        assert_eq!(li.line_of(0), 0);
        assert_eq!(li.line_of(3), 0);
        assert_eq!(li.line_of(4), 1);
        assert_eq!(li.line_of(8), 2);
    }
}
