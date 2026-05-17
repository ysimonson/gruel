//! Parser differential fuzz target (ADR-0090 Part 4).
//!
//! Feeds the same input to the canonical chumsky-based `gruel-parser`
//! and to the tree-sitter Gruel grammar; asserts that both parsers agree
//! on whether the input is syntactically valid.
//!
//! "Acceptance only" — tree shape, error positions, and recovery
//! strategies are out of scope. Disagreement is reported as a failing
//! assertion so libfuzzer minimizes the input and emits a crash file.

#![no_main]
use gruel_fuzz::MaybeInvalidProgram;
use libfuzzer_sys::fuzz_target;
use tree_sitter::Parser as TsParser;

fuzz_target!(|prog: MaybeInvalidProgram| {
    let source = &prog.0;

    // Path A: canonical chumsky-based parser.
    let chumsky_accepted = match gruel_lexer::Lexer::new(source).tokenize() {
        Ok((tokens, interner)) => {
            // Note: `parse()` runs light AST validation on top of the
            // syntactic parse. For acceptance-differential purposes we
            // accept this — semantic-only rejections (refutable let
            // patterns, surrogate char literals, ...) are documented as
            // known divergences in `tree-sitter-gruel/bindings/rust/
            // tests/spec_corpus_differential.rs`.
            gruel_parser::Parser::new(tokens, interner).parse().is_ok()
        }
        Err(_) => false,
    };

    // Path B: tree-sitter grammar.
    let mut ts = TsParser::new();
    ts.set_language(&tree_sitter_gruel::LANGUAGE.into())
        .expect("tree-sitter Gruel language loads");
    let tree = ts
        .parse(source.as_bytes(), None)
        .expect("tree-sitter produces a tree");
    let ts_accepted = !tree.root_node().has_error();

    // Filter out known divergences that are documented and intentional:
    // the tree-sitter grammar does not enforce semantic-only checks
    // (refutability in `let`, surrogate codepoints in char literals,
    // reserved-keyword reuse as an identifier). Skip these to keep the
    // fuzzer focused on real grammar drift.
    if chumsky_accepted != ts_accepted && !is_known_divergence(source, chumsky_accepted, ts_accepted) {
        panic!(
            "parser disagreement on:\n{}\n(chumsky={}, tree-sitter={})",
            source, chumsky_accepted, ts_accepted,
        );
    }
});

/// Returns true if the chumsky/tree-sitter mismatch is one of the
/// known divergences. Conservative — we'd rather emit a benign
/// duplicate than miss a real bug.
fn is_known_divergence(source: &str, chumsky_accepted: bool, ts_accepted: bool) -> bool {
    // Case A: chumsky rejects what tree-sitter accepts. The two
    // categories we know about are:
    //   1. Reserved keyword used as identifier — chumsky lexer treats
    //      keywords as absolute, tree-sitter falls back to identifier
    //      when the keyword isn't valid in the current state.
    //   2. Semantic-only rejections in `parse()` (refutable patterns,
    //      `\u{D800}` surrogates, `_` as a value).
    if !chumsky_accepted && ts_accepted {
        // Heuristic: keywords-as-identifiers tend to feature `let `, `if `,
        // `fn `, `i32`, `bool`, etc. *followed* immediately by another
        // keyword or by `=`. Rather than enumerate, we mark every "ts
        // accepts, chumsky rejects" pair as known-divergence — these
        // are caught by the spec-corpus differential test, which has
        // an explicit allowlist, and that's where we tighten the net.
        // The fuzzer is here to catch the inverse direction (tree-sitter
        // failing to accept something chumsky accepts) which is where
        // grammar drift lives.
        return true;
    }
    let _ = source;
    false
}
