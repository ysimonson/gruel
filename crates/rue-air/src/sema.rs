//! Semantic analysis - RIR to AIR conversion.
//!
//! Sema performs type checking and converts untyped RIR to typed AIR.
//! This is analogous to Zig's Sema phase.

use crate::inst::{Air, AirInst, AirInstData, AirRef};
use crate::types::Type;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_intern::{Interner, Symbol};
use rue_rir::{InstData, InstRef, Rir};
use rue_span::Span;

/// Result of analyzing a function.
pub struct AnalyzedFunction {
    pub name: String,
    pub air: Air,
}

/// Semantic analyzer that converts RIR to AIR.
pub struct Sema<'a> {
    rir: &'a Rir,
    interner: &'a Interner,
}

impl<'a> Sema<'a> {
    /// Create a new semantic analyzer.
    pub fn new(rir: &'a Rir, interner: &'a Interner) -> Self {
        Self { rir, interner }
    }

    /// Analyze all functions in the RIR.
    pub fn analyze_all(&self) -> CompileResult<Vec<AnalyzedFunction>> {
        let mut functions = Vec::new();

        for (_, inst) in self.rir.iter() {
            if let InstData::FnDecl {
                name,
                return_type,
                body,
            } = &inst.data
            {
                let fn_name = self.interner.get(*name).to_string();
                let ret_type = self.resolve_type(*return_type, inst.span)?;

                let air = self.analyze_function(ret_type, *body)?;

                functions.push(AnalyzedFunction { name: fn_name, air });
            }
        }

        Ok(functions)
    }

    /// Analyze a single function, producing AIR.
    fn analyze_function(&self, return_type: Type, body: InstRef) -> CompileResult<Air> {
        let mut air = Air::new(return_type);

        // Analyze the body expression
        let body_ref = self.analyze_inst(&mut air, body, return_type)?;

        // Add implicit return
        air.add_inst(AirInst {
            data: AirInstData::Ret(body_ref),
            ty: return_type,
            span: self.rir.get(body).span,
        });

        Ok(air)
    }

    /// Analyze an RIR instruction, producing AIR instructions.
    fn analyze_inst(
        &self,
        air: &mut Air,
        inst_ref: InstRef,
        expected_type: Type,
    ) -> CompileResult<AirRef> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::IntConst(value) => {
                // Integer constants are always i32 for now
                let ty = Type::I32;

                // Type check
                if ty != expected_type && !expected_type.is_error() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: expected_type.name().to_string(),
                            found: ty.name().to_string(),
                        },
                        inst.span,
                    ));
                }

                Ok(air.add_inst(AirInst {
                    data: AirInstData::Const(*value),
                    ty,
                    span: inst.span,
                }))
            }

            InstData::FnDecl { .. } => {
                // Function declarations are handled at the top level
                unreachable!("FnDecl should not appear in expression context")
            }

            InstData::Ret(inner) => {
                let inner_ref = self.analyze_inst(air, *inner, expected_type)?;
                Ok(air.add_inst(AirInst {
                    data: AirInstData::Ret(inner_ref),
                    ty: expected_type,
                    span: inst.span,
                }))
            }

            InstData::Block { .. } => {
                // Blocks not yet implemented
                unimplemented!("blocks")
            }
        }
    }

    /// Resolve a type symbol to a Type.
    ///
    /// Uses symbol comparison instead of string comparison for efficiency.
    fn resolve_type(&self, type_sym: Symbol, span: Span) -> CompileResult<Type> {
        let well_known = self.interner.well_known();

        if type_sym == well_known.i32 {
            Ok(Type::I32)
        } else {
            Err(CompileError::new(
                ErrorKind::UnexpectedToken {
                    expected: "type",
                    found: self.interner.get(type_sym).to_string(),
                },
                span,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    fn compile_to_air(source: &str) -> CompileResult<Vec<AnalyzedFunction>> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize()?;
        let mut parser = Parser::new(tokens);
        let ast = parser.parse()?;

        let mut interner = Interner::new();
        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        let sema = Sema::new(&rir, &interner);
        sema.analyze_all()
    }

    #[test]
    fn test_analyze_simple_function() {
        let functions = compile_to_air("fn main() -> i32 { 42 }").unwrap();

        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "main");

        let air = &functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        assert_eq!(air.len(), 2); // Const + Ret
    }
}
