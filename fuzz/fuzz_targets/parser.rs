#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        let lexer = gruel_lexer::Lexer::new(source);
        if let Ok((tokens, interner)) = lexer.tokenize() {
            let parser = gruel_parser::Parser::new(tokens, interner);
            let _ = parser.parse();
        }
    }
});
