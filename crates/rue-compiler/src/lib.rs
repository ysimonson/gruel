//! Rue compiler driver.
//!
//! This crate orchestrates the compilation pipeline:
//! Source → Lexer → Parser → AstGen → Sema → CodeGen → ELF
//!
//! It re-exports types from the component crates for convenience.

// Re-export commonly used types
pub use rue_air::{Air, AnalyzedFunction, Sema, Type};
pub use rue_codegen::{CodeGen, X86Mir};
pub use rue_elf::build_elf;
pub use rue_linker::{Linker, ObjectBuilder, ObjectFile};
pub use rue_error::{CompileError, CompileResult, ErrorKind};
pub use rue_intern::{Interner, Symbol};
pub use rue_lexer::{Lexer, Token, TokenKind};
pub use rue_parser::{Ast, Expr, Function, Parser};
pub use rue_rir::{AstGen, Rir, RirPrinter};
pub use rue_span::Span;

/// Intermediate compilation state, allowing inspection at each stage.
pub struct CompileState {
    pub interner: Interner,
    pub rir: Rir,
    pub functions: Vec<AnalyzedFunction>,
}

/// Compile source code through all phases up to (but not including) codegen.
///
/// Returns the compile state which can be inspected for debugging.
pub fn compile_to_air(source: &str) -> CompileResult<CompileState> {
    // Phase 1: Lexing
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize()?;

    // Phase 2: Parsing
    let mut parser = Parser::new(tokens);
    let ast = parser.parse()?;

    // Phase 3: AST to RIR (untyped IR)
    let mut interner = Interner::new();
    let astgen = AstGen::new(&ast, &mut interner);
    let rir = astgen.generate();

    // Phase 4: Semantic analysis (RIR to AIR)
    let sema = Sema::new(&rir, &interner);
    let functions = sema.analyze_all()?;

    Ok(CompileState {
        interner,
        rir,
        functions,
    })
}

/// Compile source code to an ELF binary.
///
/// This is the main entry point for compilation.
pub fn compile(source: &str) -> CompileResult<Vec<u8>> {
    let state = compile_to_air(source)?;

    // Check for main function
    let main_fn = state
        .functions
        .iter()
        .find(|f| f.name == "main")
        .ok_or_else(|| CompileError::new(ErrorKind::NoMainFunction, Span::default()))?;

    // Phase 5: Code generation (AIR to machine code)
    let codegen = CodeGen::new(&main_fn.air);
    let machine_code = codegen.generate();

    // Phase 6: Build object file
    let obj_bytes = ObjectBuilder::new("main")
        .code(machine_code.code)
        .build();

    // Phase 7: Link to executable
    let obj = ObjectFile::parse(&obj_bytes)
        .map_err(|e| CompileError::new(ErrorKind::LinkError(e.to_string()), Span::default()))?;

    let mut linker = Linker::new();
    linker
        .add_object(obj)
        .map_err(|e| CompileError::new(ErrorKind::LinkError(e.to_string()), Span::default()))?;

    let elf = linker
        .link("main")
        .map_err(|e| CompileError::new(ErrorKind::LinkError(e.to_string()), Span::default()))?;

    Ok(elf)
}

/// Generate X86Mir from AIR (for debugging/inspection).
pub fn generate_mir(air: &Air) -> X86Mir {
    rue_codegen::x86_64::Lower::new(air).lower()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_simple() {
        let elf = compile("fn main() -> i32 { 42 }").unwrap();
        // Should produce a valid ELF
        assert_eq!(&elf[0..4], &[0x7F, b'E', b'L', b'F']);
    }

    #[test]
    fn test_compile_no_main() {
        let result = compile("fn foo() -> i32 { 42 }");
        assert!(result.is_err());
    }
}
