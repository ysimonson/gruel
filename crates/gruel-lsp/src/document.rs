//! Document store with incremental text sync (ADR-0091).
//!
//! Each open file is represented by a [`DocState`] holding the latest text,
//! version, and a cached [`crate::position::LineMap`]. Text sync uses
//! `TextDocumentSyncKind::INCREMENTAL` — patches are applied in place.

use std::path::PathBuf;

use lsp_types::{TextDocumentContentChangeEvent, Url};

use crate::position::{LineMap, PositionEncoding, position_to_byte};

/// State for one open or known document.
#[derive(Debug, Clone)]
pub struct DocState {
    pub uri: Url,
    pub path: PathBuf,
    pub text: String,
    pub version: i32,
    pub line_map: LineMap,
    /// True iff the editor currently has a buffer open for this file.
    pub open: bool,
}

impl DocState {
    pub fn new(uri: Url, text: String, version: i32, open: bool) -> Self {
        let path = uri.to_file_path().unwrap_or_else(|_| PathBuf::from(uri.path()));
        let line_map = LineMap::new(&text);
        Self {
            uri,
            path,
            text,
            version,
            line_map,
            open,
        }
    }

    /// Apply an LSP incremental change. Returns true on success.
    pub fn apply_change(
        &mut self,
        change: TextDocumentContentChangeEvent,
        encoding: PositionEncoding,
    ) -> bool {
        match change.range {
            Some(range) => {
                let start = position_to_byte(&self.line_map, &self.text, range.start, encoding)
                    as usize;
                let end =
                    position_to_byte(&self.line_map, &self.text, range.end, encoding) as usize;
                if start > end || end > self.text.len() {
                    return false;
                }
                self.text.replace_range(start..end, &change.text);
            }
            None => {
                // Full-document replace.
                self.text = change.text;
            }
        }
        self.line_map = LineMap::new(&self.text);
        true
    }

    pub fn set_text(&mut self, text: String, version: i32) {
        self.text = text;
        self.version = version;
        self.line_map = LineMap::new(&self.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};

    fn doc(text: &str) -> DocState {
        DocState::new(
            Url::parse("file:///tmp/test.gruel").unwrap(),
            text.to_string(),
            1,
            true,
        )
    }

    #[test]
    fn incremental_replace() {
        let mut d = doc("fn main() -> i32 { 0 }");
        let change = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position { line: 0, character: 19 },
                end: Position { line: 0, character: 20 },
            }),
            range_length: None,
            text: "42".to_string(),
        };
        assert!(d.apply_change(change, PositionEncoding::Utf8));
        assert_eq!(d.text, "fn main() -> i32 { 42 }");
    }

    #[test]
    fn incremental_insert_then_lines_recompute() {
        let mut d = doc("a\nb\nc");
        let change = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position { line: 1, character: 1 },
                end: Position { line: 1, character: 1 },
            }),
            range_length: None,
            text: "\nINSERTED".to_string(),
        };
        assert!(d.apply_change(change, PositionEncoding::Utf8));
        assert_eq!(d.text, "a\nb\nINSERTED\nc");
        // Lines should be 4 now: ["a", "b", "INSERTED", "c"]
        assert_eq!(d.line_map.line_count(), 4);
    }

    #[test]
    fn full_replace() {
        let mut d = doc("foo");
        let change = TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "totally new".to_string(),
        };
        assert!(d.apply_change(change, PositionEncoding::Utf8));
        assert_eq!(d.text, "totally new");
    }
}
