//! Builds the tree-sitter Gruel parser from the committed `src/parser.c`.
//!
//! The grammar source lives in `tree-sitter-gruel/grammar.js`; `src/` is the
//! result of `tree-sitter generate` and is committed so contributors do not
//! need `node` installed to build this crate.

use std::path::PathBuf;

fn main() {
    // Crate root is `tree-sitter-gruel/bindings/rust/`; the generated parser
    // lives two directories up at `tree-sitter-gruel/src/`.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../src");

    let parser_c = src.join("parser.c");
    println!("cargo:rerun-if-changed={}", parser_c.display());

    cc::Build::new()
        .include(&src)
        .file(parser_c)
        .warnings(false)
        .compile("tree-sitter-gruel");
}
