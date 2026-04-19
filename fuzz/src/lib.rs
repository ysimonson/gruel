//! Structured input generators for cargo-fuzz targets.
//!
//! Uses the `arbitrary` crate to generate syntactically valid Gruel programs
//! and structured x86-64 instruction sequences. libFuzzer's coverage feedback
//! guides generation toward interesting inputs.

use arbitrary::{Arbitrary, Unstructured};

// ---------------------------------------------------------------------------
// Source-level generators
// ---------------------------------------------------------------------------

/// A syntactically and semantically valid Gruel program.
#[derive(Debug)]
pub struct GruelProgram(pub String);

/// A syntactically valid program that may contain semantic errors
/// (missing main, type mismatches, undefined variables, duplicates).
#[derive(Debug)]
pub struct MaybeInvalidProgram(pub String);

impl<'a> Arbitrary<'a> for GruelProgram {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let mut src = String::new();

        // Optional helper functions (0-3)
        let num_helpers: u8 = u.int_in_range(0..=3)?;
        let mut helpers = Vec::new();
        for i in 0..num_helpers {
            let name = format!("helper_{}", i);
            let num_params: u8 = u.int_in_range(0..=2)?;
            let params: Vec<String> = (0..num_params).map(|j| format!("p{}", j)).collect();
            let param_list: Vec<String> = params.iter().map(|p| format!("{}: i32", p)).collect();
            let body = gen_i32_expr(u, &params, 2)?;
            src.push_str(&format!(
                "fn {}({}) -> i32 {{ {} }}\n\n",
                name,
                param_list.join(", "),
                body,
            ));
            helpers.push((name, num_params));
        }

        // Optional struct
        if u.ratio(1, 4)? {
            let num_fields: u8 = u.int_in_range(1..=3)?;
            let fields: Vec<String> = (0..num_fields)
                .map(|i| format!("    f{}: i32", i))
                .collect();
            src.push_str(&format!("struct Data {{\n{}\n}}\n\n", fields.join(",\n"),));
        }

        // main function
        let body = gen_body(u, &helpers, 2)?;
        src.push_str(&format!("fn main() -> i32 {{\n{}}}\n", body));

        Ok(GruelProgram(src))
    }
}

impl<'a> Arbitrary<'a> for MaybeInvalidProgram {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let mut src = String::new();

        // Sometimes omit main entirely
        if u.ratio(3, 4)? {
            let body = gen_body(u, &[], 2)?;
            src.push_str(&format!("fn main() -> i32 {{\n{}}}\n", body));
        }

        // Sometimes add a duplicate main
        if u.ratio(1, 4)? {
            src.push_str("fn main() -> i32 { 0 }\n");
        }

        // Sometimes add type-mismatched function
        if u.ratio(1, 3)? {
            src.push_str("fn bad() -> i32 { true }\n");
        }

        // Sometimes add undefined variable usage
        if u.ratio(1, 3)? {
            src.push_str("fn undef() -> i32 { xyz_not_defined }\n");
        }

        // Sometimes add a function using an undeclared type
        if u.ratio(1, 4)? {
            src.push_str("fn wrong_type() -> Nonexistent { 0 }\n");
        }

        // Ensure non-empty
        if src.is_empty() {
            src.push_str("fn main() -> i32 { 0 }\n");
        }

        Ok(MaybeInvalidProgram(src))
    }
}

/// Generate a function body: a sequence of let-bindings followed by a return
/// expression.
fn gen_body(
    u: &mut Unstructured<'_>,
    helpers: &[(String, u8)],
    depth: u8,
) -> arbitrary::Result<String> {
    let mut out = String::new();
    let mut vars: Vec<String> = Vec::new();

    let num_lets: u8 = u.int_in_range(0..=5)?;
    for i in 0..num_lets {
        let name = format!("v{}", i);
        let expr = gen_i32_expr_with_calls(u, &vars, helpers, depth)?;
        out.push_str(&format!("    let {}: i32 = {};\n", name, expr));
        vars.push(name);
    }

    let ret = gen_i32_expr_with_calls(u, &vars, helpers, depth)?;
    out.push_str(&format!("    {}\n", ret));
    Ok(out)
}

/// Generate an i32-typed expression. Only uses constructs that are guaranteed
/// to type-check when all referenced variables are `i32`.
fn gen_i32_expr(u: &mut Unstructured<'_>, vars: &[String], depth: u8) -> arbitrary::Result<String> {
    gen_i32_expr_with_calls(u, vars, &[], depth)
}

fn gen_i32_expr_with_calls(
    u: &mut Unstructured<'_>,
    vars: &[String],
    helpers: &[(String, u8)],
    depth: u8,
) -> arbitrary::Result<String> {
    if depth == 0 {
        return gen_i32_leaf(u, vars);
    }

    match u.int_in_range(0u8..=7)? {
        // Literal
        0 => gen_i32_literal(u),
        // Variable
        1 if !vars.is_empty() => {
            let v = u.choose(vars)?;
            Ok(v.clone())
        }
        // Binary arithmetic
        1 | 2 => {
            let op = u.choose(&["+", "-", "*"])?;
            let lhs = gen_i32_expr_with_calls(u, vars, helpers, depth - 1)?;
            let rhs = gen_i32_expr_with_calls(u, vars, helpers, depth - 1)?;
            Ok(format!("({} {} {})", lhs, op, rhs))
        }
        // Unary negation
        3 => {
            let e = gen_i32_expr_with_calls(u, vars, helpers, depth - 1)?;
            Ok(format!("(-{})", e))
        }
        // If-else
        4 => {
            let cond = gen_bool_expr(u, vars, depth - 1)?;
            let then = gen_i32_expr_with_calls(u, vars, helpers, depth - 1)?;
            let else_ = gen_i32_expr_with_calls(u, vars, helpers, depth - 1)?;
            Ok(format!("if {} {{ {} }} else {{ {} }}", cond, then, else_))
        }
        // Block with inner let
        5 => {
            let inner_name = format!("blk{}", u.int_in_range(0u16..=999)?);
            let val = gen_i32_expr_with_calls(u, vars, helpers, depth - 1)?;
            let mut inner_vars = vars.to_vec();
            inner_vars.push(inner_name.clone());
            let ret = gen_i32_expr_with_calls(u, &inner_vars, helpers, depth - 1)?;
            Ok(format!("{{ let {}: i32 = {}; {} }}", inner_name, val, ret))
        }
        // Helper call
        6 if !helpers.is_empty() => {
            let (name, nparams) = u.choose(helpers)?;
            let args: Vec<String> = (0..*nparams)
                .map(|_| gen_i32_expr_with_calls(u, vars, helpers, depth - 1))
                .collect::<arbitrary::Result<_>>()?;
            Ok(format!("{}({})", name, args.join(", ")))
        }
        // Bitwise
        _ => {
            let op = u.choose(&["&", "|", "^"])?;
            let lhs = gen_i32_expr_with_calls(u, vars, helpers, depth - 1)?;
            let rhs = gen_i32_expr_with_calls(u, vars, helpers, depth - 1)?;
            Ok(format!("({} {} {})", lhs, op, rhs))
        }
    }
}

fn gen_i32_leaf(u: &mut Unstructured<'_>, vars: &[String]) -> arbitrary::Result<String> {
    if !vars.is_empty() && u.ratio(1, 2)? {
        let v = u.choose(vars)?;
        Ok(v.clone())
    } else {
        gen_i32_literal(u)
    }
}

fn gen_i32_literal(u: &mut Unstructured<'_>) -> arbitrary::Result<String> {
    // Mix of small common values and arbitrary i32s
    if u.ratio(1, 2)? {
        let small = u.choose(&[0i32, 1, -1, 2, 42, 100, 255, -128])?;
        Ok(small.to_string())
    } else {
        let n: i32 = u.arbitrary()?;
        Ok(n.to_string())
    }
}

/// Generate a bool-typed expression.
fn gen_bool_expr(
    u: &mut Unstructured<'_>,
    vars: &[String],
    depth: u8,
) -> arbitrary::Result<String> {
    if depth == 0 {
        let b: bool = u.arbitrary()?;
        return Ok(b.to_string());
    }

    match u.int_in_range(0u8..=3)? {
        // Literal
        0 => {
            let b: bool = u.arbitrary()?;
            Ok(b.to_string())
        }
        // Comparison of i32s
        1 => {
            let op = u.choose(&["==", "!=", "<", ">", "<=", ">="])?;
            let lhs = gen_i32_expr(u, vars, depth - 1)?;
            let rhs = gen_i32_expr(u, vars, depth - 1)?;
            Ok(format!("({} {} {})", lhs, op, rhs))
        }
        // Logical not
        2 => {
            let e = gen_bool_expr(u, vars, depth - 1)?;
            Ok(format!("(!{})", e))
        }
        // Logical binary
        _ => {
            let op = u.choose(&["&&", "||"])?;
            let lhs = gen_bool_expr(u, vars, depth - 1)?;
            let rhs = gen_bool_expr(u, vars, depth - 1)?;
            Ok(format!("({} {} {})", lhs, op, rhs))
        }
    }
}

// ---------------------------------------------------------------------------
// Comptime differential fuzzing generator
// ---------------------------------------------------------------------------

/// A program suitable for comptime/runtime differential comparison.
///
/// Generates programs using only constructs supported by both the comptime
/// interpreter and the runtime: i32 arithmetic, booleans, control flow,
/// `comptime_unroll for` with `@range`, `@typeInfo`/`@field`, and `@dbg`
/// for observable output. No I/O, strings, or non-deterministic operations.
///
/// The comptime path wraps the body in `comptime { ... }` with loops
/// pre-expanded into plain statements (since the comptime interpreter
/// doesn't handle `comptime_unroll for`). The runtime path uses the actual
/// `comptime_unroll for` syntax. Both should produce identical `@dbg` output.
#[derive(Debug)]
pub struct ComptimeProgram {
    /// Optional struct definition (top-level, emitted before fn main).
    struct_def: Option<ComptimeStructDef>,
    /// Body for the comptime path (loops manually expanded).
    comptime_body: String,
    /// Body for the runtime path (uses comptime_unroll for).
    runtime_body: String,
}

/// A simple struct with i32 fields, used for `@typeInfo`/`@field` fuzzing.
#[derive(Debug)]
struct ComptimeStructDef {
    name: String,
    fields: Vec<(String, i32)>,
}

impl ComptimeProgram {
    /// Get the comptime body for diagnostic output.
    pub fn body(&self) -> &str {
        &self.runtime_body
    }

    /// Generate the struct definition preamble, if any.
    fn struct_preamble(&self) -> String {
        match &self.struct_def {
            Some(sd) => {
                let fields: Vec<String> = sd
                    .fields
                    .iter()
                    .map(|(name, _)| format!("    {}: i32", name))
                    .collect();
                format!("struct {} {{\n{}\n}}\n\n", sd.name, fields.join(",\n"))
            }
            None => String::new(),
        }
    }

    /// Wrap the body for comptime evaluation.
    /// Loops are pre-expanded; `@dbg` output goes to the compiler buffer.
    pub fn comptime_source(&self) -> String {
        format!(
            "{}fn main() -> i32 {{\n    comptime {{\n{}\n    }};\n    0\n}}",
            self.struct_preamble(),
            self.comptime_body
        )
    }

    /// Wrap the body for runtime execution.
    /// Uses `comptime_unroll for`; `@dbg` output goes to stdout.
    pub fn runtime_source(&self) -> String {
        format!(
            "{}fn main() -> i32 {{\n{}\n    0\n}}",
            self.struct_preamble(),
            self.runtime_body
        )
    }
}

impl<'a> Arbitrary<'a> for ComptimeProgram {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let mut comptime_body = String::new();
        let mut runtime_body = String::new();
        let mut vars: Vec<String> = Vec::new();

        // Optionally generate a struct for @typeInfo/@field testing
        let struct_def = if u.ratio(1, 3)? {
            let num_fields: u8 = u.int_in_range(1..=4)?;
            let fields: Result<Vec<(String, i32)>, _> = (0..num_fields)
                .map(|i| {
                    let val = *u.choose(&[0i32, 1, -1, 5, 10, 42, -100])?;
                    Ok((format!("f{}", i), val))
                })
                .collect();
            let fields = fields?;
            Some(ComptimeStructDef {
                name: "FuzzStruct".to_string(),
                fields,
            })
        } else {
            None
        };

        // Generate 1-8 statements, mixing let+@dbg and comptime_unroll
        let num_stmts: u8 = u.int_in_range(1..=8)?;
        for i in 0..num_stmts {
            match u.int_in_range(0u8..=2)? {
                // Regular let + @dbg (same in both paths)
                0 => {
                    let name = format!("ct{}", i);
                    let expr = gen_comptime_i32_expr(u, &vars, 2)?;
                    let let_stmt = format!("    let {}: i32 = {};\n", name, expr);
                    let dbg_stmt = format!("    @dbg({});\n", name);
                    comptime_body.push_str(&let_stmt);
                    comptime_body.push_str(&dbg_stmt);
                    runtime_body.push_str(&let_stmt);
                    runtime_body.push_str(&dbg_stmt);
                    vars.push(name);
                }
                // comptime_unroll for with @range
                1 => {
                    let range_end: u8 = u.int_in_range(0..=4)?;
                    let loop_var = format!("idx{}", i);

                    // Build the inner expression template (uses loop_var placeholder)
                    let inner_template = if vars.is_empty() {
                        "{}".to_string()
                    } else {
                        let var = u.choose(&vars)?;
                        format!("({} + {{}})", var)
                    };

                    // Runtime: actual comptime_unroll for
                    runtime_body.push_str(&format!(
                        "    comptime_unroll for {} in comptime {{ @range({}) }} {{\n",
                        loop_var, range_end
                    ));
                    let runtime_expr = inner_template.replace("{}", &loop_var);
                    runtime_body.push_str(&format!("        @dbg({});\n", runtime_expr));
                    runtime_body.push_str("    }\n");

                    // Comptime: manually expanded iterations
                    for idx in 0..range_end {
                        let expanded_expr = inner_template.replace("{}", &idx.to_string());
                        comptime_body.push_str(&format!("    @dbg({});\n", expanded_expr));
                    }
                }
                // comptime_unroll for with @typeInfo/@field (if struct exists)
                _ => {
                    if let Some(ref sd) = struct_def {
                        let inst_name = format!("s{}", i);
                        let field_inits: Vec<String> = sd
                            .fields
                            .iter()
                            .map(|(name, val)| format!("{}: {}", name, val))
                            .collect();
                        let let_stmt = format!(
                            "    let {} = {} {{ {} }};\n",
                            inst_name,
                            sd.name,
                            field_inits.join(", ")
                        );

                        // Runtime: comptime_unroll for with @typeInfo/@field
                        runtime_body.push_str(&let_stmt);
                        runtime_body.push_str(&format!(
                            "    comptime_unroll for fld in comptime {{ @typeInfo({}).fields }} {{\n",
                            sd.name
                        ));
                        runtime_body
                            .push_str(&format!("        @dbg(@field({}, fld.name));\n", inst_name));
                        runtime_body.push_str("    }\n");

                        // Comptime: manually expanded field accesses
                        comptime_body.push_str(&let_stmt);
                        for (field_name, _) in &sd.fields {
                            comptime_body
                                .push_str(&format!("    @dbg({}.{});\n", inst_name, field_name));
                        }
                    } else {
                        // No struct available — fall back to let + @dbg
                        let name = format!("ct{}", i);
                        let expr = gen_comptime_i32_expr(u, &vars, 2)?;
                        let let_stmt = format!("    let {}: i32 = {};\n", name, expr);
                        let dbg_stmt = format!("    @dbg({});\n", name);
                        comptime_body.push_str(&let_stmt);
                        comptime_body.push_str(&dbg_stmt);
                        runtime_body.push_str(&let_stmt);
                        runtime_body.push_str(&dbg_stmt);
                        vars.push(name);
                    }
                }
            }
        }

        // Optionally dbg a boolean expression (same in both paths)
        if u.ratio(1, 3)? {
            let bool_expr = gen_bool_expr(u, &vars, 1)?;
            let dbg_stmt = format!("    @dbg({});\n", bool_expr);
            comptime_body.push_str(&dbg_stmt);
            runtime_body.push_str(&dbg_stmt);
        }

        Ok(ComptimeProgram {
            struct_def,
            comptime_body,
            runtime_body,
        })
    }
}

/// Generate an i32 expression valid in both comptime and runtime contexts.
/// Only uses: literals, variables, arithmetic (+, -, *, &, |, ^), negation,
/// if-else, and blocks with inner lets.
fn gen_comptime_i32_expr(
    u: &mut Unstructured<'_>,
    vars: &[String],
    depth: u8,
) -> arbitrary::Result<String> {
    if depth == 0 {
        return gen_comptime_i32_leaf(u, vars);
    }

    match u.int_in_range(0u8..=5)? {
        // Literal
        0 => gen_comptime_i32_literal(u),
        // Variable
        1 if !vars.is_empty() => {
            let v = u.choose(vars)?;
            Ok(v.clone())
        }
        // Binary arithmetic (avoid division to prevent div-by-zero panics)
        1 | 2 => {
            let op = u.choose(&["+", "-", "*", "&", "|", "^"])?;
            let lhs = gen_comptime_i32_expr(u, vars, depth - 1)?;
            let rhs = gen_comptime_i32_expr(u, vars, depth - 1)?;
            Ok(format!("({} {} {})", lhs, op, rhs))
        }
        // Unary negation
        3 => {
            let e = gen_comptime_i32_expr(u, vars, depth - 1)?;
            Ok(format!("(-{})", e))
        }
        // If-else
        4 => {
            let cond = gen_bool_expr(u, vars, depth - 1)?;
            let then = gen_comptime_i32_expr(u, vars, depth - 1)?;
            let else_ = gen_comptime_i32_expr(u, vars, depth - 1)?;
            Ok(format!("if {} {{ {} }} else {{ {} }}", cond, then, else_))
        }
        // Block with inner let
        _ => {
            let inner_name = format!("inner{}", u.int_in_range(0u16..=999)?);
            let val = gen_comptime_i32_expr(u, vars, depth - 1)?;
            let mut inner_vars = vars.to_vec();
            inner_vars.push(inner_name.clone());
            let ret = gen_comptime_i32_expr(u, &inner_vars, depth - 1)?;
            Ok(format!("{{ let {}: i32 = {}; {} }}", inner_name, val, ret))
        }
    }
}

fn gen_comptime_i32_leaf(u: &mut Unstructured<'_>, vars: &[String]) -> arbitrary::Result<String> {
    if !vars.is_empty() && u.ratio(1, 2)? {
        let v = u.choose(vars)?;
        Ok(v.clone())
    } else {
        gen_comptime_i32_literal(u)
    }
}

fn gen_comptime_i32_literal(u: &mut Unstructured<'_>) -> arbitrary::Result<String> {
    // Use small values to avoid arithmetic overflow panics
    let small = u.choose(&[0i32, 1, -1, 2, -2, 3, 5, 7, 10, 42, 100, -100])?;
    Ok(small.to_string())
}
