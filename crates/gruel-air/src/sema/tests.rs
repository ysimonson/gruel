#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::inst::{AirInstData, AirRef};
    use crate::sema::{Sema, SemaOutput};
    use crate::types::Type;
    use gruel_lexer::Lexer;
    use gruel_parser::Parser;
    use gruel_rir::AstGen;
    use gruel_util::{BinOp, CompileErrors, ErrorKind, MultiErrorResult, PreviewFeatures, UnaryOp};

    fn compile_to_air(source: &str) -> MultiErrorResult<SemaOutput> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from_error)?;
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse()?;

        let astgen = AstGen::new(&ast, &interner);
        let rir = astgen.generate();

        let sema = Sema::new(&rir, &interner, PreviewFeatures::default());
        sema.analyze_all()
    }

    #[test]
    fn test_analyze_simple_function() {
        let output = compile_to_air("fn main() -> i32 { 42 }").unwrap();
        let functions = &output.functions;

        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "main");

        let air = &functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        assert_eq!(air.len(), 2); // Const + Ret
    }

    #[test]
    fn test_analyze_addition() {
        let output = compile_to_air("fn main() -> i32 { 1 + 2 }").unwrap();

        let air = &output.functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        // Const(1) + Const(2) + Add + Ret = 4 instructions
        assert_eq!(air.len(), 4);

        // Check that add instruction exists with correct type
        let add_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(add_inst.data, AirInstData::Bin(BinOp::Add, _, _)));
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
        let output = compile_to_air("fn main() -> i32 { -42 }").unwrap();

        let air = &output.functions[0].air;
        // Const(42) + Neg + Ret = 3 instructions
        assert_eq!(air.len(), 3);

        let neg_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(neg_inst.data, AirInstData::Unary(UnaryOp::Neg, _)));
        assert_eq!(neg_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_complex_expr() {
        let output = compile_to_air("fn main() -> i32 { (1 + 2) * 3 }").unwrap();

        let air = &output.functions[0].air;
        // Const(1) + Const(2) + Add + Const(3) + Mul + Ret = 6 instructions
        assert_eq!(air.len(), 6);

        // Check that result is multiplication
        let mul_inst = air.get(AirRef::from_raw(4));
        assert!(matches!(mul_inst.data, AirInstData::Bin(BinOp::Mul, _, _)));
    }

    #[test]
    fn test_analyze_let_binding() {
        let output = compile_to_air("fn main() -> i32 { let x = 42; x }").unwrap();

        assert_eq!(output.functions.len(), 1);
        assert_eq!(output.functions[0].num_locals, 1);

        let air = &output.functions[0].air;
        // Const(42) + StorageLive + Alloc + Block([StorageLive], Alloc) + Load + Block([alloc block], Load) + Ret = 7 instructions
        assert_eq!(air.len(), 7);

        // Check storage_live instruction
        let storage_live_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(
            storage_live_inst.data,
            AirInstData::StorageLive { slot: 0 }
        ));

        // Check alloc instruction
        let alloc_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(
            alloc_inst.data,
            AirInstData::Alloc { slot: 0, .. }
        ));

        // Check load instruction
        let load_inst = air.get(AirRef::from_raw(4));
        assert!(matches!(load_inst.data, AirInstData::Load { slot: 0 }));

        // Check block instruction groups the alloc with the load
        let block_inst = air.get(AirRef::from_raw(5));
        assert!(matches!(block_inst.data, AirInstData::Block { .. }));
    }

    #[test]
    fn test_analyze_let_mut_assignment() {
        let output = compile_to_air("fn main() -> i32 { let mut x = 10; x = 20; x }").unwrap();

        let air = &output.functions[0].air;
        // Const(10) + StorageLive + Alloc + Block([StorageLive], Alloc) + Const(20) + Store + Load + Block([alloc block, Store], Load) + Ret = 9 instructions
        assert_eq!(air.len(), 9);

        // Check store instruction
        let store_inst = air.get(AirRef::from_raw(5));
        assert!(matches!(
            store_inst.data,
            AirInstData::Store { slot: 0, .. }
        ));

        // Check block instruction groups statements
        let block_inst = air.get(AirRef::from_raw(7));
        assert!(matches!(block_inst.data, AirInstData::Block { .. }));
    }

    #[test]
    fn test_undefined_variable() {
        let result = compile_to_air("fn main() -> i32 { x }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::UndefinedVariable(_)
        ));
    }

    #[test]
    fn test_assign_to_immutable() {
        let result = compile_to_air("fn main() -> i32 { let x = 10; x = 20; x }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::AssignToImmutable(_)
        ));
    }

    #[test]
    fn test_multiple_variables() {
        let output = compile_to_air("fn main() -> i32 { let x = 10; let y = 20; x + y }").unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }

    #[test]
    fn test_empty_block_evaluates_to_unit() {
        // Empty block should evaluate to () and not panic
        let output = compile_to_air("fn main() { let _x: () = {}; }").unwrap();

        let air = &output.functions[0].air;
        // Should have a UnitConst instruction for the empty block
        let has_unit_const = air
            .iter()
            .any(|(_, inst)| matches!(inst.data, AirInstData::UnitConst));
        assert!(has_unit_const, "Empty block should produce UnitConst");
    }

    // =========================================================================
    // Error recovery tests
    // =========================================================================
    // These tests verify that one type error does not cause cascading errors.
    // The issue gruel-wqyw tracks the implementation of better error recovery.

    #[test]
    fn test_single_error_no_cascade_simple() {
        // A simple case where adding an integer and boolean should report exactly one error
        let result = compile_to_air("fn main() -> i32 { 1 + true }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(
            errors.len(),
            1,
            "Should have exactly 1 error, not cascading errors"
        );

        // Verify the error is about type mismatch (numeric vs bool)
        let error = errors.iter().next().unwrap();
        assert!(
            matches!(&error.kind, ErrorKind::TypeMismatch { expected, found }
                if expected.contains("numeric") && found.contains("bool")),
            "Error should mention numeric and bool, got: {:?}",
            error.kind
        );
    }

    #[test]
    fn test_single_error_no_cascade_with_function_call() {
        // The error-typed variable is used in a function call - should not cascade
        let result = compile_to_air(
            "fn foo(a: i32, b: i32) -> i32 { a + b }
             fn main() -> i32 {
                 let x = 1 + true;
                 foo(x, 1)
             }",
        );
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(
            errors.len(),
            1,
            "Should have exactly 1 error for the original type mismatch"
        );
    }

    #[test]
    fn test_single_error_no_cascade_deep_chain() {
        // Deep chain of operations using error-typed value - should not cascade
        let result = compile_to_air(
            "fn main() -> i32 {
                 let x = 1 + true;
                 let y = x + 1;
                 let z = y * 2;
                 let w = z - 3;
                 let v = w / 4;
                 v
             }",
        );
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(
            errors.len(),
            1,
            "Should have exactly 1 error, not 5 cascading errors"
        );
    }

    #[test]
    fn test_bool_plus_int_error() {
        // Reversed order: bool + int should also give one error
        let result = compile_to_air("fn main() -> i32 { true + 1 }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_arithmetic_on_bool_type_error() {
        // Using bool in any arithmetic should be an error
        let result = compile_to_air("fn main() -> i32 { true * true }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
    }

    // =========================================================================
    // Scope management tests
    // =========================================================================
    // These tests verify that variable scoping works correctly, including
    // shadowing, nested scopes, and proper cleanup when exiting scopes.

    #[test]
    fn test_variable_shadowing_same_type() {
        // Variable shadowing with the same type should work
        let output = compile_to_air(
            "fn main() -> i32 {
                let x = 10;
                let x = 20;  // Shadow x with a new binding
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }

    #[test]
    fn test_variable_shadowing_different_type() {
        // Variable shadowing with a different type should work
        let output = compile_to_air(
            "fn main() -> bool {
                let x = 10;
                let x = true;  // Shadow x with a different type
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
        assert_eq!(output.functions[0].air.return_type(), Type::BOOL);
    }

    #[test]
    fn test_nested_scope_variable_not_visible_outside() {
        // Variable declared in inner scope should not be visible outside
        let result = compile_to_air(
            "fn main() -> i32 {
                {
                    let x = 10;
                }
                x  // Error: x is not in scope
            }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::UndefinedVariable(_)
        ));
    }

    #[test]
    fn test_shadowed_variable_restored_after_scope() {
        // After inner scope ends, the outer variable should be visible again
        let output = compile_to_air(
            "fn main() -> i32 {
                let x = 10;
                {
                    let x = 20;  // Shadow x in inner scope
                }
                x  // Should be 10 (outer x)
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }

    #[test]
    fn test_deeply_nested_scopes() {
        // Variables in deeply nested scopes should work correctly
        let output = compile_to_air(
            "fn main() -> i32 {
                let a = 1;
                {
                    let b = 2;
                    {
                        let c = 3;
                        {
                            let d = 4;
                            a + b + c + d
                        }
                    }
                }
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].num_locals, 4);
    }

    #[test]
    fn test_if_else_scope_isolation() {
        // Variables in if/else branches should not leak
        let result = compile_to_air(
            "fn main() -> i32 {
                if true {
                    let x = 10;
                    x
                } else {
                    y  // Error: y not defined in this branch
                }
            }",
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_loop_scope_isolation() {
        // Variables in loop body should not leak outside
        let result = compile_to_air(
            "fn main() -> i32 {
                let mut i = 0;
                loop {
                    let inner = 10;
                    i = i + 1;
                    if i > 5 {
                        break inner;
                    }
                }
            }",
        );

        // This should compile successfully
        assert!(result.is_ok());
    }

    // =========================================================================
    // Declaration gathering tests
    // =========================================================================
    // These tests verify that declarations are properly gathered and validated.

    #[test]
    fn test_duplicate_struct_names() {
        // Two structs with the same name should error
        let result = compile_to_air(
            "struct Foo { x: i32 }
             struct Foo { y: bool }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateTypeDefinition { .. }
        ));
    }

    #[test]
    fn test_duplicate_enum_names() {
        // Two enums with the same name should error
        let result = compile_to_air(
            "enum Color { Red, Green }
             enum Color { Blue, Yellow }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateTypeDefinition { .. }
        ));
    }

    #[test]
    fn test_struct_and_enum_name_collision() {
        // A struct and enum with the same name should error
        let result = compile_to_air(
            "struct Foo { x: i32 }
             enum Foo { A, B }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateTypeDefinition { .. }
        ));
    }

    #[test]
    fn test_duplicate_struct_field() {
        // Duplicate field names in a struct should error
        let result = compile_to_air(
            "struct Foo { x: i32, x: bool }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateField { .. }
        ));
    }

    #[test]
    fn test_duplicate_enum_variant() {
        // Duplicate variant names in an enum should error
        let result = compile_to_air(
            "enum Color { Red, Blue, Red }
             fn main() -> i32 { 0 }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::DuplicateVariant { .. }
        ));
    }

    #[test]
    fn test_struct_field_type_resolution() {
        // Struct with field of another struct type should resolve correctly
        let output = compile_to_air(
            "@mark(copy) struct Inner { x: i32 }
             @mark(copy) struct Outer { inner: Inner }
             fn main() -> i32 {
                let o = Outer { inner: Inner { x: 42 } };
                o.inner.x
             }",
        )
        .unwrap();

        // ADR-0081: BUILTIN_TYPES is empty; no synthetic structs are
        // injected by this helper (the prelude is skipped).
        assert_eq!(output.type_pool.stats().struct_count, 2); // Inner, Outer
    }

    #[test]
    fn test_copy_struct_with_copy_fields() {
        // @derive(Copy) struct with only Copy fields should compile
        let output = compile_to_air(
            "@mark(copy) struct Point { x: i32, y: i32 }
             fn main() -> i32 {
                let p = Point { x: 1, y: 2 };
                let q = p;  // Copy, not move
                p.x + q.x
             }",
        )
        .unwrap();

        assert!(
            output
                .type_pool
                .all_struct_ids()
                .iter()
                .map(|id| output.type_pool.struct_def(*id))
                .any(|s| s.name == "Point" && s.is_copy)
        );
    }

    // ADR-0079: the field-Copy invariant for `@derive(Copy)` is now
    // enforced by the prelude `derive Copy` body via `comptime if`
    // + `@implements` + `@compile_error`. The unit-test path
    // (`compile_to_air`) intentionally skips the prelude, so this
    // path no longer catches the violation here. The spec suite's
    // `types.move-semantics::copy_struct_non_copy_field_error`
    // covers the prelude-loaded path end-to-end.

    #[test]
    fn test_recursive_struct_via_array() {
        // Self-referential struct through array is not allowed (no arrays of non-copy structs yet)
        // But circular reference through other means should be detected
        let result = compile_to_air(
            "struct Node { value: i32 }
             fn main() -> i32 { 0 }",
        );

        // Simple non-recursive struct should work
        assert!(result.is_ok());
    }

    #[test]
    fn test_function_signature_resolution() {
        // Function parameters and return types should resolve correctly
        let output = compile_to_air(
            "fn add(a: i32, b: i32) -> i32 { a + b }
             fn main() -> i32 { add(1, 2) }",
        )
        .unwrap();

        assert_eq!(output.functions.len(), 2);
    }

    // ADR-0081: the registry-driven `String` retired with `STRING_TYPE`;
    // String is now a regular struct declared in `prelude/string.gruel`
    // and the unit-test harness `compile_to_air` deliberately skips the
    // prelude. End-to-end String coverage lives in
    // `crates/gruel-spec/cases/types/{strings,mutable-strings,
    // char_string,string_vec_bridge}.toml`.

    // =========================================================================
    // Move tracking integration tests
    // =========================================================================
    // These tests verify move semantics work correctly through the full pipeline.

    #[test]
    fn test_use_after_move_error() {
        // Using a moved value should error. ADR-0083: a struct of all-Copy
        // fields now infers Copy under uniform structural inference, so we
        // declare the struct `linear` (or attach a `fn drop`) to keep it
        // non-Copy. `linear` doesn't require preview gating in this helper.
        let result = compile_to_air(
            "@mark(linear) struct NonCopy { x: i32 }
             fn consume(n: NonCopy) -> i32 { n.x }
             fn main() -> i32 {
                 let n = NonCopy { x: 42 };
                 let x = consume(n);
                 consume(n)  // Error: n was moved
             }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        let err = errors.iter().next().unwrap();
        assert!(
            matches!(err.kind, ErrorKind::UseAfterMove { .. }),
            "expected UseAfterMove, got {:?}",
            err.kind
        );
    }

    #[test]
    fn test_partial_move_banned() {
        // ADR-0036: Moving a non-copy field out of a struct is always an error.
        // Users must destructure the entire struct instead.
        // ADR-0083: a struct of all-Copy fields now infers Copy under
        // uniform inference. To keep `Inner` non-Copy without making the
        // outer type linear (which short-circuits the partial-move check),
        // attach a `fn drop` to `Inner`: Drop ⊥ Copy.
        let result = compile_to_air(
            "struct Inner {
                 x: i32,
                 fn drop(self) { @ignore_unused(self); }
             }
             struct Outer { a: Inner, b: i32 }
             fn consume(i: Inner) -> i32 { i.x }
             fn main() -> i32 {
                 let o = Outer { a: Inner { x: 1 }, b: 2 };
                 let x = consume(o.a);  // Error: cannot move field
                 o.b
             }",
        );

        assert!(result.is_err());
        let errors = result.unwrap_err();
        let err = errors.iter().next().unwrap();
        assert!(
            matches!(err.kind, ErrorKind::CannotMoveField { .. }),
            "expected CannotMoveField, got {:?}",
            err.kind
        );
    }

    #[test]
    fn test_copy_type_not_moved() {
        // Copy types should not be moved, allowing multiple uses
        let output = compile_to_air(
            "@mark(copy) struct Point { x: i32, y: i32 }
             fn use_point(p: Point) -> i32 { p.x }
             fn main() -> i32 {
                 let p = Point { x: 1, y: 2 };
                 let a = use_point(p);
                 let b = use_point(p);  // OK: Point is Copy
                 a + b
             }",
        )
        .unwrap();

        assert_eq!(output.functions.len(), 2);
    }

    // =========================================================================
    // Type inference tests
    // =========================================================================
    // These tests verify type inference works correctly.

    #[test]
    fn test_integer_literal_infers_i32_by_default() {
        // Unconstrained integer literal should default to i32
        let output = compile_to_air(
            "fn main() -> i32 {
                let x = 42;
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_integer_literal_infers_from_context() {
        // Integer literal should infer type from context
        let output = compile_to_air(
            "fn main() -> i64 {
                let x: i64 = 42;
                x
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I64);
    }

    #[test]
    fn test_integer_literal_infers_from_return_type() {
        // Integer literal should infer type from function return type
        let output = compile_to_air("fn main() -> u8 { 42 }").unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::U8);
    }

    #[test]
    fn test_integer_literal_infers_from_binary_op() {
        // Integer literal should infer type from binary operation context
        let output = compile_to_air(
            "fn main() -> i64 {
                let x: i64 = 10;
                x + 5  // 5 should infer to i64
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I64);
    }

    // =========================================================================
    // Array type tests
    // =========================================================================

    #[test]
    fn test_array_type_inference() {
        // Array element type should be inferred
        let output = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                arr[0]
            }",
        )
        .unwrap();

        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_array_index_signed_rejected() {
        // Array index must be usize; a signed integer is rejected.
        let result = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                let i: i32 = 1;
                arr[i]
            }",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_array_index_u64_rejected() {
        // Array index must be exactly usize; u64 is rejected.
        let result = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                let i: u64 = 1;
                arr[i]
            }",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_array_index_literal_infers_usize() {
        // Integer literal used as array index infers to usize.
        let output = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                arr[1]
            }",
        )
        .unwrap();
        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_array_index_accepts_usize() {
        let output = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2, 3];
                let i: usize = 1;
                arr[i]
            }",
        )
        .unwrap();
        assert_eq!(output.functions[0].air.return_type(), Type::I32);
    }

    #[test]
    fn test_array_length_mismatch() {
        // Array length in type annotation must match initializer
        let result = compile_to_air(
            "fn main() -> i32 {
                let arr: [i32; 3] = [1, 2];  // Error: length mismatch
                arr[0]
            }",
        );

        assert!(result.is_err());
    }

    // ========================================================================
    // Type Intern Pool tests (ADR-0024 Phase 1)
    //
    // These tests verify that the TypeInternPool is correctly populated during
    // declaration collection and that its contents match the existing type
    // registries (struct_defs, enum_defs).
    // ========================================================================

    /// Helper to gather declarations and return the Sema state for testing.
    fn gather_declarations_for_testing(source: &str) -> Sema<'static> {
        // We need to leak the interner for the static lifetime
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();

        let astgen = AstGen::new(&ast, &interner);
        let rir = astgen.generate();

        // Leak both to get 'static lifetime for testing
        let rir = Box::leak(Box::new(rir));
        let interner = Box::leak(Box::new(interner));

        let mut sema = Sema::new(rir, interner, PreviewFeatures::default());
        sema.inject_builtin_types();
        sema.register_type_names().unwrap();
        sema.resolve_declarations().unwrap();
        sema
    }

    // ADR-0081: removed `test_type_pool_populated_with_builtin_string`;
    // String moved to the prelude and `gather_declarations_for_testing`
    // deliberately skips the prelude. End-to-end String coverage lives in
    // the spec test suite.

    #[test]
    fn test_type_pool_populated_with_user_struct() {
        let sema = gather_declarations_for_testing(
            "struct Point { x: i32, y: i32 }
             fn main() -> i32 { 0 }",
        );

        let point_name = sema.interner.get("Point").unwrap();

        // Check the pool has the struct
        let pool_point = sema.type_pool.get_struct_by_name(point_name);
        assert!(pool_point.is_some(), "Point should be in the type pool");

        // Verify struct lookup has it
        let registry_point = sema.structs.get(&point_name);
        assert!(
            registry_point.is_some(),
            "Point should be in struct registry"
        );

        // Check the pool definition
        let pool_def = sema.type_pool.get_struct_def(pool_point.unwrap()).unwrap();

        assert_eq!(pool_def.name, "Point");
        assert_eq!(pool_def.fields.len(), 2);
        assert_eq!(pool_def.fields[0].name, "x");
        assert_eq!(pool_def.fields[1].name, "y");
        assert!(
            !pool_def.is_builtin,
            "Point should not be marked as builtin"
        );
    }

    #[test]
    fn test_type_pool_populated_with_enum() {
        let sema = gather_declarations_for_testing(
            "enum Color { Red, Green, Blue }
             fn main() -> i32 { 0 }",
        );

        let color_name = sema.interner.get("Color").unwrap();

        // Check the pool has the enum
        let pool_color = sema.type_pool.get_enum_by_name(color_name);
        assert!(pool_color.is_some(), "Color should be in the type pool");

        // Verify pool and registry agree - enum_id is now pool-based
        let registry_color = sema.enums.get(&color_name);
        assert!(registry_color.is_some(), "Color should be in enum registry");

        // Use type_pool.enum_def() to get the definition using pool-based EnumId
        let enum_id = *registry_color.unwrap();
        let pool_def = sema.type_pool.enum_def(enum_id);

        assert_eq!(pool_def.name, "Color");
        assert_eq!(pool_def.variants.len(), 3);
        assert_eq!(pool_def.variants[0].name, "Red");
        assert_eq!(pool_def.variants[1].name, "Green");
        assert_eq!(pool_def.variants[2].name, "Blue");
    }

    #[test]
    fn test_type_pool_copy_struct() {
        let sema = gather_declarations_for_testing(
            "@mark(copy) struct Data { value: i32 }
             fn main() -> i32 { 0 }",
        );

        let data_name = sema.interner.get("Data").unwrap();
        let pool_data = sema.type_pool.get_struct_by_name(data_name).unwrap();
        let pool_def = sema.type_pool.get_struct_def(pool_data).unwrap();

        assert!(
            pool_def.is_copy,
            "Data should be marked as `@mark(copy) struct`"
        );
    }

    #[test]
    fn test_type_pool_stats() {
        let sema = gather_declarations_for_testing(
            "struct A {}
             struct B {}
             enum E { X }
             fn main() -> i32 { 0 }",
        );

        let stats = sema.type_pool.stats();

        // 2 structs: A + B. ADR-0081 retired the synthetic `String` and
        // ADR-0078 Phase 3 moved the prelude-resident enums out of the
        // builtin injection path; this helper deliberately skips the
        // prelude, so neither contributes here.
        assert_eq!(stats.struct_count, 2);
        // 1 enum: just E from user source.
        assert_eq!(stats.enum_count, 1);
        // No arrays in Phase 1
        assert_eq!(stats.array_count, 0);
        // Total: 3 composite types (struct_count + enum_count +
        // array_count). ADR-0081 dropped the `Vec(u8)` interning that
        // came in via the synthetic String's field type.
        assert_eq!(stats.total, 3);
    }

    #[test]
    fn test_type_pool_all_registries_match() {
        // Test with multiple types to verify complete consistency
        let sema = gather_declarations_for_testing(
            "struct Point { x: i32, y: i32 }
             struct Empty {}
             @mark(copy) struct Value { v: bool }
             enum Status { Ok, Error }
             enum Direction { Up, Down, Left, Right }
             fn main() -> i32 { 0 }",
        );

        // Verify all structs in registry are in pool
        for (name_spur, &struct_id) in &sema.structs {
            // Use type_pool.struct_def() which takes pool-based struct_id
            let pool_def = sema.type_pool.struct_def(struct_id);

            // Also verify the pool can look up by name
            let pool_type = sema.type_pool.get_struct_by_name(*name_spur);
            assert!(
                pool_type.is_some(),
                "Struct '{}' should be in pool by name",
                pool_def.name
            );
        }

        // Verify all enums in registry are in pool
        for (name_spur, &enum_id) in &sema.enums {
            // Use type_pool.enum_def() which takes pool-based enum_id
            let pool_def = sema.type_pool.enum_def(enum_id);

            // Also verify the pool can look up by name
            let pool_type = sema.type_pool.get_enum_by_name(*name_spur);
            assert!(
                pool_type.is_some(),
                "Enum '{}' should be in pool by name",
                pool_def.name
            );
        }

        // Verify stats are available. ADR-0081 retired the synthetic
        // String builtin; counts come from the user source alone.
        let stats = sema.type_pool.stats();
        assert!(stats.struct_count > 0);
        assert!(stats.enum_count > 0);
    }

    // ------------------------------------------------------------------
    // ADR-0051: lower_pattern produces recursive AirPattern trees for
    // every match arm shape (the default path after Phase 4c).
    // ------------------------------------------------------------------
    mod recursive_pattern_lowering {
        use super::*;
        use crate::inst::{AirInstData, AirPattern};

        fn compile_with_recursive(source: &str) -> SemaOutput {
            let lexer = Lexer::new(source);
            let (tokens, interner) = lexer.tokenize().unwrap();
            let parser = Parser::new(tokens, interner);
            let (ast, interner) = parser.parse().unwrap();

            let astgen = AstGen::new(&ast, &interner);
            let rir = astgen.generate();

            let sema = Sema::new(&rir, &interner, PreviewFeatures::default());
            sema.analyze_all().unwrap()
        }

        /// Collect all match arms across every function in the output.
        fn collect_match_arms(output: &SemaOutput) -> Vec<AirPattern> {
            let mut out = Vec::new();
            for f in &output.functions {
                let air = &f.air;
                for (_, inst) in air.iter() {
                    if let AirInstData::Match {
                        arms_start,
                        arms_len,
                        ..
                    } = inst.data
                    {
                        for (pat, _) in air.get_match_arms(arms_start, arms_len) {
                            out.push(pat);
                        }
                    }
                }
            }
            out
        }

        fn assert_shape(a: &AirPattern, expected_tag: &str) {
            let got = match a {
                AirPattern::Wildcard => "Wildcard",
                AirPattern::Int(_) => "Int",
                AirPattern::Bool(_) => "Bool",
                AirPattern::EnumVariant { .. } => "EnumVariant",
                AirPattern::EnumUnitVariant { .. } => "EnumUnitVariant",
                AirPattern::EnumDataVariant { .. } => "EnumDataVariant",
                AirPattern::EnumStructVariant { .. } => "EnumStructVariant",
                AirPattern::Bind { .. } => "Bind",
                AirPattern::Tuple { .. } => "Tuple",
                AirPattern::Struct { .. } => "Struct",
            };
            assert_eq!(got, expected_tag, "pattern = {:?}", a);
        }

        #[test]
        fn wildcard_lowers_to_wildcard() {
            let output = compile_with_recursive("fn main() -> i32 { match 1 { _ => 0 } }");
            let arms = collect_match_arms(&output);
            assert_eq!(arms.len(), 1);
            assert_shape(&arms[0], "Wildcard");
        }

        #[test]
        fn int_literal_lowers_to_int() {
            let output = compile_with_recursive("fn main() -> i32 { match 1 { 1 => 1, _ => 0 } }");
            let arms = collect_match_arms(&output);
            assert_eq!(arms.len(), 2);
            assert!(matches!(arms[0], AirPattern::Int(1)));
            assert_shape(&arms[1], "Wildcard");
        }

        #[test]
        fn bool_lowers_to_bool() {
            let output =
                compile_with_recursive("fn main() -> i32 { match true { true => 1, false => 0 } }");
            let arms = collect_match_arms(&output);
            assert_eq!(arms.len(), 2);
            assert!(matches!(arms[0], AirPattern::Bool(true)));
            assert!(matches!(arms[1], AirPattern::Bool(false)));
        }

        #[test]
        fn unit_variant_path_lowers_to_enum_unit_variant() {
            let output = compile_with_recursive(
                "enum Color { Red, Green, Blue }
                 fn main() -> i32 {
                     let c = Color::Red;
                     match c {
                         Color::Red => 1,
                         Color::Green => 2,
                         Color::Blue => 3,
                     }
                 }",
            );
            let arms = collect_match_arms(&output);
            assert_eq!(arms.len(), 3);
            for a in &arms {
                assert_shape(a, "EnumUnitVariant");
            }
        }

        #[test]
        fn data_variant_bindings_lower_to_bind_leaves() {
            let output = compile_with_recursive(
                "enum Opt { Some(i32), None }
                 fn main() -> i32 {
                     let o = Opt::Some(5);
                     match o {
                         Opt::Some(x) => x,
                         Opt::None => 0,
                     }
                 }",
            );
            let arms = collect_match_arms(&output);
            assert_eq!(arms.len(), 2);
            match &arms[0] {
                AirPattern::EnumDataVariant { fields, .. } => {
                    assert_eq!(fields.len(), 1);
                    assert!(
                        matches!(&fields[0], AirPattern::Bind { inner: None, .. }),
                        "expected Bind leaf, got {:?}",
                        &fields[0]
                    );
                }
                other => panic!("expected EnumDataVariant, got {:?}", other),
            }
            assert_shape(&arms[1], "EnumUnitVariant");
        }

        #[test]
        fn data_variant_wildcard_binding_lowers_to_wildcard() {
            let output = compile_with_recursive(
                "enum Opt { Some(i32), None }
                 fn main() -> i32 {
                     let o = Opt::Some(5);
                     match o {
                         Opt::Some(_) => 1,
                         Opt::None => 0,
                     }
                 }",
            );
            let arms = collect_match_arms(&output);
            match &arms[0] {
                AirPattern::EnumDataVariant { fields, .. } => {
                    assert_eq!(fields.len(), 1);
                    assert!(matches!(&fields[0], AirPattern::Wildcard));
                }
                other => panic!("expected EnumDataVariant, got {:?}", other),
            }
        }

        #[test]
        fn data_variant_rest_expands_to_wildcards() {
            let output = compile_with_recursive(
                "enum Triple { T(i32, i32, i32) }
                 fn main() -> i32 {
                     let t = Triple::T(1, 2, 3);
                     match t {
                         Triple::T(x, ..) => x,
                     }
                 }",
            );
            let arms = collect_match_arms(&output);
            match &arms[0] {
                AirPattern::EnumDataVariant { fields, .. } => {
                    assert_eq!(fields.len(), 3, "rest should expand to fill arity");
                    assert!(matches!(&fields[0], AirPattern::Bind { .. }));
                    assert!(matches!(&fields[1], AirPattern::Wildcard));
                    assert!(matches!(&fields[2], AirPattern::Wildcard));
                }
                other => panic!("expected EnumDataVariant, got {:?}", other),
            }
        }

        #[test]
        fn struct_variant_lowers_to_enum_struct_variant_in_declaration_order() {
            let output = compile_with_recursive(
                "enum Shape { Circle { radius: i32 }, Square { side: i32 } }
                 fn main() -> i32 {
                     let s = Shape::Circle { radius: 5 };
                     match s {
                         Shape::Circle { radius } => radius,
                         Shape::Square { side } => side,
                     }
                 }",
            );
            let arms = collect_match_arms(&output);
            for a in &arms {
                assert_shape(a, "EnumStructVariant");
            }
            match &arms[0] {
                AirPattern::EnumStructVariant { fields, .. } => {
                    assert_eq!(fields.len(), 1);
                    assert_eq!(fields[0].0, 0, "radius is field 0");
                    assert!(matches!(&fields[0].1, AirPattern::Bind { .. }));
                }
                other => panic!("expected EnumStructVariant, got {:?}", other),
            }
        }

        #[test]
        fn struct_variant_rest_fills_unlisted_with_wildcard() {
            let output = compile_with_recursive(
                "enum Pt { Coord { x: i32, y: i32 } }
                 fn main() -> i32 {
                     let p = Pt::Coord { x: 1, y: 2 };
                     match p {
                         Pt::Coord { x, .. } => x,
                     }
                 }",
            );
            let arms = collect_match_arms(&output);
            match &arms[0] {
                AirPattern::EnumStructVariant { fields, .. } => {
                    assert_eq!(fields.len(), 2);
                    // x listed → Bind; y absent → Wildcard
                    assert!(matches!(&fields[0].1, AirPattern::Bind { .. }));
                    assert!(matches!(&fields[1].1, AirPattern::Wildcard));
                }
                other => panic!("expected EnumStructVariant, got {:?}", other),
            }
        }
    }
}
