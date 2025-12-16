//! AST to RIR generation.
//!
//! AstGen converts the Abstract Syntax Tree into RIR instructions.
//! This is analogous to Zig's AstGen phase.

use rue_intern::Interner;
use rue_parser::{Ast, Expr, Function, Item};
use rue_span::Span;

use crate::inst::{Inst, InstData, InstRef, Rir};

/// Generates RIR from an AST.
pub struct AstGen<'a> {
    /// The AST being processed
    ast: &'a Ast,
    /// String interner for symbols
    interner: &'a mut Interner,
    /// Output RIR
    rir: Rir,
}

impl<'a> AstGen<'a> {
    /// Create a new AstGen for the given AST.
    pub fn new(ast: &'a Ast, interner: &'a mut Interner) -> Self {
        Self {
            ast,
            interner,
            rir: Rir::new(),
        }
    }

    /// Generate RIR from the AST.
    pub fn generate(mut self) -> Rir {
        for item in &self.ast.items {
            self.gen_item(item);
        }
        self.rir
    }

    fn gen_item(&mut self, item: &Item) {
        match item {
            Item::Function(func) => {
                self.gen_function(func);
            }
        }
    }

    fn gen_function(&mut self, func: &Function) -> InstRef {
        // Intern the function name and return type
        let name = self.interner.intern(&func.name.name);
        let return_type = self.interner.intern(&func.return_type.name);

        // Generate body expression
        let body = self.gen_expr(&func.body);

        // Create function declaration instruction
        self.rir.add_inst(Inst {
            data: InstData::FnDecl {
                name,
                return_type,
                body,
            },
            span: func.span,
        })
    }

    fn gen_expr(&mut self, expr: &Expr) -> InstRef {
        match expr {
            Expr::Int(lit) => self.rir.add_inst(Inst {
                data: InstData::IntConst(lit.value),
                span: lit.span,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;
    use rue_parser::Parser;

    #[test]
    fn test_gen_simple_function() {
        let mut lexer = Lexer::new("fn main() -> i32 { 42 }");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        let mut interner = Interner::new();
        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        // Should have 2 instructions: IntConst(42), FnDecl
        assert_eq!(rir.len(), 2);

        // Check the function declaration
        let (_, fn_inst) = rir.iter().last().unwrap();
        match &fn_inst.data {
            InstData::FnDecl {
                name,
                return_type,
                body,
            } => {
                assert_eq!(interner.get(*name), "main");
                assert_eq!(interner.get(*return_type), "i32");
                // Body should be the int constant
                let body_inst = rir.get(*body);
                assert!(matches!(body_inst.data, InstData::IntConst(42)));
            }
            _ => panic!("expected FnDecl"),
        }
    }
}
