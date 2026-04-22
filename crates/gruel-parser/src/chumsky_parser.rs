//! Chumsky-based parser for the Gruel programming language.
//!
//! This module provides a parser implementation using chumsky combinators
//! with Pratt parsing for expression precedence.

use crate::ast::{
    AnonStructField, ArgMode, ArrayLitExpr, AssignStatement, AssignTarget, AssocFnCallExpr, Ast,
    BinaryExpr, BinaryOp, BlockExpr, BoolLit, BreakExpr, CallArg, CallExpr, CheckedBlockExpr,
    ComptimeBlockExpr, ComptimeUnrollForExpr, ConstDecl, ContinueExpr, Directive, DirectiveArg,
    Directives, DropFn, EnumDecl, EnumStructLitExpr, EnumVariant, Expr, FieldDecl, FieldExpr,
    FieldInit, FieldPattern, FloatLit, ForExpr, Function, Ident, IfExpr, IndexExpr, IntLit,
    IntrinsicArg, IntrinsicCallExpr, Item, LetStatement, LoopExpr, MatchArm, MatchExpr, Method,
    MethodCallExpr, NegIntLit, Param, ParamMode, ParenExpr, PathExpr, PathPattern, Pattern,
    ReturnExpr, SelfExpr, SelfParam, Statement, StringLit, StructDecl, StructLitExpr,
    TupleElemPattern, TupleExpr, TupleIndexExpr, TypeExpr, TypeLitExpr, UnaryExpr, UnaryOp,
    UnitLit, Visibility, WhileExpr,
};
use chumsky::input::{Input as ChumskyInput, MapExtra, Stream, ValueInput};
use chumsky::prelude::*;
use chumsky::recovery::via_parser;
use chumsky::recursive::Direct;
use gruel_error::{
    CompileError, CompileErrors, ErrorKind, MultiErrorResult, PreviewFeature, PreviewFeatures,
};
use gruel_lexer::TokenKind;
use gruel_span::{FileId, Span};
use lasso::{Spur, ThreadedRodeo};
use std::borrow::Cow;

use chumsky::extra::SimpleState;

/// Pre-interned symbols for primitive type names and special keywords.
/// These are interned once when the parser is created and reused for all parsing.
#[derive(Clone, Copy)]
pub struct PrimitiveTypeSpurs {
    pub i8: Spur,
    pub i16: Spur,
    pub i32: Spur,
    pub i64: Spur,
    pub isize: Spur,
    pub u8: Spur,
    pub u16: Spur,
    pub u32: Spur,
    pub u64: Spur,
    pub usize: Spur,
    pub f16: Spur,
    pub f32: Spur,
    pub f64: Spur,
    pub bool: Spur,
    /// Self type keyword - used in methods to refer to the containing struct type
    pub self_type: Spur,
}

impl PrimitiveTypeSpurs {
    /// Create a new set of primitive type symbols by interning them.
    pub fn new(interner: &mut ThreadedRodeo) -> Self {
        Self {
            i8: interner.get_or_intern("i8"),
            i16: interner.get_or_intern("i16"),
            i32: interner.get_or_intern("i32"),
            i64: interner.get_or_intern("i64"),
            isize: interner.get_or_intern("isize"),
            u8: interner.get_or_intern("u8"),
            u16: interner.get_or_intern("u16"),
            u32: interner.get_or_intern("u32"),
            u64: interner.get_or_intern("u64"),
            usize: interner.get_or_intern("usize"),
            f16: interner.get_or_intern("f16"),
            f32: interner.get_or_intern("f32"),
            f64: interner.get_or_intern("f64"),
            bool: interner.get_or_intern("bool"),
            self_type: interner.get_or_intern("Self"),
        }
    }
}

/// Parser state containing primitive type symbols and file ID.
///
/// This struct holds mutable state needed during parsing:
/// - Pre-interned primitive type symbols for efficient lookup
/// - The FileId for the current file being parsed (for multi-file compilation)
#[derive(Clone, Copy)]
pub struct ParserState {
    /// Pre-interned primitive type symbols.
    pub syms: PrimitiveTypeSpurs,
    /// The file ID for spans in this file.
    pub file_id: FileId,
}

impl ParserState {
    /// Create a new parser state with the given symbols and file ID.
    pub fn new(syms: PrimitiveTypeSpurs, file_id: FileId) -> Self {
        Self { syms, file_id }
    }
}

/// Type alias for parser extras that carries parser state.
/// This replaces the previous thread-local approach with compile-time safe state passing.
type ParserExtras<'src> = extra::Full<Rich<'src, TokenKind>, SimpleState<ParserState>, ()>;

/// Type-erased parser to keep symbol names short. Boxing at each function boundary
/// prevents monomorphized type chains from growing exponentially in length.
type GruelParser<'src, I, O> = Boxed<'src, 'src, I, O, ParserExtras<'src>>;

/// Convert a `usize` offset to `u32`, asserting it fits in debug builds.
///
/// # Panics
///
/// In debug builds, panics if `offset` exceeds `u32::MAX`.
/// This would only happen for source files larger than 4GB.
#[inline]
fn offset_to_u32(offset: usize) -> u32 {
    debug_assert!(
        offset <= u32::MAX as usize,
        "offset {} exceeds u32::MAX (source file too large)",
        offset
    );
    offset as u32
}

/// Convert chumsky SimpleSpan to gruel_span::Span with a specific file ID.
///
/// # Panics
///
/// In debug builds, panics if `span.start` or `span.end` exceeds `u32::MAX`.
/// This would only happen for source files larger than 4GB.
fn to_gruel_span_with_file(span: SimpleSpan, file_id: FileId) -> Span {
    Span::with_file(file_id, offset_to_u32(span.start), offset_to_u32(span.end))
}

/// Convert chumsky SimpleSpan to gruel_span::Span using the default file ID.
/// Only used for error conversion where we don't have access to the parser state.
fn to_gruel_span(span: SimpleSpan) -> Span {
    Span::new(offset_to_u32(span.start), offset_to_u32(span.end))
}

/// Extract a Span with file ID from the parser extra.
/// This is the primary way to create spans during parsing.
#[inline]
fn span_from_extra<'src, I>(e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>) -> Span
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let file_id = e.state().0.file_id;
    to_gruel_span_with_file(e.span(), file_id)
}

/// Parser that produces Ident from identifier tokens
fn ident_parser<'src, I>() -> GruelParser<'src, I, Ident>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    select! {
        TokenKind::Ident(name) = e => Ident {
            name,
            span: span_from_extra(e),
        },
    }
    .boxed()
}

/// Parser for primitive type keywords: i8, i16, i32, i64, u8, u16, u32, u64, bool
/// These are reserved keywords that cannot be used as identifiers.
fn primitive_type_parser<'src, I>() -> GruelParser<'src, I, TypeExpr>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Access primitive type symbols from parser state via e.state().0
    // SimpleState<T> wraps T in a .0 field
    // Type annotation on closure parameter is needed to help Rust infer the Extra type
    let i8_parser =
        just(TokenKind::I8).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.i8,
                span: span_from_extra(e),
            })
        });
    let i16_parser =
        just(TokenKind::I16).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.i16,
                span: span_from_extra(e),
            })
        });
    let i32_parser =
        just(TokenKind::I32).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.i32,
                span: span_from_extra(e),
            })
        });
    let i64_parser =
        just(TokenKind::I64).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.i64,
                span: span_from_extra(e),
            })
        });
    let u8_parser =
        just(TokenKind::U8).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.u8,
                span: span_from_extra(e),
            })
        });
    let u16_parser =
        just(TokenKind::U16).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.u16,
                span: span_from_extra(e),
            })
        });
    let u32_parser =
        just(TokenKind::U32).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.u32,
                span: span_from_extra(e),
            })
        });
    let u64_parser =
        just(TokenKind::U64).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.u64,
                span: span_from_extra(e),
            })
        });
    let isize_parser =
        just(TokenKind::Isize).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.isize,
                span: span_from_extra(e),
            })
        });
    let usize_parser =
        just(TokenKind::Usize).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.usize,
                span: span_from_extra(e),
            })
        });
    let f16_parser =
        just(TokenKind::F16).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.f16,
                span: span_from_extra(e),
            })
        });
    let f32_parser =
        just(TokenKind::F32).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.f32,
                span: span_from_extra(e),
            })
        });
    let f64_parser =
        just(TokenKind::F64).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.f64,
                span: span_from_extra(e),
            })
        });
    let bool_parser =
        just(TokenKind::Bool).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.bool,
                span: span_from_extra(e),
            })
        });

    choice((
        i8_parser.boxed(),
        i16_parser.boxed(),
        i32_parser.boxed(),
        i64_parser.boxed(),
        isize_parser.boxed(),
        u8_parser.boxed(),
        u16_parser.boxed(),
        u32_parser.boxed(),
        u64_parser.boxed(),
        usize_parser.boxed(),
        f16_parser.boxed(),
        f32_parser.boxed(),
        f64_parser.boxed(),
        bool_parser.boxed(),
    ))
    .boxed()
}

/// Parser for type expressions: primitive types (i32, bool, etc.), () for unit, ! for never, or [T; N] for arrays
fn type_parser<'src, I>() -> GruelParser<'src, I, TypeExpr>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    recursive(
        move |ty: Recursive<Direct<'src, 'src, I, TypeExpr, ParserExtras<'src>>>| {
            // Unit type: ()
            let unit_type = just(TokenKind::LParen)
                .then(just(TokenKind::RParen))
                .map_with(|_, e| TypeExpr::Unit(span_from_extra(e)));

            // Never type: !
            let never_type =
                just(TokenKind::Bang).map_with(|_, e| TypeExpr::Never(span_from_extra(e)));

            // Array type: [T; N]
            let array_type = just(TokenKind::LBracket)
                .ignore_then(ty.clone())
                .then_ignore(just(TokenKind::Semi))
                .then(select! {
                    TokenKind::Int(n) => n,
                })
                .then_ignore(just(TokenKind::RBracket))
                .map_with(|(element, length), e| TypeExpr::Array {
                    element: Box::new(element),
                    length,
                    span: span_from_extra(e),
                });

            // Anonymous struct type: struct { field: Type, ... }
            // Used in comptime type construction
            let anon_struct_field: GruelParser<'src, I, AnonStructField> = ident_parser()
                .then_ignore(just(TokenKind::Colon))
                .then(ty.clone())
                .map_with(|(name, field_ty), e| AnonStructField {
                    name,
                    ty: field_ty,
                    span: span_from_extra(e),
                })
                .boxed();

            let anon_struct_fields: GruelParser<'src, I, Vec<AnonStructField>> = anon_struct_field
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .boxed();

            let anon_struct_type = just(TokenKind::Struct)
                .ignore_then(just(TokenKind::LBrace))
                .ignore_then(anon_struct_fields)
                .then_ignore(just(TokenKind::RBrace))
                .map_with(|fields, e| TypeExpr::AnonymousStruct {
                    fields,
                    methods: vec![],
                    span: span_from_extra(e),
                });

            // Pointer type: ptr const T or ptr mut T
            let ptr_const_type = just(TokenKind::Ptr)
                .ignore_then(just(TokenKind::Const))
                .ignore_then(ty.clone())
                .map_with(|pointee, e| TypeExpr::PointerConst {
                    pointee: Box::new(pointee),
                    span: span_from_extra(e),
                });

            let ptr_mut_type = just(TokenKind::Ptr)
                .ignore_then(just(TokenKind::Mut))
                .ignore_then(ty.clone())
                .map_with(|pointee, e| TypeExpr::PointerMut {
                    pointee: Box::new(pointee),
                    span: span_from_extra(e),
                });

            // Named type: user-defined types like MyStruct
            let named_type = ident_parser().map(TypeExpr::Named);

            // Self type: Self keyword used in methods to refer to the containing struct
            let self_type = just(TokenKind::SelfType).map_with(|_, e| {
                let span = span_from_extra(e);
                TypeExpr::Named(Ident {
                    name: e.state().syms.self_type,
                    span,
                })
            });

            // Tuple type: (T,) for 1-tuples, (T, U) or (T, U,) for 2+-tuples.
            // Must be tried after unit_type (which matches `()`) but before named_type.
            // A 1-tuple requires a trailing comma to distinguish it from parenthesised
            // types (not currently supported). We parse: LParen <ty> Comma (<ty>,)* RParen.
            let tuple_type = just(TokenKind::LParen)
                .ignore_then(ty.clone())
                .then_ignore(just(TokenKind::Comma))
                .then(
                    ty.clone()
                        .separated_by(just(TokenKind::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>(),
                )
                .then_ignore(just(TokenKind::RParen))
                .map_with(|(first, rest), e| {
                    let mut elems = Vec::with_capacity(1 + rest.len());
                    elems.push(first);
                    elems.extend(rest);
                    TypeExpr::Tuple {
                        elems,
                        span: span_from_extra(e),
                    }
                });

            // NOTE: Split into sub-groups to keep Choice<tuple> symbol length < 4K.
            // 9 elements of Boxed<I,TypeExpr,E> in one tuple would produce ~5K symbols on macOS.
            let types_a: GruelParser<'src, I, TypeExpr> = choice((
                unit_type.boxed(),
                never_type.boxed(),
                array_type.boxed(),
                anon_struct_type.boxed(),
                ptr_const_type.boxed(),
            ))
            .boxed();
            let types_b: GruelParser<'src, I, TypeExpr> = choice((
                ptr_mut_type.boxed(),
                primitive_type_parser().boxed(),
                self_type.boxed(),
                tuple_type.boxed(),
                named_type.boxed(),
            ))
            .boxed();
            choice((types_a, types_b)).boxed()
        },
    )
    .boxed()
}

/// Parser for parameter mode: inout, borrow, or comptime
fn param_mode_parser<'src, I>() -> GruelParser<'src, I, ParamMode>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        just(TokenKind::Inout).to(ParamMode::Inout).boxed(),
        just(TokenKind::Borrow).to(ParamMode::Borrow).boxed(),
        just(TokenKind::Comptime).to(ParamMode::Comptime).boxed(),
    ))
    .boxed()
}

/// Parser for function parameters: [comptime] [inout|borrow] name: type
fn param_parser<'src, I>() -> GruelParser<'src, I, Param>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    just(TokenKind::Comptime)
        .or_not()
        .then(param_mode_parser().or_not())
        .then(ident_parser())
        .then_ignore(just(TokenKind::Colon))
        .then(type_parser())
        .map_with(|(((is_comptime, mode), name), ty), e| Param {
            is_comptime: is_comptime.is_some(),
            mode: mode.unwrap_or(ParamMode::Normal),
            name,
            ty,
            span: span_from_extra(e),
        })
        .boxed()
}

/// Parser for struct field declarations: name: type
fn field_decl_parser<'src, I>() -> GruelParser<'src, I, FieldDecl>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(type_parser())
        .map_with(|(name, ty), e| FieldDecl {
            name,
            ty,
            span: span_from_extra(e),
        })
        .boxed()
}

/// Parser for comma-separated struct field declarations
fn field_decls_parser<'src, I>() -> GruelParser<'src, I, Vec<FieldDecl>>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    field_decl_parser()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .boxed()
}

/// Parser for comma-separated parameters
fn params_parser<'src, I>() -> GruelParser<'src, I, Vec<Param>>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    param_parser()
        .separated_by(just(TokenKind::Comma))
        .collect::<Vec<_>>()
        .boxed()
}

/// Parser for a single directive: @name or @name(arg1, arg2, ...)
fn directive_parser<'src, I>() -> GruelParser<'src, I, Directive>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    just(TokenKind::At)
        .ignore_then(ident_parser())
        .then(
            ident_parser()
                .map(DirectiveArg::Ident)
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
                .or_not(),
        )
        .map_with(|(name, args), e| Directive {
            name,
            args: args.unwrap_or_default(),
            span: span_from_extra(e),
        })
        .boxed()
}

/// Parser for zero or more directives
fn directives_parser<'src, I>() -> GruelParser<'src, I, Directives>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Chumsky's collect() requires its Container trait, which SmallVec doesn't implement.
    // So we collect to Vec first, then convert. The overhead is minimal since most
    // items have 0-1 directives (Vec is cheap for empty/small collections).
    directive_parser()
        .repeated()
        .collect::<Vec<_>>()
        .map(|v| v.into_iter().collect())
        .boxed()
}

/// Parser for argument mode: inout or borrow
fn arg_mode_parser<'src, I>() -> GruelParser<'src, I, ArgMode>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        just(TokenKind::Inout).to(ArgMode::Inout).boxed(),
        just(TokenKind::Borrow).to(ArgMode::Borrow).boxed(),
    ))
    .boxed()
}

/// Parser for a single call argument: [inout|borrow] expr
fn call_arg_parser<'src, I>(expr: GruelParser<'src, I, Expr>) -> GruelParser<'src, I, CallArg>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    arg_mode_parser()
        .or_not()
        .then(expr)
        .map_with(|(mode, expr), e| CallArg {
            mode: mode.unwrap_or(ArgMode::Normal),
            expr,
            span: span_from_extra(e),
        })
        .boxed()
}

/// Parser for comma-separated call arguments with optional inout
fn call_args_parser<'src, I>(expr: GruelParser<'src, I, Expr>) -> GruelParser<'src, I, Vec<CallArg>>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    call_arg_parser(expr)
        .separated_by(just(TokenKind::Comma))
        .collect::<Vec<_>>()
        .boxed()
}

/// Parser for comma-separated expression arguments (for contexts that don't support inout)
fn args_parser<'src, I>(expr: GruelParser<'src, I, Expr>) -> GruelParser<'src, I, Vec<Expr>>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    expr.separated_by(just(TokenKind::Comma))
        .collect::<Vec<_>>()
        .boxed()
}

/// Parser for struct field initializers: name: expr
fn field_init_parser<'src, I>(expr: GruelParser<'src, I, Expr>) -> GruelParser<'src, I, FieldInit>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Field init with explicit value: `name: expr`
    let explicit = ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(expr)
        .map_with(|(name, value), e| FieldInit {
            name,
            value: Box::new(value),
            span: span_from_extra(e),
        });

    // Field init shorthand: `name` means `name: name`
    let shorthand = ident_parser().map_with(|name, e| FieldInit {
        value: Box::new(Expr::Ident(name)),
        name,
        span: span_from_extra(e),
    });

    choice((explicit, shorthand)).boxed()
}

/// Parser for comma-separated field initializers
fn field_inits_parser<'src, I>(
    expr: GruelParser<'src, I, Expr>,
) -> GruelParser<'src, I, Vec<FieldInit>>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    field_init_parser(expr)
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .boxed()
}

/// Helper to create binary expression
fn make_binary(left: Expr, op: BinaryOp, right: Expr) -> Expr {
    let span = Span::new(left.span().start, right.span().end);
    Expr::Binary(BinaryExpr {
        left: Box::new(left),
        op,
        right: Box::new(right),
        span,
    })
}

/// Helper to create unary expression
fn make_unary(op: UnaryOp, operand: Expr, op_span: SimpleSpan) -> Expr {
    let span = Span::new(offset_to_u32(op_span.start), operand.span().end);
    Expr::Unary(UnaryExpr {
        op,
        operand: Box::new(operand),
        span,
    })
}

/// Expression parser using manual precedence climbing.
///
/// Operator precedence (tightest binding first):
///   unary prefix: -, !, ~
///   multiplicative: *, /, %
///   additive: +, -
///   shift: <<, >>
///   comparison: ==, !=, <, >, <=, >=
///   bitwise AND: &
///   bitwise XOR: ^
///   bitwise OR: |
///   logical AND: &&
///   logical OR: ||  (loosest binding)
///
/// Each level is immediately `.boxed()` to keep the Rc'd type short and avoid
/// the huge drop-glue symbols that Chumsky's Pratt parser would generate (the
/// Pratt operator-tuple type embeds `I` dozens of times).
fn expr_parser<'src, I>() -> GruelParser<'src, I, Expr>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    recursive(
        |expr: Recursive<Direct<'src, 'src, I, Expr, ParserExtras<'src>>>| {
            // Atom parser – primary expressions (highest precedence).
            let atom: GruelParser<I, Expr> = atom_parser(expr.clone().boxed());

            // Unary prefix operators: -, !, ~  (right-associative; stack and apply inside-out)
            let unary: GruelParser<I, Expr> = {
                let prefix_op: GruelParser<I, (UnaryOp, SimpleSpan)> = choice((
                    just(TokenKind::Minus)
                        .map_with(|_, e| (UnaryOp::Neg, e.span()))
                        .boxed(),
                    just(TokenKind::Bang)
                        .map_with(|_, e| (UnaryOp::Not, e.span()))
                        .boxed(),
                    just(TokenKind::Tilde)
                        .map_with(|_, e| (UnaryOp::BitNot, e.span()))
                        .boxed(),
                ))
                .boxed();
                prefix_op
                    .repeated()
                    .collect::<Vec<_>>()
                    .then(atom.clone())
                    .map(|(mut ops, mut rhs)| {
                        // Apply operators from innermost (rightmost) outward.
                        ops.reverse();
                        for (op, span) in ops {
                            rhs = make_unary(op, rhs, span);
                        }
                        rhs
                    })
                    .boxed()
            };

            // Helper macro: build one left-associative binary level.
            // Each call boxes the result, keeping the drop-glue type short.
            macro_rules! left_binary {
                ($prev:expr, $op_parser:expr) => {{
                    let prev: GruelParser<I, Expr> = $prev;
                    let op: GruelParser<I, BinaryOp> = $op_parser;
                    prev.clone()
                        .foldl(op.then(prev).repeated(), |l, (op, r)| make_binary(l, op, r))
                        .boxed()
                }};
            }

            // Multiplicative: *, /, %
            let multiplicative: GruelParser<I, Expr> = left_binary!(
                unary,
                choice((
                    just(TokenKind::Star).to(BinaryOp::Mul).boxed(),
                    just(TokenKind::Slash).to(BinaryOp::Div).boxed(),
                    just(TokenKind::Percent).to(BinaryOp::Mod).boxed(),
                ))
                .boxed()
            );

            // Additive: +, -
            let additive: GruelParser<I, Expr> = left_binary!(
                multiplicative,
                choice((
                    just(TokenKind::Plus).to(BinaryOp::Add).boxed(),
                    just(TokenKind::Minus).to(BinaryOp::Sub).boxed(),
                ))
                .boxed()
            );

            // Shift: <<, >>
            let shift: GruelParser<I, Expr> = left_binary!(
                additive,
                choice((
                    just(TokenKind::LtLt).to(BinaryOp::Shl).boxed(),
                    just(TokenKind::GtGt).to(BinaryOp::Shr).boxed(),
                ))
                .boxed()
            );

            // Comparison: ==, !=, <, >, <=, >=
            let comparison: GruelParser<I, Expr> = left_binary!(
                shift,
                choice((
                    just(TokenKind::EqEq).to(BinaryOp::Eq).boxed(),
                    just(TokenKind::BangEq).to(BinaryOp::Ne).boxed(),
                    just(TokenKind::Lt).to(BinaryOp::Lt).boxed(),
                    just(TokenKind::Gt).to(BinaryOp::Gt).boxed(),
                    just(TokenKind::LtEq).to(BinaryOp::Le).boxed(),
                    just(TokenKind::GtEq).to(BinaryOp::Ge).boxed(),
                ))
                .boxed()
            );

            // Bitwise AND: &
            let bitwise_and: GruelParser<I, Expr> = left_binary!(
                comparison,
                just(TokenKind::Amp).to(BinaryOp::BitAnd).boxed()
            );

            // Bitwise XOR: ^
            let bitwise_xor: GruelParser<I, Expr> = left_binary!(
                bitwise_and,
                just(TokenKind::Caret).to(BinaryOp::BitXor).boxed()
            );

            // Bitwise OR: |
            let bitwise_or: GruelParser<I, Expr> = left_binary!(
                bitwise_xor,
                just(TokenKind::Pipe).to(BinaryOp::BitOr).boxed()
            );

            // Logical AND: &&
            let logical_and: GruelParser<I, Expr> = left_binary!(
                bitwise_or,
                just(TokenKind::AmpAmp).to(BinaryOp::And).boxed()
            );

            // Logical OR: || (lowest binary precedence)
            let logical_or: GruelParser<I, Expr> = left_binary!(
                logical_and,
                just(TokenKind::PipePipe).to(BinaryOp::Or).boxed()
            );

            logical_or
        },
    )
    .boxed()
}

/// Parser for patterns in match arms
fn pattern_parser<'src, I>() -> GruelParser<'src, I, Pattern>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    recursive(
        |pat: Recursive<Direct<'src, 'src, _, Pattern, ParserExtras<'src>>>| {
            // Wildcard pattern: _
            let wildcard =
                just(TokenKind::Underscore).map_with(|_, e| Pattern::Wildcard(span_from_extra(e)));

            // Integer literal pattern (positive or zero)
            let int_pat = select! {
                TokenKind::Int(n) = e => Pattern::Int(IntLit {
                    value: n,
                    span: span_from_extra(e),
                }),
            };

            // Negative integer literal pattern: - followed by integer
            let neg_int_pat = just(TokenKind::Minus)
                .then(select! { TokenKind::Int(n) => n })
                .map_with(|(_, n), e| {
                    Pattern::NegInt(NegIntLit {
                        value: n,
                        span: span_from_extra(e),
                    })
                });

            // Boolean literal patterns
            let bool_true = select! {
                TokenKind::True = e => Pattern::Bool(BoolLit {
                    value: true,
                    span: span_from_extra(e),
                }),
            };

            let bool_false = select! {
                TokenKind::False = e => Pattern::Bool(BoolLit {
                    value: false,
                    span: span_from_extra(e),
                }),
            };

            // A rest pattern `..`: two adjacent Dot tokens (the lexer has no DotDot).
            // `..` is only legal inside a tuple/struct/variant sequence. ADR-0049 Phase 6.
            let rest_token = just(TokenKind::Dot)
                .then(just(TokenKind::Dot))
                .map_with(|_, e| span_from_extra(e));

            // A leaf binding in a pattern sub-position: `_`, `x`, or `mut x`. These can
            // still appear anywhere a full sub-pattern is legal; full sub-patterns go
            // through `pat` (the recursive reference).
            let leaf_binding = choice((
                just(TokenKind::Underscore).map_with(|_, e| Pattern::Wildcard(span_from_extra(e))),
                just(TokenKind::Mut)
                    .ignore_then(ident_parser())
                    .map_with(|name, e| Pattern::Ident {
                        is_mut: true,
                        name,
                        span: span_from_extra(e),
                    }),
                ident_parser().map_with(|name, e| Pattern::Ident {
                    is_mut: false,
                    name,
                    span: span_from_extra(e),
                }),
            ));

            // Sub-pattern: any full pattern OR a leaf binding (as a shortcut). In match
            // contexts we need `mut x` at the sub-pattern position, which the recursive
            // `pat` doesn't emit (only full patterns do).
            //
            // Order: try the recursive pattern first (for nested Enum::V / Struct { .. } /
            // (.., ..) shapes), then fall back to the leaf binding for bare idents,
            // wildcards, and `mut x`. A bare ident matches both as a Pattern::Ident inside
            // `pat` and as a leaf binding — either path yields the same AST.
            let sub_pattern = choice((pat.clone(), leaf_binding.clone())).boxed();

            // One element in a tuple / variant-tuple sequence: either `..` or a sub-pattern.
            let tuple_elem = choice((
                rest_token.clone().map(TupleElemPattern::Rest),
                sub_pattern.clone().map(TupleElemPattern::Pattern),
            ))
            .boxed();

            // `(e1, e2, ...)` tuple-like sequence body.
            let tuple_suffix = tuple_elem
                .clone()
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen));

            // Named field in a struct / struct-variant pattern: `field`, `field: sub`,
            // `mut field`, `mut field: sub`, or the rest sentinel `..`.
            let field_rest = rest_token.clone().map_with(|span, _e| FieldPattern {
                field_name: None,
                sub: None,
                is_mut: false,
                span,
            });
            let field_explicit = just(TokenKind::Mut)
                .or_not()
                .then(ident_parser())
                .then_ignore(just(TokenKind::Colon))
                .then(sub_pattern.clone())
                .map_with(|((is_mut, field_name), sub), e| FieldPattern {
                    field_name: Some(field_name),
                    sub: Some(sub),
                    is_mut: is_mut.is_some(),
                    span: span_from_extra(e),
                });
            let field_shorthand =
                just(TokenKind::Mut)
                    .or_not()
                    .then(ident_parser())
                    .map_with(|(is_mut, name), e| FieldPattern {
                        field_name: Some(name),
                        sub: None,
                        is_mut: is_mut.is_some(),
                        span: span_from_extra(e),
                    });
            let field_pat = choice((field_rest, field_explicit, field_shorthand)).boxed();

            let struct_suffix = field_pat
                .clone()
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace));

            // Enum for the suffix on an enum variant pattern: tuple, struct, or none.
            #[derive(Debug, Clone)]
            enum PatternSuffix {
                Tuple(Vec<TupleElemPattern>),
                Struct(Vec<FieldPattern>),
            }

            let pattern_suffix = choice((
                tuple_suffix.clone().map(PatternSuffix::Tuple),
                struct_suffix.clone().map(PatternSuffix::Struct),
            ));

            // Self path pattern: Self::Variant, Self::Variant(sub, ...), or Self::Variant { field: sub, ... }
            let self_path_pat = just(TokenKind::SelfType)
                .ignore_then(just(TokenKind::ColonColon))
                .ignore_then(ident_parser())
                .then(pattern_suffix.clone().or_not())
                .map_with(|(variant, suffix_opt), e| {
                    let span = span_from_extra(e);
                    let type_name = Ident {
                        name: e.state().syms.self_type,
                        span,
                    };
                    match suffix_opt {
                        Some(PatternSuffix::Tuple(fields)) => Pattern::DataVariant {
                            base: None,
                            type_name,
                            variant,
                            fields,
                            span,
                        },
                        Some(PatternSuffix::Struct(fields)) => Pattern::StructVariant {
                            base: None,
                            type_name,
                            variant,
                            fields,
                            span,
                        },
                        None => Pattern::Path(PathPattern {
                            base: None,
                            type_name,
                            variant,
                            span,
                        }),
                    }
                });

            // IDENT (followed by `{`, `::`, or nothing): either a struct destructure,
            // a unit/data/struct variant, or a bare ident binding.
            //
            // `IDENT { ... }` → Pattern::Struct
            // `IDENT::IDENT[(...)]` / `IDENT::IDENT{ ... }` → Pattern::{Path, DataVariant, StructVariant}
            let struct_destructure =
                ident_parser()
                    .then(struct_suffix.clone())
                    .map_with(|(type_name, fields), e| Pattern::Struct {
                        type_name,
                        fields,
                        span: span_from_extra(e),
                    });

            let simple_path_pat = ident_parser()
                .then_ignore(just(TokenKind::ColonColon))
                .then(ident_parser())
                .then(pattern_suffix.clone().or_not())
                .map_with(|((type_name, variant), suffix_opt), e| match suffix_opt {
                    Some(PatternSuffix::Tuple(fields)) => Pattern::DataVariant {
                        base: None,
                        type_name,
                        variant,
                        fields,
                        span: span_from_extra(e),
                    },
                    Some(PatternSuffix::Struct(fields)) => Pattern::StructVariant {
                        base: None,
                        type_name,
                        variant,
                        fields,
                        span: span_from_extra(e),
                    },
                    None => Pattern::Path(PathPattern {
                        base: None,
                        type_name,
                        variant,
                        span: span_from_extra(e),
                    }),
                });

            // Qualified path pattern: module.Enum::Variant or module.sub.Enum::Variant
            let qualified_path_pat = ident_parser()
                .then(
                    just(TokenKind::Dot)
                        .ignore_then(ident_parser())
                        .repeated()
                        .at_least(1)
                        .collect::<Vec<_>>(),
                )
                .then_ignore(just(TokenKind::ColonColon))
                .then(ident_parser())
                .then(pattern_suffix.or_not())
                .map_with(|(((first, mut rest), variant), suffix_opt), e| {
                    let type_name = rest.pop().expect("at_least(1) guarantees non-empty");

                    let base_expr = if rest.is_empty() {
                        Expr::Ident(first)
                    } else {
                        let mut base = Expr::Ident(first);
                        for field in rest {
                            let span = base.span().extend_to(field.span.end);
                            base = Expr::Field(FieldExpr {
                                base: Box::new(base),
                                field,
                                span,
                            });
                        }
                        base
                    };

                    match suffix_opt {
                        Some(PatternSuffix::Tuple(fields)) => Pattern::DataVariant {
                            base: Some(Box::new(base_expr)),
                            type_name,
                            variant,
                            fields,
                            span: span_from_extra(e),
                        },
                        Some(PatternSuffix::Struct(fields)) => Pattern::StructVariant {
                            base: Some(Box::new(base_expr)),
                            type_name,
                            variant,
                            fields,
                            span: span_from_extra(e),
                        },
                        None => Pattern::Path(PathPattern {
                            base: Some(Box::new(base_expr)),
                            type_name,
                            variant,
                            span: span_from_extra(e),
                        }),
                    }
                });

            // Tuple pattern (match context or nested position): `(p1, p2, ...)` with at
            // least one comma, or `(p,)` for a 1-tuple. `(p)` remains a parenthesised
            // pattern — not supported here (redundant: just write `p`).
            let tuple_pat = just(TokenKind::LParen)
                .ignore_then(tuple_elem.clone())
                .then_ignore(just(TokenKind::Comma))
                .then(
                    tuple_elem
                        .clone()
                        .separated_by(just(TokenKind::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>(),
                )
                .then_ignore(just(TokenKind::RParen))
                .map_with(|(first, rest), e| {
                    let mut elems = Vec::with_capacity(1 + rest.len());
                    elems.push(first);
                    elems.extend(rest);
                    Pattern::Tuple {
                        elems,
                        span: span_from_extra(e),
                    }
                });

            // Plain ident / mut-ident leaf as a top-level pattern (bare binding).
            let ident_leaf =
                just(TokenKind::Mut)
                    .or_not()
                    .then(ident_parser())
                    .map_with(|(is_mut, name), e| Pattern::Ident {
                        is_mut: is_mut.is_some(),
                        name,
                        span: span_from_extra(e),
                    });

            choice((
                wildcard.boxed(),
                neg_int_pat.boxed(),
                int_pat.boxed(),
                bool_true.boxed(),
                bool_false.boxed(),
                tuple_pat.boxed(),
                // Try qualified path first (has more structure), then Self path, then
                // simple path, then struct destructure, then bare ident. Struct destructure
                // and simple path both start with IDENT; `IDENT::` disambiguates. Struct
                // destructure requires `IDENT {`, a combination that doesn't conflict with
                // the bare ident parser which is tried last.
                qualified_path_pat.boxed(),
                self_path_pat.boxed(),
                simple_path_pat.boxed(),
                struct_destructure.boxed(),
                ident_leaf.boxed(),
            ))
            .boxed()
        },
    )
    .boxed()
}

/// Parser for a single match arm: pattern => expr
fn match_arm_parser<'src, I>(expr: GruelParser<'src, I, Expr>) -> GruelParser<'src, I, MatchArm>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    pattern_parser()
        .then_ignore(just(TokenKind::FatArrow))
        .then(expr)
        .map_with(|(pattern, body), e| MatchArm {
            pattern,
            body: Box::new(body),
            span: span_from_extra(e),
        })
        .boxed()
}

/// Parser for literal expressions: integers, strings, booleans, and unit
fn literal_parser<'src, I>() -> GruelParser<'src, I, Expr>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Integer literal
    let int_lit = select! {
        TokenKind::Int(n) = e => Expr::Int(IntLit {
            value: n,
            span: span_from_extra(e),
        }),
    };

    // Floating-point literal
    let float_lit = select! {
        TokenKind::Float(bits) = e => Expr::Float(FloatLit {
            bits,
            span: span_from_extra(e),
        }),
    };

    // String literal
    let string_lit = select! {
        TokenKind::String(s) = e => Expr::String(StringLit {
            value: s,
            span: span_from_extra(e),
        }),
    };

    // Boolean literals
    let bool_true = select! {
        TokenKind::True = e => Expr::Bool(BoolLit {
            value: true,
            span: span_from_extra(e),
        }),
    };

    let bool_false = select! {
        TokenKind::False = e => Expr::Bool(BoolLit {
            value: false,
            span: span_from_extra(e),
        }),
    };

    // Unit literal: ()
    let unit_lit = just(TokenKind::LParen)
        .then(just(TokenKind::RParen))
        .map_with(|_, e| {
            Expr::Unit(UnitLit {
                span: span_from_extra(e),
            })
        });

    choice((
        int_lit.boxed(),
        float_lit.boxed(),
        string_lit.boxed(),
        bool_true.boxed(),
        bool_false.boxed(),
        unit_lit.boxed(),
    ))
    .boxed()
}

/// Parser for control flow expressions: break, continue, return, if, while, loop, match
fn control_flow_parser<'src, I>(expr: GruelParser<'src, I, Expr>) -> GruelParser<'src, I, Expr>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Break
    let break_expr = select! {
        TokenKind::Break = e => Expr::Break(BreakExpr { span: span_from_extra(e) }),
    };

    // Continue
    let continue_expr = select! {
        TokenKind::Continue = e => Expr::Continue(ContinueExpr { span: span_from_extra(e) }),
    };

    // Return expression: return <expr>? (expression is optional for unit-returning functions)
    let return_expr = just(TokenKind::Return)
        .ignore_then(expr.clone().or_not())
        .map_with(|value, e| {
            Expr::Return(ReturnExpr {
                value: value.map(Box::new),
                span: span_from_extra(e),
            })
        });

    // If expression - defined with recursive reference to allow `else if` chains
    let if_expr: GruelParser<'src, I, Expr> = recursive(
        |if_expr_rec: Recursive<Direct<'src, 'src, I, Expr, ParserExtras<'src>>>| {
            just(TokenKind::If)
                .ignore_then(expr.clone())
                .then(maybe_unit_block_parser(expr.clone()))
                .then(
                    just(TokenKind::Else)
                        .ignore_then(choice((
                            // else if: wrap the nested if in a synthetic block
                            if_expr_rec
                                .map_with(|nested_if, e| {
                                    let span = span_from_extra(e);
                                    BlockExpr {
                                        statements: Vec::new(),
                                        expr: Box::new(nested_if),
                                        span,
                                    }
                                })
                                .boxed(),
                            // else { ... }: parse a regular block
                            maybe_unit_block_parser(expr.clone()),
                        )))
                        .or_not(),
                )
                .map_with(|((cond, then_block), else_block), e| {
                    Expr::If(IfExpr {
                        cond: Box::new(cond),
                        then_block,
                        else_block,
                        span: span_from_extra(e),
                    })
                })
                .boxed()
        },
    )
    .boxed();

    // While expression
    let while_expr: GruelParser<'src, I, Expr> = just(TokenKind::While)
        .ignore_then(expr.clone())
        .then(maybe_unit_block_parser(expr.clone()))
        .map_with(|(cond, body), e| {
            Expr::While(WhileExpr {
                cond: Box::new(cond),
                body,
                span: span_from_extra(e),
            })
        })
        .boxed();

    // For-in expression: for [mut] ident in expr { body }
    let for_expr: GruelParser<'src, I, Expr> = just(TokenKind::For)
        .ignore_then(just(TokenKind::Mut).or_not())
        .then(ident_parser())
        .then_ignore(just(TokenKind::In))
        .then(expr.clone())
        .then(maybe_unit_block_parser(expr.clone()))
        .map_with(|(((is_mut, binding), iterable), body), e| {
            Expr::For(ForExpr {
                binding,
                is_mut: is_mut.is_some(),
                iterable: Box::new(iterable),
                body,
                span: span_from_extra(e),
            })
        })
        .boxed();

    // Loop expression (infinite loop)
    let loop_expr: GruelParser<'src, I, Expr> = just(TokenKind::Loop)
        .ignore_then(maybe_unit_block_parser(expr.clone()))
        .map_with(|body, e| {
            Expr::Loop(LoopExpr {
                body,
                span: span_from_extra(e),
            })
        })
        .boxed();

    // Match expression: match scrutinee { pattern => expr, ... }
    let match_expr: GruelParser<'src, I, Expr> = just(TokenKind::Match)
        .ignore_then(expr.clone())
        .then(
            match_arm_parser(expr.clone())
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|(scrutinee, arms), e| {
            Expr::Match(MatchExpr {
                scrutinee: Box::new(scrutinee),
                arms,
                span: span_from_extra(e),
            })
        })
        .boxed();

    // Comptime unroll for expression: comptime_unroll for ident in expr { body }
    let comptime_unroll_for_expr: GruelParser<'src, I, Expr> = just(TokenKind::ComptimeUnroll)
        .ignore_then(just(TokenKind::For))
        .ignore_then(ident_parser())
        .then_ignore(just(TokenKind::In))
        .then(expr.clone())
        .then(maybe_unit_block_parser(expr.clone()))
        .map_with(|((binding, iterable), body), e| {
            Expr::ComptimeUnrollFor(ComptimeUnrollForExpr {
                binding,
                iterable: Box::new(iterable),
                body,
                span: span_from_extra(e),
            })
        })
        .boxed();

    choice((
        break_expr.boxed(),
        continue_expr.boxed(),
        return_expr.boxed(),
        if_expr,
        while_expr,
        for_expr,
        comptime_unroll_for_expr,
        loop_expr,
        match_expr,
    ))
    .boxed()
}

/// What can follow an identifier: call args, struct fields, path (::variant), path call (::fn()), or nothing
#[derive(Clone)]
enum IdentSuffix {
    Call(Vec<CallArg>),
    StructLit(Vec<FieldInit>),
    Path(Ident),                          // ::Variant (for enum variants)
    PathCall(Ident, Vec<CallArg>),        // ::function() (for associated functions)
    PathStructLit(Ident, Vec<FieldInit>), // ::Variant { field: value } (for enum struct variants)
    None,
}

/// Parser for identifier-based expressions: identifiers, function calls, struct literals, and paths
fn call_and_access_parser<'src, I>(expr: GruelParser<'src, I, Expr>) -> GruelParser<'src, I, Expr>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser()
        .then(
            choice((
                // Function call: (args)
                call_args_parser(expr.clone())
                    .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
                    .map(IdentSuffix::Call)
                    .boxed(),
                // Struct literal: { field: value, ... } or { field, ... } (shorthand)
                // Lookahead: require `{ }` or `{ ident :` or `{ ident ,`
                // to disambiguate from blocks like `if cond { expr }`
                just(TokenKind::LBrace)
                    .then(
                        choice((
                            just(TokenKind::RBrace).ignored(),
                            select! { TokenKind::Ident(_) => () }
                                .then_ignore(just(TokenKind::Colon))
                                .ignored(),
                            select! { TokenKind::Ident(_) => () }
                                .then_ignore(just(TokenKind::Comma))
                                .ignored(),
                        ))
                        .rewind(),
                    )
                    .rewind()
                    .ignore_then(
                        field_inits_parser(expr.clone())
                            .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
                    )
                    .map(IdentSuffix::StructLit)
                    .boxed(),
                // Associated function call: ::function(args)
                just(TokenKind::ColonColon)
                    .ignore_then(ident_parser())
                    .then(
                        call_args_parser(expr.clone())
                            .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
                    )
                    .map(|(func, args)| IdentSuffix::PathCall(func, args))
                    .boxed(),
                // Enum struct variant literal: ::Variant { field: value, ... }
                just(TokenKind::ColonColon)
                    .ignore_then(ident_parser())
                    .then(
                        field_inits_parser(expr.clone())
                            .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
                    )
                    .map(|(variant, fields)| IdentSuffix::PathStructLit(variant, fields))
                    .boxed(),
                // Path: ::Variant (for enum variants)
                just(TokenKind::ColonColon)
                    .ignore_then(ident_parser())
                    .map(IdentSuffix::Path)
                    .boxed(),
            ))
            .or_not()
            .map(|opt| opt.unwrap_or(IdentSuffix::None)),
        )
        .map_with(|(name, suffix), e| match suffix {
            IdentSuffix::Call(args) => Expr::Call(CallExpr {
                name,
                args,
                span: span_from_extra(e),
            }),
            IdentSuffix::StructLit(fields) => Expr::StructLit(StructLitExpr {
                base: None, // No module prefix for simple `StructName { ... }`
                name,
                fields,
                span: span_from_extra(e),
            }),
            IdentSuffix::PathCall(function, args) => Expr::AssocFnCall(AssocFnCallExpr {
                base: None, // No module prefix for simple `Type::function()`
                type_name: name,
                function,
                args,
                span: span_from_extra(e),
            }),
            IdentSuffix::PathStructLit(variant, fields) => Expr::EnumStructLit(EnumStructLitExpr {
                base: None,
                type_name: name,
                variant,
                fields,
                span: span_from_extra(e),
            }),
            IdentSuffix::Path(variant) => Expr::Path(PathExpr {
                base: None, // No module prefix for simple `Enum::Variant`
                type_name: name,
                variant,
                span: span_from_extra(e),
            }),
            IdentSuffix::None => Expr::Ident(name),
        })
        .boxed()
}

/// Suffix for field access (.field), method call (.method(args)), indexing ([expr]),
/// qualified struct literals (.Type { ... }), and qualified paths (.Enum::Variant)
#[derive(Clone)]
enum Suffix {
    /// Simple field access: .field
    Field(Ident),
    /// Tuple field access: .0, .1, ... (ADR-0048).
    /// The u32 is the index; the Span is the position of the integer literal.
    TupleField(u32, Span),
    /// Method call with method name, arguments, and closing paren position
    MethodCall(Ident, Vec<CallArg>, u32),
    /// Index expression with the inner expression and closing bracket position
    Index(Expr, u32),
    /// Qualified struct literal: .StructName { fields }
    /// NOTE: Not yet wired up due to grammar ambiguity with field access + block
    QualifiedStructLit(Ident, Vec<FieldInit>, u32),
    /// Qualified path (enum variant): .EnumName::Variant
    QualifiedPath(Ident, Ident, u32),
    /// Qualified associated function call: .TypeName::function(args)
    QualifiedAssocFnCall(Ident, Ident, Vec<CallArg>, u32),
    /// Qualified enum struct variant literal: .EnumName::Variant { field: value }
    QualifiedEnumStructLit(Ident, Ident, Vec<FieldInit>, u32),
}

/// Wraps a primary expression parser with field access, method call, and indexing suffixes
fn with_suffix_parser<'src, I>(
    primary: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
    expr: GruelParser<'src, I, Expr>,
) -> GruelParser<'src, I, Expr>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Method call: .ident(args)
    let method_call_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .then(
            call_args_parser(expr.clone())
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
        )
        .map_with(|(method, args), e| {
            Suffix::MethodCall(method, args, offset_to_u32(e.span().end))
        });

    // Qualified struct literal: .StructName { field: value, ... }
    // We need to distinguish between:
    //   - `module.Point { x: 1 }` - qualified struct literal
    //   - `obj.field { 1 }` - field access followed by a block expression
    //
    // The key difference is that struct literals have `{ ident: value, ... }` while
    // block expressions have `{ expr }`. We use a lookahead to require that `{` is
    // followed by `}` (empty struct) or `ident :` (non-empty struct) to confirm it's
    // a struct literal, not field access followed by a block.
    //
    // Lookahead check: succeeds (without consuming) if `{ }` or `{ ident : `
    let struct_lit_lookahead = just(TokenKind::LBrace)
        .then(
            choice((
                // Empty struct: { }
                just(TokenKind::RBrace).ignored(),
                // Non-empty struct: { ident : ...
                select! { TokenKind::Ident(_) => () }
                    .then_ignore(just(TokenKind::Colon))
                    .ignored(),
            ))
            // We're just checking the pattern exists, not consuming
            .rewind(),
        )
        .rewind();

    // NOTE: Box the lookahead itself to prevent its complex nested type from
    // accumulating in qualified_struct_lit_suffix before that parser is boxed.
    let struct_lit_lookahead: GruelParser<'src, I, _> = struct_lit_lookahead.boxed();

    // NOTE: .boxed() is required here to shorten the monomorphized type name.
    // The lookahead creates deeply nested generics that exceed macOS linker limits.
    // We split into a boxed head (name + lookahead) and then chain the body.
    let struct_lit_name: GruelParser<'src, I, Ident> = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .then_ignore(struct_lit_lookahead)
        .boxed();
    let qualified_struct_lit_suffix = struct_lit_name
        .then(
            field_inits_parser(expr.clone())
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|(name, fields), e| {
            Suffix::QualifiedStructLit(name, fields, offset_to_u32(e.span().end))
        })
        .boxed();

    // NOTE: .boxed() on the shared head prevents type accumulation in both
    // qualified_assoc_fn_suffix and qualified_path_suffix.
    let type_and_member: GruelParser<'src, I, (Ident, Ident)> = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .then_ignore(just(TokenKind::ColonColon))
        .then(ident_parser())
        .boxed();

    // Qualified associated function call: .TypeName::function(args)
    let qualified_assoc_fn_suffix = type_and_member
        .clone()
        .then(
            call_args_parser(expr.clone())
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
        )
        .map_with(|((type_name, function), args), e| {
            Suffix::QualifiedAssocFnCall(type_name, function, args, offset_to_u32(e.span().end))
        });

    // Qualified enum struct variant literal: .EnumName::Variant { field: value, ... }
    let qualified_enum_struct_lit_suffix = type_and_member
        .clone()
        .then(
            field_inits_parser(expr.clone())
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|((type_name, variant), fields), e| {
            Suffix::QualifiedEnumStructLit(type_name, variant, fields, offset_to_u32(e.span().end))
        });

    // Qualified path (enum variant): .EnumName::Variant
    // We capture the end position from the variant ident before the negative lookahead
    let qualified_path_suffix = type_and_member
        .map(|(type_name, variant): (Ident, Ident)| {
            let end = variant.span.end;
            (type_name, variant, end)
        })
        .then_ignore(none_of([TokenKind::LParen, TokenKind::LBrace]).rewind())
        .map(|(type_name, variant, end)| Suffix::QualifiedPath(type_name, variant, end));

    // Field access: .ident (but NOT followed by '(', '::', or struct literal pattern)
    // The qualified_struct_lit_suffix is tried first and uses lookahead, so field_suffix
    // only matches when we're certain it's field access, not a qualified struct literal.
    let field_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .then_ignore(none_of([TokenKind::LParen, TokenKind::ColonColon]).rewind())
        .map(Suffix::Field);

    // Tuple field access: .N where N is a non-negative integer literal (ADR-0048).
    //
    // Note: the lexer tokenises `0.1` / `1e10` as a single `Float` token, so
    // `t.0.1` and `t.1e10` fail to parse as nested tuple access. Users must
    // write `(t.0).1` for nested access. Float re-splitting in field position
    // is deferred to a future ADR.
    //
    // Indices larger than u32::MAX are clamped to u32::MAX; tuples cannot have
    // more than u32::MAX elements, so sema will report this as out-of-bounds.
    let tuple_field_suffix = just(TokenKind::Dot)
        .ignore_then(select! {
            TokenKind::Int(n) = e => (n, span_from_extra(e)),
        })
        .map(|(n, span)| {
            let idx = if n > u32::MAX as u64 {
                u32::MAX
            } else {
                n as u32
            };
            Suffix::TupleField(idx, span)
        });

    let index_suffix = expr
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map_with(|index, e| Suffix::Index(index, offset_to_u32(e.span().end)));

    // Order matters: more specific patterns must come before less specific ones
    // - qualified_assoc_fn_suffix before qualified_path_suffix (both have ::)
    // - method_call_suffix before field_suffix (both start with .ident)
    // - qualified_struct_lit_suffix before field_suffix (uses lookahead for { ident: } pattern)
    // - qualified_path_suffix before field_suffix (both start with .ident)
    //
    // NOTE: .boxed() is required here to shorten the monomorphized type name.
    // Without it, macOS's ld64 linker fails with "symbol name too long" because
    // the 6-way choice creates extremely long generic type names.
    primary
        .foldl(
            choice((
                method_call_suffix.boxed(),
                qualified_assoc_fn_suffix.boxed(),
                qualified_enum_struct_lit_suffix.boxed(),
                qualified_struct_lit_suffix,
                qualified_path_suffix.boxed(),
                field_suffix.boxed(),
                tuple_field_suffix.boxed(),
                index_suffix.boxed(),
            ))
            .boxed()
            .repeated(),
            |base, suffix| match suffix {
                Suffix::Field(field) => {
                    // Extend the base span to include the field, preserving file_id
                    let span = base.span().extend_to(field.span.end);
                    Expr::Field(FieldExpr {
                        base: Box::new(base),
                        field,
                        span,
                    })
                }
                Suffix::TupleField(index, index_span) => {
                    let span = base.span().extend_to(index_span.end);
                    Expr::TupleIndex(TupleIndexExpr {
                        base: Box::new(base),
                        index,
                        span,
                        index_span,
                    })
                }
                Suffix::MethodCall(method, args, end) => {
                    // Extend the base span to the end of the call, preserving file_id
                    let span = base.span().extend_to(end);
                    Expr::MethodCall(MethodCallExpr {
                        receiver: Box::new(base),
                        method,
                        args,
                        span,
                    })
                }
                Suffix::Index(index, end) => {
                    // Extend the base span to the end of the index, preserving file_id
                    let span = base.span().extend_to(end);
                    Expr::Index(IndexExpr {
                        base: Box::new(base),
                        index: Box::new(index),
                        span,
                    })
                }
                Suffix::QualifiedStructLit(name, fields, end) => {
                    // module.StructName { ... } → StructLitExpr with base
                    let span = base.span().extend_to(end);
                    Expr::StructLit(StructLitExpr {
                        base: Some(Box::new(base)),
                        name,
                        fields,
                        span,
                    })
                }
                Suffix::QualifiedPath(type_name, variant, end) => {
                    // module.EnumName::Variant → PathExpr with base
                    let span = base.span().extend_to(end);
                    Expr::Path(PathExpr {
                        base: Some(Box::new(base)),
                        type_name,
                        variant,
                        span,
                    })
                }
                Suffix::QualifiedEnumStructLit(type_name, variant, fields, end) => {
                    // module.EnumName::Variant { ... } → EnumStructLitExpr with base
                    let span = base.span().extend_to(end);
                    Expr::EnumStructLit(EnumStructLitExpr {
                        base: Some(Box::new(base)),
                        type_name,
                        variant,
                        fields,
                        span,
                    })
                }
                Suffix::QualifiedAssocFnCall(type_name, function, args, end) => {
                    // module.TypeName::function(args) → AssocFnCallExpr with base
                    let span = base.span().extend_to(end);
                    Expr::AssocFnCall(AssocFnCallExpr {
                        base: Some(Box::new(base)),
                        type_name,
                        function,
                        args,
                        span,
                    })
                }
            },
        )
        .boxed()
}

/// Atom parser - primary expressions (literals, identifiers, parens, blocks, control flow)
fn atom_parser<'src, I>(expr: GruelParser<'src, I, Expr>) -> GruelParser<'src, I, Expr>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Self expression (in method bodies)
    let self_expr = select! {
        TokenKind::SelfValue = e => Expr::SelfExpr(SelfExpr { span: span_from_extra(e) }),
    };

    // Parenthesized expression or tuple literal.
    //
    // `(e)`         -> ParenExpr
    // `(e,)`        -> TupleExpr with one element
    // `(e, f, ...)` -> TupleExpr with 2+ elements
    // `()` is handled separately by `unit_lit` (see literal_parser).
    //
    // Implementation: parse `LParen expr`, then optionally `Comma expr_list`,
    // then `RParen`. If the comma is absent we have a paren; if it's present
    // we have a tuple (possibly 1-arity if no more elements follow).
    let paren_or_tuple = just(TokenKind::LParen)
        .ignore_then(expr.clone())
        .then(
            just(TokenKind::Comma)
                .ignore_then(
                    expr.clone()
                        .separated_by(just(TokenKind::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>(),
                )
                .or_not(),
        )
        .then_ignore(just(TokenKind::RParen))
        .map_with(|(first, rest), e| match rest {
            None => Expr::Paren(ParenExpr {
                inner: Box::new(first),
                span: span_from_extra(e),
            }),
            Some(rest) => {
                let mut elems = Vec::with_capacity(1 + rest.len());
                elems.push(first);
                elems.extend(rest);
                Expr::Tuple(TupleExpr {
                    elems,
                    span: span_from_extra(e),
                })
            }
        });
    let paren_expr = paren_or_tuple;

    // Block expression
    let block_expr = block_parser(expr.clone());

    // Comptime block expression: comptime { expr }
    let comptime_expr = just(TokenKind::Comptime)
        .ignore_then(block_parser(expr.clone()))
        .map_with(|inner_expr, e| {
            Expr::Comptime(ComptimeBlockExpr {
                expr: Box::new(inner_expr),
                span: span_from_extra(e),
            })
        });

    // Checked block expression: checked { expr }
    // Unchecked operations are only allowed inside checked blocks
    let checked_expr = just(TokenKind::Checked)
        .ignore_then(block_parser(expr.clone()))
        .map_with(|inner_expr, e| {
            Expr::Checked(CheckedBlockExpr {
                expr: Box::new(inner_expr),
                span: span_from_extra(e),
            })
        });

    // Intrinsic argument: can be either a type or an expression
    // We parse as type only for unambiguous type syntax (primitives, (), !, [T;N])
    // Bare identifiers are parsed as expressions since they could be variables
    let unambiguous_type = {
        // Unit type: ()
        let unit_type = just(TokenKind::LParen)
            .then(just(TokenKind::RParen))
            .map_with(|_, e| IntrinsicArg::Type(TypeExpr::Unit(span_from_extra(e))));

        // Never type: !
        let never_type = just(TokenKind::Bang)
            .map_with(|_, e| IntrinsicArg::Type(TypeExpr::Never(span_from_extra(e))));

        // Array type: [T; N]
        let array_type = just(TokenKind::LBracket)
            .ignore_then(type_parser())
            .then_ignore(just(TokenKind::Semi))
            .then(select! {
                TokenKind::Int(n) => n,
            })
            .then_ignore(just(TokenKind::RBracket))
            .map_with(|(element, length), e| {
                IntrinsicArg::Type(TypeExpr::Array {
                    element: Box::new(element),
                    length,
                    span: span_from_extra(e),
                })
            });

        // Primitive type keywords (these can't be variable names)
        let primitive_type = primitive_type_parser().map(IntrinsicArg::Type);

        choice((
            unit_type.boxed(),
            never_type.boxed(),
            array_type.boxed(),
            primitive_type.boxed(),
        ))
        .boxed()
    };

    // Try unambiguous type syntax first, then fall back to expression.
    // Boxed so the SeparatedBy type is short when used in intrinsic_call args.
    let intrinsic_arg: GruelParser<I, IntrinsicArg> = choice((
        unambiguous_type,
        expr.clone().map(IntrinsicArg::Expr).boxed(),
    ))
    .boxed();

    // Shared intrinsic args parser (boxed to keep downstream types short).
    let intrinsic_args: GruelParser<I, Vec<IntrinsicArg>> = intrinsic_arg
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
        .boxed();

    // Intrinsic call: @name(args)
    let intrinsic_call: GruelParser<I, Expr> = just(TokenKind::At)
        .ignore_then(ident_parser())
        .then(intrinsic_args.clone())
        .map_with(|(name, args), e| {
            Expr::IntrinsicCall(IntrinsicCallExpr {
                name,
                args,
                span: span_from_extra(e),
            })
        })
        .boxed();

    // @import(args) - lexer tokenizes @import as a single token with interned "import" Spur
    let import_call: GruelParser<I, Expr> = select! {
        TokenKind::AtImport(import_spur) = e => (import_spur, span_from_extra(e)),
    }
    .then(intrinsic_args)
    .map_with(|((import_spur, import_span), args), e| {
        Expr::IntrinsicCall(IntrinsicCallExpr {
            name: Ident {
                name: import_spur,
                span: import_span,
            },
            args,
            span: span_from_extra(e),
        })
    })
    .boxed();

    // Combined intrinsic parser: try @import first, then general @name pattern
    let any_intrinsic_call: GruelParser<I, Expr> = choice((import_call, intrinsic_call)).boxed();

    // Array literal: [expr, expr, ...]
    let array_lit = args_parser(expr.clone())
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map_with(|elements, e| {
            Expr::ArrayLit(ArrayLitExpr {
                elements,
                span: span_from_extra(e),
            })
        });

    // Type literal expression: i32, bool, etc. used as values
    // This enables generic function calls like identity(i32, 42)
    let type_lit_expr = primitive_type_parser().map_with(|type_expr, e| {
        Expr::TypeLit(TypeLitExpr {
            type_expr,
            span: span_from_extra(e),
        })
    });

    // Anonymous struct type as expression: struct { field: Type, ... fn method(...) { ... } ... }
    // This enables comptime type construction like:
    //   fn Pair(comptime T: type) -> type { struct { first: T, second: T } }
    // With methods (Zig-style):
    //   fn Vec(comptime T: type) -> type { struct { ptr: u64, fn push(self, item: T) { ... } } }
    let anon_struct_field: GruelParser<'src, I, AnonStructField> = ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(type_parser())
        .map_with(|(name, field_ty), e| AnonStructField {
            name,
            ty: field_ty,
            span: span_from_extra(e),
        })
        .boxed();

    let anon_struct_fields: GruelParser<'src, I, Vec<AnonStructField>> = anon_struct_field
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .boxed();

    // Parse method for anonymous struct using inline method parsing
    // Methods inside anonymous structs follow the same syntax as impl block methods
    let anon_struct_method = anon_struct_method_parser(expr.clone());

    let anon_struct_header: GruelParser<'src, I, (Vec<AnonStructField>, Vec<Method>)> =
        just(TokenKind::Struct)
            .ignore_then(just(TokenKind::LBrace))
            .ignore_then(anon_struct_fields)
            .then(
                // Then parse methods (not comma-separated, each ends with })
                anon_struct_method.repeated().collect::<Vec<_>>(),
            )
            .boxed();

    let anon_struct_type_expr = anon_struct_header
        .then_ignore(just(TokenKind::RBrace))
        .map_with(|(fields, methods), e| {
            let span = span_from_extra(e);
            Expr::TypeLit(TypeLitExpr {
                type_expr: TypeExpr::AnonymousStruct {
                    fields,
                    methods,
                    span,
                },
                span,
            })
        });

    // Anonymous enum type as expression: enum { Variant, Variant(T), ... fn method(...) { ... } ... }
    // This enables comptime type construction like:
    //   fn Option(comptime T: type) -> type { enum { Some(T), None } }
    let anon_enum_method = anon_struct_method_parser(expr.clone());

    let anon_enum_type_expr = just(TokenKind::Enum)
        .ignore_then(just(TokenKind::LBrace))
        .ignore_then(enum_variants_parser())
        .then(
            // Then parse methods (not comma-separated, each ends with })
            anon_enum_method.repeated().collect::<Vec<_>>(),
        )
        .then_ignore(just(TokenKind::RBrace))
        .map_with(|(variants, methods), e| {
            let span = span_from_extra(e);
            Expr::TypeLit(TypeLitExpr {
                type_expr: TypeExpr::AnonymousEnum {
                    variants,
                    methods,
                    span,
                },
                span,
            })
        });

    // Self type expression: Self { field: value } (struct literal with Self as type)
    // This enables constructing instances of anonymous struct types from methods
    let self_type_expr = just(TokenKind::SelfType)
        .ignore_then(
            field_inits_parser(expr.clone())
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|fields, e| {
            let span = span_from_extra(e);
            Expr::StructLit(StructLitExpr {
                base: None,
                name: Ident {
                    name: e.state().syms.self_type,
                    span,
                },
                fields,
                span,
            })
        });

    // Self::Variant(args) — associated function call on Self (for anonymous enum variant construction)
    let self_assoc_fn_call = just(TokenKind::SelfType)
        .ignore_then(just(TokenKind::ColonColon))
        .ignore_then(ident_parser())
        .then(
            call_args_parser(expr.clone())
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
        )
        .map_with(|(function, args), e| {
            let span = span_from_extra(e);
            Expr::AssocFnCall(AssocFnCallExpr {
                base: None,
                type_name: Ident {
                    name: e.state().syms.self_type,
                    span,
                },
                function,
                args,
                span,
            })
        });

    // Self::Variant { field: value, ... } — struct variant construction on Self
    let self_enum_struct_lit = just(TokenKind::SelfType)
        .ignore_then(just(TokenKind::ColonColon))
        .ignore_then(ident_parser())
        .then(
            field_inits_parser(expr.clone())
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|(variant, fields), e| {
            let span = span_from_extra(e);
            Expr::EnumStructLit(EnumStructLitExpr {
                base: None,
                type_name: Ident {
                    name: e.state().syms.self_type,
                    span,
                },
                variant,
                fields,
                span,
            })
        });

    // Self::Variant — unit variant on Self (for anonymous enum variant construction)
    let self_enum_variant = just(TokenKind::SelfType)
        .ignore_then(just(TokenKind::ColonColon))
        .ignore_then(ident_parser())
        .then_ignore(none_of([TokenKind::LParen, TokenKind::LBrace]).rewind())
        .map_with(|variant, e| {
            let span = span_from_extra(e);
            Expr::Path(PathExpr {
                base: None,
                type_name: Ident {
                    name: e.state().syms.self_type,
                    span,
                },
                variant,
                span,
            })
        });

    // Primary expression (before field access and indexing)
    // Note: literal_parser() includes unit_lit which must come before paren_expr
    // so () is parsed as unit, not empty parens
    // Note: self_expr must come before call_and_access_parser since self is a keyword
    // Note: self_type_expr must come before call_and_access_parser since Self is a keyword
    // Note: comptime_expr and checked_expr must come before block_expr since they start with keywords
    // Note: type_lit_expr must come before call_and_access_parser since type names are keywords
    // Note: anon_struct_type_expr must come before call_and_access_parser since struct is a keyword
    //
    // NOTE: Split into sub-groups to keep Choice<tuple> symbol length < 4K.
    // 13 elements of Boxed<I,Expr,E> in one tuple would produce ~7K symbols on macOS.
    let primary_a: GruelParser<'src, I, Expr> = choice((
        literal_parser(),
        control_flow_parser(expr.clone()),
        self_expr.boxed(),
        self_assoc_fn_call.boxed(),
        self_enum_struct_lit.boxed(),
        self_enum_variant.boxed(),
        self_type_expr.boxed(),
        any_intrinsic_call.boxed(),
    ))
    .boxed();
    let primary_b: GruelParser<'src, I, Expr> = choice((
        array_lit.boxed(),
        anon_struct_type_expr.boxed(),
        anon_enum_type_expr.boxed(),
        type_lit_expr.boxed(),
        call_and_access_parser(expr.clone()),
        paren_expr.boxed(),
    ))
    .boxed();
    let primary_c: GruelParser<'src, I, Expr> = choice((
        comptime_expr.boxed(),
        checked_expr.boxed(),
        block_expr.boxed(),
    ))
    .boxed();
    let primary: GruelParser<'src, I, Expr> = choice((primary_a, primary_b, primary_c)).boxed();

    // Wrap primary expressions with field access, method call, and indexing suffixes
    with_suffix_parser(primary, expr)
}

/// A block item is either a statement or an expression (potentially the final one)
#[derive(Debug, Clone)]
enum BlockItem {
    Statement(Statement),
    Expr(Expr),
}

/// What token follows an expression in a block (used to determine its role)
#[derive(Debug, Clone, Copy)]
enum ExprFollower {
    /// Semicolon follows - expression is a statement
    Semi,
    /// Right brace follows (not consumed) - expression is the final/return value
    RBrace,
    /// Some other token follows - only valid for control flow expressions
    Other,
    /// End of input - only valid for control flow expressions
    End,
}

/// Parser for a let binding pattern (ADR-0049): delegates to the generic
/// `pattern_parser()`. Refutability is enforced by sema (Phase 3). The
/// post-parse preview-feature validator (`validate_preview_patterns`) rejects
/// nested / rest shapes when `nested_patterns` is off.
fn let_pattern_parser<'src, I>() -> GruelParser<'src, I, Pattern>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    pattern_parser()
}

/// Parser for let statements: [@directive]* let [mut] pattern [: type] = expr;
fn let_statement_parser<'src, I>(
    expr: GruelParser<'src, I, Expr>,
) -> GruelParser<'src, I, Statement>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Box after 2 thens to keep accumulated type short.
    let let_head: GruelParser<I, (Directives, bool, Pattern)> = directives_parser()
        .then(just(TokenKind::Let).ignore_then(just(TokenKind::Mut).or_not().map(|m| m.is_some())))
        .then(let_pattern_parser())
        .map(|((d, m), p)| (d, m, p))
        .boxed();

    let let_tail: GruelParser<I, (Option<TypeExpr>, Expr)> = just(TokenKind::Colon)
        .ignore_then(type_parser())
        .or_not()
        .then(just(TokenKind::Eq).ignore_then(expr))
        .then_ignore(just(TokenKind::Semi))
        .boxed();

    let_head
        .then(let_tail)
        .map_with(|((directives, is_mut, pattern), (ty, init)), e| {
            Statement::Let(LetStatement {
                directives,
                is_mut,
                pattern,
                ty,
                init: Box::new(init),
                span: span_from_extra(e),
            })
        })
        .boxed()
}

/// Suffix for assignment targets: either .field or [index]
#[derive(Clone)]
enum AssignSuffix {
    Field(Ident),
    Index(Expr),
}

/// Parser for assignment target: variable, field access, or index access
/// Parses: name or name.field or name[idx] or name.field[idx].field...
fn assign_target_parser<'src, I>(
    expr: GruelParser<'src, I, Expr>,
) -> GruelParser<'src, I, AssignTarget>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let field_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .map(AssignSuffix::Field);

    let index_suffix = expr
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map(AssignSuffix::Index);

    ident_parser()
        .then(
            choice((field_suffix.boxed(), index_suffix.boxed()))
                .repeated()
                .collect::<Vec<_>>(),
        )
        .map(|(base_ident, suffixes)| {
            if suffixes.is_empty() {
                // Simple variable: x
                AssignTarget::Var(base_ident)
            } else {
                // Chain of field/index accesses: x.a[0].b...
                // Build up the expression from left to right, consuming suffixes by value
                let mut base_expr = Expr::Ident(base_ident);
                let mut suffixes = suffixes.into_iter().peekable();
                while let Some(suffix) = suffixes.next() {
                    let is_last = suffixes.peek().is_none();
                    if is_last {
                        // The last suffix determines the target type
                        return match suffix {
                            AssignSuffix::Field(field) => {
                                let span = Span::new(base_expr.span().start, field.span.end);
                                AssignTarget::Field(FieldExpr {
                                    base: Box::new(base_expr),
                                    field,
                                    span,
                                })
                            }
                            AssignSuffix::Index(index) => {
                                let span = Span::new(base_expr.span().start, index.span().end);
                                AssignTarget::Index(IndexExpr {
                                    base: Box::new(base_expr),
                                    index: Box::new(index),
                                    span,
                                })
                            }
                        };
                    }
                    // Build intermediate expressions
                    match suffix {
                        AssignSuffix::Field(field) => {
                            let span = Span::new(base_expr.span().start, field.span.end);
                            base_expr = Expr::Field(FieldExpr {
                                base: Box::new(base_expr),
                                field,
                                span,
                            });
                        }
                        AssignSuffix::Index(index) => {
                            let span = Span::new(base_expr.span().start, index.span().end);
                            base_expr = Expr::Index(IndexExpr {
                                base: Box::new(base_expr),
                                index: Box::new(index),
                                span,
                            });
                        }
                    }
                }
                // This is unreachable since we already checked suffixes.is_empty()
                unreachable!()
            }
        })
        .boxed()
}

/// Parser for assignment statements: target = expr;
/// Supports variable (x = 5), field (point.x = 5), and index (arr[0] = 5) assignment
fn assign_statement_parser<'src, I>(
    expr: GruelParser<'src, I, Expr>,
) -> GruelParser<'src, I, Statement>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    assign_target_parser(expr.clone())
        .then_ignore(just(TokenKind::Eq))
        .then(expr)
        .then_ignore(just(TokenKind::Semi))
        .map_with(|(target, value), e| {
            Statement::Assign(AssignStatement {
                target,
                value: Box::new(value),
                span: span_from_extra(e),
            })
        })
        .boxed()
}

/// Returns true if the expression is a control flow construct or block that can appear
/// as a statement without a trailing semicolon.
///
/// This includes:
/// - Control flow: if, while, match, loop, break, continue, return
/// - Block expressions: { ... }
///
/// Block expressions are included because they are syntactically similar to control flow:
/// they are compound statements that naturally terminate without a semicolon.
fn is_control_flow_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::If(_)
            | Expr::Match(_)
            | Expr::While(_)
            | Expr::For(_)
            | Expr::ComptimeUnrollFor(_)
            | Expr::Loop(_)
            | Expr::Break(_)
            | Expr::Continue(_)
            | Expr::Return(_)
            | Expr::Block(_)
    )
}

/// Returns true if the expression diverges (has the Never type).
/// These expressions can be promoted to the final expression of a block
/// since Never coerces to any type.
fn is_diverging_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Break(_) | Expr::Continue(_) | Expr::Return(_) | Expr::Loop(_)
    )
}

/// Parses a single item within a block.
///
/// # Block Item Grammar
///
/// A block contains zero or more items. Each item is one of:
/// - **Let statement**: `let x = expr;` (always requires semicolon)
/// - **Assignment statement**: `target = expr;` (always requires semicolon)
/// - **Expression statement**: `expr;` (requires semicolon for most expressions)
/// - **Control flow statement**: `if/while/match/loop/break/continue/return ...`
///   (no semicolon needed when mid-block)
/// - **Final expression**: `expr` at the very end of a block (no semicolon, becomes
///   the block's return value)
///
/// # Parsing Strategy: Lookahead with `rewind()`
///
/// The challenge is distinguishing between:
/// 1. `{ foo; bar }` - `foo;` is a statement, `bar` is the final expression
/// 2. `{ if c { 1 } else { 2 } x }` - the `if` is a statement, `x` is final
/// 3. `{ if c { 1 } else { 2 } }` - the `if` IS the final expression
///
/// We use `rewind()` as a non-consuming lookahead to peek at what follows:
///
/// - `none_of([RBrace, Semi]).rewind()`: Succeeds if the NEXT token is neither
///   `}` nor `;`. The `.rewind()` means we check without consuming the token.
///   This identifies control flow in the middle of a block.
///
/// - `just(RBrace).rewind()`: Succeeds if the NEXT token is `}`. This identifies
///   the final expression of a block.
///
/// # Why `try_map()` for Control Flow?
///
/// After parsing an expression followed by a non-`}` non-`;` token, we need to
/// validate it's actually a control flow expression. If it's something like `x`
/// followed by `y`, that's a syntax error (missing semicolon). We use `try_map()`
/// to:
/// 1. Accept the parse if it's a control flow expression (valid without semicolon)
/// 2. Reject it otherwise, allowing chumsky to backtrack and try other branches
///
/// # Parse Order Matters
///
/// The `choice()` tries parsers in order. We must try:
/// 1. `let_stmt` first (starts with `let` keyword)
/// 2. `assign_stmt` second (identifier followed by `.`/`[` chain then `=`)
/// 3. `expr_with_semi` (any expression followed by `;`)
/// 4. `control_flow_stmt` (control flow NOT followed by `}` - mid-block)
/// 5. `final_expr` (any expression followed by `}` - end of block)
///
/// The assignment parser is tried before general expressions because `x = 5;`
/// could otherwise be misparsed as expression `x` followed by unexpected `=`.
fn block_item_parser<'src, I>(expr: GruelParser<'src, I, Expr>) -> GruelParser<'src, I, BlockItem>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Let statement: `let x: T = expr;`
    // Always requires a trailing semicolon.
    let let_stmt = let_statement_parser(expr.clone()).map(BlockItem::Statement);

    // Assignment statement: `target = expr;`
    // Target can be: variable (`x`), field (`a.b.c`), or index (`a[i]`).
    // Always requires a trailing semicolon.
    let assign_stmt = assign_statement_parser(expr.clone()).map(BlockItem::Statement);

    // Expression-based item: parse expression ONCE, then decide based on what follows.
    // This avoids O(2^n) complexity from repeatedly re-parsing expressions with backtracking.
    //
    // After parsing an expression, we check the next token:
    // - `;` → expression statement (value discarded)
    // - `}` → final expression (block's return value)
    // - other → mid-block control flow (only valid for if/while/match/loop/break/continue/return)
    let expr_item = expr
        .then(
            choice((
                just(TokenKind::Semi).to(ExprFollower::Semi).boxed(),
                just(TokenKind::RBrace)
                    .rewind()
                    .to(ExprFollower::RBrace)
                    .boxed(),
                any().rewind().to(ExprFollower::Other).boxed(),
            ))
            .or(end().to(ExprFollower::End)),
        )
        .try_map(|(e, follower), span| match follower {
            ExprFollower::Semi => {
                // Expression followed by semicolon: `expr;`
                Ok(BlockItem::Statement(Statement::Expr(e)))
            }
            ExprFollower::RBrace => {
                // Final expression: `{ ... expr }`
                Ok(BlockItem::Expr(e))
            }
            ExprFollower::Other | ExprFollower::End => {
                // Mid-block control flow (no semicolon, not at end)
                // Only control flow expressions are valid here
                if is_control_flow_expr(&e) {
                    Ok(BlockItem::Statement(Statement::Expr(e)))
                } else {
                    Err(Rich::custom(span, "expected semicolon after expression"))
                }
            }
        });

    // Try parsers in order. Earlier parsers take precedence.
    // This order ensures:
    // - Keywords (`let`) are matched before being parsed as identifiers
    // - Assignments (`x = 5;`) are matched before `x` is parsed as an expression
    choice((let_stmt.boxed(), assign_stmt.boxed(), expr_item.boxed())).boxed()
}

/// Process block items into statements and final expression
fn process_block_items(items: Vec<BlockItem>, block_span: Span) -> (Vec<Statement>, Expr) {
    let mut statements = Vec::new();
    let mut final_expr = None;

    for item in items {
        match item {
            BlockItem::Statement(stmt) => {
                // Had a non-semicolon expr before, but now we have more items
                // This shouldn't happen with correct grammar, but handle gracefully
                if let Some(e) = final_expr.take() {
                    statements.push(Statement::Expr(e));
                }
                statements.push(stmt);
            }
            BlockItem::Expr(e) => {
                if let Some(prev) = final_expr.take() {
                    // Had a non-semicolon expr before this one - that's invalid
                    // but we'll treat the previous as a statement for error recovery
                    statements.push(Statement::Expr(prev));
                }
                final_expr = Some(e);
            }
        }
    }

    let expr = final_expr.unwrap_or_else(|| {
        // No explicit final expression. Check if the last statement is a diverging
        // expression (break, continue, return) - if so, promote it to the final
        // expression since it has type Never which coerces to any type.
        if let Some(Statement::Expr(e)) = statements.last()
            && is_diverging_expr(e)
        {
            // Safe to unwrap: we just checked last() is Some(Statement::Expr(_))
            let Statement::Expr(e) = statements.pop().unwrap() else {
                unreachable!()
            };
            return e;
        }
        // Fallback: use a unit expression (block produces unit type)
        Expr::Unit(UnitLit {
            span: Span::new(block_span.end, block_span.end),
        })
    });

    (statements, expr)
}

/// Parser for blocks that may end without a final expression (for if/while bodies)
fn maybe_unit_block_parser<'src, I>(
    expr: GruelParser<'src, I, Expr>,
) -> GruelParser<'src, I, BlockExpr>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    block_item_parser(expr)
        .repeated()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace))
        .map_with(|items, e| {
            let span = span_from_extra(e);
            let (statements, final_expr) = process_block_items(items, span);
            BlockExpr {
                statements,
                expr: Box::new(final_expr),
                span,
            }
        })
        .boxed()
}

/// Parser for blocks that require a final expression: { statements... expr }
fn block_parser<'src, I>(expr: GruelParser<'src, I, Expr>) -> GruelParser<'src, I, Expr>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    block_item_parser(expr)
        .repeated()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace))
        .map_with(|items, e| {
            let span = span_from_extra(e);
            let (statements, final_expr) = process_block_items(items, span);
            Expr::Block(BlockExpr {
                statements,
                expr: Box::new(final_expr),
                span,
            })
        })
        .boxed()
}

/// Parser for function definitions: [@directive]* [pub] [unchecked] fn name(params) -> Type { body }
fn function_parser<'src, I>() -> GruelParser<'src, I, Function>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();

    // Parse optional visibility (pub keyword)
    let visibility = just(TokenKind::Pub).or_not().map(|opt| {
        if opt.is_some() {
            Visibility::Public
        } else {
            Visibility::Private
        }
    });

    // Box after 2 thens to keep the accumulated type short.
    let fn_head: GruelParser<I, (Directives, Visibility, bool)> = directives_parser()
        .then(visibility)
        .then(just(TokenKind::Unchecked).or_not())
        .map(|((d, v), u)| (d, v, u.is_some()))
        .boxed();

    let fn_sig: GruelParser<I, (Ident, Vec<Param>)> = just(TokenKind::Fn)
        .ignore_then(ident_parser())
        .then(params_parser().delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)))
        .boxed();

    fn_head
        .then(fn_sig)
        .then(just(TokenKind::Arrow).ignore_then(type_parser()).or_not())
        .then(block_parser(expr))
        .map_with(
            |((((directives, visibility, is_unchecked), (name, params)), return_type), body), e| {
                Function {
                    directives,
                    visibility,
                    is_unchecked,
                    name,
                    params,
                    return_type,
                    body,
                    span: span_from_extra(e),
                }
            },
        )
        .boxed()
}

/// Parser for struct definitions with inline methods:
/// [@directive]* [pub] [linear] struct Name { field: Type, ... fn method(self) { ... } }
///
/// Fields come first (comma-separated), then methods (no separators needed).
fn struct_parser<'src, I>() -> GruelParser<'src, I, StructDecl>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Parse optional visibility (pub keyword)
    let visibility = just(TokenKind::Pub).or_not().map(|opt| {
        if opt.is_some() {
            Visibility::Public
        } else {
            Visibility::Private
        }
    });

    // Box the struct header after 3 thens to keep the accumulated type short.
    let struct_head: GruelParser<I, (Directives, Visibility, bool, Ident)> = directives_parser()
        .then(visibility)
        .then(just(TokenKind::Linear).or_not())
        .then(just(TokenKind::Struct).ignore_then(ident_parser()))
        .map(|(((d, v), l), name)| (d, v, l.is_some(), name))
        .boxed();

    // Box the struct body so DelimitedBy wraps a short Boxed type.
    let struct_body: GruelParser<I, (Vec<FieldDecl>, Vec<Method>)> = field_decls_parser()
        .then(method_parser().repeated().collect::<Vec<_>>())
        .boxed();

    struct_head
        .then(struct_body.delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)))
        .map_with(
            |((directives, visibility, is_linear, name), (fields, methods)), e| StructDecl {
                directives,
                visibility,
                is_linear,
                name,
                fields,
                methods,
                span: span_from_extra(e),
            },
        )
        .boxed()
}

/// Parser for enum variant: unit, tuple `(Type, ...)`, or struct `{ field: Type, ... }`
fn enum_variant_parser<'src, I>() -> GruelParser<'src, I, EnumVariant>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    use crate::ast::{EnumVariantField, EnumVariantKind};

    // Tuple-style fields: (Type, Type, ...)
    let tuple_fields = type_parser()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen));

    // Struct-style fields: { name: Type, name: Type, ... }
    let struct_field = ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(type_parser())
        .map_with(|(name, ty), e| EnumVariantField {
            name,
            ty,
            span: span_from_extra(e),
        });
    let struct_fields = struct_field
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace));

    // Combine: name + optional (tuple | struct) fields
    let variant_kind = choice((
        tuple_fields.map(EnumVariantKind::Tuple),
        struct_fields.map(EnumVariantKind::Struct),
    ))
    .or_not()
    .map(|opt| opt.unwrap_or(EnumVariantKind::Unit));

    ident_parser()
        .then(variant_kind)
        .map_with(|(name, kind), e| EnumVariant {
            name,
            kind,
            span: span_from_extra(e),
        })
        .boxed()
}

/// Parser for comma-separated enum variants
fn enum_variants_parser<'src, I>() -> GruelParser<'src, I, Vec<EnumVariant>>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    enum_variant_parser()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .boxed()
}

/// Parser for enum definitions: [pub] enum Name { Variant1, Variant2, ... }
fn enum_parser<'src, I>() -> GruelParser<'src, I, EnumDecl>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Parse optional visibility (pub keyword)
    let visibility = just(TokenKind::Pub).or_not().map(|opt| {
        if opt.is_some() {
            Visibility::Public
        } else {
            Visibility::Private
        }
    });

    visibility
        .then(just(TokenKind::Enum).ignore_then(ident_parser()))
        .then(enum_variants_parser().delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)))
        .map_with(|((visibility, name), variants), e| EnumDecl {
            visibility,
            name,
            variants,
            span: span_from_extra(e),
        })
        .boxed()
}

/// Parser for method definitions: [@directive]* fn name(self, params) -> Type { body }
/// Methods differ from functions in that they can have `self` as the first parameter.
fn method_parser<'src, I>() -> GruelParser<'src, I, Method>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();
    method_parser_with_expr(expr)
}

/// Parser for method definitions inside anonymous structs.
/// Takes an expression parser as a parameter to avoid creating a new one.
fn anon_struct_method_parser<'src, I>(
    expr: GruelParser<'src, I, Expr>,
) -> GruelParser<'src, I, Method>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    method_parser_with_expr(expr)
}

/// Shared implementation for method parsing.
/// Takes an expression parser to allow reuse from different contexts.
fn method_parser_with_expr<'src, I>(
    expr: GruelParser<'src, I, Expr>,
) -> GruelParser<'src, I, Method>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Parse optional self parameter
    let self_param = just(TokenKind::SelfValue).map_with(|_, e| SelfParam {
        span: span_from_extra(e),
    });

    // Parse self followed by optional regular params
    let self_then_params = self_param
        .then(
            just(TokenKind::Comma)
                .ignore_then(params_parser())
                .or_not()
                .map(|opt| opt.unwrap_or_default()),
        )
        .map(|(self_param, params)| (Some(self_param), params));

    // Parse just regular params (no self) - this is an associated function
    let just_params = params_parser().map(|params| (None, params));

    // Box params_with_optional_self so DelimitedBy wraps a short type.
    let params_with_optional_self: GruelParser<I, (Option<SelfParam>, Vec<Param>)> =
        choice((self_then_params.boxed(), just_params.boxed())).boxed();

    // Box the method head early to keep subsequent type accumulation short.
    let method_head: GruelParser<I, (Directives, Ident)> = directives_parser()
        .then(just(TokenKind::Fn).ignore_then(ident_parser()))
        .boxed();

    let method_params: GruelParser<I, (Option<SelfParam>, Vec<Param>)> = params_with_optional_self
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
        .boxed();

    let method_return: GruelParser<I, Option<TypeExpr>> = just(TokenKind::Arrow)
        .ignore_then(type_parser())
        .or_not()
        .boxed();

    method_head
        .then(method_params)
        .then(method_return)
        .then(block_parser(expr))
        .map_with(
            |((((directives, name), (receiver, params)), return_type), body), e| Method {
                directives,
                name,
                receiver,
                params,
                return_type,
                body,
                span: span_from_extra(e),
            },
        )
        .boxed()
}

/// Parser for drop fn declarations: drop fn TypeName(self) { body }
fn drop_fn_parser<'src, I>() -> GruelParser<'src, I, DropFn>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();

    // Parse self parameter
    let self_param = just(TokenKind::SelfValue).map_with(|_, e| SelfParam {
        span: span_from_extra(e),
    });

    // NOTE: Box after accumulating the head to keep the final MapWith type short.
    let drop_fn_sig: GruelParser<'src, I, (Ident, SelfParam)> = just(TokenKind::Drop)
        .ignore_then(just(TokenKind::Fn))
        .ignore_then(ident_parser())
        .then(self_param.delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)))
        .boxed();

    drop_fn_sig
        .then(block_parser(expr))
        .map_with(|((type_name, self_param), body), e| DropFn {
            type_name,
            self_param,
            body,
            span: span_from_extra(e),
        })
        .boxed()
}

/// Parser for const declarations: [pub] const name [: Type] = expr;
///
/// Used for module re-exports:
/// ```gruel
/// pub const strings = @import("utils/strings.gruel");
/// pub const helper = @import("utils/internal.gruel").helper;
/// ```
fn const_parser<'src, I>() -> GruelParser<'src, I, ConstDecl>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();

    // Parse optional visibility (pub keyword)
    let visibility = just(TokenKind::Pub).or_not().map(|opt| {
        if opt.is_some() {
            Visibility::Public
        } else {
            Visibility::Private
        }
    });

    // Box after 2 thens to keep accumulated type short.
    let const_head: GruelParser<I, (Directives, Visibility, Ident)> = directives_parser()
        .then(visibility)
        .then(just(TokenKind::Const).ignore_then(ident_parser()))
        .map(|((d, v), n)| (d, v, n))
        .boxed();

    let const_tail: GruelParser<I, (Option<TypeExpr>, Expr)> = just(TokenKind::Colon)
        .ignore_then(type_parser())
        .or_not()
        .then(just(TokenKind::Eq).ignore_then(expr))
        .then_ignore(just(TokenKind::Semi))
        .boxed();

    const_head
        .then(const_tail)
        .map_with(
            |((directives, visibility, name), (ty, init)), e| ConstDecl {
                directives,
                visibility,
                name,
                ty,
                init: Box::new(init),
                span: span_from_extra(e),
            },
        )
        .boxed()
}

/// Parser for top-level items (functions, structs, enums, drop fns, and consts)
fn item_parser<'src, I>() -> GruelParser<'src, I, Item>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        function_parser().map(Item::Function).boxed(),
        struct_parser().map(Item::Struct).boxed(),
        enum_parser().map(Item::Enum).boxed(),
        drop_fn_parser().map(Item::DropFn).boxed(),
        const_parser().map(Item::Const).boxed(),
    ))
    .boxed()
}

/// Parser that matches tokens that can start an item (for recovery).
/// This is a "lookahead" - it peeks but doesn't consume.
fn item_start<'src, I>() -> GruelParser<'src, I, ()>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // NOTE: Split into sub-groups to keep Choice<tuple> symbol length < 4K.
    // 9 elements of Boxed<I,(),E> in one tuple wrapped in Rewind would produce ~5K symbols on macOS.
    let item_start_a: GruelParser<'src, I, ()> = choice((
        just(TokenKind::Fn).ignored().boxed(),
        just(TokenKind::Struct).ignored().boxed(),
        just(TokenKind::Enum).ignored().boxed(),
        just(TokenKind::Drop).ignored().boxed(),
        just(TokenKind::Const).ignored().boxed(),
    ))
    .boxed();
    let item_start_b: GruelParser<'src, I, ()> = choice((
        just(TokenKind::Pub).ignored().boxed(),
        just(TokenKind::Linear).ignored().boxed(),
        just(TokenKind::Unchecked).ignored().boxed(),
        just(TokenKind::At).ignored().boxed(), // For @directives
    ))
    .boxed();
    choice((item_start_a, item_start_b))
        .rewind() // Peek without consuming
        .boxed()
}

/// Recovery parser that skips tokens until finding an item start.
/// Consumes at least one token to guarantee progress, then skips until
/// we find a token that could start an item.
fn error_recovery<'src, I>() -> GruelParser<'src, I, Item>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Skip at least one token to make progress, capturing the span
    any()
        .map_with(|_, extra| extra.span())
        // Then skip any more tokens that don't start an item
        .then(
            any()
                .and_is(item_start().not())
                .repeated()
                .collect::<Vec<_>>(),
        )
        .map(|(start_span, _): (SimpleSpan, Vec<TokenKind>)| {
            // Convert SimpleSpan to Span
            Item::Error(to_gruel_span(start_span))
        })
        .boxed()
}

/// Parser for top-level items with error recovery.
/// When an item fails to parse, we skip tokens until we find the start of
/// another item, emit an Error node, and continue parsing.
fn item_with_recovery<'src, I>() -> GruelParser<'src, I, Item>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    item_parser()
        .recover_with(via_parser(error_recovery()))
        .boxed()
}

/// Main parser that produces an AST
fn ast_parser<'src, I>() -> GruelParser<'src, I, Ast>
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    item_with_recovery()
        .repeated()
        .collect::<Vec<_>>()
        .then_ignore(end())
        .map(|items| Ast { items })
        .boxed()
}

/// Format a RichPattern for display in error messages.
fn format_pattern(pattern: &chumsky::error::RichPattern<'_, TokenKind>) -> String {
    use chumsky::error::RichPattern;
    match pattern {
        RichPattern::Token(tok) => tok.name().to_string(),
        RichPattern::Label(label) => label.to_string(),
        RichPattern::Identifier(ident) => format!("'{}'", ident),
        RichPattern::Any => "any token".to_string(),
        RichPattern::SomethingElse => "something else".to_string(),
        RichPattern::EndOfInput => "end of input".to_string(),
    }
}

/// Convert chumsky Rich error to CompileError, preserving rich context.
fn convert_error(err: Rich<'_, TokenKind>) -> CompileError {
    let span = to_gruel_span(*err.span());

    // Build the base error from the reason
    let mut error = match err.reason() {
        chumsky::error::RichReason::ExpectedFound { expected, found } => {
            let expected_str: Cow<'static, str> = if expected.is_empty() {
                Cow::Borrowed("something")
            } else {
                Cow::Owned(
                    expected
                        .iter()
                        .take(3) // Limit to first 3 for readability
                        .map(format_pattern)
                        .collect::<Vec<_>>()
                        .join(" or "),
                )
            };

            let found_str: Cow<'static, str> = match found.as_ref() {
                Some(t) => Cow::Owned(t.name().to_string()),
                None => Cow::Borrowed("end of file"),
            };

            CompileError::new(
                ErrorKind::UnexpectedToken {
                    expected: expected_str,
                    found: found_str,
                },
                span,
            )
        }
        chumsky::error::RichReason::Custom(msg) => {
            // Preserve the custom error message directly
            CompileError::new(ErrorKind::ParseError(msg.clone()), span)
        }
    };

    // Add labelled contexts as secondary labels
    for (pattern, ctx_span) in err.contexts() {
        let label_msg = format!("while parsing {}", format_pattern(pattern));
        let label_span = to_gruel_span(*ctx_span);
        error = error.with_label(label_msg, label_span);
    }

    error
}

/// Chumsky-based parser that converts tokens into an AST.
pub struct ChumskyParser {
    tokens: Vec<(TokenKind, SimpleSpan)>,
    source_len: usize,
    interner: ThreadedRodeo,
    /// File ID for spans in this file.
    file_id: FileId,
    /// Preview features enabled for this parse (ADR-0005, ADR-0049).
    ///
    /// The parser always accepts the full grammar (including preview syntax); a
    /// post-parse validation pass emits errors if preview-only syntax is used
    /// without the corresponding flag.
    preview_features: PreviewFeatures,
}

impl ChumskyParser {
    /// Create a new parser from tokens and an interner produced by the lexer.
    pub fn new(tokens: Vec<gruel_lexer::Token>, interner: ThreadedRodeo) -> Self {
        let source_len = tokens.last().map(|t| t.span.end as usize).unwrap_or(0);
        // Extract file_id from the first token (all tokens in a file have the same file_id)
        let file_id = tokens
            .first()
            .map(|t| t.span.file_id)
            .unwrap_or(FileId::DEFAULT);

        let spanned_tokens: Vec<(TokenKind, SimpleSpan)> = tokens
            .into_iter()
            .filter(|t| t.kind != TokenKind::Eof) // Remove EOF, chumsky handles end differently
            .map(|t| {
                (
                    t.kind,
                    SimpleSpan::new(t.span.start as usize, t.span.end as usize),
                )
            })
            .collect();
        Self {
            tokens: spanned_tokens,
            source_len,
            interner,
            file_id,
            preview_features: PreviewFeatures::new(),
        }
    }

    /// Set the preview features enabled for this parse. Required to use any
    /// syntax gated behind a `--preview` flag (ADR-0005).
    pub fn with_preview_features(mut self, features: PreviewFeatures) -> Self {
        self.preview_features = features;
        self
    }

    /// Parse the tokens into an AST, returning the AST and the interner.
    ///
    /// Returns all parse errors if parsing fails, not just the first one.
    pub fn parse(mut self) -> MultiErrorResult<(Ast, ThreadedRodeo)> {
        // Pre-intern primitive type symbols and create parser state with file ID
        let syms = PrimitiveTypeSpurs::new(&mut self.interner);
        let parser_state = ParserState::new(syms, self.file_id);
        let mut state = SimpleState(parser_state);

        // Create a stream from the token iterator
        let token_iter = self.tokens.iter().cloned();
        let stream = Stream::from_iter(token_iter);

        // Map the stream to split (Token, Span) tuples
        let eoi: SimpleSpan = (self.source_len..self.source_len).into();
        let mapped = stream.map(eoi, |(tok, span)| (tok, span));

        let ast = ast_parser()
            .parse_with_state(mapped, &mut state)
            .into_result()
            .map_err(|errs| {
                let errors: Vec<CompileError> = errs.into_iter().map(convert_error).collect();
                CompileErrors::from(errors)
            })?;

        // Post-parse preview-feature validation.
        let mut preview_errors = Vec::new();
        validate_preview_patterns(&ast, &self.preview_features, &mut preview_errors);
        if !preview_errors.is_empty() {
            return Err(CompileErrors::from(preview_errors));
        }

        Ok((ast, self.interner))
    }
}

/// Walk the AST and emit errors for preview-only pattern syntax when the
/// corresponding preview feature isn't enabled (ADR-0049).
fn validate_preview_patterns(
    ast: &Ast,
    features: &PreviewFeatures,
    errors: &mut Vec<CompileError>,
) {
    if features.contains(&PreviewFeature::NestedPatterns) {
        return;
    }
    for item in &ast.items {
        validate_item(item, errors);
    }
}

fn validate_item(item: &Item, errors: &mut Vec<CompileError>) {
    match item {
        Item::Function(f) => validate_expr(&f.body, errors),
        Item::Struct(s) => {
            for m in &s.methods {
                validate_expr(&m.body, errors);
            }
        }
        Item::Enum(e) => {
            // EnumDecl doesn't carry methods in the AST; methods live on a wrapper.
            // Walk variant types (nothing to validate) and move on.
            let _ = e;
        }
        Item::Const(c) => validate_expr(&c.init, errors),
        Item::DropFn(d) => validate_expr(&d.body, errors),
        Item::Error(_) => {}
    }
}

fn validate_block(block: &crate::ast::BlockExpr, errors: &mut Vec<CompileError>) {
    use crate::ast::AssignTarget;
    for stmt in &block.statements {
        match stmt {
            Statement::Let(l) => {
                check_let_pattern(&l.pattern, errors);
                validate_expr(&l.init, errors);
            }
            Statement::Assign(a) => {
                validate_expr(&a.value, errors);
                match &a.target {
                    AssignTarget::Var(_) => {}
                    AssignTarget::Field(f) => validate_expr(&f.base, errors),
                    AssignTarget::Index(i) => {
                        validate_expr(&i.base, errors);
                        validate_expr(&i.index, errors);
                    }
                }
            }
            Statement::Expr(e) => validate_expr(e, errors),
        }
    }
    validate_expr(&block.expr, errors);
}

fn validate_expr(expr: &Expr, errors: &mut Vec<CompileError>) {
    use crate::ast::IntrinsicArg;
    match expr {
        Expr::Block(b) => validate_block(b, errors),
        Expr::Match(m) => {
            validate_expr(&m.scrutinee, errors);
            for arm in &m.arms {
                check_match_pattern(&arm.pattern, errors);
                validate_expr(&arm.body, errors);
            }
        }
        Expr::If(i) => {
            validate_expr(&i.cond, errors);
            validate_block(&i.then_block, errors);
            if let Some(e) = &i.else_block {
                validate_block(e, errors);
            }
        }
        Expr::While(w) => {
            validate_expr(&w.cond, errors);
            validate_block(&w.body, errors);
        }
        Expr::For(f) => {
            validate_expr(&f.iterable, errors);
            validate_block(&f.body, errors);
        }
        Expr::Loop(l) => validate_block(&l.body, errors),
        Expr::Binary(b) => {
            validate_expr(&b.left, errors);
            validate_expr(&b.right, errors);
        }
        Expr::Unary(u) => validate_expr(&u.operand, errors),
        Expr::Call(c) => {
            for arg in &c.args {
                validate_expr(&arg.expr, errors);
            }
        }
        Expr::IntrinsicCall(i) => {
            for arg in &i.args {
                if let IntrinsicArg::Expr(e) = arg {
                    validate_expr(e, errors);
                }
            }
        }
        Expr::MethodCall(m) => {
            validate_expr(&m.receiver, errors);
            for arg in &m.args {
                validate_expr(&arg.expr, errors);
            }
        }
        Expr::AssocFnCall(a) => {
            for arg in &a.args {
                validate_expr(&arg.expr, errors);
            }
        }
        Expr::Field(f) => validate_expr(&f.base, errors),
        Expr::TupleIndex(t) => validate_expr(&t.base, errors),
        Expr::Index(i) => {
            validate_expr(&i.base, errors);
            validate_expr(&i.index, errors);
        }
        Expr::Return(r) => {
            if let Some(v) = &r.value {
                validate_expr(v, errors);
            }
        }
        Expr::Paren(p) => validate_expr(&p.inner, errors),
        Expr::StructLit(s) => {
            for f in &s.fields {
                validate_expr(&f.value, errors);
            }
        }
        Expr::EnumStructLit(s) => {
            for f in &s.fields {
                validate_expr(&f.value, errors);
            }
        }
        Expr::ArrayLit(a) => {
            for e in &a.elements {
                validate_expr(e, errors);
            }
        }
        Expr::Tuple(t) => {
            for e in &t.elems {
                validate_expr(e, errors);
            }
        }
        Expr::Comptime(c) => validate_expr(&c.expr, errors),
        Expr::ComptimeUnrollFor(_) | Expr::Checked(_) => {
            // These contain expressions but were not previously validated for patterns.
            // Any match/let inside is reachable through normal expression traversal if
            // they choose to expose it; keep conservative for now.
        }
        Expr::TypeLit(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Bool(_)
        | Expr::Unit(_)
        | Expr::Ident(_)
        | Expr::Path(_)
        | Expr::SelfExpr(_)
        | Expr::Break(_)
        | Expr::Continue(_)
        | Expr::Error(_) => {}
    }
}

fn check_let_pattern(pat: &Pattern, errors: &mut Vec<CompileError>) {
    // In a let, the legal flat shapes are: Wildcard, Ident, Struct (flat), Tuple (flat).
    // Anything else — or nested sub-patterns / rest — is nested-patterns territory.
    match pat {
        Pattern::Wildcard(_) | Pattern::Ident { .. } => {}
        Pattern::Struct { fields, .. } => {
            for fp in fields {
                check_flat_field_pattern(fp, errors);
            }
        }
        Pattern::Tuple { elems, .. } => {
            for e in elems {
                check_flat_tuple_elem(e, errors);
            }
        }
        // Pattern forms that are refutable can't appear in let anyway; sema will
        // reject them in Phase 3. Don't duplicate that error here.
        _ => {}
    }
}

fn check_flat_field_pattern(fp: &FieldPattern, errors: &mut Vec<CompileError>) {
    match (&fp.field_name, &fp.sub) {
        (None, _) => errors.push(preview_required_err(
            fp.span,
            "rest pattern `..` in struct destructure",
        )),
        (Some(_), None) => {}
        (Some(_), Some(Pattern::Wildcard(_))) => {}
        (Some(_), Some(Pattern::Ident { .. })) => {}
        (Some(_), Some(other)) => errors.push(preview_required_err(
            other.span(),
            "nested sub-pattern in struct destructure",
        )),
    }
}

fn check_flat_tuple_elem(elem: &TupleElemPattern, errors: &mut Vec<CompileError>) {
    match elem {
        TupleElemPattern::Rest(span) => {
            errors.push(preview_required_err(*span, "rest pattern `..` in tuple"));
        }
        TupleElemPattern::Pattern(Pattern::Wildcard(_)) => {}
        TupleElemPattern::Pattern(Pattern::Ident { .. }) => {}
        TupleElemPattern::Pattern(other) => errors.push(preview_required_err(
            other.span(),
            "nested sub-pattern in tuple destructure",
        )),
    }
}

fn check_match_pattern(pat: &Pattern, errors: &mut Vec<CompileError>) {
    // Match arms previously accepted: Wildcard, Int, NegInt, Bool, Path,
    // DataVariant/StructVariant with flat bindings. Reject tuple patterns
    // and nested sub-patterns when the preview flag is off.
    match pat {
        Pattern::Tuple { span, .. } => {
            errors.push(preview_required_err(*span, "tuple pattern in match arm"))
        }
        Pattern::Struct { span, .. } => errors.push(preview_required_err(
            *span,
            "struct destructure pattern in match arm",
        )),
        Pattern::DataVariant { fields, .. } => {
            for e in fields {
                check_flat_tuple_elem(e, errors);
            }
        }
        Pattern::StructVariant { fields, .. } => {
            for fp in fields {
                check_flat_field_pattern(fp, errors);
            }
        }
        Pattern::Wildcard(_)
        | Pattern::Ident { .. }
        | Pattern::Int(_)
        | Pattern::NegInt(_)
        | Pattern::Bool(_)
        | Pattern::Path(_) => {}
    }
}

fn preview_required_err(span: Span, what: &str) -> CompileError {
    CompileError::new(
        ErrorKind::PreviewFeatureRequired {
            feature: PreviewFeature::NestedPatterns,
            what: format!("{} (ADR-0049)", what),
        },
        span,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gruel_lexer::Lexer;

    /// Result type for parsing that includes both the AST and interner.
    /// Provides convenient access to both the parsed AST and symbol resolution.
    #[derive(Debug)]
    struct ParseResult {
        ast: Ast,
        interner: ThreadedRodeo,
    }

    impl ParseResult {
        /// Get the string for a symbol.
        fn get(&self, sym: Spur) -> &str {
            self.interner.resolve(&sym)
        }
    }

    /// Result type for expression parsing that includes both the expr and interner.
    #[derive(Debug)]
    struct ExprResult {
        expr: Expr,
        interner: ThreadedRodeo,
    }

    impl ExprResult {
        /// Get the string for a symbol.
        fn get(&self, sym: Spur) -> &str {
            self.interner.resolve(&sym)
        }
    }

    fn parse(source: &str) -> MultiErrorResult<ParseResult> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from)?;
        let parser = ChumskyParser::new(tokens, interner);
        let (ast, interner) = parser.parse()?;
        Ok(ParseResult { ast, interner })
    }

    fn parse_expr(source: &str) -> MultiErrorResult<ExprResult> {
        let result = parse(&format!("fn main() -> i32 {{ {} }}", source))?;
        let interner = result.interner;
        let expr = match result.ast.items.into_iter().next().unwrap() {
            Item::Function(f) => match f.body {
                Expr::Block(block) => *block.expr,
                other => other,
            },
            Item::Struct(_) => panic!("parse_expr helper should only be used with functions"),
            Item::Enum(_) => panic!("parse_expr helper should only be used with functions"),
            Item::DropFn(_) => panic!("parse_expr helper should only be used with functions"),
            Item::Const(_) => panic!("parse_expr helper should only be used with functions"),
            Item::Error(_) => panic!("parse_expr helper should only be used with functions"),
        };
        Ok(ExprResult { expr, interner })
    }

    #[test]
    fn test_chumsky_parse_main() {
        let result = parse("fn main() -> i32 { 42 }").unwrap();

        assert_eq!(result.ast.items.len(), 1);
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(result.get(f.name.name), "main");
                match f.return_type.as_ref().unwrap() {
                    TypeExpr::Named(ident) => assert_eq!(result.get(ident.name), "i32"),
                    _ => panic!("expected Named type"),
                }
                match &f.body {
                    Expr::Block(block) => match block.expr.as_ref() {
                        Expr::Int(lit) => assert_eq!(lit.value, 42),
                        _ => panic!("expected Int"),
                    },
                    _ => panic!("expected Block"),
                }
            }
            Item::Struct(_) => panic!("expected Function"),
            Item::Enum(_) => panic!("expected Function"),
            Item::DropFn(_) => panic!("expected Function"),
            Item::Const(_) => panic!("expected Function"),
            Item::Error(_) => panic!("expected Function"),
        }
    }

    #[test]
    fn test_chumsky_parse_addition() {
        let result = parse_expr("1 + 2").unwrap();
        match result.expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::Add));
                match (*bin.left, *bin.right) {
                    (Expr::Int(l), Expr::Int(r)) => {
                        assert_eq!(l.value, 1);
                        assert_eq!(r.value, 2);
                    }
                    _ => panic!("expected Int operands"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_chumsky_parse_precedence() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3)
        let result = parse_expr("1 + 2 * 3").unwrap();
        match result.expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::Add));
                match *bin.left {
                    Expr::Int(l) => assert_eq!(l.value, 1),
                    _ => panic!("expected Int"),
                }
                match *bin.right {
                    Expr::Binary(inner) => {
                        assert!(matches!(inner.op, BinaryOp::Mul));
                    }
                    _ => panic!("expected Binary"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_chumsky_parse_let_binding() {
        let result = parse("fn main() -> i32 { let x = 42; x }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => {
                    assert_eq!(block.statements.len(), 1);
                    match &block.statements[0] {
                        Statement::Let(let_stmt) => {
                            assert!(!let_stmt.is_mut);
                            match &let_stmt.pattern {
                                Pattern::Ident { name, .. } => {
                                    assert_eq!(result.get(name.name), "x")
                                }
                                other => panic!("expected Ident, got {:?}", other),
                            }
                        }
                        _ => panic!("expected Let"),
                    }
                }
                _ => panic!("expected Block"),
            },
            Item::Struct(_) => panic!("expected Function"),
            Item::Enum(_) => panic!("expected Function"),
            Item::DropFn(_) => panic!("expected Function"),
            Item::Const(_) => panic!("expected Function"),
            Item::Error(_) => panic!("expected Function"),
        }
    }

    #[test]
    fn test_while_simple() {
        // Simplest while case
        let result = parse("fn main() -> i32 { while true { } 0 }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
    }

    #[test]
    fn test_while_with_statement() {
        // While with assignment
        let result = parse("fn main() -> i32 { while true { x = 1; } 0 }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
    }

    #[test]
    fn test_function_calls() {
        let result =
            parse("fn add(a: i32, b: i32) -> i32 { a + b } fn main() -> i32 { add(1, 2) }")
                .unwrap();
        assert_eq!(result.ast.items.len(), 2);
    }

    #[test]
    fn test_if_else() {
        let result = parse("fn main() -> i32 { if true { 1 } else { 0 } }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
    }

    #[test]
    fn test_nested_control_flow() {
        let result =
            parse("fn main() -> i32 { let mut x = 0; while x < 10 { x = x + 1; } x }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
    }

    // ==================== Struct Method Parsing Tests ====================

    #[test]
    fn test_struct_with_single_method() {
        let result = parse("struct Point { x: i32, fn get_x(self) -> i32 { self.x } }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                assert_eq!(result.get(struct_decl.name.name), "Point");
                assert_eq!(struct_decl.methods.len(), 1);
                let method = &struct_decl.methods[0];
                assert_eq!(result.get(method.name.name), "get_x");
                assert!(method.receiver.is_some()); // has self
                assert!(method.params.is_empty()); // no additional params
            }
            _ => panic!("expected Struct"),
        }
    }

    #[test]
    fn test_struct_method_with_params() {
        let result =
            parse("struct Point { x: i32, fn add(self, n: i32) -> i32 { self.x + n } }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                let method = &struct_decl.methods[0];
                assert_eq!(result.get(method.name.name), "add");
                assert!(method.receiver.is_some());
                assert_eq!(method.params.len(), 1);
                assert_eq!(result.get(method.params[0].name.name), "n");
            }
            _ => panic!("expected Struct"),
        }
    }

    #[test]
    fn test_struct_associated_function() {
        // Associated function (no self)
        let result = parse(
            "struct Point { x: i32, y: i32, fn new(x: i32, y: i32) -> Point { Point { x: x, y: y } } }",
        )
        .unwrap();
        assert_eq!(result.ast.items.len(), 1);
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                let method = &struct_decl.methods[0];
                assert_eq!(result.get(method.name.name), "new");
                assert!(method.receiver.is_none()); // no self
                assert_eq!(method.params.len(), 2);
            }
            _ => panic!("expected Struct"),
        }
    }

    #[test]
    fn test_struct_multiple_methods() {
        let result = parse(
            "struct Counter {
                 value: i32,
                 fn new() -> Counter { Counter { value: 0 } }
                 fn get(self) -> i32 { self.value }
                 fn increment(self) -> i32 { self.value + 1 }
             }",
        )
        .unwrap();
        assert_eq!(result.ast.items.len(), 1);
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                assert_eq!(struct_decl.methods.len(), 3);
                // First is associated function (no self)
                assert!(struct_decl.methods[0].receiver.is_none());
                assert_eq!(result.get(struct_decl.methods[0].name.name), "new");
                // Second is method (has self)
                assert!(struct_decl.methods[1].receiver.is_some());
                assert_eq!(result.get(struct_decl.methods[1].name.name), "get");
                // Third is method (has self)
                assert!(struct_decl.methods[2].receiver.is_some());
                assert_eq!(result.get(struct_decl.methods[2].name.name), "increment");
            }
            _ => panic!("expected Struct"),
        }
    }

    #[test]
    fn test_struct_method_with_directive() {
        let result = parse("struct Foo { @inline fn bar(self) -> i32 { 42 } }").unwrap();
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                let method = &struct_decl.methods[0];
                assert_eq!(method.directives.len(), 1);
                assert_eq!(result.get(method.directives[0].name.name), "inline");
            }
            _ => panic!("expected Struct"),
        }
    }

    // ==================== Method Call Parsing Tests ====================

    #[test]
    fn test_method_call_simple() {
        let result = parse_expr("x.foo()").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(result.get(call.method.name), "foo");
                assert!(call.args.is_empty());
                match call.receiver.as_ref() {
                    Expr::Ident(ident) => assert_eq!(result.get(ident.name), "x"),
                    _ => panic!("expected Ident receiver"),
                }
            }
            _ => panic!("expected MethodCall, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_method_call_with_args() {
        let result = parse_expr("point.add(5, 10)").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(result.get(call.method.name), "add");
                assert_eq!(call.args.len(), 2);
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_method_call_chained() {
        let result = parse_expr("x.foo().bar()").unwrap();
        match &result.expr {
            Expr::MethodCall(outer) => {
                assert_eq!(result.get(outer.method.name), "bar");
                match outer.receiver.as_ref() {
                    Expr::MethodCall(inner) => {
                        assert_eq!(result.get(inner.method.name), "foo");
                    }
                    _ => panic!("expected inner MethodCall"),
                }
            }
            _ => panic!("expected outer MethodCall"),
        }
    }

    #[test]
    fn test_method_call_on_field_access() {
        let result = parse_expr("obj.field.method()").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(result.get(call.method.name), "method");
                match call.receiver.as_ref() {
                    Expr::Field(field) => {
                        assert_eq!(result.get(field.field.name), "field");
                    }
                    _ => panic!("expected Field receiver"),
                }
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_field_access_not_method_call() {
        // .field (not followed by parens) should parse as FieldExpr, not MethodCall
        let result = parse_expr("x.field").unwrap();
        match &result.expr {
            Expr::Field(field) => {
                assert_eq!(result.get(field.field.name), "field");
            }
            _ => panic!("expected Field, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_method_call_on_struct_literal() {
        let result =
            parse("struct Point { x: i32 } fn main() -> i32 { Point { x: 1 }.get() }").unwrap();
        match &result.ast.items[1] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::MethodCall(call) => {
                        assert_eq!(result.get(call.method.name), "get");
                        match call.receiver.as_ref() {
                            Expr::StructLit(lit) => {
                                assert_eq!(result.get(lit.name.name), "Point")
                            }
                            _ => panic!("expected StructLit receiver"),
                        }
                    }
                    _ => panic!("expected MethodCall"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_method_call_on_paren_expr() {
        let result = parse_expr("(x).method()").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(result.get(call.method.name), "method");
                match call.receiver.as_ref() {
                    Expr::Paren(_) => {}
                    _ => panic!("expected Paren receiver"),
                }
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_method_call_mixed_with_index() {
        // Complex chain: array[0].method().field
        let result = parse_expr("arr[0].get().value").unwrap();
        match &result.expr {
            Expr::Field(field) => {
                assert_eq!(result.get(field.field.name), "value");
                match field.base.as_ref() {
                    Expr::MethodCall(call) => {
                        assert_eq!(result.get(call.method.name), "get");
                        match call.receiver.as_ref() {
                            Expr::Index(_) => {}
                            _ => panic!("expected Index receiver"),
                        }
                    }
                    _ => panic!("expected MethodCall"),
                }
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn test_associated_function_call() {
        // Type::function(args) syntax
        let result = parse_expr("Point::new(1, 2)").unwrap();
        match &result.expr {
            Expr::AssocFnCall(call) => {
                assert_eq!(result.get(call.type_name.name), "Point");
                assert_eq!(result.get(call.function.name), "new");
                assert_eq!(call.args.len(), 2);
            }
            _ => panic!("expected AssocFnCall, got {:?}", result.expr),
        }
    }

    // ==================== Borrow Parameter Parsing Tests ====================

    #[test]
    fn test_borrow_param_simple() {
        // Function with a borrow parameter
        let result = parse("fn read(borrow x: i32) -> i32 { x }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                assert_eq!(result.get(f.params[0].name.name), "x");
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_borrow_param_with_struct_type() {
        // Borrow parameter with a user-defined type
        let result =
            parse("struct Point { x: i32 } fn read(borrow p: Point) -> i32 { p.x }").unwrap();
        match &result.ast.items[1] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                assert_eq!(result.get(f.params[0].name.name), "p");
                match &f.params[0].ty {
                    TypeExpr::Named(ident) => assert_eq!(result.get(ident.name), "Point"),
                    _ => panic!("expected Named type"),
                }
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_borrow_param_mixed_with_normal() {
        // Mixed borrow and normal parameters
        let result = parse("fn add(borrow a: i32, b: i32) -> i32 { a + b }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                assert_eq!(result.get(f.params[0].name.name), "a");
                assert_eq!(f.params[1].mode, ParamMode::Normal);
                assert_eq!(result.get(f.params[1].name.name), "b");
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_borrow_param_mixed_with_inout() {
        // Borrow and inout parameters in the same function
        let result = parse("fn modify(borrow a: i32, inout b: i32) { b = a; }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                assert_eq!(result.get(f.params[0].name.name), "a");
                assert_eq!(f.params[1].mode, ParamMode::Inout);
                assert_eq!(result.get(f.params[1].name.name), "b");
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_borrow_param_with_array_type() {
        // Borrow parameter with array type
        let result = parse("fn first(borrow arr: [i32; 3]) -> i32 { arr[0] }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                match &f.params[0].ty {
                    TypeExpr::Array { length, .. } => assert_eq!(*length, 3),
                    _ => panic!("expected Array type"),
                }
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_borrow_param_multiple() {
        // Multiple borrow parameters
        let result = parse("fn sum(borrow a: i32, borrow b: i32) -> i32 { a + b }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                assert_eq!(f.params[1].mode, ParamMode::Borrow);
            }
            _ => panic!("expected Function"),
        }
    }

    // ==================== Borrow Argument Parsing Tests ====================

    #[test]
    fn test_borrow_arg_simple() {
        // Function call with a borrow argument
        let result = parse_expr("read(borrow x)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert_eq!(result.get(call.name.name), "read");
                assert_eq!(call.args.len(), 1);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
                match &call.args[0].expr {
                    Expr::Ident(ident) => assert_eq!(result.get(ident.name), "x"),
                    _ => panic!("expected Ident argument"),
                }
            }
            _ => panic!("expected Call, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_borrow_arg_mixed_with_normal() {
        // Mixed borrow and normal arguments
        let result = parse_expr("foo(borrow a, b)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert_eq!(call.args.len(), 2);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
                assert_eq!(call.args[1].mode, ArgMode::Normal);
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn test_borrow_arg_mixed_with_inout() {
        // Borrow and inout arguments in the same call
        let result = parse_expr("modify(borrow a, inout b)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert_eq!(call.args.len(), 2);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
                assert_eq!(call.args[1].mode, ArgMode::Inout);
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn test_borrow_arg_multiple() {
        // Multiple borrow arguments
        let result = parse_expr("sum(borrow x, borrow y)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert_eq!(call.args.len(), 2);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
                assert_eq!(call.args[1].mode, ArgMode::Borrow);
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn test_borrow_arg_with_field_access() {
        // Borrow argument with field access expression
        let result = parse_expr("read(borrow point.x)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert_eq!(call.args.len(), 1);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
                match &call.args[0].expr {
                    Expr::Field(field) => assert_eq!(result.get(field.field.name), "x"),
                    _ => panic!("expected Field expression"),
                }
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn test_borrow_arg_in_method_call() {
        // Borrow argument in method call
        let result = parse_expr("obj.method(borrow x)").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(call.args.len(), 1);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_borrow_arg_in_associated_function() {
        // Borrow argument in associated function call
        let result = parse_expr("Foo::bar(borrow x)").unwrap();
        match &result.expr {
            Expr::AssocFnCall(call) => {
                assert_eq!(call.args.len(), 1);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
            }
            _ => panic!("expected AssocFnCall"),
        }
    }

    #[test]
    fn test_borrow_helper_methods() {
        // Test CallArg helper methods
        let result = parse_expr("foo(borrow x, inout y, z)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert!(call.args[0].is_borrow());
                assert!(!call.args[0].is_inout());
                assert!(call.args[1].is_inout());
                assert!(!call.args[1].is_borrow());
                assert!(!call.args[2].is_borrow());
                assert!(!call.args[2].is_inout());
            }
            _ => panic!("expected Call"),
        }
    }

    // ==================== Block Expression Statement Tests ====================

    #[test]
    fn test_block_statement_followed_by_identifier() {
        // Block expression as a statement followed by an identifier expression
        // This is the regression test for gruel-wo1g
        let result = parse(
            "fn main() -> i32 {
                let a = 1;
                {
                    let b = 2;
                }
                a
            }",
        )
        .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => {
                    // Should have 2 statements: let a = 1; and { let b = 2; }
                    assert_eq!(block.statements.len(), 2);
                    // First statement is a let
                    assert!(matches!(&block.statements[0], Statement::Let(_)));
                    // Second statement is an expression statement containing a block
                    match &block.statements[1] {
                        Statement::Expr(Expr::Block(_)) => {}
                        _ => panic!("expected Expr(Block), got {:?}", block.statements[1]),
                    }
                    // Final expression should be 'a' (simple identifier)
                    match block.expr.as_ref() {
                        Expr::Ident(ident) => assert_eq!(result.get(ident.name), "a"),
                        _ => panic!("expected Ident expression, got {:?}", block.expr),
                    }
                }
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    // ==================== Error Conversion Tests ====================

    #[test]
    fn test_error_preserves_span() {
        // Parse invalid syntax and verify error has span information
        let result = parse("fn main() -> i32 { let = 42; }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        // Get the first error and verify it has span information
        let error = errors.first().expect("should have at least one error");
        assert!(error.has_span());
        assert!(error.span().is_some());
    }

    #[test]
    fn test_error_expected_found() {
        // Missing expression after let
        let result = parse("fn main() -> i32 { let x = ; }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let error = errors.first().expect("should have at least one error");
        // Error message should describe what was expected vs found
        let msg = error.to_string();
        assert!(
            msg.contains("expected") || msg.contains("found"),
            "error message: {}",
            msg
        );
    }

    #[test]
    fn test_error_unexpected_eof() {
        // Unterminated block
        let result = parse("fn main() -> i32 {");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let error = errors.first().expect("should have at least one error");
        let msg = error.to_string();
        // Should indicate end of file was reached unexpectedly
        assert!(
            msg.contains("end of file") || msg.contains("expected"),
            "error message: {}",
            msg
        );
    }

    #[test]
    fn test_format_pattern_token() {
        use chumsky::error::RichPattern;
        use chumsky::util::MaybeRef;

        let pattern = RichPattern::Token(MaybeRef::Val(TokenKind::Plus));
        let formatted = format_pattern(&pattern);
        // TokenKind::name() returns quoted form like "'+'"
        assert_eq!(formatted, "'+'");
    }

    #[test]
    fn test_format_pattern_label() {
        use chumsky::error::RichPattern;

        let pattern: RichPattern<'_, TokenKind> = RichPattern::Label(Cow::Borrowed("expression"));
        let formatted = format_pattern(&pattern);
        assert_eq!(formatted, "expression");
    }

    #[test]
    fn test_format_pattern_identifier() {
        use chumsky::error::RichPattern;

        let pattern: RichPattern<'_, TokenKind> = RichPattern::Identifier("while".to_string());
        let formatted = format_pattern(&pattern);
        assert_eq!(formatted, "'while'");
    }

    #[test]
    fn test_format_pattern_any() {
        use chumsky::error::RichPattern;

        let pattern: RichPattern<'_, TokenKind> = RichPattern::Any;
        let formatted = format_pattern(&pattern);
        assert_eq!(formatted, "any token");
    }

    #[test]
    fn test_format_pattern_end_of_input() {
        use chumsky::error::RichPattern;

        let pattern: RichPattern<'_, TokenKind> = RichPattern::EndOfInput;
        let formatted = format_pattern(&pattern);
        assert_eq!(formatted, "end of input");
    }

    #[test]
    fn test_parse_error_no_empty_found_clause() {
        // Test that error messages don't have empty "found" clauses
        // This was a bug when Custom errors were mapped to UnexpectedToken with empty found
        let result = parse("fn main() -> i32 { let x = ; }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let error = errors.first().expect("should have at least one error");
        let msg = error.to_string();
        // Should not end with "found " (empty found)
        assert!(
            !msg.ends_with("found "),
            "error message should not have trailing empty 'found': {}",
            msg
        );
    }

    #[test]
    fn test_parse_error_variant_display() {
        // Directly test that ParseError displays correctly
        let error = CompileError::new(
            ErrorKind::ParseError("expected semicolon after expression".to_string()),
            gruel_span::Span::new(0, 10),
        );
        assert_eq!(error.to_string(), "expected semicolon after expression");
    }

    #[test]
    fn test_offset_to_u32_normal() {
        // Normal values should convert without issue
        assert_eq!(offset_to_u32(0), 0);
        assert_eq!(offset_to_u32(42), 42);
        assert_eq!(offset_to_u32(1000000), 1000000);
        assert_eq!(offset_to_u32(u32::MAX as usize), u32::MAX);
    }

    #[test]
    #[should_panic(expected = "offset 4294967296 exceeds u32::MAX")]
    #[cfg(debug_assertions)]
    fn test_offset_to_u32_overflow_panics_in_debug() {
        // Value just over u32::MAX should panic in debug builds
        let _ = offset_to_u32((u32::MAX as usize) + 1);
    }

    #[test]
    fn test_to_gruel_span_normal() {
        // Normal spans should convert without issue
        let simple = SimpleSpan::new(10, 20);
        let gruel = to_gruel_span(simple);
        assert_eq!(gruel.start, 10);
        assert_eq!(gruel.end, 20);
    }

    #[test]
    fn test_parse_returns_multiple_errors() {
        // Test that we return ALL parse errors, not just the first one.
        // This uses source with multiple syntax errors that can be detected at once.
        // Note: The actual number of errors depends on Chumsky's error recovery,
        // but this test ensures the infrastructure returns all errors it finds.
        let source = "fn main() { let }"; // Missing variable name and expression

        let result = parse(source);
        assert!(result.is_err(), "Expected parsing to fail");

        // We should get at least one error
        let errors = result.unwrap_err();
        assert!(
            !errors.is_empty(),
            "Expected at least one error but got none"
        );

        // Verify we can iterate over all errors
        let error_count = errors.len();
        assert!(
            error_count >= 1,
            "Expected at least 1 error, got {}",
            error_count
        );
    }

    #[test]
    fn test_parse_error_collection_preserves_all() {
        // Test that the error collection mechanism preserves all errors.
        // This directly tests the CompileErrors::from(Vec<CompileError>) path.
        let errors = vec![
            CompileError::without_span(ErrorKind::UnexpectedToken {
                expected: std::borrow::Cow::Borrowed("ident"),
                found: std::borrow::Cow::Borrowed("let"),
            }),
            CompileError::without_span(ErrorKind::UnexpectedToken {
                expected: std::borrow::Cow::Borrowed("expr"),
                found: std::borrow::Cow::Borrowed("rbrace"),
            }),
        ];

        let compile_errors = CompileErrors::from(errors);
        assert_eq!(compile_errors.len(), 2, "Expected 2 errors to be preserved");
    }

    // ==================== Qualified Struct Literal Tests ====================

    #[test]
    fn test_qualified_struct_literal() {
        // module.Point { x: 1, y: 2 } should parse as a qualified struct literal
        let result = parse_expr("mod.Point { x: 1, y: 2 }").unwrap();
        match &result.expr {
            Expr::StructLit(lit) => {
                // Verify it has a base (the module)
                assert!(
                    lit.base.is_some(),
                    "qualified struct literal should have a base"
                );
                match lit.base.as_ref().unwrap().as_ref() {
                    Expr::Ident(ident) => assert_eq!(result.get(ident.name), "mod"),
                    _ => panic!("base should be Ident, got {:?}", lit.base),
                }
                // Verify struct name
                assert_eq!(result.get(lit.name.name), "Point");
                // Verify fields
                assert_eq!(lit.fields.len(), 2);
                assert_eq!(result.get(lit.fields[0].name.name), "x");
                assert_eq!(result.get(lit.fields[1].name.name), "y");
            }
            _ => panic!("expected StructLit, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_qualified_struct_literal_empty() {
        // module.Empty {} should parse as a qualified struct literal with no fields
        let result = parse_expr("mod.Empty {}").unwrap();
        match &result.expr {
            Expr::StructLit(lit) => {
                assert!(
                    lit.base.is_some(),
                    "qualified struct literal should have a base"
                );
                assert_eq!(result.get(lit.name.name), "Empty");
                assert_eq!(lit.fields.len(), 0);
            }
            _ => panic!("expected StructLit, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_field_access_then_block() {
        // obj.field; { 1 } should parse as field access (discarded) followed by block expression
        // Note: obj.field { 1 } (without semicolon) is a syntax error because there's no
        // operator between the field access and the block.
        let result = parse("fn main() -> i32 { x.field; { 1 } }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => {
                    // The block should have a statement (field access discarded) and
                    // a final expression (the inner block returning 1)
                    assert_eq!(block.statements.len(), 1);
                    match &block.statements[0] {
                        Statement::Expr(Expr::Field(field)) => {
                            assert_eq!(result.get(field.field.name), "field");
                        }
                        _ => panic!("expected Field statement, got {:?}", block.statements[0]),
                    }
                    match block.expr.as_ref() {
                        Expr::Block(inner) => match inner.expr.as_ref() {
                            Expr::Int(lit) => assert_eq!(lit.value, 1),
                            _ => panic!("expected Int, got {:?}", inner.expr),
                        },
                        _ => panic!("expected Block, got {:?}", block.expr),
                    }
                }
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_field_access_block_without_semicolon_is_error() {
        // obj.field { 1 } without semicolon is a syntax error - it's not a struct literal
        // (because { 1 } doesn't match the { ident: } pattern) and it's not valid syntax.
        let result = parse("fn main() -> i32 { x.field { 1 } }");
        assert!(
            result.is_err(),
            "field + block without semicolon should be syntax error"
        );
    }

    #[test]
    fn test_chained_qualified_struct_literal() {
        // a.b.Point { x: 1 } - nested field access then struct literal
        let result = parse_expr("a.b.Point { x: 1 }").unwrap();
        match &result.expr {
            Expr::StructLit(lit) => {
                // Should have a base that's a.b
                assert!(lit.base.is_some());
                match lit.base.as_ref().unwrap().as_ref() {
                    Expr::Field(field) => {
                        assert_eq!(result.get(field.field.name), "b");
                        match field.base.as_ref() {
                            Expr::Ident(ident) => assert_eq!(result.get(ident.name), "a"),
                            _ => panic!("inner base should be Ident"),
                        }
                    }
                    _ => panic!("base should be Field, got {:?}", lit.base),
                }
                assert_eq!(result.get(lit.name.name), "Point");
            }
            _ => panic!("expected StructLit, got {:?}", result.expr),
        }
    }

    // ==================== Anonymous Struct Method Parsing Tests ====================

    #[test]
    fn test_anon_struct_with_fields_only() {
        // Anonymous struct with only fields (no methods)
        let result = parse("fn make_type() -> type { struct { x: i32, y: i32 } }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(result.get(f.name.name), "make_type");
                match &f.body {
                    Expr::Block(block) => match block.expr.as_ref() {
                        Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                            TypeExpr::AnonymousStruct {
                                fields, methods, ..
                            } => {
                                assert_eq!(fields.len(), 2);
                                assert_eq!(result.get(fields[0].name.name), "x");
                                assert_eq!(result.get(fields[1].name.name), "y");
                                assert!(methods.is_empty());
                            }
                            _ => panic!("expected AnonymousStruct"),
                        },
                        _ => panic!("expected TypeLit"),
                    },
                    _ => panic!("expected Block"),
                }
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_anon_struct_with_method() {
        // Anonymous struct with a single method
        let result =
            parse("fn make_type() -> type { struct { x: i32, fn get_x(self) -> i32 { self.x } } }")
                .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct {
                            fields, methods, ..
                        } => {
                            assert_eq!(fields.len(), 1);
                            assert_eq!(result.get(fields[0].name.name), "x");
                            assert_eq!(methods.len(), 1);
                            assert_eq!(result.get(methods[0].name.name), "get_x");
                            assert!(
                                methods[0].receiver.is_some(),
                                "method should have self receiver"
                            );
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_anon_struct_with_associated_function() {
        // Anonymous struct with an associated function (no self)
        let result = parse(
            "fn make_type() -> type { struct { x: i32, fn new() -> Self { Self { x: 0 } } } }",
        )
        .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct {
                            fields, methods, ..
                        } => {
                            assert_eq!(fields.len(), 1);
                            assert_eq!(methods.len(), 1);
                            assert_eq!(result.get(methods[0].name.name), "new");
                            assert!(
                                methods[0].receiver.is_none(),
                                "associated function should not have self"
                            );
                            // Check return type is Self
                            match &methods[0].return_type {
                                Some(TypeExpr::Named(ident)) => {
                                    assert_eq!(result.get(ident.name), "Self");
                                }
                                _ => panic!("expected Self return type"),
                            }
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_anon_struct_with_multiple_methods() {
        // Anonymous struct with multiple methods
        let result = parse(
            r#"
            fn make_type() -> type {
                struct {
                    value: i32,
                    fn get(self) -> i32 { self.value }
                    fn set(self, v: i32) -> Self { Self { value: v } }
                }
            }
        "#,
        )
        .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct {
                            fields, methods, ..
                        } => {
                            assert_eq!(fields.len(), 1);
                            assert_eq!(result.get(fields[0].name.name), "value");
                            assert_eq!(methods.len(), 2);
                            assert_eq!(result.get(methods[0].name.name), "get");
                            assert_eq!(result.get(methods[1].name.name), "set");
                            // Check set has a parameter
                            assert_eq!(methods[1].params.len(), 1);
                            assert_eq!(result.get(methods[1].params[0].name.name), "v");
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_anon_struct_methods_only() {
        // Anonymous struct with only methods (no fields)
        let result =
            parse("fn make_type() -> type { struct { fn new() -> Self { Self { } } } }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct {
                            fields, methods, ..
                        } => {
                            assert!(fields.is_empty());
                            assert_eq!(methods.len(), 1);
                            assert_eq!(result.get(methods[0].name.name), "new");
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_self_type_in_return() {
        // Test Self as return type
        let result =
            parse("fn make_type() -> type { struct { x: i32, fn clone(self) -> Self { self } } }")
                .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct { methods, .. } => {
                            match &methods[0].return_type {
                                Some(TypeExpr::Named(ident)) => {
                                    assert_eq!(result.get(ident.name), "Self");
                                }
                                _ => panic!("expected Self return type"),
                            }
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_self_type_in_param() {
        // Test Self as parameter type
        let result = parse(
            "fn make_type() -> type { struct { fn combine(self, other: Self) -> Self { self } } }",
        )
        .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct { methods, .. } => {
                            // Check parameter type is Self
                            let param = &methods[0].params[0];
                            match &param.ty {
                                TypeExpr::Named(ident) => {
                                    assert_eq!(result.get(ident.name), "Self");
                                }
                                _ => panic!("expected Self param type"),
                            }
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_self_type_standalone() {
        // Test Self as a standalone type (e.g., in struct method context)
        // This just tests lexing/parsing of Self as a type keyword
        let result = parse("struct Foo { x: i32, fn clone(self) -> Self { self } }").unwrap();
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                let method = &struct_decl.methods[0];
                match &method.return_type {
                    Some(TypeExpr::Named(ident)) => {
                        assert_eq!(result.get(ident.name), "Self");
                    }
                    _ => panic!("expected Self return type"),
                }
            }
            _ => panic!("expected Struct"),
        }
    }

    // ========================================================================
    // Tuple parser tests (ADR-0048, phase 1)
    // ========================================================================

    #[test]
    fn test_tuple_expr_pair() {
        let result = parse_expr("(1, 2)").unwrap();
        match &result.expr {
            Expr::Tuple(t) => {
                assert_eq!(t.elems.len(), 2);
                match &t.elems[0] {
                    Expr::Int(lit) => assert_eq!(lit.value, 1),
                    _ => panic!("expected Int"),
                }
                match &t.elems[1] {
                    Expr::Int(lit) => assert_eq!(lit.value, 2),
                    _ => panic!("expected Int"),
                }
            }
            other => panic!("expected Tuple, got {:?}", other),
        }
    }

    #[test]
    fn test_tuple_expr_triple_mixed() {
        let result = parse_expr("(1, true, 3)").unwrap();
        match &result.expr {
            Expr::Tuple(t) => {
                assert_eq!(t.elems.len(), 3);
                assert!(matches!(t.elems[0], Expr::Int(_)));
                assert!(matches!(t.elems[1], Expr::Bool(_)));
                assert!(matches!(t.elems[2], Expr::Int(_)));
            }
            _ => panic!("expected Tuple"),
        }
    }

    #[test]
    fn test_tuple_expr_singleton_needs_trailing_comma() {
        // (x,) is a 1-tuple
        let result = parse_expr("(42,)").unwrap();
        match &result.expr {
            Expr::Tuple(t) => {
                assert_eq!(t.elems.len(), 1);
                assert!(matches!(t.elems[0], Expr::Int(_)));
            }
            _ => panic!("expected Tuple singleton"),
        }
    }

    #[test]
    fn test_paren_expr_without_comma_is_not_tuple() {
        // (x) stays a parenthesised expression, not a 1-tuple
        let result = parse_expr("(42)").unwrap();
        assert!(matches!(result.expr, Expr::Paren(_)));
    }

    #[test]
    fn test_unit_literal_is_not_tuple() {
        let result = parse_expr("()").unwrap();
        assert!(matches!(result.expr, Expr::Unit(_)));
    }

    #[test]
    fn test_tuple_expr_trailing_comma_allowed() {
        let result = parse_expr("(1, 2,)").unwrap();
        match &result.expr {
            Expr::Tuple(t) => assert_eq!(t.elems.len(), 2),
            _ => panic!("expected Tuple"),
        }
    }

    #[test]
    fn test_tuple_index_zero() {
        let result = parse_expr("t.0").unwrap();
        match &result.expr {
            Expr::TupleIndex(ti) => {
                assert_eq!(ti.index, 0);
                assert!(matches!(*ti.base, Expr::Ident(_)));
            }
            _ => panic!("expected TupleIndex"),
        }
    }

    #[test]
    fn test_tuple_index_multi_digit() {
        let result = parse_expr("t.42").unwrap();
        match &result.expr {
            Expr::TupleIndex(ti) => assert_eq!(ti.index, 42),
            _ => panic!("expected TupleIndex"),
        }
    }

    #[test]
    fn test_parenthesised_nested_tuple_access() {
        // (t.0).1 works; t.0.1 doesn't because the lexer treats `0.1` as Float
        let result = parse_expr("(t.0).1").unwrap();
        match &result.expr {
            Expr::TupleIndex(outer) => {
                assert_eq!(outer.index, 1);
                match &*outer.base {
                    Expr::Paren(p) => match &*p.inner {
                        Expr::TupleIndex(inner) => assert_eq!(inner.index, 0),
                        _ => panic!("expected inner TupleIndex"),
                    },
                    _ => panic!("expected Paren wrapping inner access"),
                }
            }
            _ => panic!("expected outer TupleIndex"),
        }
    }

    #[test]
    fn test_tuple_type_pair() {
        let result = parse("fn f() -> (i32, bool) { (1, true) }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match f.return_type.as_ref().unwrap() {
                TypeExpr::Tuple { elems, .. } => {
                    assert_eq!(elems.len(), 2);
                }
                other => panic!("expected Tuple type, got {:?}", other),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_tuple_type_singleton() {
        let result = parse("fn f() -> (i32,) { (1,) }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match f.return_type.as_ref().unwrap() {
                TypeExpr::Tuple { elems, .. } => assert_eq!(elems.len(), 1),
                _ => panic!("expected Tuple type"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_unit_type_still_unit() {
        let result = parse("fn f() -> () { () }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match f.return_type.as_ref().unwrap() {
                TypeExpr::Unit(_) => {}
                other => panic!("expected Unit type, got {:?}", other),
            },
            _ => panic!("expected Function"),
        }
    }

    fn assert_tuple_ident(
        elem: &TupleElemPattern,
        result: &ParseResult,
        expected: &str,
        expected_mut: bool,
    ) {
        match elem {
            TupleElemPattern::Pattern(Pattern::Ident { name, is_mut, .. }) => {
                assert_eq!(result.get(name.name), expected);
                assert_eq!(*is_mut, expected_mut);
            }
            other => panic!("expected Pattern::Ident, got {:?}", other),
        }
    }

    #[test]
    fn test_tuple_destructure_basic() {
        let result = parse("fn main() -> i32 { let (a, b) = (1, 2); a }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match &block.statements[0] {
                    Statement::Let(let_stmt) => match &let_stmt.pattern {
                        Pattern::Tuple { elems, .. } => {
                            assert_eq!(elems.len(), 2);
                            assert_tuple_ident(&elems[0], &result, "a", false);
                            assert_tuple_ident(&elems[1], &result, "b", false);
                        }
                        _ => panic!("expected Tuple pattern"),
                    },
                    _ => panic!("expected Let"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_tuple_destructure_mut_and_wildcard() {
        let result = parse("fn main() -> i32 { let (mut a, _) = (1, 2); a }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match &block.statements[0] {
                    Statement::Let(let_stmt) => match &let_stmt.pattern {
                        Pattern::Tuple { elems, .. } => {
                            assert_eq!(elems.len(), 2);
                            assert_tuple_ident(&elems[0], &result, "a", true);
                            assert!(matches!(
                                &elems[1],
                                TupleElemPattern::Pattern(Pattern::Wildcard(_))
                            ));
                        }
                        _ => panic!("expected Tuple pattern"),
                    },
                    _ => panic!("expected Let"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_tuple_destructure_singleton() {
        let result = parse("fn main() -> i32 { let (x,) = (42,); x }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match &block.statements[0] {
                    Statement::Let(let_stmt) => match &let_stmt.pattern {
                        Pattern::Tuple { elems, .. } => {
                            assert_eq!(elems.len(), 1);
                        }
                        _ => panic!("expected Tuple pattern"),
                    },
                    _ => panic!("expected Let"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    // ==================== ADR-0049 Phase 2: parser shapes ====================
    //
    // These tests drive the parser directly with the nested_patterns preview feature
    // enabled (bypassing the post-parse validator). They only check AST shape — sema
    // and lowering open up in later phases.

    fn parse_with_nested(source: &str) -> MultiErrorResult<ParseResult> {
        use gruel_error::{PreviewFeature, PreviewFeatures};
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let mut features = PreviewFeatures::new();
        features.insert(PreviewFeature::NestedPatterns);
        let parser = ChumskyParser::new(tokens, interner).with_preview_features(features);
        parser
            .parse()
            .map(|(ast, interner)| ParseResult { ast, interner })
    }

    fn find_first_let_pattern(result: &ParseResult) -> &Pattern {
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match &block.statements[0] {
                    Statement::Let(l) => &l.pattern,
                    _ => panic!("expected let"),
                },
                _ => panic!("expected block"),
            },
            _ => panic!("expected function"),
        }
    }

    fn find_first_match_arm_pattern(result: &ParseResult) -> &Pattern {
        match &result.ast.items[0] {
            Item::Function(f) => {
                fn find_match(e: &Expr) -> Option<&Pattern> {
                    match e {
                        Expr::Block(b) => find_match(&b.expr),
                        Expr::Match(m) => Some(&m.arms[0].pattern),
                        _ => None,
                    }
                }
                find_match(&f.body).expect("expected a match expression")
            }
            _ => panic!("expected function"),
        }
    }

    #[test]
    fn test_nested_let_struct_in_struct() {
        let source = r#"
            fn main() -> i32 {
                let Outer { inner: Inner { x, y }, tag } = make();
                0
            }
        "#;
        let result = parse_with_nested(source).unwrap();
        match find_first_let_pattern(&result) {
            Pattern::Struct { fields, .. } => {
                assert_eq!(fields.len(), 2);
                // inner: Inner { x, y }
                let inner = &fields[0];
                assert_eq!(result.get(inner.field_name.unwrap().name), "inner");
                match inner.sub.as_ref().unwrap() {
                    Pattern::Struct {
                        fields: inner_fields,
                        ..
                    } => assert_eq!(inner_fields.len(), 2),
                    other => panic!("expected nested Struct, got {:?}", other),
                }
                // tag (shorthand)
                let tag = &fields[1];
                assert_eq!(result.get(tag.field_name.unwrap().name), "tag");
                assert!(tag.sub.is_none());
            }
            other => panic!("expected Pattern::Struct, got {:?}", other),
        }
    }

    #[test]
    fn test_nested_let_tuple_of_tuples() {
        let source = "fn main() -> i32 { let ((a, b), c) = ((1, 2), 3); 0 }";
        let result = parse_with_nested(source).unwrap();
        match find_first_let_pattern(&result) {
            Pattern::Tuple { elems, .. } => {
                assert_eq!(elems.len(), 2);
                match &elems[0] {
                    TupleElemPattern::Pattern(Pattern::Tuple {
                        elems: inner_elems, ..
                    }) => assert_eq!(inner_elems.len(), 2),
                    other => panic!("expected nested tuple, got {:?}", other),
                }
            }
            other => panic!("expected Pattern::Tuple, got {:?}", other),
        }
    }

    #[test]
    fn test_tuple_pattern_in_match() {
        let source = r#"
            fn main() -> i32 {
                match pair() {
                    (0, 0) => 0,
                    _ => 1,
                }
            }
        "#;
        let result = parse_with_nested(source).unwrap();
        match find_first_match_arm_pattern(&result) {
            Pattern::Tuple { elems, .. } => {
                assert_eq!(elems.len(), 2);
                for e in elems {
                    match e {
                        TupleElemPattern::Pattern(Pattern::Int(lit)) => {
                            assert_eq!(lit.value, 0)
                        }
                        other => panic!("expected Int sub-pattern, got {:?}", other),
                    }
                }
            }
            other => panic!("expected Pattern::Tuple in match, got {:?}", other),
        }
    }

    #[test]
    fn test_rest_in_tuple() {
        let source = "fn main() -> i32 { let (a, .., z) = quintuple(); 0 }";
        let result = parse_with_nested(source).unwrap();
        match find_first_let_pattern(&result) {
            Pattern::Tuple { elems, .. } => {
                assert_eq!(elems.len(), 3);
                assert!(matches!(&elems[1], TupleElemPattern::Rest(_)));
            }
            other => panic!("expected Pattern::Tuple, got {:?}", other),
        }
    }

    #[test]
    fn test_rest_in_struct() {
        let source = "fn main() -> i32 { let Point { x, .. } = p(); 0 }";
        let result = parse_with_nested(source).unwrap();
        match find_first_let_pattern(&result) {
            Pattern::Struct { fields, .. } => {
                assert_eq!(fields.len(), 2);
                assert!(fields[0].field_name.is_some());
                assert!(fields[1].field_name.is_none()); // `..` sentinel
            }
            other => panic!("expected Pattern::Struct, got {:?}", other),
        }
    }

    #[test]
    fn test_preview_gate_rejects_nested_without_flag() {
        // Without the preview flag, nested destructure must error.
        let source = "fn main() -> i32 { let ((a, b), c) = t(); 0 }";
        let result = parse(source);
        assert!(result.is_err(), "expected preview error without flag");
    }

    #[test]
    fn test_preview_gate_rejects_tuple_match_without_flag() {
        let source = "fn main() -> i32 { match p() { (0, 0) => 0, _ => 1 } }";
        let result = parse(source);
        assert!(result.is_err(), "expected preview error without flag");
    }

    #[test]
    fn test_preview_gate_rejects_rest_without_flag() {
        let source = "fn main() -> i32 { let (a, .., z) = q(); 0 }";
        let result = parse(source);
        assert!(result.is_err(), "expected preview error without flag");
    }

    #[test]
    fn test_preview_gate_accepts_flat_without_flag() {
        // Flat pre-existing forms must continue to parse without the flag.
        let source = "fn main() -> i32 { let (a, b) = pair(); 0 }";
        let result = parse(source);
        assert!(
            result.is_ok(),
            "flat tuple destructure should parse without flag"
        );
    }
}
