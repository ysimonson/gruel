//! Abstract Syntax Tree types for Gruel.
//!
//! The AST represents the syntactic structure of the source code.
//! It closely mirrors the source syntax and preserves all information
//! needed for error reporting.
//!
//! ## SmallVec Usage
//!
//! Some non-recursive Vec fields use SmallVec to avoid heap allocation for
//! common small sizes:
//! - `Directives` (SmallVec<[Directive; 1]>) - most items have 0-1 directives
//!
//! ## Vec Usage (Cannot Use SmallVec)
//!
//! Vec fields containing recursive types (Expr) cannot use SmallVec because
//! Expr's size cannot be determined at compile time. These include:
//! - `Vec<CallArg>` - CallArg contains Expr
//! - `Vec<MatchArm>` - contains Expr
//! - `Vec<FieldInit>` - contains Box<Expr>
//! - `Vec<IntrinsicArg>` - contains Expr
//! - `Vec<Statement>` - Statement contains Expr
//! - `Vec<Expr>` - directly recursive
//!
//! The IR layers (RIR, AIR, CFG) use index-based references which avoid
//! this issue and are already efficiently allocated.

use std::fmt;

use gruel_builtins::Posture;
use gruel_util::Span;
use lasso::{Key, Spur};
use smallvec::SmallVec;

/// Type alias for a small vector of directives.
/// Most items have 0-1 directives, so we inline capacity for 1.
pub type Directives = SmallVec<[Directive; 1]>;

/// A complete source file (list of items).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ast {
    pub items: Vec<Item>,
}

/// A directive that modifies compiler behavior for the following item or statement.
///
/// Directives use the `@name(args)` syntax and appear before items or statements.
/// For example: `@allow(unused_variable)`
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Directive {
    /// The directive name (without the @)
    pub name: Ident,
    /// Arguments to the directive
    pub args: Vec<DirectiveArg>,
    /// Span covering the entire directive
    pub span: Span,
}

/// An argument to a directive.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DirectiveArg {
    /// An identifier argument (e.g., `unused_variable` in `@allow(unused_variable)`)
    Ident(Ident),
    /// A string-literal argument (e.g., `"drop"` in `@lang("drop")`).
    /// ADR-0079.
    String(StringLit),
}

/// A top-level item in a source file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Item {
    Function(Function),
    Struct(StructDecl),
    Enum(EnumDecl),
    /// Interface declaration (ADR-0056) - a structurally typed set of method
    /// requirements.
    Interface(InterfaceDecl),
    /// Derive declaration (ADR-0058) - a set of method declarations attached
    /// to a host type via `@derive(...)`.
    Derive(DeriveDecl),
    /// Constant declaration (e.g., `const math = @import("math");`)
    Const(ConstDecl),
    /// Error node for recovered parse errors at item level.
    /// Used by error recovery to continue parsing after a syntax error.
    Error(Span),
}

/// A constant declaration.
///
/// Constants are compile-time values. In the context of the module system,
/// they're used for re-exports:
/// ```gruel
/// // _utils.gruel (directory module root)
/// pub const strings = @import("utils/strings.gruel");
/// pub const helper = @import("utils/internal.gruel").helper;
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConstDecl {
    /// Directives applied to this const
    pub directives: Directives,
    /// Visibility of this constant
    pub visibility: Visibility,
    /// Constant name
    pub name: Ident,
    /// Optional type annotation (usually inferred)
    pub ty: Option<TypeExpr>,
    /// Initializer expression
    pub init: Box<Expr>,
    /// Span covering the entire const declaration
    pub span: Span,
}

/// A struct declaration.
///
/// Structs can contain both fields and methods. Methods are defined inline
/// within the struct block, not in separate impl blocks.
///
/// ```gruel
/// struct Point {
///     x: i32,
///     y: i32,
///
///     fn distance(self) -> i32 {
///         self.x + self.y
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StructDecl {
    /// Directives applied to this struct (e.g., `@mark(copy)`, `@derive(...)`)
    pub directives: Directives,
    /// Visibility of this struct
    pub visibility: Visibility,
    /// Declared ownership posture (ADR-0080). `Affine` when neither
    /// `@mark(copy)` nor `@mark(linear)` appears in the directive list.
    pub posture: Posture,
    /// Struct name
    pub name: Ident,
    /// Struct fields
    pub fields: Vec<FieldDecl>,
    /// Methods defined on this struct
    pub methods: Vec<Method>,
    /// Span covering the entire struct declaration
    pub span: Span,
}

/// A field declaration in a struct.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FieldDecl {
    /// Visibility (ADR-0073). Defaults to `Private` when `pub` is absent.
    pub visibility: Visibility,
    /// Field name
    pub name: Ident,
    /// Field type
    pub ty: TypeExpr,
    /// Span covering the entire field declaration
    pub span: Span,
}

/// An enum declaration.
///
/// Like structs, enums may declare inline methods after their variants.
///
/// ```gruel
/// enum Option {
///     Some(i32),
///     None,
///
///     fn is_some(self) -> bool {
///         match self { Self::Some(_) => true, Self::None => false }
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnumDecl {
    /// Directives applied to this enum (e.g., `@derive(...)`, `@lang("ordering")`).
    pub directives: Directives,
    /// Visibility of this enum
    pub visibility: Visibility,
    /// Declared ownership posture (ADR-0080). `Affine` when neither
    /// `@mark(copy)` nor `@mark(linear)` appears in the directive list.
    pub posture: Posture,
    /// Enum name
    pub name: Ident,
    /// Enum variants
    pub variants: Vec<EnumVariant>,
    /// Methods defined on this enum
    pub methods: Vec<Method>,
    /// Span covering the entire enum declaration
    pub span: Span,
}

/// A variant in an enum declaration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnumVariant {
    /// Variant name
    pub name: Ident,
    /// The kind of variant (unit, tuple, or struct).
    pub kind: EnumVariantKind,
    /// Span covering the variant
    pub span: Span,
}

/// The kind of an enum variant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnumVariantKind {
    /// Unit variant: `Red`
    Unit,
    /// Tuple variant: `Some(i32, i32)`
    Tuple(Vec<TypeExpr>),
    /// Struct variant: `Circle { radius: i32 }`
    Struct(Vec<EnumVariantField>),
}

/// A named field in a struct-style enum variant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnumVariantField {
    /// Visibility (ADR-0073). Defaults to `Private` when `pub` is absent.
    pub visibility: Visibility,
    /// Field name
    pub name: Ident,
    /// Field type
    pub ty: TypeExpr,
    /// Span covering the field
    pub span: Span,
}

/// An interface declaration (ADR-0056).
///
/// Interfaces are structurally typed sets of method requirements. A type
/// conforms to an interface iff its method set covers every method signature
/// required here.
///
/// ```gruel
/// interface Drop {
///     fn __drop(self);
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InterfaceDecl {
    /// Directives applied to this interface (e.g., `@lang("drop")`).
    pub directives: Directives,
    /// Visibility (currently always private; module-system support is future
    /// work).
    pub visibility: Visibility,
    /// Interface name
    pub name: Ident,
    /// Required method signatures, in declaration order. The order is
    /// significant: it determines vtable slot indices in the runtime
    /// dispatch path (Phase 4).
    pub methods: Vec<MethodSig>,
    /// Span covering the entire interface declaration
    pub span: Span,
}

/// A method signature inside an interface declaration.
///
/// No body and no associated functions (no-`self`) are allowed in MVP.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MethodSig {
    /// Method name
    pub name: Ident,
    /// The receiver. Required (associated functions are not yet allowed in
    /// interfaces).
    pub receiver: SelfParam,
    /// Method parameters (excluding self)
    pub params: Vec<Param>,
    /// Return type (None means implicit unit `()`)
    pub return_type: Option<TypeExpr>,
    /// Span covering the entire signature (`fn name(...) -> R;`)
    pub span: Span,
}

/// A derive declaration (ADR-0058).
///
/// Holds a list of method declarations whose body refers to the host type
/// as `Self`. Methods are spliced into a host type's method list when a
/// `@derive(Name)` directive on a struct or enum names this derive.
///
/// ```gruel
/// derive Drop {
///     fn __drop(self) {
///         comptime_unroll for f in @type_info(Self).fields {
///             drop(@field(self, f.name));
///         }
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeriveDecl {
    /// Derive name (e.g., `Drop`)
    pub name: Ident,
    /// Method declarations inside the derive body
    pub methods: Vec<Method>,
    /// Span covering the entire derive item
    pub span: Span,
}

/// A method definition in an impl block.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Method {
    /// Directives applied to this method
    pub directives: Directives,
    /// Visibility (ADR-0073). Defaults to `Private` when `pub` is absent.
    pub visibility: Visibility,
    /// Method name
    pub name: Ident,
    /// Whether this method takes self (None = associated function, Some = method with receiver)
    pub receiver: Option<SelfParam>,
    /// Method parameters (excluding self)
    pub params: Vec<Param>,
    /// Return type (None means implicit unit `()`)
    pub return_type: Option<TypeExpr>,
    /// Method body
    pub body: Expr,
    /// Span covering the entire method
    pub span: Span,
}

/// A self parameter in a method.
///
/// ADR-0076 made `self : T` the sole spelling for receivers; the legacy
/// `inout self` / `borrow self` keyword forms are gone. The receiver kind
/// (by-value, `Ref(Self)`, `MutRef(Self)`) is determined at parse time and
/// recorded here directly to avoid downstream stages having to re-classify
/// the annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SelfParam {
    /// Receiver kind, derived from the optional `: T` annotation.
    pub kind: SelfReceiverKind,
    /// Span covering the receiver.
    pub span: Span,
}

/// Classification of a [`SelfParam`] based on the parsed annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SelfReceiverKind {
    /// `self`, `self : Self`, or any annotation not matching `Ref(Self)` /
    /// `MutRef(Self)`. Lowered as a by-value receiver.
    #[default]
    ByValue,
    /// `self : Ref(Self)` — shared borrow receiver.
    Ref,
    /// `self : MutRef(Self)` — exclusive mutable borrow receiver.
    MutRef,
}

/// Visibility of an item (function, struct, enum, etc.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Visibility {
    /// Private to the current file (default)
    #[default]
    Private,
    /// Public - visible to importers
    Public,
}

/// A function definition.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Function {
    /// Directives applied to this function
    pub directives: Directives,
    /// Visibility of this function
    pub visibility: Visibility,
    /// Whether this function is marked `unchecked` (can only be called from checked blocks)
    pub is_unchecked: bool,
    /// Function name
    pub name: Ident,
    /// Function parameters
    pub params: Vec<Param>,
    /// Return type (None means implicit unit `()`)
    pub return_type: Option<TypeExpr>,
    /// Function body
    pub body: Expr,
    /// Span covering the entire function
    pub span: Span,
}

/// Parameter passing mode.
///
/// ADR-0076 retired the `inout` / `borrow` keyword forms; ref-ness is now
/// encoded in the parameter type (`Ref(T)` / `MutRef(T)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ParamMode {
    /// Normal pass-by-value parameter
    #[default]
    Normal,
    /// Comptime parameter - evaluated at compile time (used for type parameters)
    Comptime,
}

/// A function parameter.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Param {
    /// Whether this parameter is evaluated at compile time
    pub is_comptime: bool,
    /// Parameter passing mode (normal or inout)
    pub mode: ParamMode,
    /// Parameter name
    pub name: Ident,
    /// Parameter type
    pub ty: TypeExpr,
    /// Span covering the entire parameter
    pub span: Span,
}

/// An identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ident {
    pub name: Spur,
    pub span: Span,
}

/// A type expression in the AST.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TypeExpr {
    /// A simple named type (e.g., i32, bool, MyStruct)
    Named(Ident),
    /// Unit type: ()
    Unit(Span),
    /// Never type: !
    Never(Span),
    /// Array type: [T; N] where T is the element type and N is the length
    Array {
        element: Box<TypeExpr>,
        length: u64,
        span: Span,
    },
    /// Anonymous struct type: struct { field: Type, fn method(...) { ... }, ... }
    /// Used in comptime type construction (e.g., `fn Pair(comptime T: type) -> type { struct { first: T, second: T } }`)
    /// Methods can be included inside the struct definition (Zig-style).
    AnonymousStruct {
        /// Directives applied to the struct expression (e.g., `@derive(...)`,
        /// ADR-0058). Parsed before the `struct` keyword.
        directives: Directives,
        /// Declared ownership posture (ADR-0080). `Affine` when neither
        /// `@mark(copy)` nor `@mark(linear)` appears in the directive list.
        posture: Posture,
        /// Field declarations (name and type)
        fields: Vec<AnonStructField>,
        /// Method definitions inside the anonymous struct
        methods: Vec<Method>,
        span: Span,
    },
    /// Anonymous enum type: enum { Variant, Variant(T), Variant { field: T }, fn method(...) { ... }, ... }
    /// Used in comptime type construction (e.g., `fn Option(comptime T: type) -> type { enum { Some(T), None } }`)
    /// Methods can be included inside the enum definition (Zig-style).
    AnonymousEnum {
        /// Directives applied to the enum expression (ADR-0058).
        directives: Directives,
        /// Declared ownership posture (ADR-0080). `Affine` when neither
        /// `@mark(copy)` nor `@mark(linear)` appears in the directive list.
        posture: Posture,
        /// Enum variants
        variants: Vec<EnumVariant>,
        /// Method definitions inside the anonymous enum
        methods: Vec<Method>,
        span: Span,
    },
    /// Anonymous interface type: interface { fn name(self) [-> T]; ... }
    /// (ADR-0057) — comptime-constructed interface type, parallel to
    /// `AnonymousStruct` and `AnonymousEnum`. Used inside `fn ... -> type`
    /// bodies to build parameterized interfaces.
    AnonymousInterface {
        /// Required method signatures (no bodies, no associated functions).
        methods: Vec<MethodSig>,
        span: Span,
    },
    /// Parameterized type call (ADR-0057): `Name(arg1, arg2, ...)` used in
    /// type position to invoke a comptime function returning `type`.
    /// Resolves at sema time by evaluating the call at comptime.
    TypeCall {
        /// The function being called (e.g. `Sized`).
        callee: Ident,
        /// Type-or-expression arguments. Currently restricted to type
        /// expressions; expressions like integer literals can be added when
        /// comptime value parameters become common at type positions.
        args: Vec<TypeExpr>,
        span: Span,
    },
    /// Tuple type: `(T,)`, `(T, U)`, `(T, U, V)`, ... (ADR-0048)
    ///
    /// `()` remains the unit type. A 1-tuple requires a trailing comma (`(T,)`)
    /// to distinguish it from a parenthesised type.
    Tuple { elems: Vec<TypeExpr>, span: Span },
}

/// A field in an anonymous struct type expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnonStructField {
    /// Field name
    pub name: Ident,
    /// Field type
    pub ty: TypeExpr,
    /// Span covering the entire field declaration
    pub span: Span,
}

impl TypeExpr {
    /// Get the span of this type expression.
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Named(ident) => ident.span,
            TypeExpr::Unit(span) => *span,
            TypeExpr::Never(span) => *span,
            TypeExpr::Array { span, .. } => *span,
            TypeExpr::AnonymousStruct { span, .. } => *span,
            TypeExpr::AnonymousEnum { span, .. } => *span,
            TypeExpr::AnonymousInterface { span, .. } => *span,
            TypeExpr::TypeCall { span, .. } => *span,
            TypeExpr::Tuple { span, .. } => *span,
        }
    }
}

impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeExpr::Named(ident) => write!(f, "sym:{}", ident.name.into_usize()),
            TypeExpr::Unit(_) => write!(f, "()"),
            TypeExpr::Never(_) => write!(f, "!"),
            TypeExpr::Array {
                element, length, ..
            } => write!(f, "[{}; {}]", element, length),
            TypeExpr::AnonymousStruct {
                fields, methods, ..
            } => {
                write!(f, "struct {{ ")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "sym:{}: {}", field.name.name.into_usize(), field.ty)?;
                }
                for (i, method) in methods.iter().enumerate() {
                    if !fields.is_empty() || i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "fn sym:{}", method.name.name.into_usize())?;
                }
                write!(f, " }}")
            }
            TypeExpr::AnonymousEnum {
                variants, methods, ..
            } => {
                write!(f, "enum {{ ")?;
                for (i, variant) in variants.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "sym:{}", variant.name.name.into_usize())?;
                }
                for (i, method) in methods.iter().enumerate() {
                    if !variants.is_empty() || i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "fn sym:{}", method.name.name.into_usize())?;
                }
                write!(f, " }}")
            }
            TypeExpr::AnonymousInterface { methods, .. } => {
                write!(f, "interface {{ ")?;
                for (i, m) in methods.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "fn sym:{}", m.name.name.into_usize())?;
                }
                write!(f, " }}")
            }
            TypeExpr::TypeCall { callee, args, .. } => {
                write!(f, "sym:{}(", callee.name.into_usize())?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", a)?;
                }
                write!(f, ")")
            }
            TypeExpr::Tuple { elems, .. } => {
                write!(f, "(")?;
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", elem)?;
                }
                if elems.len() == 1 {
                    write!(f, ",")?;
                }
                write!(f, ")")
            }
        }
    }
}

/// A unit literal expression - represents `()` or implicit unit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnitLit {
    pub span: Span,
}

/// An expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Expr {
    /// Integer literal
    Int(IntLit),
    /// Floating-point literal
    Float(FloatLit),
    /// String literal
    String(StringLit),
    /// Character literal — Unicode scalar value (ADR-0071)
    Char(CharLit),
    /// Boolean literal
    Bool(BoolLit),
    /// Unit literal (explicit `()` or implicit unit for blocks without final expression)
    Unit(UnitLit),
    /// Identifier reference (variable)
    Ident(Ident),
    /// Binary operation (e.g., `a + b`)
    Binary(BinaryExpr),
    /// Unary operation (e.g., `-x`)
    Unary(UnaryExpr),
    /// Parenthesized expression (e.g., `(a + b)`)
    Paren(ParenExpr),
    /// Block with statements and final expression
    Block(BlockExpr),
    /// If expression (e.g., `if cond { a } else { b }`)
    If(IfExpr),
    /// Match expression (e.g., `match x { 1 => a, _ => b }`)
    Match(MatchExpr),
    /// While expression (e.g., `while cond { body }`)
    While(WhileExpr),
    /// For-in expression (e.g., `for x in arr { body }`)
    For(ForExpr),
    /// Loop expression - infinite loop (e.g., `loop { body }`)
    Loop(LoopExpr),
    /// Function call (e.g., `foo(1, 2)`)
    Call(CallExpr),
    /// Break statement (exits the innermost loop)
    Break(BreakExpr),
    /// Continue statement (skips to the next iteration of the innermost loop)
    Continue(ContinueExpr),
    /// Return statement (returns a value from the current function)
    Return(ReturnExpr),
    /// Struct literal (e.g., `Point { x: 1, y: 2 }`)
    StructLit(StructLitExpr),
    /// Field access (e.g., `point.x`)
    Field(FieldExpr),
    /// Method call (e.g., `point.distance()`)
    MethodCall(MethodCallExpr),
    /// Intrinsic call (e.g., `@dbg(42)`)
    IntrinsicCall(IntrinsicCallExpr),
    /// Array literal (e.g., `[1, 2, 3]`)
    ArrayLit(ArrayLitExpr),
    /// Array indexing (e.g., `arr[0]`)
    Index(IndexExpr),
    /// Path expression (e.g., `Color::Red`)
    Path(PathExpr),
    /// Enum struct variant literal (e.g., `Shape::Circle { radius: 5 }`)
    EnumStructLit(EnumStructLitExpr),
    /// Associated function call (e.g., `Point::origin()`)
    AssocFnCall(AssocFnCallExpr),
    /// Self expression (e.g., `self` in method bodies)
    SelfExpr(SelfExpr),
    /// Comptime block expression (e.g., `comptime { 1 + 2 }`)
    Comptime(ComptimeBlockExpr),
    /// Comptime unroll for expression (e.g., `comptime_unroll for field in info.fields { ... }`)
    ComptimeUnrollFor(ComptimeUnrollForExpr),
    /// Checked block expression (e.g., `checked { @ptr_read(p) }`)
    Checked(CheckedBlockExpr),
    /// Type literal expression (e.g., `i32` used as a value in generic function calls)
    TypeLit(TypeLitExpr),
    /// Tuple literal expression (e.g., `(1, true)`, `(42,)`) (ADR-0048)
    Tuple(TupleExpr),
    /// Tuple index expression (e.g., `t.0`, `t.1`) (ADR-0048)
    TupleIndex(TupleIndexExpr),
    /// Range expression for slice subscripts (ADR-0064): `..`, `a..`, `..b`,
    /// `a..b`, `a..b :s`. Only legal as the index of an `IndexExpr`; sema
    /// rejects bare ranges in other positions.
    Range(RangeExpr),
    /// Anonymous function expression (e.g., `fn(x: i32) -> i32 { x + 1 }`) (ADR-0055)
    AnonFn(AnonFnExpr),
    /// Error node for recovered parse errors.
    /// Used by error recovery to continue parsing after a syntax error.
    Error(Span),
}

/// An integer literal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IntLit {
    pub value: u64,
    pub span: Span,
}

/// A floating-point literal, stored as f64 bits for Eq compatibility.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FloatLit {
    /// The f64 value stored as bits via `f64::to_bits()`.
    pub bits: u64,
    pub span: Span,
}

/// A string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StringLit {
    pub value: Spur,
    pub span: Span,
}

/// A character literal — Unicode scalar value (ADR-0071).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CharLit {
    pub value: u32,
    pub span: Span,
}

/// A boolean literal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoolLit {
    pub value: bool,
    pub span: Span,
}

/// A binary expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BinaryExpr {
    pub left: Box<Expr>,
    pub op: BinaryOp,
    pub right: Box<Expr>,
    pub span: Span,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BinaryOp {
    // Arithmetic
    Add, // +
    Sub, // -
    Mul, // *
    Div, // /
    Mod, // %
    // Comparison
    Eq, // ==
    Ne, // !=
    Lt, // <
    Gt, // >
    Le, // <=
    Ge, // >=
    // Logical
    And, // &&
    Or,  // ||
    // Bitwise
    BitAnd, // &
    BitOr,  // |
    BitXor, // ^
    Shl,    // <<
    Shr,    // >>
}

/// A unary expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnaryExpr {
    pub op: UnaryOp,
    pub operand: Box<Expr>,
    pub span: Span,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UnaryOp {
    Neg,    // -
    Not,    // !
    BitNot, // ~
    /// Immutable reference construction: `&x` (ADR-0062). Operand must be an lvalue.
    Ref,
    /// Mutable reference construction: `&mut x` (ADR-0062). Operand must be an lvalue.
    MutRef,
}

/// A parenthesized expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParenExpr {
    pub inner: Box<Expr>,
    pub span: Span,
}

/// A block expression containing statements and a final expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlockExpr {
    /// Statements in the block
    pub statements: Vec<Statement>,
    /// Final expression (the value of the block)
    pub expr: Box<Expr>,
    pub span: Span,
}

/// An if expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IfExpr {
    /// Condition (must be bool)
    pub cond: Box<Expr>,
    /// Then branch
    pub then_block: BlockExpr,
    /// Optional else branch
    pub else_block: Option<BlockExpr>,
    pub span: Span,
    /// ADR-0079 follow-up: `comptime if cond { … } else { … }` — sema
    /// evaluates `cond` at comptime and emits *only* the chosen
    /// branch's runtime AIR, never analyzing the discarded one.
    /// Lets the prelude `derive Clone` / `derive Copy` dispatch on
    /// `@type_info(Self).kind` without forcing the unused branch to
    /// type-check against the host type.
    pub is_comptime: bool,
}

/// A match expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MatchExpr {
    /// The value being matched (scrutinee)
    pub scrutinee: Box<Expr>,
    /// Match arms
    pub arms: Vec<MatchArm>,
    pub span: Span,
}

/// A single arm in a match expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MatchArm {
    /// The pattern to match
    pub pattern: Pattern,
    /// The body expression
    pub body: Box<Expr>,
    pub span: Span,
}

/// A pattern in a let binding or match arm (ADR-0049).
///
/// A single recursive `Pattern` type is used in both contexts. Sema enforces
/// refutability per context (let requires irrefutable patterns, match accepts any).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Pattern {
    /// Wildcard pattern `_` - matches anything, irrefutable
    Wildcard(Span),
    /// Named binding (`x` or `mut x`), irrefutable
    Ident {
        is_mut: bool,
        name: Ident,
        span: Span,
    },
    /// Integer literal pattern (positive or zero), refutable
    Int(IntLit),
    /// Negative integer literal pattern (e.g., `-1`, `-42`), refutable
    NegInt(NegIntLit),
    /// Boolean literal pattern, refutable
    Bool(BoolLit),
    /// Path pattern (e.g., `Color::Red` for a unit enum variant), refutable
    Path(PathPattern),
    /// Data variant pattern with positional sub-patterns (e.g., `Option::Some(x)`)
    DataVariant {
        /// Optional module prefix
        base: Option<Box<Expr>>,
        /// Enum type name
        type_name: Ident,
        /// Variant name
        variant: Ident,
        /// Sub-patterns for each field (may include `..` via `TupleElemPattern::Rest`)
        fields: Vec<TupleElemPattern>,
        span: Span,
    },
    /// Struct variant pattern with named field sub-patterns (e.g., `Shape::Circle { radius }`)
    StructVariant {
        /// Optional module prefix
        base: Option<Box<Expr>>,
        /// Enum type name
        type_name: Ident,
        /// Variant name
        variant: Ident,
        /// Named field sub-patterns (may include `..` via a field with `field_name: None`)
        fields: Vec<FieldPattern>,
        span: Span,
    },
    /// Struct destructure pattern (e.g., `Point { x, y }`), irrefutable when all sub-patterns are.
    Struct {
        /// The struct type name (for anonymous structs, resolved via a local type alias)
        type_name: Ident,
        /// Named field sub-patterns (may include `..`)
        fields: Vec<FieldPattern>,
        span: Span,
    },
    /// Tuple destructure pattern (e.g., `(a, b)` or `(a, .., c)`), irrefutable when all sub-patterns are.
    Tuple {
        elems: Vec<TupleElemPattern>,
        span: Span,
    },
    /// ADR-0079 Phase 3: a `comptime_unroll for` arm template. Only
    /// valid at the top level of a match arm — sema rejects it
    /// elsewhere. The arm fires once per element of `iterable`; the
    /// compiler synthesizes a variant-specific concrete pattern per
    /// iteration and substitutes `binding` as a comptime value in
    /// the arm body.
    ComptimeUnrollArm {
        binding: Ident,
        iterable: Box<Expr>,
        span: Span,
    },
}

/// One position in a tuple-like sequence (tuple pattern or data-variant fields):
/// either a sub-pattern or the rest-pattern `..` (ADR-0049 Phase 6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TupleElemPattern {
    Pattern(Pattern),
    /// `..` at this position; matches zero or more remaining positions.
    Rest(Span),
}

/// A named field binding in a struct / struct-variant pattern.
///
/// When `field_name` is `None`, this is the `..` sentinel (ADR-0049 Phase 6).
/// Otherwise, `sub` may be:
/// - `None` — shorthand: `field` (binds field to a same-named local).
/// - `Some(Pattern::Wildcard(..))` — `field: _` drops the field.
/// - `Some(Pattern::Ident { ... })` — `field: name` renames.
/// - `Some(other)` — recursive destructure of the field's value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FieldPattern {
    pub field_name: Option<Ident>,
    pub sub: Option<Pattern>,
    /// Only meaningful for shorthand or when `sub` is `Pattern::Ident`.
    pub is_mut: bool,
    pub span: Span,
}

impl TupleElemPattern {
    pub fn span(&self) -> Span {
        match self {
            TupleElemPattern::Pattern(p) => p.span(),
            TupleElemPattern::Rest(s) => *s,
        }
    }
}

/// A negative integer literal pattern.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NegIntLit {
    /// The absolute value of the negative integer
    pub value: u64,
    /// Span covering the entire pattern (minus sign and literal)
    pub span: Span,
}

/// A path pattern (e.g., `Color::Red` or `module.Color::Red` for enum variant matching).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PathPattern {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Color::Red`)
    pub base: Option<Box<Expr>>,
    /// The type name (e.g., `Color`)
    pub type_name: Ident,
    /// The variant name (e.g., `Red`)
    pub variant: Ident,
    pub span: Span,
}

impl Pattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard(span) => *span,
            Pattern::Ident { span, .. } => *span,
            Pattern::Int(lit) => lit.span,
            Pattern::NegInt(lit) => lit.span,
            Pattern::Bool(lit) => lit.span,
            Pattern::Path(path) => path.span,
            Pattern::DataVariant { span, .. } => *span,
            Pattern::StructVariant { span, .. } => *span,
            Pattern::Struct { span, .. } => *span,
            Pattern::Tuple { span, .. } => *span,
            Pattern::ComptimeUnrollArm { span, .. } => *span,
        }
    }
}

/// Argument passing mode.
///
/// ADR-0076 retired the `inout` / `borrow` argument prefixes; references are
/// now constructed with `&x` / `&mut x` and pass as `Normal` arguments whose
/// type carries the ref-ness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ArgMode {
    /// Normal pass-by-value argument
    #[default]
    Normal,
}

/// An argument in a function call.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CallArg {
    /// The passing mode for this argument
    pub mode: ArgMode,
    /// The argument expression
    pub expr: Expr,
    /// Span covering the entire argument
    pub span: Span,
}

/// A function call expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CallExpr {
    /// Function name
    pub name: Ident,
    /// Arguments
    pub args: Vec<CallArg>,
    pub span: Span,
}

/// An argument to an intrinsic call (can be an expression or a type).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IntrinsicArg {
    /// An expression argument (e.g., `@dbg(42)`)
    Expr(Expr),
    /// A type argument (e.g., `@size_of(i32)`)
    Type(TypeExpr),
}

/// An intrinsic call expression (e.g., `@dbg(42)` or `@size_of(i32)`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IntrinsicCallExpr {
    /// Intrinsic name (without the @)
    pub name: Ident,
    /// Arguments (can be expressions or types)
    pub args: Vec<IntrinsicArg>,
    pub span: Span,
}

/// A struct literal expression (e.g., `Point { x: 1, y: 2 }` or `module.Point { x: 1, y: 2 }`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StructLitExpr {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Point { ... }`)
    pub base: Option<Box<Expr>>,
    /// Struct type name
    pub name: Ident,
    /// Field initializers
    pub fields: Vec<FieldInit>,
    pub span: Span,
}

/// A field initializer in a struct literal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FieldInit {
    /// Field name
    pub name: Ident,
    /// Field value
    pub value: Box<Expr>,
    pub span: Span,
}

/// A tuple literal expression (e.g., `(1, true)`, `(42,)`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TupleExpr {
    /// Element expressions
    pub elems: Vec<Expr>,
    pub span: Span,
}

/// An anonymous function expression (e.g., `fn(x: i32) -> i32 { x + 1 }`).
///
/// ADR-0055: desugars to an anonymous struct with a single `__call` method
/// and is instantiated as an empty struct literal. Each `AnonFn` site produces
/// a distinct type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnonFnExpr {
    /// Parameters (all require type annotations, like named functions).
    pub params: Vec<Param>,
    /// Return type (None means implicit unit `()`, same as named functions).
    pub return_type: Option<TypeExpr>,
    /// Function body (always a block).
    pub body: BlockExpr,
    /// Span covering the entire `fn(...) { ... }` expression.
    pub span: Span,
}

/// A tuple index expression (e.g., `t.0`, `t.1`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TupleIndexExpr {
    /// Base expression (the tuple value)
    pub base: Box<Expr>,
    /// Numeric index (0-based)
    pub index: u32,
    /// Span of the whole expression (base through the index token)
    pub span: Span,
    /// Span of just the index token (for diagnostics)
    pub index_span: Span,
}

/// A field access expression (e.g., `point.x`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FieldExpr {
    /// Base expression (the struct value)
    pub base: Box<Expr>,
    /// Field name
    pub field: Ident,
    pub span: Span,
}

/// A method call expression (e.g., `point.distance()`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MethodCallExpr {
    /// Base expression (the receiver)
    pub receiver: Box<Expr>,
    /// Method name
    pub method: Ident,
    /// Arguments (excluding self)
    pub args: Vec<CallArg>,
    pub span: Span,
}

/// An array literal expression (e.g., `[1, 2, 3]`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArrayLitExpr {
    /// Array elements
    pub elements: Vec<Expr>,
    pub span: Span,
}

/// An array index expression (e.g., `arr[0]`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndexExpr {
    /// The array being indexed
    pub base: Box<Expr>,
    /// The index expression. May be `Expr::Range(_)` for slice subscripts
    /// (ADR-0064); sema enforces that range subscripts appear only as the
    /// place under `&` / `&mut`.
    pub index: Box<Expr>,
    pub span: Span,
}

/// A range expression used as a slice subscript (ADR-0064).
///
/// Ranges are recognized only inside `[ … ]`. `lo` and `hi` are optional
/// (defaulting to 0 and `arr.len()` respectively).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RangeExpr {
    pub lo: Option<Box<Expr>>,
    pub hi: Option<Box<Expr>>,
    pub span: Span,
}

/// A path expression (e.g., `Color::Red` or `module.Color::Red` for enum variant).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PathExpr {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Color::Red`)
    pub base: Option<Box<Expr>>,
    /// The type name (e.g., `Color`)
    pub type_name: Ident,
    /// The variant name (e.g., `Red`)
    pub variant: Ident,
    pub span: Span,
}

/// An enum struct variant literal expression (e.g., `Shape::Circle { radius: 5 }`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnumStructLitExpr {
    /// Optional module/namespace prefix
    pub base: Option<Box<Expr>>,
    /// The enum type name (e.g., `Shape`)
    pub type_name: Ident,
    /// The variant name (e.g., `Circle`)
    pub variant: Ident,
    /// Field initializers
    pub fields: Vec<FieldInit>,
    pub span: Span,
}

/// An associated function call expression (e.g., `Point::origin()` or `module.Point::origin()`).
///
/// ADR-0063 also accepts a type-call LHS: `Ptr(i32)::null()`. The
/// parenthesised arguments after `type_name` are stored in `type_args`. They
/// are empty for the legacy form (`Point::origin()`) and non-empty for the
/// type-call form. Sema interprets each `type_args` entry as a type
/// expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AssocFnCallExpr {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Point::origin()`)
    pub base: Option<Box<Expr>>,
    /// The type name (e.g., `Point` or `Ptr`)
    pub type_name: Ident,
    /// Type arguments for a type-call LHS (e.g., `[i32]` in `Ptr(i32)::null()`).
    /// Empty for the plain `Type::function()` form.
    pub type_args: Vec<Expr>,
    /// The function name (e.g., `origin`)
    pub function: Ident,
    /// Arguments
    pub args: Vec<CallArg>,
    pub span: Span,
}

/// A statement (does not produce a value).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Statement {
    /// Let binding: `let x = expr;` or `let mut x = expr;`
    Let(LetStatement),
    /// Assignment: `x = expr;`
    Assign(AssignStatement),
    /// Expression statement: `expr;`
    Expr(Expr),
}

/// A let binding statement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LetStatement {
    /// Directives applied to this let binding
    pub directives: Directives,
    /// Whether the binding is mutable.
    ///
    /// For ident patterns this is the binding's mutability. For destructuring
    /// patterns, per-binding mutability is stored on each sub-pattern; this
    /// top-level flag is ignored.
    pub is_mut: bool,
    /// The binding pattern (ADR-0049). Must be irrefutable in a let statement.
    pub pattern: Pattern,
    /// Optional type annotation
    pub ty: Option<TypeExpr>,
    /// Initializer expression
    pub init: Box<Expr>,
    pub span: Span,
}

/// An assignment statement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AssignStatement {
    /// Assignment target (variable or field)
    pub target: AssignTarget,
    /// Value expression
    pub value: Box<Expr>,
    pub span: Span,
}

/// An assignment target.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AssignTarget {
    /// Variable assignment (e.g., `x = 5`)
    Var(Ident),
    /// Field assignment (e.g., `point.x = 5`)
    Field(FieldExpr),
    /// Index assignment (e.g., `arr[0] = 5`)
    Index(IndexExpr),
}

/// A while loop expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WhileExpr {
    /// Condition (must be bool)
    pub cond: Box<Expr>,
    /// Loop body
    pub body: BlockExpr,
    pub span: Span,
}

/// A for-in loop expression (e.g., `for x in expr { body }`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForExpr {
    /// Loop variable name
    pub binding: Ident,
    /// Whether the loop variable is mutable (`for mut x in ...`)
    pub is_mut: bool,
    /// The iterable expression (array or Range)
    pub iterable: Box<Expr>,
    /// Loop body
    pub body: BlockExpr,
    pub span: Span,
}

/// An infinite loop expression.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LoopExpr {
    /// Loop body
    pub body: BlockExpr,
    pub span: Span,
}

/// A break expression (exits the innermost loop).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BreakExpr {
    pub span: Span,
}

/// A continue expression (skips to the next iteration of the innermost loop).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContinueExpr {
    pub span: Span,
}

/// A return expression (returns a value from the current function).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReturnExpr {
    /// The value to return (None for `return;` in unit-returning functions)
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

/// A self expression (the `self` keyword in method bodies).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SelfExpr {
    pub span: Span,
}

/// A comptime block expression (e.g., `comptime { 1 + 2 }`).
/// The expression inside must be evaluable at compile time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComptimeBlockExpr {
    /// The expression to evaluate at compile time
    pub expr: Box<Expr>,
    pub span: Span,
}

/// A comptime_unroll for expression.
/// The collection is evaluated at compile time, then the body is unrolled once per element.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComptimeUnrollForExpr {
    /// Loop variable name
    pub binding: Ident,
    /// The iterable expression (must be comptime-known)
    pub iterable: Box<Expr>,
    /// Loop body
    pub body: BlockExpr,
    pub span: Span,
}

/// A checked block expression (e.g., `checked { @ptr_read(p) }`).
/// Unchecked operations (raw pointer manipulation, calling unchecked functions)
/// are only allowed inside checked blocks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckedBlockExpr {
    /// The expression inside the checked block
    pub expr: Box<Expr>,
    pub span: Span,
}

/// A type literal expression (e.g., `i32` used as a value).
/// This represents a type used as a value in expression context, typically
/// as an argument to a generic function with comptime parameters.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TypeLitExpr {
    /// The type being used as a value
    pub type_expr: TypeExpr,
    pub span: Span,
}

impl Expr {
    /// Get the span of this expression.
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(lit) => lit.span,
            Expr::Float(lit) => lit.span,
            Expr::String(lit) => lit.span,
            Expr::Char(lit) => lit.span,
            Expr::Bool(lit) => lit.span,
            Expr::Unit(lit) => lit.span,
            Expr::Ident(ident) => ident.span,
            Expr::Binary(bin) => bin.span,
            Expr::Unary(un) => un.span,
            Expr::Paren(paren) => paren.span,
            Expr::Block(block) => block.span,
            Expr::If(if_expr) => if_expr.span,
            Expr::Match(match_expr) => match_expr.span,
            Expr::While(while_expr) => while_expr.span,
            Expr::For(for_expr) => for_expr.span,
            Expr::Loop(loop_expr) => loop_expr.span,
            Expr::Call(call) => call.span,
            Expr::Break(break_expr) => break_expr.span,
            Expr::Continue(continue_expr) => continue_expr.span,
            Expr::Return(return_expr) => return_expr.span,
            Expr::StructLit(struct_lit) => struct_lit.span,
            Expr::Field(field_expr) => field_expr.span,
            Expr::MethodCall(method_call) => method_call.span,
            Expr::IntrinsicCall(intrinsic) => intrinsic.span,
            Expr::ArrayLit(array_lit) => array_lit.span,
            Expr::Index(index_expr) => index_expr.span,
            Expr::Path(path_expr) => path_expr.span,
            Expr::EnumStructLit(lit) => lit.span,
            Expr::AssocFnCall(assoc_fn_call) => assoc_fn_call.span,
            Expr::SelfExpr(self_expr) => self_expr.span,
            Expr::Comptime(comptime_expr) => comptime_expr.span,
            Expr::ComptimeUnrollFor(e) => e.span,
            Expr::Checked(checked_expr) => checked_expr.span,
            Expr::TypeLit(type_lit) => type_lit.span,
            Expr::Tuple(tuple) => tuple.span,
            Expr::TupleIndex(ti) => ti.span,
            Expr::Range(range_expr) => range_expr.span,
            Expr::AnonFn(anon_fn) => anon_fn.span,
            Expr::Error(span) => *span,
        }
    }
}

// Display implementations for AST pretty-printing

impl fmt::Display for Ast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for item in &self.items {
            match item {
                Item::Function(func) => fmt_function(f, func, 0)?,
                Item::Struct(s) => fmt_struct(f, s, 0)?,
                Item::Enum(e) => fmt_enum(f, e, 0)?,
                Item::Interface(i) => fmt_interface(f, i, 0)?,
                Item::Derive(d) => fmt_derive(f, d, 0)?,
                Item::Const(c) => fmt_const(f, c, 0)?,
                Item::Error(span) => writeln!(f, "Error({:?})", span)?,
            }
        }
        Ok(())
    }
}

fn indent(f: &mut fmt::Formatter<'_>, level: usize) -> fmt::Result {
    for _ in 0..level {
        write!(f, "  ")?;
    }
    Ok(())
}

fn fmt_struct(f: &mut fmt::Formatter<'_>, s: &StructDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    for directive in &s.directives {
        write!(f, "@sym:{} ", directive.name.name.into_usize())?;
    }
    match s.posture {
        Posture::Copy => write!(f, "@mark(copy) ")?,
        Posture::Linear => write!(f, "@mark(linear) ")?,
        Posture::Affine => {}
    }
    writeln!(f, "Struct sym:{}", s.name.name.into_usize())?;
    for field in &s.fields {
        indent(f, level + 1)?;
        writeln!(
            f,
            "Field sym:{} : {}",
            field.name.name.into_usize(),
            field.ty
        )?;
    }
    for method in &s.methods {
        fmt_method(f, method, level + 1)?;
    }
    Ok(())
}

fn fmt_enum(f: &mut fmt::Formatter<'_>, e: &EnumDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    writeln!(f, "Enum sym:{}", e.name.name.into_usize())?;
    for variant in &e.variants {
        indent(f, level + 1)?;
        writeln!(f, "Variant sym:{}", variant.name.name.into_usize())?;
    }
    for method in &e.methods {
        fmt_method(f, method, level + 1)?;
    }
    Ok(())
}

fn fmt_const(f: &mut fmt::Formatter<'_>, c: &ConstDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    for directive in &c.directives {
        write!(f, "@sym:{} ", directive.name.name.into_usize())?;
    }
    if c.visibility == Visibility::Public {
        write!(f, "pub ")?;
    }
    write!(f, "Const sym:{}", c.name.name.into_usize())?;
    if let Some(ref ty) = c.ty {
        write!(f, ": {}", ty)?;
    }
    writeln!(f)?;
    fmt_expr(f, &c.init, level + 1)?;
    Ok(())
}

fn fmt_interface(f: &mut fmt::Formatter<'_>, iface: &InterfaceDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    if iface.visibility == Visibility::Public {
        write!(f, "pub ")?;
    }
    writeln!(f, "Interface sym:{}", iface.name.name.into_usize())?;
    for sig in &iface.methods {
        indent(f, level + 1)?;
        write!(f, "MethodSig sym:{}(self", sig.name.name.into_usize())?;
        for param in &sig.params {
            write!(f, ", ")?;
            fmt_param(f, param)?;
        }
        write!(f, ")")?;
        if let Some(ref ret) = sig.return_type {
            write!(f, " -> {}", ret)?;
        }
        writeln!(f)?;
    }
    Ok(())
}

fn fmt_derive(f: &mut fmt::Formatter<'_>, d: &DeriveDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    writeln!(f, "Derive sym:{}", d.name.name.into_usize())?;
    for method in &d.methods {
        fmt_method(f, method, level + 1)?;
    }
    Ok(())
}

fn fmt_method(f: &mut fmt::Formatter<'_>, method: &Method, level: usize) -> fmt::Result {
    indent(f, level)?;
    write!(f, "Method sym:{}", method.name.name.into_usize())?;
    write!(f, "(")?;
    if method.receiver.is_some() {
        write!(f, "self")?;
        if !method.params.is_empty() {
            write!(f, ", ")?;
        }
    }
    for (i, param) in method.params.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        fmt_param(f, param)?;
    }
    write!(f, ")")?;
    if let Some(ref ret) = method.return_type {
        write!(f, " -> {}", ret)?;
    }
    writeln!(f)?;
    fmt_expr(f, &method.body, level + 1)?;
    Ok(())
}

fn fmt_param(f: &mut fmt::Formatter<'_>, param: &Param) -> fmt::Result {
    match param.mode {
        ParamMode::Comptime => write!(f, "comptime ")?,
        ParamMode::Normal => {}
    }
    write!(f, "sym:{}: {}", param.name.name.into_usize(), param.ty)
}

fn fmt_call_arg(f: &mut fmt::Formatter<'_>, arg: &CallArg, level: usize) -> fmt::Result {
    match arg.mode {
        ArgMode::Normal => fmt_expr(f, &arg.expr, level),
    }
}

fn fmt_function(f: &mut fmt::Formatter<'_>, func: &Function, level: usize) -> fmt::Result {
    indent(f, level)?;
    if func.is_unchecked {
        write!(f, "unchecked ")?;
    }
    write!(f, "Function sym:{}", func.name.name.into_usize())?;
    if !func.params.is_empty() {
        write!(f, "(")?;
        for (i, param) in func.params.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            fmt_param(f, param)?;
        }
        write!(f, ")")?;
    }
    if let Some(ref ret) = func.return_type {
        write!(f, " -> {}", ret)?;
    }
    writeln!(f)?;
    fmt_expr(f, &func.body, level + 1)?;
    Ok(())
}

fn fmt_expr(f: &mut fmt::Formatter<'_>, expr: &Expr, level: usize) -> fmt::Result {
    indent(f, level)?;
    match expr {
        Expr::Int(lit) => writeln!(f, "Int({})", lit.value),
        Expr::Float(lit) => writeln!(f, "Float({})", f64::from_bits(lit.bits)),
        Expr::String(lit) => writeln!(f, "String(sym:{})", lit.value.into_usize()),
        Expr::Char(lit) => writeln!(f, "Char(U+{:04X})", lit.value),
        Expr::Bool(lit) => writeln!(f, "Bool({})", lit.value),
        Expr::Unit(_) => writeln!(f, "Unit"),
        Expr::Ident(ident) => writeln!(f, "Ident(sym:{})", ident.name.into_usize()),
        Expr::Binary(bin) => {
            writeln!(f, "Binary {:?}", bin.op)?;
            fmt_expr(f, &bin.left, level + 1)?;
            fmt_expr(f, &bin.right, level + 1)
        }
        Expr::Unary(un) => {
            writeln!(f, "Unary {:?}", un.op)?;
            fmt_expr(f, &un.operand, level + 1)
        }
        Expr::Paren(paren) => {
            writeln!(f, "Paren")?;
            fmt_expr(f, &paren.inner, level + 1)
        }
        Expr::Block(block) => {
            writeln!(f, "Block")?;
            for stmt in &block.statements {
                fmt_stmt(f, stmt, level + 1)?;
            }
            fmt_expr(f, &block.expr, level + 1)
        }
        Expr::If(if_expr) => {
            writeln!(f, "If")?;
            indent(f, level + 1)?;
            writeln!(f, "Cond:")?;
            fmt_expr(f, &if_expr.cond, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Then:")?;
            fmt_block_expr(f, &if_expr.then_block, level + 2)?;
            if let Some(ref else_block) = if_expr.else_block {
                indent(f, level + 1)?;
                writeln!(f, "Else:")?;
                fmt_block_expr(f, else_block, level + 2)?;
            }
            Ok(())
        }
        Expr::Match(match_expr) => {
            writeln!(f, "Match")?;
            indent(f, level + 1)?;
            writeln!(f, "Scrutinee:")?;
            fmt_expr(f, &match_expr.scrutinee, level + 2)?;
            for arm in &match_expr.arms {
                indent(f, level + 1)?;
                writeln!(f, "Arm {:?} =>", arm.pattern)?;
                fmt_expr(f, &arm.body, level + 2)?;
            }
            Ok(())
        }
        Expr::While(while_expr) => {
            writeln!(f, "While")?;
            indent(f, level + 1)?;
            writeln!(f, "Cond:")?;
            fmt_expr(f, &while_expr.cond, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Body:")?;
            fmt_block_expr(f, &while_expr.body, level + 2)
        }
        Expr::For(for_expr) => {
            writeln!(
                f,
                "For {}sym:{}",
                if for_expr.is_mut { "mut " } else { "" },
                for_expr.binding.name.into_usize()
            )?;
            indent(f, level + 1)?;
            writeln!(f, "Iterable:")?;
            fmt_expr(f, &for_expr.iterable, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Body:")?;
            fmt_block_expr(f, &for_expr.body, level + 2)
        }
        Expr::Loop(loop_expr) => {
            writeln!(f, "Loop")?;
            fmt_block_expr(f, &loop_expr.body, level + 1)
        }
        Expr::Call(call) => {
            writeln!(f, "Call sym:{}", call.name.name.into_usize())?;
            for arg in &call.args {
                fmt_call_arg(f, arg, level + 1)?;
            }
            Ok(())
        }
        Expr::IntrinsicCall(intrinsic) => {
            writeln!(f, "Intrinsic @sym:{}", intrinsic.name.name.into_usize())?;
            for arg in &intrinsic.args {
                match arg {
                    IntrinsicArg::Expr(expr) => fmt_expr(f, expr, level + 1)?,
                    IntrinsicArg::Type(ty) => {
                        indent(f, level + 1)?;
                        writeln!(f, "Type {:?}", ty)?;
                    }
                }
            }
            Ok(())
        }
        Expr::Break(_) => writeln!(f, "Break"),
        Expr::Continue(_) => writeln!(f, "Continue"),
        Expr::Return(ret) => {
            if let Some(ref value) = ret.value {
                writeln!(f, "Return")?;
                fmt_expr(f, value, level + 1)
            } else {
                writeln!(f, "Return (unit)")
            }
        }
        Expr::StructLit(lit) => {
            writeln!(f, "StructLit sym:{}", lit.name.name.into_usize())?;
            for field in &lit.fields {
                indent(f, level + 1)?;
                writeln!(f, "sym:{} =", field.name.name.into_usize())?;
                fmt_expr(f, &field.value, level + 2)?;
            }
            Ok(())
        }
        Expr::Field(field) => {
            writeln!(f, "Field .sym:{}", field.field.name.into_usize())?;
            fmt_expr(f, &field.base, level + 1)
        }
        Expr::MethodCall(method_call) => {
            writeln!(
                f,
                "MethodCall .sym:{}",
                method_call.method.name.into_usize()
            )?;
            indent(f, level + 1)?;
            writeln!(f, "Receiver:")?;
            fmt_expr(f, &method_call.receiver, level + 2)?;
            if !method_call.args.is_empty() {
                indent(f, level + 1)?;
                writeln!(f, "Args:")?;
                for arg in &method_call.args {
                    fmt_call_arg(f, arg, level + 2)?;
                }
            }
            Ok(())
        }
        Expr::ArrayLit(array) => {
            writeln!(f, "ArrayLit")?;
            for elem in &array.elements {
                fmt_expr(f, elem, level + 1)?;
            }
            Ok(())
        }
        Expr::Index(index) => {
            writeln!(f, "Index")?;
            indent(f, level + 1)?;
            writeln!(f, "Base:")?;
            fmt_expr(f, &index.base, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Index:")?;
            fmt_expr(f, &index.index, level + 2)
        }
        Expr::Path(path) => writeln!(
            f,
            "Path sym:{}::sym:{}",
            path.type_name.name.into_usize(),
            path.variant.name.into_usize()
        ),
        Expr::EnumStructLit(lit) => {
            writeln!(
                f,
                "EnumStructLit sym:{}::sym:{}",
                lit.type_name.name.into_usize(),
                lit.variant.name.into_usize()
            )?;
            for field in &lit.fields {
                indent(f, level + 1)?;
                writeln!(f, "field sym:{}:", field.name.name.into_usize())?;
                fmt_expr(f, &field.value, level + 2)?;
            }
            Ok(())
        }
        Expr::AssocFnCall(assoc_fn_call) => {
            writeln!(
                f,
                "AssocFnCall sym:{}::sym:{}",
                assoc_fn_call.type_name.name.into_usize(),
                assoc_fn_call.function.name.into_usize()
            )?;
            for arg in &assoc_fn_call.args {
                fmt_call_arg(f, arg, level + 1)?;
            }
            Ok(())
        }
        Expr::SelfExpr(_) => {
            writeln!(f, "SelfExpr")
        }
        Expr::Comptime(comptime) => {
            writeln!(f, "Comptime")?;
            fmt_expr(f, &comptime.expr, level + 1)
        }
        Expr::ComptimeUnrollFor(unroll) => {
            writeln!(
                f,
                "ComptimeUnrollFor sym:{}",
                unroll.binding.name.into_usize()
            )?;
            indent(f, level + 1)?;
            writeln!(f, "Iterable:")?;
            fmt_expr(f, &unroll.iterable, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Body:")?;
            fmt_block_expr(f, &unroll.body, level + 2)
        }
        Expr::Checked(checked) => {
            writeln!(f, "Checked")?;
            fmt_expr(f, &checked.expr, level + 1)
        }
        Expr::TypeLit(type_lit) => {
            writeln!(f, "TypeLit({})", type_lit.type_expr)
        }
        Expr::Tuple(tuple) => {
            writeln!(f, "Tuple[{}]", tuple.elems.len())?;
            for elem in &tuple.elems {
                fmt_expr(f, elem, level + 1)?;
            }
            Ok(())
        }
        Expr::TupleIndex(ti) => {
            writeln!(f, "TupleIndex .{}", ti.index)?;
            fmt_expr(f, &ti.base, level + 1)
        }
        Expr::Range(range_expr) => {
            writeln!(f, "Range")?;
            if let Some(lo) = &range_expr.lo {
                indent(f, level + 1)?;
                writeln!(f, "Lo:")?;
                fmt_expr(f, lo, level + 2)?;
            }
            if let Some(hi) = &range_expr.hi {
                indent(f, level + 1)?;
                writeln!(f, "Hi:")?;
                fmt_expr(f, hi, level + 2)?;
            }
            Ok(())
        }
        Expr::AnonFn(anon_fn) => {
            writeln!(f, "AnonFn (params: {})", anon_fn.params.len())?;
            for param in &anon_fn.params {
                indent(f, level + 1)?;
                writeln!(f, "Param(sym:{})", param.name.name.into_usize())?;
            }
            if let Some(ret) = &anon_fn.return_type {
                indent(f, level + 1)?;
                writeln!(f, "Return -> {:?}", ret)?;
            }
            indent(f, level + 1)?;
            writeln!(f, "Body")?;
            fmt_block_expr(f, &anon_fn.body, level + 2)
        }
        Expr::Error(span) => {
            writeln!(f, "Error({:?})", span)
        }
    }
}

fn fmt_block_expr(f: &mut fmt::Formatter<'_>, block: &BlockExpr, level: usize) -> fmt::Result {
    for stmt in &block.statements {
        fmt_stmt(f, stmt, level)?;
    }
    fmt_expr(f, &block.expr, level)
}

/// Render a `Pattern` into the single-line bind-site form used by `Statement::Let`
/// display. This is a compact diagnostic-style format, not a parse-roundtrip form.
fn fmt_pattern(f: &mut fmt::Formatter<'_>, pat: &Pattern) -> fmt::Result {
    match pat {
        Pattern::Wildcard(_) => write!(f, " _"),
        Pattern::Ident {
            is_mut: true, name, ..
        } => write!(f, " mut sym:{}", name.name.into_usize()),
        Pattern::Ident { name, .. } => write!(f, " sym:{}", name.name.into_usize()),
        Pattern::Int(lit) => write!(f, " {}", lit.value),
        Pattern::NegInt(lit) => write!(f, " -{}", lit.value),
        Pattern::Bool(lit) => write!(f, " {}", lit.value),
        Pattern::Path(path) => write!(
            f,
            " sym:{}::sym:{}",
            path.type_name.name.into_usize(),
            path.variant.name.into_usize()
        ),
        Pattern::DataVariant {
            type_name,
            variant,
            fields,
            ..
        } => {
            write!(
                f,
                " sym:{}::sym:{}(",
                type_name.name.into_usize(),
                variant.name.into_usize()
            )?;
            for (i, elem) in fields.iter().enumerate() {
                if i > 0 {
                    write!(f, ",")?;
                }
                fmt_tuple_elem(f, elem)?;
            }
            write!(f, " )")
        }
        Pattern::StructVariant {
            type_name,
            variant,
            fields,
            ..
        } => {
            write!(
                f,
                " sym:{}::sym:{} {{",
                type_name.name.into_usize(),
                variant.name.into_usize()
            )?;
            for (i, fld) in fields.iter().enumerate() {
                if i > 0 {
                    write!(f, ",")?;
                }
                fmt_field_pattern(f, fld)?;
            }
            write!(f, " }}")
        }
        Pattern::Struct {
            type_name, fields, ..
        } => {
            write!(f, " sym:{} {{", type_name.name.into_usize())?;
            for (i, fld) in fields.iter().enumerate() {
                if i > 0 {
                    write!(f, ",")?;
                }
                fmt_field_pattern(f, fld)?;
            }
            write!(f, " }}")
        }
        Pattern::Tuple { elems, .. } => {
            write!(f, " (")?;
            for (i, elem) in elems.iter().enumerate() {
                if i > 0 {
                    write!(f, ",")?;
                }
                fmt_tuple_elem(f, elem)?;
            }
            if elems.len() == 1 {
                write!(f, ",")?;
            }
            write!(f, " )")
        }
        Pattern::ComptimeUnrollArm { binding, .. } => {
            write!(f, " comptime_unroll for sym:{}", binding.name.into_usize())
        }
    }
}

fn fmt_tuple_elem(f: &mut fmt::Formatter<'_>, elem: &TupleElemPattern) -> fmt::Result {
    match elem {
        TupleElemPattern::Pattern(p) => fmt_pattern(f, p),
        TupleElemPattern::Rest(_) => write!(f, " .."),
    }
}

fn fmt_field_pattern(f: &mut fmt::Formatter<'_>, fld: &FieldPattern) -> fmt::Result {
    match &fld.field_name {
        None => write!(f, " .."),
        Some(name) => {
            if fld.is_mut {
                write!(f, " mut")?;
            }
            write!(f, " sym:{}", name.name.into_usize())?;
            if let Some(sub) = &fld.sub {
                write!(f, ":")?;
                fmt_pattern(f, sub)?;
            }
            Ok(())
        }
    }
}

fn fmt_stmt(f: &mut fmt::Formatter<'_>, stmt: &Statement, level: usize) -> fmt::Result {
    indent(f, level)?;
    match stmt {
        Statement::Let(let_stmt) => {
            write!(f, "Let")?;
            if let_stmt.is_mut {
                write!(f, " mut")?;
            }
            fmt_pattern(f, &let_stmt.pattern)?;
            if let Some(ref ty) = let_stmt.ty {
                write!(f, ": {}", ty)?;
            }
            writeln!(f)?;
            fmt_expr(f, &let_stmt.init, level + 1)
        }
        Statement::Assign(assign) => {
            match &assign.target {
                AssignTarget::Var(ident) => writeln!(f, "Assign sym:{}", ident.name.into_usize())?,
                AssignTarget::Field(field) => {
                    writeln!(f, "Assign field .sym:{}", field.field.name.into_usize())?;
                    fmt_expr(f, &field.base, level + 1)?;
                }
                AssignTarget::Index(index) => {
                    writeln!(f, "Assign index")?;
                    indent(f, level + 1)?;
                    writeln!(f, "Base:")?;
                    fmt_expr(f, &index.base, level + 2)?;
                    indent(f, level + 1)?;
                    writeln!(f, "Index:")?;
                    fmt_expr(f, &index.index, level + 2)?;
                }
            }
            fmt_expr(f, &assign.value, level + 1)
        }
        Statement::Expr(expr) => {
            writeln!(f, "ExprStmt")?;
            fmt_expr(f, expr, level + 1)
        }
    }
}

// ============================================================================
// Struct-of-Arrays (SOA) AST Layout
// ============================================================================
//
// This section implements Zig-style SOA layout for the AST.
// See docs/designs/soa-ast-layout.md for full design rationale.
//
// Key characteristics:
// - Fixed-size nodes (tag + main_token + lhs + rhs)
// - Index-based references (no lifetimes)
// - Extra data array for nodes with >2 children
// - Single allocation for entire AST
// - Better cache locality than tree-based approach
//
// Migration: This will eventually replace the tree-based Ast above.
// For now, both representations coexist during Phase 2-3 migration.

/// Node index - references a node in the SOA AST.
///
/// Nodes are stored in parallel arrays (tags, data, extra) and referenced
/// by their index. This is similar to how RIR uses InstRef.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NodeIndex(pub u32);

impl NodeIndex {
    /// Create a new node index.
    pub const fn new(idx: u32) -> Self {
        NodeIndex(idx)
    }

    /// Get the raw index value.
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Get the index as usize for array indexing.
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Sentinel value representing "no node" or "null node".
/// Used for optional children (e.g., else block in if expression).
pub const NULL_NODE: NodeIndex = NodeIndex(u32::MAX);

/// Encode a UnaryOp into a u32 for storage in NodeData.
pub fn encode_unary_op(op: UnaryOp) -> u32 {
    match op {
        UnaryOp::Neg => 0,
        UnaryOp::Not => 1,
        UnaryOp::BitNot => 2,
        UnaryOp::Ref => 3,
        UnaryOp::MutRef => 4,
    }
}

/// Node tag - identifies what kind of node this is.
///
/// The tag determines how to interpret the lhs/rhs fields in NodeData.
/// See docs/designs/soa-ast-layout.md for encoding details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum NodeTag {
    // ===== Items (top-level declarations) =====
    /// Function declaration: fn name(params) -> ret { body }
    /// - lhs: index into extra (param count + param nodes)
    /// - rhs: body expression node
    Function,

    /// Struct declaration: struct Name { fields... methods... }
    /// - lhs: index into extra (field count + field nodes)
    /// - rhs: index into extra (method count + method nodes)
    StructDecl,

    /// Enum declaration: enum Name { variants... }
    /// - lhs: index into extra (variant count + variant nodes)
    /// - rhs: 0 (unused)
    EnumDecl,

    /// Constant declaration: const name: type = init;
    /// - lhs: type expression node (or NULL_NODE if inferred)
    /// - rhs: initializer expression node
    ConstDecl,

    // ===== Expressions - Literals =====
    /// Integer literal: 42
    /// - lhs: low 32 bits of u64 value
    /// - rhs: high 32 bits of u64 value
    IntLit,

    /// String literal: "hello"
    /// - lhs: Spur index (u32) for interned string
    /// - rhs: 0 (unused)
    StringLit,

    /// Boolean literal: true, false
    /// - lhs: 0 (false) or 1 (true)
    /// - rhs: 0 (unused)
    BoolLit,

    /// Unit literal: ()
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    UnitLit,

    // ===== Expressions - Identifiers and Paths =====
    /// Identifier: variable_name
    /// - lhs: Spur index (u32) for identifier name
    /// - rhs: 0 (unused)
    Ident,

    /// Path expression: Color::Red
    /// - lhs: type name identifier node
    /// - rhs: variant name identifier node
    Path,

    // ===== Expressions - Operations =====
    /// Unary expression: -x, !x, ~x
    /// - lhs: operand expression node
    /// - rhs: operator kind (u32 from UnaryOp enum)
    UnaryExpr,

    /// Parenthesized expression: (expr)
    /// - lhs: inner expression node
    /// - rhs: 0 (unused)
    ParenExpr,

    /// Binary expression: a + b, a == b, etc.
    /// - main_token: the operator token
    /// - lhs: left operand expression node
    /// - rhs: right operand expression node
    BinaryExpr,

    // ===== Expressions - Control Flow =====
    /// If expression: if cond { then } else { else_block }
    /// - lhs: condition expression node
    /// - rhs: index into extra (then_block, else_block or NULL_NODE)
    IfExpr,

    /// Match expression: match x { arms... }
    /// - lhs: scrutinee expression node
    /// - rhs: index into extra (arm count + arm nodes)
    MatchExpr,

    /// While loop: while cond { body }
    /// - lhs: condition expression node
    /// - rhs: body block expression node
    WhileExpr,

    /// For-in loop: for [mut] x in expr { body }
    /// - lhs: iterable expression node
    /// - rhs: body block expression node
    /// - extra: binding name (Spur index), is_mut flag
    ForExpr,

    /// Infinite loop: loop { body }
    /// - lhs: body block expression node
    /// - rhs: 0 (unused)
    LoopExpr,

    /// Break statement: break
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    BreakExpr,

    /// Continue statement: continue
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    ContinueExpr,

    /// Return statement: return expr
    /// - lhs: value expression node (or NULL_NODE for implicit unit return)
    /// - rhs: 0 (unused)
    ReturnExpr,

    // ===== Expressions - Blocks and Statements =====
    /// Block expression: { stmts...; final_expr }
    /// - lhs: index into extra (stmt count + stmt nodes)
    /// - rhs: final expression node
    BlockExpr,

    /// Let statement: let x: type = init;
    /// - lhs: pattern node (identifier or wildcard)
    /// - rhs: index into extra (flags, type_expr or NULL_NODE, init_expr)
    LetStmt,

    /// Assignment statement: x = value;
    /// - lhs: target node (Ident, FieldExpr, or IndexExpr)
    /// - rhs: value expression node
    AssignStmt,

    /// Expression statement: expr;
    /// - lhs: expression node
    /// - rhs: 0 (unused)
    ExprStmt,

    // ===== Expressions - Function Calls =====
    /// Function call: func(args...)
    /// - lhs: callee identifier node
    /// - rhs: index into extra (arg count + arg nodes)
    Call,

    /// Method call: receiver.method(args...)
    /// - lhs: receiver expression node
    /// - rhs: index into extra (method name, arg count, arg nodes)
    MethodCall,

    /// Intrinsic call: @intrinsic(args...)
    /// - lhs: intrinsic name identifier node
    /// - rhs: index into extra (arg count + arg nodes)
    IntrinsicCall,

    /// Associated function call: Type::func(args...)
    /// - lhs: type name identifier node
    /// - rhs: index into extra (fn name, arg count, arg nodes)
    AssocFnCall,

    // ===== Expressions - Struct Operations =====
    /// Struct literal: Point { x: 1, y: 2 }
    /// - lhs: struct name identifier node
    /// - rhs: index into extra (field init count + field init nodes)
    StructLit,

    /// Field access: obj.field
    /// - lhs: base expression node
    /// - rhs: field name identifier node
    FieldExpr,

    /// Field initializer in struct literal: field_name: value
    /// - lhs: field name identifier node
    /// - rhs: value expression node
    FieldInit,

    // ===== Expressions - Array Operations =====
    /// Array literal: [1, 2, 3]
    /// - lhs: index into extra (element count + element nodes)
    /// - rhs: 0 (unused, count stored in extra)
    ArrayLit,

    /// Array index: arr[index]
    /// - lhs: base expression node
    /// - rhs: index expression node
    IndexExpr,

    // ===== Expressions - Special =====
    /// Self expression: self
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    SelfExpr,

    /// Comptime block: comptime { expr }
    /// - lhs: inner expression node
    /// - rhs: 0 (unused)
    ComptimeBlockExpr,

    /// Checked block: checked { expr }
    /// - lhs: inner expression node
    /// - rhs: 0 (unused)
    CheckedBlockExpr,

    /// Type literal: i32 (used as value)
    /// - lhs: type expression node
    /// - rhs: 0 (unused)
    TypeLit,

    // ===== Type Expressions =====
    /// Named type: i32, MyStruct
    /// - lhs: name identifier node
    /// - rhs: 0 (unused)
    TypeNamed,

    /// Unit type: ()
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    TypeUnit,

    /// Never type: !
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    TypeNever,

    /// Array type: [T; N]
    /// - lhs: element type expression node
    /// - rhs: length (u32, stored directly)
    TypeArray,

    /// Anonymous struct type: struct { fields... methods... }
    /// - lhs: index into extra (field count + field nodes)
    /// - rhs: index into extra (method count + method nodes)
    TypeAnonStruct,

    // ===== Patterns =====
    /// Wildcard pattern: _
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    PatternWildcard,

    /// Integer literal pattern: 42, -1
    /// - lhs: low 32 bits of value
    /// - rhs: high 32 bits of value
    PatternInt,

    /// Boolean literal pattern: true, false
    /// - lhs: 0 (false) or 1 (true)
    /// - rhs: 0 (unused)
    PatternBool,

    /// Path pattern: Color::Red
    /// - lhs: type name identifier node
    /// - rhs: variant name identifier node
    PatternPath,

    // ===== Other Nodes =====
    /// Function parameter: name: type
    /// - lhs: name identifier node
    /// - rhs: type expression node
    /// - extra: flags (is_comptime, mode)
    Param,

    /// Method definition
    /// - lhs: index into extra (name, receiver?, param count, params)
    /// - rhs: index into extra (return_type or NULL_NODE, body_expr)
    Method,

    /// Match arm: pattern => body
    /// - lhs: pattern node
    /// - rhs: body expression node
    MatchArm,

    /// Call argument (wraps expression with mode flags)
    /// - lhs: expression node
    /// - rhs: flags (normal=0, inout=1, borrow=2)
    CallArg,

    /// Field declaration in struct
    /// - lhs: name identifier node
    /// - rhs: type expression node
    FieldDecl,

    /// Enum variant
    /// - lhs: name identifier node
    /// - rhs: 0 (unused, payload support future)
    EnumVariant,

    /// Directive: @name(args...)
    /// - lhs: name identifier node
    /// - rhs: index into extra (arg count + arg nodes)
    Directive,

    /// Directive argument (currently just identifiers)
    /// - lhs: identifier node
    /// - rhs: 0 (unused)
    DirectiveArg,

    // ===== Error Recovery =====
    /// Error node (parse error recovery)
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    ErrorNode,
}

/// Fixed-size node data (12 bytes total).
///
/// Each node in the SOA AST has:
/// - A tag (stored in separate tags array)
/// - A main_token (for span information)
/// - Two u32 slots (lhs and rhs) whose meaning depends on the tag
///
/// This matches Zig's design: compact, cache-friendly, uniform size.
#[derive(Debug, Clone, Copy)]
pub struct NodeData {
    /// Primary token for this node.
    ///
    /// Used for:
    /// - Span information in error messages
    /// - Operator tokens (for BinaryExpr, UnaryExpr)
    /// - Keyword tokens (for if, while, etc.)
    pub main_token: u32,

    /// Left child or first data slot.
    ///
    /// Interpretation depends on NodeTag - see NodeTag documentation.
    /// Common uses:
    /// - Left operand in binary expressions
    /// - Single child in unary expressions
    /// - Index into extra_data for multi-child nodes
    /// - Direct data storage (e.g., low 32 bits of u64)
    pub lhs: u32,

    /// Right child or second data slot.
    ///
    /// Interpretation depends on NodeTag - see NodeTag documentation.
    /// Common uses:
    /// - Right operand in binary expressions
    /// - Index into extra_data for multi-child nodes
    /// - Direct data storage (e.g., high 32 bits of u64)
    /// - Flags and small enums
    pub rhs: u32,
}

/// Struct-of-Arrays AST representation.
///
/// This is the new SOA-based AST that will replace the tree-based `Ast`.
/// During migration (Phases 2-3), both representations will coexist.
///
/// Design principles:
/// - All nodes stored in parallel arrays (tags, data, extra)
/// - Nodes reference children by index, not pointers
/// - Single allocation for entire AST (better cache locality)
/// - Cheap cloning (just clone Arc, not deep copy)
///
/// See docs/designs/soa-ast-layout.md for full design.
#[derive(Debug, Clone)]
pub struct SoaAst {
    /// Node tags (what kind of node is at each index).
    ///
    /// Index i contains the tag for node NodeIndex(i).
    /// Length of this vec == number of nodes in the AST.
    pub tags: Vec<NodeTag>,

    /// Node data (main_token + lhs + rhs for each node).
    ///
    /// Parallel array to tags - tags[i] and data[i] together describe node i.
    pub data: Vec<NodeData>,

    /// Extra data storage for nodes with >2 children.
    ///
    /// Nodes that can't fit their data in lhs+rhs store additional
    /// data here. The lhs or rhs field contains an index into this array.
    ///
    /// Layout is node-type specific - see NodeTag documentation.
    pub extra: Vec<u32>,

    /// Root nodes (top-level items in the source file).
    ///
    /// These are the entry points for traversing the AST.
    /// Each element is a NodeIndex pointing to a Function, StructDecl, etc.
    pub items: Vec<NodeIndex>,
}

impl SoaAst {
    /// Create a new empty SOA AST.
    pub fn new() -> Self {
        SoaAst {
            tags: Vec::new(),
            data: Vec::new(),
            extra: Vec::new(),
            items: Vec::new(),
        }
    }

    /// Create a new SOA AST with pre-allocated capacity.
    pub fn with_capacity(nodes: usize, extra: usize) -> Self {
        SoaAst {
            tags: Vec::with_capacity(nodes),
            data: Vec::with_capacity(nodes),
            extra: Vec::with_capacity(extra),
            items: Vec::new(),
        }
    }

    /// Get the tag for a node.
    pub fn node_tag(&self, idx: NodeIndex) -> NodeTag {
        self.tags[idx.as_usize()]
    }

    /// Get the data for a node.
    pub fn node_data(&self, idx: NodeIndex) -> NodeData {
        self.data[idx.as_usize()]
    }

    /// Get the main token for a node.
    pub fn main_token(&self, idx: NodeIndex) -> u32 {
        self.data[idx.as_usize()].main_token
    }

    /// Get the number of nodes in the AST.
    pub fn node_count(&self) -> usize {
        self.tags.len()
    }

    /// Get a slice of the extra data array.
    pub fn extra_slice(&self, start: usize, len: usize) -> &[u32] {
        &self.extra[start..start + len]
    }

    // ===== Typed Accessors =====
    // These provide type-safe access to specific node types.

    /// Get the value of an integer literal.
    pub fn int_value(&self, idx: NodeIndex) -> u64 {
        debug_assert_eq!(self.node_tag(idx), NodeTag::IntLit);
        let data = self.node_data(idx);
        (data.lhs as u64) | ((data.rhs as u64) << 32)
    }

    /// Get the boolean value of a boolean literal.
    pub fn bool_value(&self, idx: NodeIndex) -> bool {
        debug_assert_eq!(self.node_tag(idx), NodeTag::BoolLit);
        let data = self.node_data(idx);
        data.lhs != 0
    }

    /// Get the string spur of a string literal.
    pub fn string_spur(&self, idx: NodeIndex) -> Spur {
        debug_assert_eq!(self.node_tag(idx), NodeTag::StringLit);
        let data = self.node_data(idx);
        Spur::try_from_usize(data.lhs as usize).expect("invalid spur")
    }

    /// Get the identifier spur.
    pub fn ident_spur(&self, idx: NodeIndex) -> Spur {
        debug_assert_eq!(self.node_tag(idx), NodeTag::Ident);
        let data = self.node_data(idx);
        Spur::try_from_usize(data.lhs as usize).expect("invalid spur")
    }

    /// Get the operands of a binary expression.
    pub fn binary_operands(&self, idx: NodeIndex) -> (NodeIndex, NodeIndex) {
        debug_assert_eq!(self.node_tag(idx), NodeTag::BinaryExpr);
        let data = self.node_data(idx);
        (NodeIndex(data.lhs), NodeIndex(data.rhs))
    }

    /// Get the operand of a unary expression.
    pub fn unary_operand(&self, idx: NodeIndex) -> NodeIndex {
        debug_assert_eq!(self.node_tag(idx), NodeTag::UnaryExpr);
        let data = self.node_data(idx);
        NodeIndex(data.lhs)
    }

    /// Get the operator kind of a unary expression.
    pub fn unary_op(&self, idx: NodeIndex) -> UnaryOp {
        debug_assert_eq!(self.node_tag(idx), NodeTag::UnaryExpr);
        let data = self.node_data(idx);
        match data.rhs {
            0 => UnaryOp::Neg,
            1 => UnaryOp::Not,
            2 => UnaryOp::BitNot,
            3 => UnaryOp::Ref,
            4 => UnaryOp::MutRef,
            _ => panic!("invalid UnaryOp encoding: {}", data.rhs),
        }
    }
}

impl Default for SoaAst {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod soa_tests {
    use super::*;

    #[test]
    fn test_node_index() {
        let idx = NodeIndex::new(42);
        assert_eq!(idx.as_u32(), 42);
        assert_eq!(idx.as_usize(), 42);
    }

    #[test]
    fn test_null_node() {
        assert_eq!(NULL_NODE.as_u32(), u32::MAX);
    }

    #[test]
    fn test_soa_ast_creation() {
        let ast = SoaAst::new();
        assert_eq!(ast.node_count(), 0);
        assert_eq!(ast.tags.len(), 0);
        assert_eq!(ast.data.len(), 0);
        assert_eq!(ast.extra.len(), 0);
    }

    #[test]
    fn test_soa_ast_with_capacity() {
        let ast = SoaAst::with_capacity(100, 50);
        assert!(ast.tags.capacity() >= 100);
        assert!(ast.data.capacity() >= 100);
        assert!(ast.extra.capacity() >= 50);
    }

    #[test]
    fn test_int_literal_encoding() {
        let mut ast = SoaAst::new();

        // Add an integer literal node
        let value = 0x123456789ABCDEF0u64;
        ast.tags.push(NodeTag::IntLit);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: (value & 0xFFFFFFFF) as u32,         // low 32 bits
            rhs: ((value >> 32) & 0xFFFFFFFF) as u32, // high 32 bits
        });

        let idx = NodeIndex(0);
        assert_eq!(ast.node_tag(idx), NodeTag::IntLit);
        assert_eq!(ast.int_value(idx), value);
    }

    #[test]
    fn test_bool_literal_encoding() {
        let mut ast = SoaAst::new();

        // Add true
        ast.tags.push(NodeTag::BoolLit);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: 1,
            rhs: 0,
        });

        // Add false
        ast.tags.push(NodeTag::BoolLit);
        ast.data.push(NodeData {
            main_token: 1,
            lhs: 0,
            rhs: 0,
        });

        assert!(ast.bool_value(NodeIndex(0)));
        assert!(!ast.bool_value(NodeIndex(1)));
    }

    #[test]
    fn test_binary_expr_encoding() {
        let mut ast = SoaAst::new();

        // Create: 1 + 2
        // First add the literals
        ast.tags.push(NodeTag::IntLit);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: 1,
            rhs: 0,
        });

        ast.tags.push(NodeTag::IntLit);
        ast.data.push(NodeData {
            main_token: 1,
            lhs: 2,
            rhs: 0,
        });

        // Then add the binary expression
        ast.tags.push(NodeTag::BinaryExpr);
        ast.data.push(NodeData {
            main_token: 2, // the '+' token
            lhs: 0,        // left operand (node 0)
            rhs: 1,        // right operand (node 1)
        });

        let binop_idx = NodeIndex(2);
        assert_eq!(ast.node_tag(binop_idx), NodeTag::BinaryExpr);

        let (left, right) = ast.binary_operands(binop_idx);
        assert_eq!(left, NodeIndex(0));
        assert_eq!(right, NodeIndex(1));
        assert_eq!(ast.int_value(left), 1);
        assert_eq!(ast.int_value(right), 2);
    }

    #[test]
    fn test_unary_expr_encoding() {
        let mut ast = SoaAst::new();

        // Create: -42
        // First add the literal
        ast.tags.push(NodeTag::IntLit);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: 42,
            rhs: 0,
        });

        // Then add the unary expression
        ast.tags.push(NodeTag::UnaryExpr);
        ast.data.push(NodeData {
            main_token: 1,                      // the '-' token
            lhs: 0,                             // operand (node 0)
            rhs: encode_unary_op(UnaryOp::Neg), // operator kind
        });

        let unary_idx = NodeIndex(1);
        assert_eq!(ast.node_tag(unary_idx), NodeTag::UnaryExpr);

        let operand = ast.unary_operand(unary_idx);
        assert_eq!(operand, NodeIndex(0));
        assert_eq!(ast.int_value(operand), 42);
        assert_eq!(ast.unary_op(unary_idx), UnaryOp::Neg);
    }

    #[test]
    fn test_ident_encoding() {
        let mut ast = SoaAst::new();

        // Mock identifier with spur index 123
        ast.tags.push(NodeTag::Ident);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: 123, // spur index
            rhs: 0,
        });

        let idx = NodeIndex(0);
        assert_eq!(ast.node_tag(idx), NodeTag::Ident);
        assert_eq!(ast.node_data(idx).lhs, 123);
    }

    #[test]
    fn test_extra_data_slice() {
        let mut ast = SoaAst::new();
        ast.extra = vec![10, 20, 30, 40, 50];

        let slice = ast.extra_slice(1, 3);
        assert_eq!(slice, &[20, 30, 40]);
    }

    #[test]
    fn test_items() {
        let mut ast = SoaAst::new();

        // Add two function nodes
        ast.tags.push(NodeTag::Function);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: 0,
            rhs: 0,
        });

        ast.tags.push(NodeTag::Function);
        ast.data.push(NodeData {
            main_token: 1,
            lhs: 0,
            rhs: 0,
        });

        ast.items = vec![NodeIndex(0), NodeIndex(1)];

        assert_eq!(ast.items.len(), 2);
        assert_eq!(ast.node_tag(ast.items[0]), NodeTag::Function);
        assert_eq!(ast.node_tag(ast.items[1]), NodeTag::Function);
    }
}
