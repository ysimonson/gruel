//! Output buffer + trivia weaving for the formatter (ADR-0093).
//!
//! Owns the indent state, resolves interned identifiers, and (Phase 4) weaves
//! `//` line comments and blank lines back into the output by consulting a
//! [`TriviaTable`] built over the raw source.

use lasso::{Spur, ThreadedRodeo};

use crate::trivia::{LineIndex, TriviaKind, TriviaTable};

/// Width of one indent level, in spaces.
pub const INDENT_WIDTH: usize = 4;

/// Output buffer driven by the emit functions in [`crate::emit`].
pub struct Printer<'a> {
    out: String,
    interner: &'a ThreadedRodeo,
    indent_level: usize,
    /// True iff the next `write_str` call should be preceded by indent
    /// whitespace (i.e. the cursor is at column 0 of a new line).
    pending_indent: bool,

    // Trivia weaving (Phase 4).
    src: &'a str,
    trivia: TriviaTable,
    /// Index into `trivia.entries`: the next entry not yet emitted.
    trivia_cursor: usize,
    line_index: LineIndex,
    /// Source byte offset of the end of the last emitted span, or 0 if
    /// nothing has been emitted yet. Used to detect trailing same-line
    /// comments.
    last_emitted_end: u32,
}

impl<'a> Printer<'a> {
    pub fn new(interner: &'a ThreadedRodeo, src: &'a str) -> Self {
        Self {
            out: String::new(),
            interner,
            indent_level: 0,
            pending_indent: false,
            src,
            trivia: TriviaTable::scan(src),
            trivia_cursor: 0,
            line_index: LineIndex::new(src),
            last_emitted_end: 0,
        }
    }

    /// Write `s` at the current cursor; emits pending indent first.
    pub fn write_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.flush_indent();
        self.out.push_str(s);
    }

    fn flush_indent(&mut self) {
        if self.pending_indent {
            for _ in 0..(self.indent_level * INDENT_WIDTH) {
                self.out.push(' ');
            }
            self.pending_indent = false;
        }
    }

    /// Resolve and emit an interned identifier.
    pub fn write_ident(&mut self, spur: Spur) {
        let s = self.interner.resolve(&spur);
        self.write_str(s);
    }

    /// Resolve an interned string without emitting it; callers re-escape
    /// before writing (e.g. string literals).
    pub fn resolve(&self, spur: Spur) -> &str {
        self.interner.resolve(&spur)
    }

    /// Begin a new line. Subsequent writes will be preceded by indent
    /// whitespace.
    pub fn newline(&mut self) {
        self.out.push('\n');
        self.pending_indent = true;
    }

    /// Emit a blank line between sibling items. Idempotent — repeated calls
    /// without intervening writes produce at most one blank line, matching the
    /// "at most one consecutive blank line" rule. Also no-ops when the cursor
    /// is directly inside a freshly-opened brace (style rule: "No blank line
    /// at the start of a block").
    pub fn blank_line(&mut self) {
        if self.out.is_empty() || self.out.ends_with("\n\n") || self.out.ends_with("{\n") {
            self.pending_indent = true;
            return;
        }
        if !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.out.push('\n');
        self.pending_indent = true;
    }

    pub fn indent(&mut self) {
        self.indent_level += 1;
    }

    pub fn dedent(&mut self) {
        debug_assert!(self.indent_level > 0, "dedent below zero");
        self.indent_level -= 1;
    }

    /// Drain every trivia entry whose `start` is strictly before `offset` —
    /// these are the comments and blank-line runs that fall *before* the
    /// next AST node.
    pub fn drain_trivia_before(&mut self, offset: u32) {
        while self.trivia_cursor < self.trivia.entries.len() {
            let entry = self.trivia.entries[self.trivia_cursor];
            if entry.start >= offset {
                break;
            }
            self.trivia_cursor += 1;
            self.emit_trivia(entry);
        }
    }

    /// Drain every remaining trivia entry — used once at end-of-AST to flush
    /// any trailing comments past the last item.
    pub fn drain_trivia_remaining(&mut self) {
        while self.trivia_cursor < self.trivia.entries.len() {
            let entry = self.trivia.entries[self.trivia_cursor];
            self.trivia_cursor += 1;
            self.emit_trivia(entry);
        }
    }

    fn emit_trivia(&mut self, entry: crate::trivia::TriviaEntry) {
        match entry.kind {
            TriviaKind::Blank => self.blank_line(),
            TriviaKind::Comment => {
                let text = &self.src[entry.start as usize..entry.end as usize];
                self.write_str(text);
                self.newline();
            }
        }
    }

    /// Drain `// comment` trivia that begins on the same source line as the
    /// most recently emitted node. The comment is appended inline with two
    /// leading spaces; the caller emits the terminating newline.
    ///
    /// Call this immediately *after* `mark_emitted_end` and *before* the
    /// `newline()` that ends the line — that ordering keeps the comment glued
    /// to the statement/item it follows in source.
    pub fn drain_trailing_comment_on_line(&mut self) {
        if self.last_emitted_end == 0 {
            return;
        }
        let prev_line = self
            .line_index
            .line_of(self.last_emitted_end.saturating_sub(1));
        while self.trivia_cursor < self.trivia.entries.len() {
            let entry = self.trivia.entries[self.trivia_cursor];
            if entry.kind != TriviaKind::Comment {
                break;
            }
            let line = self.line_index.line_of(entry.start);
            if line != prev_line {
                break;
            }
            self.trivia_cursor += 1;
            let text = &self.src[entry.start as usize..entry.end as usize];
            // Two spaces, then the comment. Don't call `write_str` because
            // pending_indent is irrelevant here — we're chaining onto the
            // already-emitted line.
            self.out.push_str("  ");
            self.out.push_str(text);
        }
    }

    /// Record that the AST node ending at `byte` has just been emitted. Used
    /// to decide whether a following comment is a trailing same-line one.
    pub fn mark_emitted_end(&mut self, byte: u32) {
        self.last_emitted_end = byte;
    }

    /// Consume the printer and return the formatted source. Guarantees exactly
    /// one trailing newline at EOF.
    pub fn finish(mut self) -> String {
        while self.out.ends_with("\n\n") {
            self.out.pop();
        }
        if !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.out
    }
}
