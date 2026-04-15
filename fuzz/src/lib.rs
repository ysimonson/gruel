//! Structured input generators for cargo-fuzz targets.
//!
//! Uses the `arbitrary` crate to generate syntactically valid Gruel programs
//! and structured x86-64 instruction sequences. libFuzzer's coverage feedback
//! guides generation toward interesting inputs.

use arbitrary::{Arbitrary, Unstructured};
use gruel_codegen::x86_64::{LabelId, Operand, Reg, X86Inst, X86Mir};

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
            let params: Vec<String> =
                (0..num_params).map(|j| format!("p{}", j)).collect();
            let param_list: Vec<String> =
                params.iter().map(|p| format!("{}: i32", p)).collect();
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
            src.push_str(&format!(
                "struct Data {{\n{}\n}}\n\n",
                fields.join(",\n"),
            ));
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
fn gen_i32_expr(
    u: &mut Unstructured<'_>,
    vars: &[String],
    depth: u8,
) -> arbitrary::Result<String> {
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
            let val =
                gen_i32_expr_with_calls(u, vars, helpers, depth - 1)?;
            let mut inner_vars = vars.to_vec();
            inner_vars.push(inner_name.clone());
            let ret =
                gen_i32_expr_with_calls(u, &inner_vars, helpers, depth - 1)?;
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
// Codegen generator
// ---------------------------------------------------------------------------

/// An arbitrary x86-64 MIR program with properly allocated labels.
#[derive(Debug)]
pub struct ArbitraryX86Mir(pub X86Mir);

impl<'a> Arbitrary<'a> for ArbitraryX86Mir {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let mut mir = X86Mir::new();

        let num_labels: u8 = u.int_in_range(0..=8)?;
        let labels: Vec<LabelId> =
            (0..num_labels).map(|_| mir.alloc_label()).collect();

        let num_insts: u8 = u.int_in_range(1..=32)?;
        for _ in 0..num_insts {
            let inst = gen_x86_inst(u, &labels)?;
            mir.push(inst);
        }

        // Define all labels at the end so every forward reference resolves.
        for &label in &labels {
            mir.push(X86Inst::Label { id: label });
        }
        mir.push(X86Inst::Ret);

        Ok(ArbitraryX86Mir(mir))
    }
}

fn arb_reg(u: &mut Unstructured<'_>) -> arbitrary::Result<Reg> {
    // All GPRs except RSP/RBP (callee-saved / frame pointer).
    let regs = [
        Reg::Rax, Reg::Rcx, Reg::Rdx, Reg::Rbx,
        Reg::Rsi, Reg::Rdi, Reg::R8,  Reg::R9,
        Reg::R10, Reg::R11, Reg::R12, Reg::R13,
        Reg::R14, Reg::R15,
    ];
    Ok(*u.choose(&regs)?)
}

fn arb_op(u: &mut Unstructured<'_>) -> arbitrary::Result<Operand> {
    Ok(Operand::Physical(arb_reg(u)?))
}

fn arb_imm32(u: &mut Unstructured<'_>) -> arbitrary::Result<i32> {
    // Bias toward boundary values
    if u.ratio(1, 3)? {
        Ok(*u.choose(&[
            0i32, 1, -1, 127, -128, 255, 256, i32::MAX, i32::MIN,
        ])?)
    } else {
        u.arbitrary()
    }
}

fn arb_shift(u: &mut Unstructured<'_>, max: u8) -> arbitrary::Result<u8> {
    if u.ratio(1, 3)? {
        // Boundary values
        let bounds: Vec<u8> = vec![0, 1, max - 1];
        Ok(*u.choose(&bounds)?)
    } else {
        u.int_in_range(0..=max - 1)
    }
}

fn gen_x86_inst(
    u: &mut Unstructured<'_>,
    labels: &[LabelId],
) -> arbitrary::Result<X86Inst> {
    let has_labels = !labels.is_empty();
    let max = if has_labels { 30u8 } else { 20 };

    let dst = arb_op(u)?;
    let src = arb_op(u)?;
    let imm = arb_imm32(u)?;

    match u.int_in_range(0..=max - 1)? {
        0  => Ok(X86Inst::MovRI32 { dst, imm }),
        1  => Ok(X86Inst::MovRI64 { dst, imm: imm as i64 }),
        2  => Ok(X86Inst::MovRR { dst, src }),
        3  => Ok(X86Inst::AddRR { dst, src }),
        4  => Ok(X86Inst::AddRR64 { dst, src }),
        5  => Ok(X86Inst::SubRR { dst, src }),
        6  => Ok(X86Inst::SubRR64 { dst, src }),
        7  => Ok(X86Inst::AddRI { dst, imm }),
        8  => Ok(X86Inst::ImulRR { dst, src }),
        9  => Ok(X86Inst::Neg { dst }),
        10 => Ok(X86Inst::AndRR { dst, src }),
        11 => Ok(X86Inst::OrRR { dst, src }),
        12 => Ok(X86Inst::XorRR { dst, src }),
        13 => Ok(X86Inst::NotR { dst }),
        14 => Ok(X86Inst::ShlRI { dst, imm: arb_shift(u, 64)? }),
        15 => Ok(X86Inst::ShrRI { dst, imm: arb_shift(u, 64)? }),
        16 => Ok(X86Inst::SarRI { dst, imm: arb_shift(u, 64)? }),
        17 => Ok(X86Inst::CmpRR { src1: dst, src2: src }),
        18 => Ok(X86Inst::CmpRI { src: dst, imm }),
        19 => Ok(X86Inst::Ret),
        // Label / jump variants (only reachable when labels exist)
        20 => Ok(X86Inst::Label { id: *u.choose(labels)? }),
        21 => Ok(X86Inst::Jz { label: *u.choose(labels)? }),
        22 => Ok(X86Inst::Jnz { label: *u.choose(labels)? }),
        23 => Ok(X86Inst::Jo { label: *u.choose(labels)? }),
        24 => Ok(X86Inst::Jb { label: *u.choose(labels)? }),
        25 => Ok(X86Inst::Jae { label: *u.choose(labels)? }),
        26 => Ok(X86Inst::Jbe { label: *u.choose(labels)? }),
        27 => Ok(X86Inst::Jge { label: *u.choose(labels)? }),
        28 => Ok(X86Inst::Jle { label: *u.choose(labels)? }),
        29 => Ok(X86Inst::Jmp { label: *u.choose(labels)? }),
        _  => Ok(X86Inst::Ret),
    }
}
