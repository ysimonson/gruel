//! Semantic analysis - RIR to AIR conversion.
//!
//! Sema performs type checking and converts untyped RIR to typed AIR.
//! This is analogous to Zig's Sema phase.

use std::collections::HashMap;

use crate::inst::{Air, AirInst, AirInstData, AirRef};
use crate::types::Type;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_intern::{Interner, Symbol};
use rue_rir::{InstData, InstRef, Rir};
use rue_span::Span;

/// Result of analyzing a function.
#[derive(Debug)]
pub struct AnalyzedFunction {
    pub name: String,
    pub air: Air,
    /// Number of local variable slots needed
    pub num_locals: u32,
}

/// Information about a local variable.
#[derive(Debug, Clone)]
struct LocalVar {
    /// Slot index for this variable
    slot: u32,
    /// Type of the variable
    ty: Type,
    /// Whether the variable is mutable
    is_mut: bool,
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

                let (air, num_locals) = self.analyze_function(ret_type, *body)?;

                functions.push(AnalyzedFunction {
                    name: fn_name,
                    air,
                    num_locals,
                });
            }
        }

        Ok(functions)
    }

    /// Analyze a single function, producing AIR.
    fn analyze_function(&self, return_type: Type, body: InstRef) -> CompileResult<(Air, u32)> {
        let mut air = Air::new(return_type);
        let mut locals: HashMap<Symbol, LocalVar> = HashMap::new();
        let mut next_slot: u32 = 0;

        // Analyze the body expression
        let body_ref = self.analyze_inst(&mut air, body, return_type, &mut locals, &mut next_slot)?;

        // Add implicit return
        air.add_inst(AirInst {
            data: AirInstData::Ret(body_ref),
            ty: return_type,
            span: self.rir.get(body).span,
        });

        Ok((air, next_slot))
    }

    /// Analyze an RIR instruction, producing AIR instructions.
    fn analyze_inst(
        &self,
        air: &mut Air,
        inst_ref: InstRef,
        expected_type: Type,
        locals: &mut HashMap<Symbol, LocalVar>,
        next_slot: &mut u32,
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

            InstData::Add { lhs, rhs } => {
                // Both operands must be i32 for now
                let lhs_ref = self.analyze_inst(air, *lhs, Type::I32, locals, next_slot)?;
                let rhs_ref = self.analyze_inst(air, *rhs, Type::I32, locals, next_slot)?;

                Ok(air.add_inst(AirInst {
                    data: AirInstData::Add(lhs_ref, rhs_ref),
                    ty: Type::I32,
                    span: inst.span,
                }))
            }

            InstData::Sub { lhs, rhs } => {
                let lhs_ref = self.analyze_inst(air, *lhs, Type::I32, locals, next_slot)?;
                let rhs_ref = self.analyze_inst(air, *rhs, Type::I32, locals, next_slot)?;

                Ok(air.add_inst(AirInst {
                    data: AirInstData::Sub(lhs_ref, rhs_ref),
                    ty: Type::I32,
                    span: inst.span,
                }))
            }

            InstData::Mul { lhs, rhs } => {
                let lhs_ref = self.analyze_inst(air, *lhs, Type::I32, locals, next_slot)?;
                let rhs_ref = self.analyze_inst(air, *rhs, Type::I32, locals, next_slot)?;

                Ok(air.add_inst(AirInst {
                    data: AirInstData::Mul(lhs_ref, rhs_ref),
                    ty: Type::I32,
                    span: inst.span,
                }))
            }

            InstData::Div { lhs, rhs } => {
                let lhs_ref = self.analyze_inst(air, *lhs, Type::I32, locals, next_slot)?;
                let rhs_ref = self.analyze_inst(air, *rhs, Type::I32, locals, next_slot)?;

                Ok(air.add_inst(AirInst {
                    data: AirInstData::Div(lhs_ref, rhs_ref),
                    ty: Type::I32,
                    span: inst.span,
                }))
            }

            InstData::Mod { lhs, rhs } => {
                let lhs_ref = self.analyze_inst(air, *lhs, Type::I32, locals, next_slot)?;
                let rhs_ref = self.analyze_inst(air, *rhs, Type::I32, locals, next_slot)?;

                Ok(air.add_inst(AirInst {
                    data: AirInstData::Mod(lhs_ref, rhs_ref),
                    ty: Type::I32,
                    span: inst.span,
                }))
            }

            InstData::Neg { operand } => {
                let operand_ref = self.analyze_inst(air, *operand, Type::I32, locals, next_slot)?;

                Ok(air.add_inst(AirInst {
                    data: AirInstData::Neg(operand_ref),
                    ty: Type::I32,
                    span: inst.span,
                }))
            }

            InstData::Alloc { name, is_mut, ty, init } => {
                // Check type annotation if provided
                if let Some(type_sym) = ty {
                    // Verify it's a valid type
                    let well_known = self.interner.well_known();
                    if *type_sym != well_known.i32 {
                        let type_name = self.interner.get(*type_sym);
                        return Err(CompileError::new(
                            ErrorKind::UnknownType(type_name.to_string()),
                            inst.span,
                        ));
                    }
                }

                // Analyze the initializer
                let init_ref = self.analyze_inst(air, *init, Type::I32, locals, next_slot)?;

                // Allocate a new slot
                let slot = *next_slot;
                *next_slot += 1;

                // Register the variable (shadowing is allowed by just overwriting)
                locals.insert(
                    *name,
                    LocalVar {
                        slot,
                        ty: Type::I32,
                        is_mut: *is_mut,
                    },
                );

                // Emit the alloc instruction
                Ok(air.add_inst(AirInst {
                    data: AirInstData::Alloc { slot, init: init_ref },
                    ty: Type::Unit,
                    span: inst.span,
                }))
            }

            InstData::VarRef { name } => {
                // Look up the variable
                let name_str = self.interner.get(*name);
                let local = locals.get(name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        inst.span,
                    )
                })?;

                // Load the variable
                Ok(air.add_inst(AirInst {
                    data: AirInstData::Load { slot: local.slot },
                    ty: local.ty,
                    span: inst.span,
                }))
            }

            InstData::Assign { name, value } => {
                // Look up the variable
                let name_str = self.interner.get(*name);
                let local = locals.get(name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        inst.span,
                    )
                })?;

                // Check mutability
                if !local.is_mut {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(name_str.to_string()),
                        inst.span,
                    ));
                }

                let slot = local.slot;
                let ty = local.ty;

                // Analyze the value
                let value_ref = self.analyze_inst(air, *value, ty, locals, next_slot)?;

                // Emit store instruction
                Ok(air.add_inst(AirInst {
                    data: AirInstData::Store { slot, value: value_ref },
                    ty: Type::Unit,
                    span: inst.span,
                }))
            }

            InstData::FnDecl { .. } => {
                // Function declarations are handled at the top level
                unreachable!("FnDecl should not appear in expression context")
            }

            InstData::Ret(inner) => {
                let inner_ref = self.analyze_inst(air, *inner, expected_type, locals, next_slot)?;
                Ok(air.add_inst(AirInst {
                    data: AirInstData::Ret(inner_ref),
                    ty: expected_type,
                    span: inst.span,
                }))
            }

            InstData::Block { extra_start, len } => {
                // Get the instruction refs from extra data
                let inst_refs = self.rir.get_extra(*extra_start, *len);

                // Save the current locals for block scoping.
                // Variables declared in this block will be removed when the block ends.
                let saved_locals = locals.clone();

                // Process all instructions in the block
                // The last one is the final expression (the block's value)
                // All other instructions are statements and should be typed as Unit
                let mut last_ref = None;
                let num_insts = inst_refs.len();
                for (i, &raw_ref) in inst_refs.iter().enumerate() {
                    let inst_ref = InstRef::from_raw(raw_ref);
                    let is_last = i == num_insts - 1;
                    // Only the final expression should match expected_type;
                    // statements (let, assign, expr;) don't need type checking
                    // against the block's expected type
                    let inst_expected_type = if is_last { expected_type } else { Type::Unit };
                    last_ref = Some(self.analyze_inst(air, inst_ref, inst_expected_type, locals, next_slot)?);
                }

                // Restore locals to remove block-scoped variables.
                // Note: We don't restore next_slot, so slots are not reused.
                // This is a future optimization opportunity.
                *locals = saved_locals;

                // Return the last instruction's result (the block's value)
                Ok(last_ref.expect("block should have at least one instruction"))
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

    #[test]
    fn test_analyze_addition() {
        let functions = compile_to_air("fn main() -> i32 { 1 + 2 }").unwrap();

        let air = &functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        // Const(1) + Const(2) + Add + Ret = 4 instructions
        assert_eq!(air.len(), 4);

        // Check that add instruction exists with correct type
        let add_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(add_inst.data, AirInstData::Add(_, _)));
        assert_eq!(add_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_all_binary_ops() {
        // Test that all binary operators compile correctly
        assert!(compile_to_air("fn main() -> i32 { 1 + 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 - 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 * 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 / 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 % 2 }").is_ok());
    }

    #[test]
    fn test_analyze_negation() {
        let functions = compile_to_air("fn main() -> i32 { -42 }").unwrap();

        let air = &functions[0].air;
        // Const(42) + Neg + Ret = 3 instructions
        assert_eq!(air.len(), 3);

        let neg_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(neg_inst.data, AirInstData::Neg(_)));
        assert_eq!(neg_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_complex_expr() {
        let functions = compile_to_air("fn main() -> i32 { (1 + 2) * 3 }").unwrap();

        let air = &functions[0].air;
        // Const(1) + Const(2) + Add + Const(3) + Mul + Ret = 6 instructions
        assert_eq!(air.len(), 6);

        // Check that result is multiplication
        let mul_inst = air.get(AirRef::from_raw(4));
        assert!(matches!(mul_inst.data, AirInstData::Mul(_, _)));
    }

    #[test]
    fn test_analyze_let_binding() {
        let functions = compile_to_air("fn main() -> i32 { let x = 42; x }").unwrap();

        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].num_locals, 1);

        let air = &functions[0].air;
        // Const(42) + Alloc + Load + Ret = 4 instructions
        assert_eq!(air.len(), 4);

        // Check alloc instruction
        let alloc_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(alloc_inst.data, AirInstData::Alloc { slot: 0, .. }));

        // Check load instruction
        let load_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(load_inst.data, AirInstData::Load { slot: 0 }));
    }

    #[test]
    fn test_analyze_let_mut_assignment() {
        let functions = compile_to_air("fn main() -> i32 { let mut x = 10; x = 20; x }").unwrap();

        let air = &functions[0].air;
        // Const(10) + Alloc + Const(20) + Store + Load + Ret = 6 instructions
        assert_eq!(air.len(), 6);

        // Check store instruction
        let store_inst = air.get(AirRef::from_raw(3));
        assert!(matches!(store_inst.data, AirInstData::Store { slot: 0, .. }));
    }

    #[test]
    fn test_undefined_variable() {
        let result = compile_to_air("fn main() -> i32 { x }");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::UndefinedVariable(_)));
    }

    #[test]
    fn test_assign_to_immutable() {
        let result = compile_to_air("fn main() -> i32 { let x = 10; x = 20; x }");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::AssignToImmutable(_)));
    }

    #[test]
    fn test_multiple_variables() {
        let functions = compile_to_air("fn main() -> i32 { let x = 10; let y = 20; x + y }").unwrap();

        assert_eq!(functions[0].num_locals, 2);
    }
}
