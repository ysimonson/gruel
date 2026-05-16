//! RIR instruction definitions.
//!
//! Instructions are stored in a dense array and referenced by index.
//! This provides good cache locality and efficient traversal.

use std::fmt;

use gruel_builtins::Posture;
use gruel_util::{BinOp, Span, UnaryOp};
use lasso::{Key, Spur};

/// A reference to an instruction in the RIR.
///
/// This is a lightweight handle (4 bytes) that indexes into the instruction array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct InstRef(u32);

impl InstRef {
    /// Create an instruction reference from a raw index.
    #[inline]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Get the raw index.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// A directive in the RIR (e.g., @allow(unused_variable))
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RirDirective {
    /// Directive name (e.g., "allow")
    pub name: Spur,
    /// Arguments (e.g., ["unused_variable"])
    pub args: Vec<Spur>,
    /// Span covering the directive
    pub span: Span,
}

/// Parameter passing mode in RIR.
///
/// Per ADR-0076, the surface language no longer has `inout` / `borrow`
/// keyword forms. The legacy mode names are gone; `MutRef` / `Ref` survive
/// as a transport mechanism for places where the param's `Type` cannot
/// itself be wrapped (notably interface-typed parameters, where the type
/// pool cannot intern `Ref(Interface)` / `MutRef(Interface)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RirParamMode {
    /// Normal pass-by-value parameter (or any reference whose ref-ness is
    /// already encoded in the parameter `Type`).
    #[default]
    Normal,
    /// Exclusive mutable borrow on a parameter whose declared type cannot
    /// itself be wrapped as `MutRef(...)` (e.g. interface params).
    MutRef,
    /// Shared immutable borrow with the same caveat as `MutRef`.
    Ref,
    /// Comptime parameter - evaluated at compile time (used for type parameters)
    Comptime,
}

/// A parameter in a function declaration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RirParam {
    /// Parameter name
    pub name: Spur,
    /// Parameter type
    pub ty: Spur,
    /// Parameter passing mode
    pub mode: RirParamMode,
    /// Whether this parameter is evaluated at compile time
    pub is_comptime: bool,
}

/// Argument passing mode in RIR.
///
/// Mirrors [`RirParamMode`]: `MutRef` / `Ref` are vestigial-but-used
/// markers carried alongside arguments whose ref-ness cannot be encoded in
/// the AIR value type (interface forwarding) so codegen still routes them
/// through the by-pointer ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RirArgMode {
    /// Normal pass-by-value argument (or a `&x` / `&mut x` whose ref-ness
    /// is already in the AIR value type).
    #[default]
    Normal,
    /// Exclusive mutable reborrow forwarded to a `MutRef`-mode parameter.
    MutRef,
    /// Shared immutable reborrow forwarded to a `Ref`-mode parameter.
    Ref,
}

/// An argument in a function call.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RirCallArg {
    /// The argument expression
    pub value: InstRef,
    /// The passing mode for this argument
    pub mode: RirArgMode,
}

/// A pattern in a match expression (RIR level - untyped).
///
/// Recursive shape introduced by ADR-0051 Phase 4: `Ident`, `Tuple`, and
/// `Struct` variants let astgen carry source-level nesting straight to sema
/// instead of pre-elaborating tuple/struct match roots. The existing
/// variant-pattern shapes (`DataVariant`, `StructVariant`) are unchanged;
/// nested sub-patterns inside variant fields still go through the
/// elaboration layer until a follow-up RIR migration replaces their flat
/// bindings with `RirPattern` leaves.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RirPattern {
    /// Wildcard pattern `_` - matches anything
    Wildcard(Span),
    /// Integer literal pattern (can be positive or negative)
    Int(i64, Span),
    /// Boolean literal pattern
    Bool(bool, Span),
    /// Path pattern for enum variants (e.g., `Color::Red` or `module.Color::Red`)
    Path {
        /// Optional module reference for qualified paths (e.g., the `module` in `module.Color::Red`)
        module: Option<InstRef>,
        /// The enum type name
        type_name: Spur,
        /// The variant name
        variant: Spur,
        /// Span of the pattern
        span: Span,
    },
    /// Data variant pattern with field bindings (e.g., `Option::Some(x)`)
    DataVariant {
        /// Optional module reference for qualified paths
        module: Option<InstRef>,
        /// The enum type name
        type_name: Spur,
        /// The variant name
        variant: Spur,
        /// Bindings for each field
        bindings: Vec<RirPatternBinding>,
        /// Span of the pattern
        span: Span,
    },
    /// Struct variant pattern with named field bindings (e.g., `Shape::Circle { radius }`)
    StructVariant {
        /// Optional module reference for qualified paths
        module: Option<InstRef>,
        /// The enum type name
        type_name: Spur,
        /// The variant name
        variant: Spur,
        /// Named field bindings
        field_bindings: Vec<RirStructPatternBinding>,
        /// Span of the pattern
        span: Span,
    },
    /// Name binding `x` or `mut x` (ADR-0051). Used at the arm root or as
    /// a sub-pattern inside `Tuple` / `Struct`. Equivalent to `x @ _`.
    Ident {
        name: Spur,
        is_mut: bool,
        span: Span,
    },
    /// Tuple pattern `(p0, p1, ...)` (ADR-0051). `elems` carries the
    /// explicit sub-patterns only; `rest_position` records where a `..`
    /// rest marker appeared so sema can expand it to wildcards filling
    /// the scrutinee's arity. `None` means no rest; `Some(i)` means the
    /// rest was written at source index `i` (so `elems[..i]` are the
    /// prefix and `elems[i..]` the suffix).
    Tuple {
        elems: Vec<RirPattern>,
        rest_position: Option<u32>,
        span: Span,
    },
    /// Named-struct pattern `TypeName { field: pat, .. }` (ADR-0051).
    /// `has_rest` marks an explicit `..` trailing the field list.
    Struct {
        module: Option<InstRef>,
        type_name: Spur,
        fields: Vec<RirStructField>,
        has_rest: bool,
        span: Span,
    },
    /// ADR-0079 Phase 3: a `comptime_unroll for` arm template. Sema
    /// evaluates `iterable` at comptime, synthesizes one regular arm
    /// per element, and substitutes `binding` as a comptime value in
    /// the arm body. Only valid at the top level of a match arm.
    ComptimeUnrollArm {
        binding: Spur,
        iterable: InstRef,
        span: Span,
    },
}

/// A binding in a data variant pattern.
///
/// Shapes:
/// - `is_wildcard = true` → `_` (no binding, matches anything)
/// - `is_wildcard = false, name = Some(x), sub_pattern = None` → `x` (bind field to `x`)
/// - `is_wildcard = false, name = None, sub_pattern = Some(p)` → `p` (nested refutable sub-pattern, ADR-0051)
/// - `is_wildcard = false, name = Some(x), sub_pattern = Some(p)` → `x @ p` (reserved; not yet
///   exposed in surface syntax)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RirPatternBinding {
    /// Whether this is a wildcard binding (`_`)
    pub is_wildcard: bool,
    /// Whether this is a mutable binding (only meaningful if not wildcard)
    pub is_mut: bool,
    /// The binding name (None for wildcard or nested sub-pattern bindings)
    pub name: Option<Spur>,
    /// Nested sub-pattern for refutable field matches like `Some(Ok(v))`.
    /// When `Some`, the binding's match is `sub_pattern` recursively;
    /// when `None`, this is a flat leaf binding.
    pub sub_pattern: Option<Box<RirPattern>>,
}

/// A named field binding in a struct variant pattern.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RirStructPatternBinding {
    /// The field name being matched
    pub field_name: Spur,
    /// The binding for this field
    pub binding: RirPatternBinding,
}

/// A field in an ADR-0051 `RirPattern::Struct` arm, carrying the matched
/// field's name plus its recursive sub-pattern.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RirStructField {
    pub field_name: Spur,
    pub pattern: RirPattern,
}

impl RirPattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            RirPattern::Wildcard(span) => *span,
            RirPattern::Int(_, span) => *span,
            RirPattern::Bool(_, span) => *span,
            RirPattern::Path { span, .. } => *span,
            RirPattern::DataVariant { span, .. } => *span,
            RirPattern::StructVariant { span, .. } => *span,
            RirPattern::Ident { span, .. } => *span,
            RirPattern::Tuple { span, .. } => *span,
            RirPattern::Struct { span, .. } => *span,
            RirPattern::ComptimeUnrollArm { span, .. } => *span,
        }
    }
}

/// Encode a `RirPatternBinding` to `out`. Layout:
/// `[flags, name_raw, <sub_pattern_tree if flags bit 2 set>]`.
/// Flags: bit 0 = is_wildcard, bit 1 = is_mut, bit 2 = has_sub_pattern.
fn encode_binding(b: &RirPatternBinding, out: &mut Vec<u32>) {
    let mut flags = if b.is_wildcard { 1u32 } else { 0 };
    if b.is_mut {
        flags |= 2;
    }
    if b.sub_pattern.is_some() {
        flags |= 4;
    }
    out.push(flags);
    out.push(b.name.map_or(u32::MAX, |s| s.into_usize() as u32));
    if let Some(sub) = &b.sub_pattern {
        encode_pattern_tree(sub, out);
    }
}

/// Decode a `RirPatternBinding` from `data`, returning the binding and
/// words consumed.
fn decode_binding(data: &[u32]) -> (RirPatternBinding, usize) {
    let flags = data[0];
    let name_raw = data[1];
    let name = if name_raw == u32::MAX {
        None
    } else {
        Some(Spur::try_from_usize(name_raw as usize).unwrap())
    };
    let mut offset = 2;
    let sub_pattern = if flags & 4 != 0 {
        let (p, consumed) = decode_pattern_tree(&data[offset..]);
        offset += consumed;
        Some(Box::new(p))
    } else {
        None
    };
    (
        RirPatternBinding {
            is_wildcard: flags & 1 != 0,
            is_mut: flags & 2 != 0,
            name,
            sub_pattern,
        },
        offset,
    )
}

/// Encode a `RirStructPatternBinding` to `out`. Layout:
/// `[field_name, <binding encoding>]`.
fn encode_struct_binding(fb: &RirStructPatternBinding, out: &mut Vec<u32>) {
    out.push(fb.field_name.into_usize() as u32);
    encode_binding(&fb.binding, out);
}

/// Decode a `RirStructPatternBinding` from `data`, returning the binding
/// and words consumed.
fn decode_struct_binding(data: &[u32]) -> (RirStructPatternBinding, usize) {
    let field_name = Spur::try_from_usize(data[0] as usize).unwrap();
    let (binding, consumed) = decode_binding(&data[1..]);
    (
        RirStructPatternBinding {
            field_name,
            binding,
        },
        1 + consumed,
    )
}

/// Encode a `RirPattern` as a self-describing tree into `out`. Unlike the
/// per-kind top-level arm encoding, this form carries no `body_ref` and
/// is used for nested sub-patterns inside ADR-0051 `Tuple` / `Struct`
/// arm elements. The layout mirrors the per-kind top-level layout minus
/// the body word, with variable-width recursion for nested patterns.
fn encode_pattern_tree(pattern: &RirPattern, out: &mut Vec<u32>) {
    match pattern {
        RirPattern::Wildcard(span) => {
            out.push(PatternKind::Wildcard as u32);
            out.push(span.start());
            out.push(span.len());
        }
        RirPattern::Int(value, span) => {
            out.push(PatternKind::Int as u32);
            out.push(span.start());
            out.push(span.len());
            out.push(*value as u32);
            out.push((*value >> 32) as u32);
        }
        RirPattern::Bool(value, span) => {
            out.push(PatternKind::Bool as u32);
            out.push(span.start());
            out.push(span.len());
            out.push(if *value { 1 } else { 0 });
        }
        RirPattern::Path {
            module,
            type_name,
            variant,
            span,
        } => {
            out.push(PatternKind::Path as u32);
            out.push(span.start());
            out.push(span.len());
            out.push(module.map_or(u32::MAX, |r| r.as_u32()));
            out.push(type_name.into_usize() as u32);
            out.push(variant.into_usize() as u32);
        }
        RirPattern::DataVariant {
            module,
            type_name,
            variant,
            bindings,
            span,
        } => {
            out.push(PatternKind::DataVariant as u32);
            out.push(span.start());
            out.push(span.len());
            out.push(module.map_or(u32::MAX, |r| r.as_u32()));
            out.push(type_name.into_usize() as u32);
            out.push(variant.into_usize() as u32);
            out.push(bindings.len() as u32);
            for b in bindings {
                encode_binding(b, out);
            }
        }
        RirPattern::StructVariant {
            module,
            type_name,
            variant,
            field_bindings,
            span,
        } => {
            out.push(PatternKind::StructVariant as u32);
            out.push(span.start());
            out.push(span.len());
            out.push(module.map_or(u32::MAX, |r| r.as_u32()));
            out.push(type_name.into_usize() as u32);
            out.push(variant.into_usize() as u32);
            out.push(field_bindings.len() as u32);
            for fb in field_bindings {
                encode_struct_binding(fb, out);
            }
        }
        RirPattern::Ident { name, is_mut, span } => {
            out.push(PatternKind::Ident as u32);
            out.push(span.start());
            out.push(span.len());
            out.push(name.into_usize() as u32);
            out.push(if *is_mut { 1 } else { 0 });
        }
        RirPattern::Tuple {
            elems,
            rest_position,
            span,
        } => {
            out.push(PatternKind::Tuple as u32);
            out.push(span.start());
            out.push(span.len());
            out.push(rest_position.map_or(u32::MAX, |i| i));
            out.push(elems.len() as u32);
            for elem in elems {
                encode_pattern_tree(elem, out);
            }
        }
        RirPattern::Struct {
            module,
            type_name,
            fields,
            has_rest,
            span,
        } => {
            out.push(PatternKind::Struct as u32);
            out.push(span.start());
            out.push(span.len());
            out.push(module.map_or(u32::MAX, |r| r.as_u32()));
            out.push(type_name.into_usize() as u32);
            out.push(if *has_rest { 1 } else { 0 });
            out.push(fields.len() as u32);
            for f in fields {
                out.push(f.field_name.into_usize() as u32);
                encode_pattern_tree(&f.pattern, out);
            }
        }
        // ADR-0079 Phase 3: an unroll-arm template only ever
        // appears at the top level of a match (sema rejects it
        // elsewhere), but we still need a tree encoding for the
        // shared encode dispatch — emit a stable shape and let
        // the decoder round-trip it identically.
        RirPattern::ComptimeUnrollArm {
            binding,
            iterable,
            span,
        } => {
            out.push(PatternKind::ComptimeUnrollArm as u32);
            out.push(span.start());
            out.push(span.len());
            out.push(binding.into_usize() as u32);
            out.push(iterable.as_u32());
        }
    }
}

/// Decode a single `RirPattern` from the tree encoding in `data`,
/// returning the pattern and how many u32 words it consumed.
fn decode_pattern_tree(data: &[u32]) -> (RirPattern, usize) {
    let kind = data[0];
    if kind == PatternKind::Wildcard as u32 {
        let span = Span::new(data[1], data[1] + data[2]);
        (RirPattern::Wildcard(span), 3)
    } else if kind == PatternKind::Int as u32 {
        let span = Span::new(data[1], data[1] + data[2]);
        let value_lo = data[3] as i64;
        let value_hi = data[4] as i64;
        let value = value_lo | (value_hi << 32);
        (RirPattern::Int(value, span), 5)
    } else if kind == PatternKind::Bool as u32 {
        let span = Span::new(data[1], data[1] + data[2]);
        let value = data[3] != 0;
        (RirPattern::Bool(value, span), 4)
    } else if kind == PatternKind::Path as u32 {
        let span = Span::new(data[1], data[1] + data[2]);
        let module_raw = data[3];
        let module = if module_raw == u32::MAX {
            None
        } else {
            Some(InstRef::from_raw(module_raw))
        };
        let type_name = Spur::try_from_usize(data[4] as usize).unwrap();
        let variant = Spur::try_from_usize(data[5] as usize).unwrap();
        (
            RirPattern::Path {
                module,
                type_name,
                variant,
                span,
            },
            6,
        )
    } else if kind == PatternKind::DataVariant as u32 {
        let span = Span::new(data[1], data[1] + data[2]);
        let module_raw = data[3];
        let module = if module_raw == u32::MAX {
            None
        } else {
            Some(InstRef::from_raw(module_raw))
        };
        let type_name = Spur::try_from_usize(data[4] as usize).unwrap();
        let variant = Spur::try_from_usize(data[5] as usize).unwrap();
        let n = data[6] as usize;
        let mut bindings = Vec::with_capacity(n);
        let mut offset = 7;
        for _ in 0..n {
            let (b, consumed) = decode_binding(&data[offset..]);
            bindings.push(b);
            offset += consumed;
        }
        (
            RirPattern::DataVariant {
                module,
                type_name,
                variant,
                bindings,
                span,
            },
            offset,
        )
    } else if kind == PatternKind::StructVariant as u32 {
        let span = Span::new(data[1], data[1] + data[2]);
        let module_raw = data[3];
        let module = if module_raw == u32::MAX {
            None
        } else {
            Some(InstRef::from_raw(module_raw))
        };
        let type_name = Spur::try_from_usize(data[4] as usize).unwrap();
        let variant = Spur::try_from_usize(data[5] as usize).unwrap();
        let n = data[6] as usize;
        let mut field_bindings = Vec::with_capacity(n);
        let mut offset = 7;
        for _ in 0..n {
            let (fb, consumed) = decode_struct_binding(&data[offset..]);
            field_bindings.push(fb);
            offset += consumed;
        }
        (
            RirPattern::StructVariant {
                module,
                type_name,
                variant,
                field_bindings,
                span,
            },
            offset,
        )
    } else if kind == PatternKind::Ident as u32 {
        let span = Span::new(data[1], data[1] + data[2]);
        let name = Spur::try_from_usize(data[3] as usize).unwrap();
        let is_mut = data[4] != 0;
        (RirPattern::Ident { name, is_mut, span }, 5)
    } else if kind == PatternKind::Tuple as u32 {
        let span = Span::new(data[1], data[1] + data[2]);
        let rest_raw = data[3];
        let rest_position = if rest_raw == u32::MAX {
            None
        } else {
            Some(rest_raw)
        };
        let n = data[4] as usize;
        let mut offset = 5;
        let mut elems = Vec::with_capacity(n);
        for _ in 0..n {
            let (p, consumed) = decode_pattern_tree(&data[offset..]);
            elems.push(p);
            offset += consumed;
        }
        (
            RirPattern::Tuple {
                elems,
                rest_position,
                span,
            },
            offset,
        )
    } else if kind == PatternKind::Struct as u32 {
        let span = Span::new(data[1], data[1] + data[2]);
        let module_raw = data[3];
        let module = if module_raw == u32::MAX {
            None
        } else {
            Some(InstRef::from_raw(module_raw))
        };
        let type_name = Spur::try_from_usize(data[4] as usize).unwrap();
        let has_rest = data[5] != 0;
        let n = data[6] as usize;
        let mut offset = 7;
        let mut fields = Vec::with_capacity(n);
        for _ in 0..n {
            let field_name = Spur::try_from_usize(data[offset] as usize).unwrap();
            offset += 1;
            let (pattern, consumed) = decode_pattern_tree(&data[offset..]);
            offset += consumed;
            fields.push(RirStructField {
                field_name,
                pattern,
            });
        }
        (
            RirPattern::Struct {
                module,
                type_name,
                fields,
                has_rest,
                span,
            },
            offset,
        )
    } else if kind == PatternKind::ComptimeUnrollArm as u32 {
        let span = Span::new(data[1], data[1] + data[2]);
        let binding = Spur::try_from_usize(data[3] as usize).unwrap();
        let iterable = InstRef::from_raw(data[4]);
        (
            RirPattern::ComptimeUnrollArm {
                binding,
                iterable,
                span,
            },
            5,
        )
    } else {
        panic!("Unknown pattern tree tag: {}", kind);
    }
}

/// Extra data marker types for type-safe storage in the extra array.
/// These types represent data stored in the extra array.
/// Stored representation of RirCallArg in the extra array.
/// Layout: [value: u32, mode: u32] = 2 u32s per arg
const CALL_ARG_SIZE: u32 = 2;

/// Stored representation of RirParam in the extra array.
/// Layout: [name: u32, ty: u32, mode: u32, is_comptime: u32] = 4 u32s per param
const PARAM_SIZE: u32 = 4;

/// Stored representation of match arm in the extra array.
/// Layout: pattern data + [body: u32]
/// Pattern data varies by kind (see PatternKind enum).
/// Pattern kinds encoded in extra array
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PatternKind {
    /// Wildcard pattern: [kind, span_start, span_len]
    Wildcard = 0,
    /// Int pattern: [kind, span_start, span_len, value_lo, value_hi]
    Int = 1,
    /// Bool pattern: [kind, span_start, span_len, value]
    Bool = 2,
    /// Path pattern: [kind, span_start, span_len, module, type_name, variant]
    /// module is u32::MAX for None, otherwise an InstRef
    Path = 3,
    /// Data variant pattern: [kind, span_start, span_len, module, type_name, variant, body, bindings_len, (flags, name)...]
    /// Each binding is 2 u32s: flags (bit0=is_wildcard, bit1=is_mut), name (Spur, u32::MAX if wildcard)
    DataVariant = 4,
    /// Struct variant pattern: [kind, span_start, span_len, module, type_name, variant, body, bindings_len, (field_name, flags, binding_name)...]
    /// Each field binding is 3 u32s: field_name (Spur), flags (bit0=is_wildcard, bit1=is_mut), binding_name (Spur, u32::MAX if wildcard)
    StructVariant = 5,
    /// ADR-0051 Ident pattern: [kind, span_start, span_len, body, name, flags]
    /// flags bit 0 = is_mut.
    Ident = 6,
    /// ADR-0051 Tuple pattern: [kind, span_start, span_len, body, elems_len, ...recursive_tree per elem]
    Tuple = 7,
    /// ADR-0051 Struct pattern: [kind, span_start, span_len, body, module_raw, type_name, has_rest, fields_len,
    ///                           (field_name, ...recursive_tree) * fields_len]
    Struct = 8,
    /// ADR-0079 Phase 3 unroll arm template: [kind, span_start, span_len, body, binding, iterable]
    ComptimeUnrollArm = 9,
}

/// Size of each pattern kind in the extra array (including body InstRef)
const PATTERN_WILDCARD_SIZE: u32 = 4; // kind, span_start, span_len, body
const PATTERN_INT_SIZE: u32 = 6; // kind, span_start, span_len, value_lo, value_hi, body
const PATTERN_BOOL_SIZE: u32 = 5; // kind, span_start, span_len, value, body
const PATTERN_PATH_SIZE: u32 = 7; // kind, span_start, span_len, module, type_name, variant, body
// DataVariant size: 8 + 2 * bindings_len (variable)

/// Stored representation of a destructure field in the extra array.
/// Layout: [field_name: u32, binding_name: u32 (0 = shorthand), is_wildcard: u32, is_mut: u32]
const DESTRUCTURE_FIELD_SIZE: u32 = 4;

/// A decoded destructure field from the extra array.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RirDestructureField {
    /// The struct field being bound
    pub field_name: Spur,
    /// Binding name (None for shorthand or wildcard)
    pub binding_name: Option<Spur>,
    /// Whether this is a wildcard binding (`field: _`)
    pub is_wildcard: bool,
    /// Whether the binding is mutable
    pub is_mut: bool,
}

/// Stored representation of struct field initializer.
/// Layout: [field_name: u32, value: u32] = 2 u32s per field
const FIELD_INIT_SIZE: u32 = 2;

/// Stored representation of struct field declaration.
/// Layout: [field_name: u32, field_type: u32] = 2 u32s per field
const FIELD_DECL_SIZE: u32 = 3;

/// Stored representation of directive in the extra array.
/// Layout: [name: u32, span_start: u32, span_len: u32, args_len: u32, args...]
/// Variable size due to args.
/// A span marking the boundaries of a function in the RIR.
///
/// This allows efficient per-function analysis by identifying which instructions
/// belong to each function without scanning the entire instruction array.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FunctionSpan {
    /// Function name symbol
    pub name: Spur,
    /// Index of the first instruction of this function's body.
    /// This is the first instruction generated for the function's expressions/statements.
    pub body_start: InstRef,
    /// Index of the FnDecl instruction for this function.
    /// This is always the last instruction of the function.
    pub decl: InstRef,
}

impl FunctionSpan {
    /// Create a new function span.
    pub fn new(name: Spur, body_start: InstRef, decl: InstRef) -> Self {
        Self {
            name,
            body_start,
            decl,
        }
    }

    /// Get the number of instructions in this function (including the FnDecl).
    pub fn instruction_count(&self) -> u32 {
        self.decl.as_u32() - self.body_start.as_u32() + 1
    }
}

/// A view into a function's instructions within the RIR.
///
/// This provides a way to iterate over just the instructions belonging to a
/// specific function, enabling per-function analysis without copying data.
#[derive(Debug)]
pub struct RirFunctionView<'a> {
    rir: &'a Rir,
    body_start: InstRef,
    decl: InstRef,
}

impl<'a> RirFunctionView<'a> {
    /// Get the instruction at the given reference.
    ///
    /// Note: The reference must be within this function's range.
    #[inline]
    pub fn get(&self, inst_ref: InstRef) -> &'a Inst {
        debug_assert!(
            inst_ref.as_u32() >= self.body_start.as_u32()
                && inst_ref.as_u32() <= self.decl.as_u32(),
            "InstRef {} is outside function range [{}, {}]",
            inst_ref,
            self.body_start,
            self.decl
        );
        self.rir.get(inst_ref)
    }

    /// Get the FnDecl instruction for this function.
    #[inline]
    pub fn fn_decl(&self) -> &'a Inst {
        self.rir.get(self.decl)
    }

    /// Iterate over all instructions in this function (including FnDecl).
    pub fn iter(&self) -> impl Iterator<Item = (InstRef, &'a Inst)> {
        let start = self.body_start.as_u32();
        let end = self.decl.as_u32() + 1;
        (start..end).map(move |i| {
            let inst_ref = InstRef::from_raw(i);
            (inst_ref, self.rir.get(inst_ref))
        })
    }

    /// Get the number of instructions in this function view.
    pub fn len(&self) -> usize {
        (self.decl.as_u32() - self.body_start.as_u32() + 1) as usize
    }

    /// Whether this view is empty (should never be true for valid functions).
    pub fn is_empty(&self) -> bool {
        self.body_start.as_u32() > self.decl.as_u32()
    }

    /// Access the underlying RIR for operations that need the full context
    /// (e.g., accessing extra data).
    pub fn rir(&self) -> &'a Rir {
        self.rir
    }
}

/// ADR-0085: a body-less extern fn declared inside a `link_extern("…") { … }`
/// block.
///
/// These are tracked as a side-set on the [`Rir`] rather than as
/// `FnDecl` instructions because their lowering pipeline diverges from
/// regular fns: no body to translate, no Gruel mangling, library name
/// to thread through to the linker.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RirExternFn {
    /// Library name (interned, as written in the `link_extern("…")` head).
    pub library: Spur,
    /// Function name (as it appears in Gruel source).
    pub name: Spur,
    /// Index into the extra array where this fn's directives start.
    pub directives_start: u32,
    /// Number of directives.
    pub directives_len: u32,
    /// Index into the extra array where this fn's params start.
    pub params_start: u32,
    /// Number of parameters.
    pub params_len: u32,
    /// Return type as a `Spur` (defaults to `()` when omitted in source).
    pub return_type: Spur,
    /// Span of the fn declaration.
    pub span: Span,
    /// Span of the enclosing `link_extern(...)` block.
    pub block_span: Span,
    /// ADR-0086: dynamic (`link_extern`) vs static (`static_link_extern`).
    #[serde(default = "default_rir_link_mode")]
    pub link_mode: RirLinkMode,
}

/// ADR-0086: mirrors `gruel_parser::ast::LinkMode` after astgen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RirLinkMode {
    Dynamic,
    Static,
}

fn default_rir_link_mode() -> RirLinkMode {
    RirLinkMode::Dynamic
}

/// The complete RIR for a source file.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Rir {
    /// All instructions in the file
    instructions: Vec<Inst>,
    /// Extra data for variable-length instruction payloads
    extra: Vec<u32>,
    /// Function boundaries for per-function analysis
    function_spans: Vec<FunctionSpan>,
    /// ADR-0085: body-less extern fns declared in `link_extern(...)` blocks.
    extern_fns: Vec<RirExternFn>,
    /// ADR-0085: `link_extern("lib") { }` blocks with no items. Tracked
    /// separately so sema can validate the library name and emit the
    /// `-l<lib>` flag even when no symbols are declared.
    /// ADR-0086 widened the tuple to `(library, link_mode, span)`.
    empty_link_extern_blocks: Vec<(Spur, RirLinkMode, Span)>,
}

impl Rir {
    /// Create a new empty RIR.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an instruction and return its reference.
    pub fn add_inst(&mut self, inst: Inst) -> InstRef {
        // Debug assertion for u32 overflow - catches pathological inputs during development
        debug_assert!(
            self.instructions.len() < u32::MAX as usize,
            "RIR instruction count overflow: {} instructions exceeds u32::MAX - 1",
            self.instructions.len()
        );

        let index = self.instructions.len() as u32;
        self.instructions.push(inst);
        InstRef::from_raw(index)
    }

    /// Get an instruction by reference.
    #[inline]
    pub fn get(&self, inst_ref: InstRef) -> &Inst {
        &self.instructions[inst_ref.0 as usize]
    }

    /// Get a mutable reference to an instruction.
    #[inline]
    pub fn get_mut(&mut self, inst_ref: InstRef) -> &mut Inst {
        &mut self.instructions[inst_ref.0 as usize]
    }

    /// The number of instructions.
    #[inline]
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    /// Whether there are no instructions.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    /// Iterate over all instructions with their references.
    pub fn iter(&self) -> impl Iterator<Item = (InstRef, &Inst)> {
        self.instructions
            .iter()
            .enumerate()
            .map(|(i, inst)| (InstRef::from_raw(i as u32), inst))
    }

    /// Add extra data and return the start index.
    pub fn add_extra(&mut self, data: &[u32]) -> u32 {
        // Debug assertions for u32 overflow - catches pathological inputs during development
        debug_assert!(
            self.extra.len() <= u32::MAX as usize,
            "RIR extra data overflow: {} entries exceeds u32::MAX",
            self.extra.len()
        );
        debug_assert!(
            self.extra.len().saturating_add(data.len()) <= u32::MAX as usize,
            "RIR extra data would overflow: {} + {} exceeds u32::MAX",
            self.extra.len(),
            data.len()
        );

        let start = self.extra.len() as u32;
        self.extra.extend_from_slice(data);
        start
    }

    /// Get extra data by index.
    #[inline]
    pub fn get_extra(&self, start: u32, len: u32) -> &[u32] {
        let start = start as usize;
        let end = start + len as usize;
        &self.extra[start..end]
    }

    // ===== Helper methods for storing/retrieving typed data in the extra array =====

    /// Store a slice of InstRefs and return (start, len).
    pub fn add_inst_refs(&mut self, refs: &[InstRef]) -> (u32, u32) {
        let data: Vec<u32> = refs.iter().map(|r| r.as_u32()).collect();
        let start = self.add_extra(&data);
        (start, refs.len() as u32)
    }

    /// Retrieve InstRefs from the extra array.
    pub fn get_inst_refs(&self, start: u32, len: u32) -> Vec<InstRef> {
        self.get_extra(start, len)
            .iter()
            .map(|&v| InstRef::from_raw(v))
            .collect()
    }

    /// Store a slice of Spurs and return (start, len).
    pub fn add_symbols(&mut self, symbols: &[Spur]) -> (u32, u32) {
        let data: Vec<u32> = symbols.iter().map(|s| s.into_usize() as u32).collect();
        let start = self.add_extra(&data);
        (start, symbols.len() as u32)
    }

    /// Retrieve Spurs from the extra array.
    pub fn get_symbols(&self, start: u32, len: u32) -> Vec<Spur> {
        self.get_extra(start, len)
            .iter()
            .map(|&v| Spur::try_from_usize(v as usize).unwrap())
            .collect()
    }

    /// Store enum variant declarations and return (start, variant_count).
    ///
    /// Each variant is encoded as variable-length data in the extra array:
    ///   `[name_spur, field_count, is_struct, field_type_0, ..., field_type_n, field_name_0?, ..., field_name_n?]`
    ///
    /// - `is_struct`: 0 for unit/tuple variants, 1 for struct variants.
    /// - For struct variants, field names follow field types.
    /// - Unit variants have `field_count = 0`.
    pub fn add_enum_variant_decls(
        &mut self,
        variants: &[(Spur, Vec<Spur>, Vec<Spur>)],
    ) -> (u32, u32) {
        let start = self.extra.len() as u32;
        for (name, field_types, field_names) in variants {
            self.extra.push(name.into_usize() as u32);
            self.extra.push(field_types.len() as u32);
            let is_struct = if field_names.is_empty() { 0u32 } else { 1u32 };
            self.extra.push(is_struct);
            for field_ty in field_types {
                self.extra.push(field_ty.into_usize() as u32);
            }
            for field_name in field_names {
                self.extra.push(field_name.into_usize() as u32);
            }
        }
        (start, variants.len() as u32)
    }

    /// Retrieve enum variant declarations from the extra array.
    /// Returns a vec of `(variant_name, field_types, field_names)` triples.
    /// `field_names` is empty for unit/tuple variants.
    pub fn get_enum_variant_decls(
        &self,
        start: u32,
        variant_count: u32,
    ) -> Vec<(Spur, Vec<Spur>, Vec<Spur>)> {
        let mut result = Vec::with_capacity(variant_count as usize);
        let mut pos = start as usize;
        for _ in 0..variant_count {
            let name = Spur::try_from_usize(self.extra[pos] as usize).unwrap();
            let field_count = self.extra[pos + 1] as usize;
            let is_struct = self.extra[pos + 2] != 0;
            pos += 3;
            let field_types: Vec<Spur> = (0..field_count)
                .map(|i| Spur::try_from_usize(self.extra[pos + i] as usize).unwrap())
                .collect();
            pos += field_count;
            let field_names = if is_struct {
                let names: Vec<Spur> = (0..field_count)
                    .map(|i| Spur::try_from_usize(self.extra[pos + i] as usize).unwrap())
                    .collect();
                pos += field_count;
                names
            } else {
                Vec::new()
            };
            result.push((name, field_types, field_names));
        }
        result
    }

    /// Store RirCallArgs and return (start, len).
    /// Layout: [value: u32, mode: u32] per arg
    pub fn add_call_args(&mut self, args: &[RirCallArg]) -> (u32, u32) {
        let mut data = Vec::with_capacity(args.len() * CALL_ARG_SIZE as usize);
        for arg in args {
            data.push(arg.value.as_u32());
            data.push(arg.mode as u32);
        }
        let start = self.add_extra(&data);
        (start, args.len() as u32)
    }

    /// Retrieve RirCallArgs from the extra array.
    pub fn get_call_args(&self, start: u32, len: u32) -> Vec<RirCallArg> {
        let data = self.get_extra(start, len * CALL_ARG_SIZE);
        let mut args = Vec::with_capacity(len as usize);
        for chunk in data.chunks(CALL_ARG_SIZE as usize) {
            let value = InstRef::from_raw(chunk[0]);
            let mode = match chunk[1] {
                0 => RirArgMode::Normal,
                1 => RirArgMode::MutRef,
                2 => RirArgMode::Ref,
                _ => RirArgMode::Normal, // Fallback, shouldn't happen
            };
            args.push(RirCallArg { value, mode });
        }
        args
    }

    /// Store RirParams and return (start, len).
    /// Layout: [name: u32, ty: u32, mode: u32, is_comptime: u32] per param
    pub fn add_params(&mut self, params: &[RirParam]) -> (u32, u32) {
        let mut data = Vec::with_capacity(params.len() * PARAM_SIZE as usize);
        for param in params {
            data.push(param.name.into_usize() as u32);
            data.push(param.ty.into_usize() as u32);
            data.push(param.mode as u32);
            data.push(param.is_comptime as u32);
        }
        let start = self.add_extra(&data);
        (start, params.len() as u32)
    }

    /// Retrieve RirParams from the extra array.
    pub fn get_params(&self, start: u32, len: u32) -> Vec<RirParam> {
        let data = self.get_extra(start, len * PARAM_SIZE);
        let mut params = Vec::with_capacity(len as usize);
        for chunk in data.chunks(PARAM_SIZE as usize) {
            let name = Spur::try_from_usize(chunk[0] as usize).unwrap();
            let ty = Spur::try_from_usize(chunk[1] as usize).unwrap();
            let mode = match chunk[2] {
                0 => RirParamMode::Normal,
                1 => RirParamMode::MutRef,
                2 => RirParamMode::Ref,
                3 => RirParamMode::Comptime,
                _ => RirParamMode::Normal, // Fallback
            };
            let is_comptime = chunk[3] != 0;
            params.push(RirParam {
                name,
                ty,
                mode,
                is_comptime,
            });
        }
        params
    }

    /// Store match arms (pattern + body pairs) and return (start, arm_count).
    /// Each arm is stored with variable size depending on pattern kind.
    pub fn add_match_arms(&mut self, arms: &[(RirPattern, InstRef)]) -> (u32, u32) {
        let start = self.extra.len() as u32;
        for (pattern, body) in arms {
            match pattern {
                RirPattern::Wildcard(span) => {
                    self.extra.push(PatternKind::Wildcard as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(body.as_u32());
                }
                RirPattern::Int(value, span) => {
                    self.extra.push(PatternKind::Int as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    // Store i64 as two u32s (little-endian)
                    self.extra.push(*value as u32);
                    self.extra.push((*value >> 32) as u32);
                    self.extra.push(body.as_u32());
                }
                RirPattern::Bool(value, span) => {
                    self.extra.push(PatternKind::Bool as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(if *value { 1 } else { 0 });
                    self.extra.push(body.as_u32());
                }
                RirPattern::Path {
                    module,
                    type_name,
                    variant,
                    span,
                } => {
                    self.extra.push(PatternKind::Path as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    // Store module as u32::MAX for None, otherwise the InstRef
                    self.extra.push(module.map_or(u32::MAX, |r| r.as_u32()));
                    self.extra.push(type_name.into_usize() as u32);
                    self.extra.push(variant.into_usize() as u32);
                    self.extra.push(body.as_u32());
                }
                RirPattern::DataVariant {
                    module,
                    type_name,
                    variant,
                    bindings,
                    span,
                } => {
                    self.extra.push(PatternKind::DataVariant as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(module.map_or(u32::MAX, |r| r.as_u32()));
                    self.extra.push(type_name.into_usize() as u32);
                    self.extra.push(variant.into_usize() as u32);
                    self.extra.push(body.as_u32());
                    self.extra.push(bindings.len() as u32);
                    for binding in bindings {
                        encode_binding(binding, &mut self.extra);
                    }
                }
                RirPattern::StructVariant {
                    module,
                    type_name,
                    variant,
                    field_bindings,
                    span,
                } => {
                    self.extra.push(PatternKind::StructVariant as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(module.map_or(u32::MAX, |r| r.as_u32()));
                    self.extra.push(type_name.into_usize() as u32);
                    self.extra.push(variant.into_usize() as u32);
                    self.extra.push(body.as_u32());
                    self.extra.push(field_bindings.len() as u32);
                    for fb in field_bindings {
                        encode_struct_binding(fb, &mut self.extra);
                    }
                }
                RirPattern::Ident { name, is_mut, span } => {
                    self.extra.push(PatternKind::Ident as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(body.as_u32());
                    self.extra.push(name.into_usize() as u32);
                    self.extra.push(if *is_mut { 1 } else { 0 });
                }
                RirPattern::Tuple {
                    elems,
                    rest_position,
                    span,
                } => {
                    self.extra.push(PatternKind::Tuple as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(body.as_u32());
                    self.extra.push(rest_position.map_or(u32::MAX, |i| i));
                    self.extra.push(elems.len() as u32);
                    for elem in elems {
                        encode_pattern_tree(elem, &mut self.extra);
                    }
                }
                RirPattern::Struct {
                    module,
                    type_name,
                    fields,
                    has_rest,
                    span,
                } => {
                    self.extra.push(PatternKind::Struct as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(body.as_u32());
                    self.extra.push(module.map_or(u32::MAX, |r| r.as_u32()));
                    self.extra.push(type_name.into_usize() as u32);
                    self.extra.push(if *has_rest { 1 } else { 0 });
                    self.extra.push(fields.len() as u32);
                    for field in fields {
                        self.extra.push(field.field_name.into_usize() as u32);
                        encode_pattern_tree(&field.pattern, &mut self.extra);
                    }
                }
                RirPattern::ComptimeUnrollArm {
                    binding,
                    iterable,
                    span,
                } => {
                    self.extra.push(PatternKind::ComptimeUnrollArm as u32);
                    self.extra.push(span.start());
                    self.extra.push(span.len());
                    self.extra.push(body.as_u32());
                    self.extra.push(binding.into_usize() as u32);
                    self.extra.push(iterable.as_u32());
                }
            }
        }
        (start, arms.len() as u32)
    }

    /// Retrieve match arms from the extra array.
    pub fn get_match_arms(&self, start: u32, arm_count: u32) -> Vec<(RirPattern, InstRef)> {
        let mut arms = Vec::with_capacity(arm_count as usize);
        let mut pos = start as usize;

        for _ in 0..arm_count {
            let kind = self.extra[pos];
            match kind {
                k if k == PatternKind::Wildcard as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let body = InstRef::from_raw(self.extra[pos + 3]);
                    arms.push((RirPattern::Wildcard(span), body));
                    pos += PATTERN_WILDCARD_SIZE as usize;
                }
                k if k == PatternKind::Int as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let value_lo = self.extra[pos + 3] as i64;
                    let value_hi = self.extra[pos + 4] as i64;
                    let value = value_lo | (value_hi << 32);
                    let body = InstRef::from_raw(self.extra[pos + 5]);
                    arms.push((RirPattern::Int(value, span), body));
                    pos += PATTERN_INT_SIZE as usize;
                }
                k if k == PatternKind::Bool as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let value = self.extra[pos + 3] != 0;
                    let body = InstRef::from_raw(self.extra[pos + 4]);
                    arms.push((RirPattern::Bool(value, span), body));
                    pos += PATTERN_BOOL_SIZE as usize;
                }
                k if k == PatternKind::Path as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    // Decode module: u32::MAX means None
                    let module_raw = self.extra[pos + 3];
                    let module = if module_raw == u32::MAX {
                        None
                    } else {
                        Some(InstRef::from_raw(module_raw))
                    };
                    let type_name = Spur::try_from_usize(self.extra[pos + 4] as usize).unwrap();
                    let variant = Spur::try_from_usize(self.extra[pos + 5] as usize).unwrap();
                    let body = InstRef::from_raw(self.extra[pos + 6]);
                    arms.push((
                        RirPattern::Path {
                            module,
                            type_name,
                            variant,
                            span,
                        },
                        body,
                    ));
                    pos += PATTERN_PATH_SIZE as usize;
                }
                k if k == PatternKind::DataVariant as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let module_raw = self.extra[pos + 3];
                    let module = if module_raw == u32::MAX {
                        None
                    } else {
                        Some(InstRef::from_raw(module_raw))
                    };
                    let type_name = Spur::try_from_usize(self.extra[pos + 4] as usize).unwrap();
                    let variant = Spur::try_from_usize(self.extra[pos + 5] as usize).unwrap();
                    let body = InstRef::from_raw(self.extra[pos + 6]);
                    let bindings_len = self.extra[pos + 7] as usize;
                    let mut bindings = Vec::with_capacity(bindings_len);
                    let mut offset = 8;
                    for _ in 0..bindings_len {
                        let (b, consumed) = decode_binding(&self.extra[pos + offset..]);
                        bindings.push(b);
                        offset += consumed;
                    }
                    arms.push((
                        RirPattern::DataVariant {
                            module,
                            type_name,
                            variant,
                            bindings,
                            span,
                        },
                        body,
                    ));
                    pos += offset;
                }
                k if k == PatternKind::StructVariant as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let module_raw = self.extra[pos + 3];
                    let module = if module_raw == u32::MAX {
                        None
                    } else {
                        Some(InstRef::from_raw(module_raw))
                    };
                    let type_name = Spur::try_from_usize(self.extra[pos + 4] as usize).unwrap();
                    let variant = Spur::try_from_usize(self.extra[pos + 5] as usize).unwrap();
                    let body = InstRef::from_raw(self.extra[pos + 6]);
                    let bindings_len = self.extra[pos + 7] as usize;
                    let mut field_bindings = Vec::with_capacity(bindings_len);
                    let mut offset = 8;
                    for _ in 0..bindings_len {
                        let (fb, consumed) = decode_struct_binding(&self.extra[pos + offset..]);
                        field_bindings.push(fb);
                        offset += consumed;
                    }
                    arms.push((
                        RirPattern::StructVariant {
                            module,
                            type_name,
                            variant,
                            field_bindings,
                            span,
                        },
                        body,
                    ));
                    pos += offset;
                }
                k if k == PatternKind::Ident as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let body = InstRef::from_raw(self.extra[pos + 3]);
                    let name = Spur::try_from_usize(self.extra[pos + 4] as usize).unwrap();
                    let is_mut = self.extra[pos + 5] != 0;
                    arms.push((RirPattern::Ident { name, is_mut, span }, body));
                    pos += 6;
                }
                k if k == PatternKind::Tuple as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let body = InstRef::from_raw(self.extra[pos + 3]);
                    let rest_raw = self.extra[pos + 4];
                    let rest_position = if rest_raw == u32::MAX {
                        None
                    } else {
                        Some(rest_raw)
                    };
                    let n = self.extra[pos + 5] as usize;
                    let mut elems = Vec::with_capacity(n);
                    let mut offset = 6;
                    for _ in 0..n {
                        let (p, consumed) = decode_pattern_tree(&self.extra[pos + offset..]);
                        elems.push(p);
                        offset += consumed;
                    }
                    arms.push((
                        RirPattern::Tuple {
                            elems,
                            rest_position,
                            span,
                        },
                        body,
                    ));
                    pos += offset;
                }
                k if k == PatternKind::Struct as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let body = InstRef::from_raw(self.extra[pos + 3]);
                    let module_raw = self.extra[pos + 4];
                    let module = if module_raw == u32::MAX {
                        None
                    } else {
                        Some(InstRef::from_raw(module_raw))
                    };
                    let type_name = Spur::try_from_usize(self.extra[pos + 5] as usize).unwrap();
                    let has_rest = self.extra[pos + 6] != 0;
                    let n = self.extra[pos + 7] as usize;
                    let mut fields = Vec::with_capacity(n);
                    let mut offset = 8;
                    for _ in 0..n {
                        let field_name =
                            Spur::try_from_usize(self.extra[pos + offset] as usize).unwrap();
                        offset += 1;
                        let (pattern, consumed) = decode_pattern_tree(&self.extra[pos + offset..]);
                        offset += consumed;
                        fields.push(RirStructField {
                            field_name,
                            pattern,
                        });
                    }
                    arms.push((
                        RirPattern::Struct {
                            module,
                            type_name,
                            fields,
                            has_rest,
                            span,
                        },
                        body,
                    ));
                    pos += offset;
                }
                k if k == PatternKind::ComptimeUnrollArm as u32 => {
                    let span_start = self.extra[pos + 1];
                    let span_len = self.extra[pos + 2];
                    let span = Span::new(span_start, span_start + span_len);
                    let body = InstRef::from_raw(self.extra[pos + 3]);
                    let binding = Spur::try_from_usize(self.extra[pos + 4] as usize).unwrap();
                    let iterable = InstRef::from_raw(self.extra[pos + 5]);
                    arms.push((
                        RirPattern::ComptimeUnrollArm {
                            binding,
                            iterable,
                            span,
                        },
                        body,
                    ));
                    pos += 6;
                }
                _ => panic!("Unknown pattern kind: {}", kind),
            }
        }
        arms
    }

    /// Store field initializers (name, value) and return (start, len).
    /// Layout: [name: u32, value: u32] per field
    pub fn add_field_inits(&mut self, fields: &[(Spur, InstRef)]) -> (u32, u32) {
        let mut data = Vec::with_capacity(fields.len() * FIELD_INIT_SIZE as usize);
        for (name, value) in fields {
            data.push(name.into_usize() as u32);
            data.push(value.as_u32());
        }
        let start = self.add_extra(&data);
        (start, fields.len() as u32)
    }

    /// Retrieve field initializers from the extra array.
    pub fn get_field_inits(&self, start: u32, len: u32) -> Vec<(Spur, InstRef)> {
        let data = self.get_extra(start, len * FIELD_INIT_SIZE);
        let mut fields = Vec::with_capacity(len as usize);
        for chunk in data.chunks(FIELD_INIT_SIZE as usize) {
            let name = Spur::try_from_usize(chunk[0] as usize).unwrap();
            let value = InstRef::from_raw(chunk[1]);
            fields.push((name, value));
        }
        fields
    }

    /// Store field declarations (name, type, is_pub) and return (start, len).
    /// Layout: [name: u32, type: u32, is_pub: u32] per field. ADR-0073 added
    /// the `is_pub` slot; pre-ADR call sites pass `false` for compatibility.
    pub fn add_field_decls(&mut self, fields: &[(Spur, Spur)]) -> (u32, u32) {
        let with_vis: Vec<(Spur, Spur, bool)> =
            fields.iter().map(|(n, t)| (*n, *t, false)).collect();
        self.add_field_decls_with_vis(&with_vis)
    }

    /// Store field declarations including visibility (ADR-0073).
    pub fn add_field_decls_with_vis(&mut self, fields: &[(Spur, Spur, bool)]) -> (u32, u32) {
        let mut data = Vec::with_capacity(fields.len() * FIELD_DECL_SIZE as usize);
        for (name, ty, is_pub) in fields {
            data.push(name.into_usize() as u32);
            data.push(ty.into_usize() as u32);
            data.push(if *is_pub { 1 } else { 0 });
        }
        let start = self.add_extra(&data);
        (start, fields.len() as u32)
    }

    /// Retrieve field declarations (name, type) from the extra array.
    pub fn get_field_decls(&self, start: u32, len: u32) -> Vec<(Spur, Spur)> {
        self.get_field_decls_with_vis(start, len)
            .into_iter()
            .map(|(n, t, _)| (n, t))
            .collect()
    }

    /// Retrieve field declarations with visibility (ADR-0073).
    pub fn get_field_decls_with_vis(&self, start: u32, len: u32) -> Vec<(Spur, Spur, bool)> {
        let data = self.get_extra(start, len * FIELD_DECL_SIZE);
        let mut fields = Vec::with_capacity(len as usize);
        for chunk in data.chunks(FIELD_DECL_SIZE as usize) {
            let name = Spur::try_from_usize(chunk[0] as usize).unwrap();
            let ty = Spur::try_from_usize(chunk[1] as usize).unwrap();
            let is_pub = chunk[2] != 0;
            fields.push((name, ty, is_pub));
        }
        fields
    }

    /// Store directives and return (start, directive_count).
    /// Layout: [name: u32, span_start: u32, span_len: u32, args_len: u32, args...] per directive
    pub fn add_directives(&mut self, directives: &[RirDirective]) -> (u32, u32) {
        let start = self.extra.len() as u32;
        for directive in directives {
            self.extra.push(directive.name.into_usize() as u32);
            self.extra.push(directive.span.start());
            self.extra.push(directive.span.len());
            self.extra.push(directive.args.len() as u32);
            for arg in &directive.args {
                self.extra.push(arg.into_usize() as u32);
            }
        }
        (start, directives.len() as u32)
    }

    /// Retrieve directives from the extra array.
    pub fn get_directives(&self, start: u32, directive_count: u32) -> Vec<RirDirective> {
        let mut directives = Vec::with_capacity(directive_count as usize);
        let mut pos = start as usize;

        for _ in 0..directive_count {
            let name = Spur::try_from_usize(self.extra[pos] as usize).unwrap();
            let span = Span::new(self.extra[pos + 1], self.extra[pos + 2]);
            let args_len = self.extra[pos + 3] as usize;
            pos += 4;

            let args: Vec<Spur> = (0..args_len)
                .map(|i| Spur::try_from_usize(self.extra[pos + i] as usize).unwrap())
                .collect();
            pos += args_len;

            directives.push(RirDirective { name, args, span });
        }
        directives
    }

    /// Store destructure fields and return (start, field_count).
    pub fn add_destructure_fields(&mut self, fields: &[RirDestructureField]) -> (u32, u32) {
        let start = self.extra.len() as u32;
        for field in fields {
            self.extra.push(field.field_name.into_usize() as u32);
            self.extra
                .push(field.binding_name.map_or(0, |s| s.into_usize() as u32));
            self.extra.push(field.is_wildcard as u32);
            self.extra.push(field.is_mut as u32);
        }
        (start, fields.len() as u32)
    }

    /// Retrieve destructure fields from the extra array.
    pub fn get_destructure_fields(&self, start: u32, count: u32) -> Vec<RirDestructureField> {
        let mut fields = Vec::with_capacity(count as usize);
        for i in 0..count {
            let pos = (start + i * DESTRUCTURE_FIELD_SIZE) as usize;
            let field_name = Spur::try_from_usize(self.extra[pos] as usize).unwrap();
            let binding_raw = self.extra[pos + 1];
            let binding_name = if binding_raw == 0 {
                None
            } else {
                Some(Spur::try_from_usize(binding_raw as usize).unwrap())
            };
            let is_wildcard = self.extra[pos + 2] != 0;
            let is_mut = self.extra[pos + 3] != 0;
            fields.push(RirDestructureField {
                field_name,
                binding_name,
                is_wildcard,
                is_mut,
            });
        }
        fields
    }

    // ===== Function span methods =====

    /// Add a function span to track function boundaries.
    /// ADR-0085: append an extern fn declaration (from inside a `link_extern`
    /// block).
    pub fn add_extern_fn(&mut self, extern_fn: RirExternFn) {
        self.extern_fns.push(extern_fn);
    }

    /// ADR-0085: read-only view over all extern fns declared in this file.
    pub fn extern_fns(&self) -> &[RirExternFn] {
        &self.extern_fns
    }

    /// ADR-0085: append an empty `link_extern("lib") { }` block.
    pub fn add_empty_link_extern_block(
        &mut self,
        library: Spur,
        link_mode: RirLinkMode,
        span: Span,
    ) {
        self.empty_link_extern_blocks
            .push((library, link_mode, span));
    }

    /// ADR-0085: read-only view over `link_extern` blocks that declared
    /// no symbols. Sema validates the library name and emits the
    /// corresponding `-l<lib>` link flag.
    pub fn empty_link_extern_blocks(&self) -> &[(Spur, RirLinkMode, Span)] {
        &self.empty_link_extern_blocks
    }

    pub fn add_function_span(&mut self, span: FunctionSpan) {
        self.function_spans.push(span);
    }

    /// Get all function spans.
    pub fn function_spans(&self) -> &[FunctionSpan] {
        &self.function_spans
    }

    /// Iterate over function spans.
    pub fn functions(&self) -> impl Iterator<Item = &FunctionSpan> {
        self.function_spans.iter()
    }

    /// Get the number of functions.
    pub fn function_count(&self) -> usize {
        self.function_spans.len()
    }

    /// Get a view of just one function's instructions.
    pub fn function_view(&self, fn_span: &FunctionSpan) -> RirFunctionView<'_> {
        RirFunctionView {
            rir: self,
            body_start: fn_span.body_start,
            decl: fn_span.decl,
        }
    }

    /// Find a function span by name.
    pub fn find_function(&self, name: Spur) -> Option<&FunctionSpan> {
        self.function_spans.iter().find(|span| span.name == name)
    }

    /// Get the current instruction count (useful for tracking body start).
    pub fn current_inst_index(&self) -> u32 {
        self.instructions.len() as u32
    }

    /// Merge multiple RIRs into a single RIR.
    ///
    /// This is used for parallel per-file RIR generation. Each file generates
    /// its own RIR in parallel, then they are merged into a single RIR with
    /// all instruction references renumbered to be valid in the merged RIR.
    ///
    /// # Arguments
    ///
    /// * `rirs` - Slice of RIRs to merge (typically one per source file)
    ///
    /// # Returns
    ///
    /// A new merged RIR containing all instructions from all input RIRs.
    pub fn merge(rirs: &[Rir]) -> Rir {
        if rirs.is_empty() {
            return Rir::new();
        }

        if rirs.len() == 1 {
            // Clone the single RIR directly
            return Rir {
                instructions: rirs[0].instructions.clone(),
                extra: rirs[0].extra.clone(),
                function_spans: rirs[0].function_spans.clone(),
                extern_fns: rirs[0].extern_fns.clone(),
                empty_link_extern_blocks: rirs[0].empty_link_extern_blocks.clone(),
            };
        }

        // Calculate total sizes for preallocation
        let total_instructions: usize = rirs.iter().map(|r| r.instructions.len()).sum();
        let total_extra: usize = rirs.iter().map(|r| r.extra.len()).sum();
        let total_functions: usize = rirs.iter().map(|r| r.function_spans.len()).sum();
        let total_extern: usize = rirs.iter().map(|r| r.extern_fns.len()).sum();
        let total_empty_blocks: usize = rirs.iter().map(|r| r.empty_link_extern_blocks.len()).sum();

        let mut merged = Rir {
            instructions: Vec::with_capacity(total_instructions),
            extra: Vec::with_capacity(total_extra),
            function_spans: Vec::with_capacity(total_functions),
            extern_fns: Vec::with_capacity(total_extern),
            empty_link_extern_blocks: Vec::with_capacity(total_empty_blocks),
        };

        // Track offsets as we merge each RIR
        let mut inst_offset: u32 = 0;
        let mut extra_offset: u32 = 0;

        for rir in rirs {
            // Merge extra data first (append raw bytes)
            merged.extra.extend_from_slice(&rir.extra);

            // Merge and renumber instructions
            for inst in &rir.instructions {
                let renumbered = Self::renumber_instruction(inst, inst_offset, extra_offset);
                merged.instructions.push(renumbered);
            }

            // Renumber InstRefs in the extra array
            // This handles call args, match arms, field inits, etc.
            Self::renumber_extra_inst_refs(
                &mut merged.extra,
                &rir.instructions,
                inst_offset,
                extra_offset,
            );

            // Merge function spans with renumbered references
            for fn_span in &rir.function_spans {
                merged.function_spans.push(FunctionSpan {
                    name: fn_span.name,
                    body_start: InstRef::from_raw(fn_span.body_start.as_u32() + inst_offset),
                    decl: InstRef::from_raw(fn_span.decl.as_u32() + inst_offset),
                });
            }

            // ADR-0085: extern fns live in the side table; their
            // directive/params indices reference the extra array, so we
            // need to shift them by the merged-file offset.
            for ext in &rir.extern_fns {
                merged.extern_fns.push(RirExternFn {
                    library: ext.library,
                    name: ext.name,
                    directives_start: ext.directives_start + extra_offset,
                    directives_len: ext.directives_len,
                    params_start: ext.params_start + extra_offset,
                    params_len: ext.params_len,
                    return_type: ext.return_type,
                    span: ext.span,
                    block_span: ext.block_span,
                    link_mode: ext.link_mode,
                });
            }

            // ADR-0085: empty `link_extern` blocks need their own
            // pass-through to preserve `-l<lib>` flags when the block
            // declares no symbols.
            merged
                .empty_link_extern_blocks
                .extend_from_slice(&rir.empty_link_extern_blocks);

            // Update offsets for the next RIR
            inst_offset += rir.instructions.len() as u32;
            extra_offset += rir.extra.len() as u32;
        }

        merged
    }

    /// Renumber a single instruction's InstRef fields.
    fn renumber_instruction(inst: &Inst, inst_offset: u32, extra_offset: u32) -> Inst {
        let renumber = |r: InstRef| InstRef::from_raw(r.as_u32() + inst_offset);
        let renumber_opt = |r: Option<InstRef>| r.map(renumber);

        let data = match &inst.data {
            // No renumbering needed for these
            InstData::IntConst(v) => InstData::IntConst(*v),
            InstData::FloatConst(bits) => InstData::FloatConst(*bits),
            InstData::BoolConst(v) => InstData::BoolConst(*v),
            InstData::CharConst(v) => InstData::CharConst(*v),
            InstData::StringConst(s) => InstData::StringConst(*s),
            InstData::UnitConst => InstData::UnitConst,
            InstData::Break => InstData::Break,
            InstData::Continue => InstData::Continue,
            InstData::VarRef { name } => InstData::VarRef { name: *name },
            InstData::ParamRef { index, name } => InstData::ParamRef {
                index: *index,
                name: *name,
            },
            InstData::EnumVariant {
                module,
                type_name,
                variant,
            } => InstData::EnumVariant {
                module: module.map(renumber),
                type_name: *type_name,
                variant: *variant,
            },
            InstData::EnumStructVariant {
                module,
                type_name,
                variant,
                fields_start,
                fields_len,
            } => InstData::EnumStructVariant {
                module: module.map(renumber),
                type_name: *type_name,
                variant: *variant,
                fields_start: *fields_start,
                fields_len: *fields_len,
            },
            InstData::TypeIntrinsic { name, type_arg } => InstData::TypeIntrinsic {
                name: *name,
                type_arg: *type_arg,
            },
            InstData::TypeInterfaceIntrinsic {
                name,
                type_arg,
                type_inst,
                interface_arg,
            } => InstData::TypeInterfaceIntrinsic {
                name: *name,
                type_arg: *type_arg,
                type_inst: type_inst.map(renumber),
                interface_arg: *interface_arg,
            },

            InstData::Bin { op, lhs, rhs } => InstData::Bin {
                op: *op,
                lhs: renumber(*lhs),
                rhs: renumber(*rhs),
            },
            InstData::Unary { op, operand } => InstData::Unary {
                op: *op,
                operand: renumber(*operand),
            },
            InstData::MakeRef { operand, is_mut } => InstData::MakeRef {
                operand: renumber(*operand),
                is_mut: *is_mut,
            },
            InstData::BareRangeSubscript => InstData::BareRangeSubscript,
            InstData::MakeSlice {
                base,
                lo,
                hi,
                is_mut,
            } => InstData::MakeSlice {
                base: renumber(*base),
                lo: renumber_opt(*lo),
                hi: renumber_opt(*hi),
                is_mut: *is_mut,
            },

            // Control flow
            InstData::Branch {
                cond,
                then_block,
                else_block,
                is_comptime,
            } => InstData::Branch {
                cond: renumber(*cond),
                then_block: renumber(*then_block),
                else_block: renumber_opt(*else_block),
                is_comptime: *is_comptime,
            },
            InstData::Loop { cond, body } => InstData::Loop {
                cond: renumber(*cond),
                body: renumber(*body),
            },
            InstData::For {
                binding,
                is_mut,
                iterable,
                body,
            } => InstData::For {
                binding: *binding,
                is_mut: *is_mut,
                iterable: renumber(*iterable),
                body: renumber(*body),
            },
            InstData::InfiniteLoop { body } => InstData::InfiniteLoop {
                body: renumber(*body),
            },
            InstData::Ret(value) => InstData::Ret(renumber_opt(*value)),

            // Match - InstRefs in extra are handled separately
            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => InstData::Match {
                scrutinee: renumber(*scrutinee),
                arms_start: *arms_start + extra_offset,
                arms_len: *arms_len,
            },

            // Block - InstRefs in extra are handled separately
            InstData::Block { extra_start, len } => InstData::Block {
                extra_start: *extra_start + extra_offset,
                len: *len,
            },

            // Variable operations
            InstData::Alloc {
                directives_start,
                directives_len,
                name,
                is_mut,
                ty,
                init,
            } => InstData::Alloc {
                directives_start: *directives_start + extra_offset,
                directives_len: *directives_len,
                name: *name,
                is_mut: *is_mut,
                ty: *ty,
                init: renumber(*init),
            },
            InstData::StructDestructure {
                type_name,
                fields_start,
                fields_len,
                init,
            } => InstData::StructDestructure {
                type_name: *type_name,
                fields_start: *fields_start + extra_offset,
                fields_len: *fields_len,
                init: renumber(*init),
            },
            InstData::Assign { name, value } => InstData::Assign {
                name: *name,
                value: renumber(*value),
            },

            // Function definition - body and params in extra
            InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub,
                is_unchecked,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                receiver_mode,
            } => InstData::FnDecl {
                directives_start: *directives_start + extra_offset,
                directives_len: *directives_len,
                is_pub: *is_pub,
                is_unchecked: *is_unchecked,
                name: *name,
                params_start: *params_start + extra_offset,
                params_len: *params_len,
                return_type: *return_type,
                body: renumber(*body),
                has_self: *has_self,
                receiver_mode: *receiver_mode,
            },

            // Constant declaration - init is an InstRef
            InstData::ConstDecl {
                directives_start,
                directives_len,
                is_pub,
                name,
                ty,
                init,
            } => InstData::ConstDecl {
                directives_start: *directives_start + extra_offset,
                directives_len: *directives_len,
                is_pub: *is_pub,
                name: *name,
                ty: *ty,
                init: renumber(*init),
            },

            // Function call - args in extra
            InstData::Call {
                name,
                args_start,
                args_len,
            } => InstData::Call {
                name: *name,
                args_start: *args_start + extra_offset,
                args_len: *args_len,
            },

            // Intrinsic - args in extra
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => InstData::Intrinsic {
                name: *name,
                args_start: *args_start + extra_offset,
                args_len: *args_len,
            },

            // Struct operations
            InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub,
                posture,
                name,
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            } => InstData::StructDecl {
                directives_start: *directives_start + extra_offset,
                directives_len: *directives_len,
                is_pub: *is_pub,
                posture: *posture,
                name: *name,
                fields_start: *fields_start + extra_offset,
                fields_len: *fields_len,
                methods_start: *methods_start + extra_offset,
                methods_len: *methods_len,
            },
            InstData::StructInit {
                module,
                type_name,
                fields_start,
                fields_len,
            } => InstData::StructInit {
                module: module.map(renumber),
                type_name: *type_name,
                fields_start: *fields_start + extra_offset,
                fields_len: *fields_len,
            },
            InstData::FieldGet { base, field } => InstData::FieldGet {
                base: renumber(*base),
                field: *field,
            },
            InstData::FieldSet { base, field, value } => InstData::FieldSet {
                base: renumber(*base),
                field: *field,
                value: renumber(*value),
            },

            // Interface declarations - method-sig refs in extra
            InstData::InterfaceDecl {
                is_pub,
                name,
                methods_start,
                methods_len,
                directives_start,
                directives_len,
            } => InstData::InterfaceDecl {
                is_pub: *is_pub,
                name: *name,
                methods_start: *methods_start + extra_offset,
                methods_len: *methods_len,
                directives_start: if *directives_len == 0 {
                    *directives_start
                } else {
                    *directives_start + extra_offset
                },
                directives_len: *directives_len,
            },
            InstData::InterfaceMethodSig {
                name,
                params_start,
                params_len,
                return_type,
                receiver_mode,
                is_unchecked,
            } => InstData::InterfaceMethodSig {
                name: *name,
                params_start: *params_start + extra_offset,
                params_len: *params_len,
                return_type: *return_type,
                receiver_mode: *receiver_mode,
                is_unchecked: *is_unchecked,
            },

            // Enum operations
            InstData::EnumDecl {
                is_pub,
                posture,
                name,
                variants_start,
                variants_len,
                methods_start,
                methods_len,
                directives_start,
                directives_len,
            } => InstData::EnumDecl {
                is_pub: *is_pub,
                posture: *posture,
                name: *name,
                variants_start: *variants_start + extra_offset,
                variants_len: *variants_len,
                methods_start: *methods_start + extra_offset,
                methods_len: *methods_len,
                directives_start: if *directives_len == 0 {
                    *directives_start
                } else {
                    *directives_start + extra_offset
                },
                directives_len: *directives_len,
            },

            // Array operations
            InstData::ArrayInit {
                elems_start,
                elems_len,
            } => InstData::ArrayInit {
                elems_start: *elems_start + extra_offset,
                elems_len: *elems_len,
            },
            InstData::IndexGet { base, index } => InstData::IndexGet {
                base: renumber(*base),
                index: renumber(*index),
            },
            InstData::IndexSet { base, index, value } => InstData::IndexSet {
                base: renumber(*base),
                index: renumber(*index),
                value: renumber(*value),
            },

            // Method operations
            InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            } => InstData::MethodCall {
                receiver: renumber(*receiver),
                method: *method,
                args_start: *args_start + extra_offset,
                args_len: *args_len,
            },
            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => InstData::AssocFnCall {
                type_name: *type_name,
                function: *function,
                args_start: *args_start + extra_offset,
                args_len: *args_len,
            },
            InstData::DeriveDecl {
                name,
                methods_start,
                methods_len,
            } => InstData::DeriveDecl {
                name: *name,
                methods_start: *methods_start + extra_offset,
                methods_len: *methods_len,
            },
            InstData::Comptime { expr } => InstData::Comptime {
                expr: renumber(*expr),
            },
            InstData::ComptimeUnrollFor {
                binding,
                iterable,
                body,
            } => InstData::ComptimeUnrollFor {
                binding: *binding,
                iterable: renumber(*iterable),
                body: renumber(*body),
            },
            InstData::Checked { expr } => InstData::Checked {
                expr: renumber(*expr),
            },
            InstData::TypeConst { type_name } => InstData::TypeConst {
                type_name: *type_name,
            },
            InstData::AnonStructType {
                directives_start,
                directives_len,
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            } => InstData::AnonStructType {
                directives_start: if *directives_len == 0 {
                    *directives_start
                } else {
                    *directives_start + extra_offset
                },
                directives_len: *directives_len,
                fields_start: *fields_start + extra_offset,
                fields_len: *fields_len,
                methods_start: *methods_start + extra_offset,
                methods_len: *methods_len,
            },
            InstData::AnonEnumType {
                directives_start,
                directives_len,
                variants_start,
                variants_len,
                methods_start,
                methods_len,
            } => InstData::AnonEnumType {
                directives_start: if *directives_len == 0 {
                    *directives_start
                } else {
                    *directives_start + extra_offset
                },
                directives_len: *directives_len,
                variants_start: *variants_start + extra_offset,
                variants_len: *variants_len,
                methods_start: *methods_start + extra_offset,
                methods_len: *methods_len,
            },
            InstData::AnonInterfaceType {
                methods_start,
                methods_len,
            } => InstData::AnonInterfaceType {
                methods_start: *methods_start + extra_offset,
                methods_len: *methods_len,
            },
            InstData::TupleInit {
                elems_start,
                elems_len,
            } => InstData::TupleInit {
                elems_start: *elems_start + extra_offset,
                elems_len: *elems_len,
            },
            InstData::AnonFnValue { method } => InstData::AnonFnValue {
                method: renumber(*method),
            },
        };

        Inst {
            data,
            span: inst.span,
        }
    }

    /// Renumber InstRefs stored in the extra array.
    ///
    /// This handles:
    /// - Block instruction refs (simple u32 array)
    /// - Array init element refs (simple u32 array)
    /// - Intrinsic arg refs (simple u32 array)
    /// - Impl method refs (simple u32 array)
    /// - Call args (value field of each arg)
    /// - Field inits (value field of each init)
    /// - Match arm bodies (last field of each arm)
    fn renumber_extra_inst_refs(
        extra: &mut [u32],
        instructions: &[Inst],
        inst_offset: u32,
        extra_offset: u32,
    ) {
        for inst in instructions {
            match &inst.data {
                // Block - contains InstRef array
                InstData::Block { extra_start, len } => {
                    let start = (*extra_start + extra_offset) as usize;
                    for i in 0..*len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Array init - contains InstRef array
                InstData::ArrayInit {
                    elems_start,
                    elems_len,
                } => {
                    let start = (*elems_start + extra_offset) as usize;
                    for i in 0..*elems_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Tuple init - contains InstRef array (same layout as ArrayInit)
                InstData::TupleInit {
                    elems_start,
                    elems_len,
                } => {
                    let start = (*elems_start + extra_offset) as usize;
                    for i in 0..*elems_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Intrinsic - contains InstRef array
                InstData::Intrinsic {
                    args_start,
                    args_len,
                    ..
                } => {
                    let start = (*args_start + extra_offset) as usize;
                    for i in 0..*args_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Struct decl - contains InstRef array for methods
                InstData::StructDecl {
                    methods_start,
                    methods_len,
                    ..
                } => {
                    let start = (*methods_start + extra_offset) as usize;
                    for i in 0..*methods_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Interface decl - contains InstRef array for method signatures
                InstData::InterfaceDecl {
                    methods_start,
                    methods_len,
                    ..
                } => {
                    let start = (*methods_start + extra_offset) as usize;
                    for i in 0..*methods_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Derive decl - contains InstRef array for method declarations
                InstData::DeriveDecl {
                    methods_start,
                    methods_len,
                    ..
                } => {
                    let start = (*methods_start + extra_offset) as usize;
                    for i in 0..*methods_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Anonymous interface type - contains InstRef array for method signatures
                InstData::AnonInterfaceType {
                    methods_start,
                    methods_len,
                } => {
                    let start = (*methods_start + extra_offset) as usize;
                    for i in 0..*methods_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Anonymous struct type - contains InstRef array for methods
                InstData::AnonStructType {
                    methods_start,
                    methods_len,
                    ..
                } => {
                    let start = (*methods_start + extra_offset) as usize;
                    for i in 0..*methods_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Anonymous enum type - contains InstRef array for methods
                InstData::AnonEnumType {
                    methods_start,
                    methods_len,
                    ..
                } => {
                    let start = (*methods_start + extra_offset) as usize;
                    for i in 0..*methods_len as usize {
                        extra[start + i] += inst_offset;
                    }
                }

                // Call args - layout: [value, mode] pairs
                InstData::Call {
                    args_start,
                    args_len,
                    ..
                }
                | InstData::MethodCall {
                    args_start,
                    args_len,
                    ..
                }
                | InstData::AssocFnCall {
                    args_start,
                    args_len,
                    ..
                } => {
                    let start = (*args_start + extra_offset) as usize;
                    for i in 0..*args_len as usize {
                        // Each arg is 2 u32s: [value, mode]
                        extra[start + i * 2] += inst_offset;
                    }
                }

                // Field inits - layout: [name, value] pairs
                InstData::StructInit {
                    fields_start,
                    fields_len,
                    ..
                }
                | InstData::EnumStructVariant {
                    fields_start,
                    fields_len,
                    ..
                } => {
                    let start = (*fields_start + extra_offset) as usize;
                    for i in 0..*fields_len as usize {
                        // Each field is 2 u32s: [name, value]
                        extra[start + i * 2 + 1] += inst_offset;
                    }
                }

                // Match arms - variable size patterns with body InstRef at end
                InstData::Match {
                    arms_start,
                    arms_len,
                    ..
                } => {
                    let mut pos = (*arms_start + extra_offset) as usize;
                    for _ in 0..*arms_len {
                        let kind = extra[pos];
                        let pattern_size = match kind {
                            k if k == PatternKind::Wildcard as u32 => {
                                PATTERN_WILDCARD_SIZE as usize
                            }
                            k if k == PatternKind::Int as u32 => PATTERN_INT_SIZE as usize,
                            k if k == PatternKind::Bool as u32 => PATTERN_BOOL_SIZE as usize,
                            k if k == PatternKind::Path as u32 => PATTERN_PATH_SIZE as usize,
                            _ => panic!("Unknown pattern kind during merge: {}", kind),
                        };
                        // The body InstRef is always the last element of each pattern
                        extra[pos + pattern_size - 1] += inst_offset;
                        pos += pattern_size;
                    }
                }

                // These don't have InstRefs in extra
                InstData::IntConst(_)
                | InstData::FloatConst(_)
                | InstData::BoolConst(_)
                | InstData::CharConst(_)
                | InstData::StringConst(_)
                | InstData::UnitConst
                | InstData::Bin { .. }
                | InstData::Unary { .. }
                | InstData::MakeRef { .. }
                | InstData::BareRangeSubscript
                | InstData::MakeSlice { .. }
                | InstData::Branch { .. }
                | InstData::Loop { .. }
                | InstData::For { .. }
                | InstData::InfiniteLoop { .. }
                | InstData::Break
                | InstData::Continue
                | InstData::Ret(_)
                | InstData::VarRef { .. }
                | InstData::ParamRef { .. }
                | InstData::Alloc { .. }
                | InstData::Assign { .. }
                | InstData::FnDecl { .. }
                | InstData::ConstDecl { .. }
                | InstData::FieldGet { .. }
                | InstData::FieldSet { .. }
                | InstData::EnumDecl { .. }
                | InstData::EnumVariant { .. }
                | InstData::IndexGet { .. }
                | InstData::IndexSet { .. }
                | InstData::TypeIntrinsic { .. }
                | InstData::TypeInterfaceIntrinsic { .. }
                | InstData::InterfaceMethodSig { .. }
                | InstData::Comptime { .. }
                | InstData::ComptimeUnrollFor { .. }
                | InstData::Checked { .. }
                | InstData::TypeConst { .. }
                | InstData::StructDestructure { .. }
                | InstData::AnonFnValue { .. } => {}
            }
        }
    }
}

/// A single RIR instruction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Inst {
    pub data: InstData,
    pub span: Span,
}

/// Instruction data - the actual operation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum InstData {
    /// Integer constant
    IntConst(u64),

    /// Floating-point constant, stored as f64 bits for Eq/Hash/Copy compatibility.
    /// Use `f64::from_bits()` to recover the value.
    FloatConst(u64),

    /// Boolean constant
    BoolConst(bool),

    /// Char constant — Unicode scalar value (ADR-0071).
    CharConst(u32),

    /// String constant (interned string content)
    StringConst(Spur),

    /// Unit constant (for blocks that produce unit type)
    UnitConst,

    /// Binary operation: `lhs <op> rhs`. Covers arithmetic, comparison,
    /// logical, and bitwise ops. The specific op is in `BinOp`. Logical
    /// `And`/`Or` are short-circuiting and are lowered to control flow
    /// during CFG construction.
    Bin {
        op: BinOp,
        lhs: InstRef,
        rhs: InstRef,
    },

    /// Unary operation: `<op> operand`. Covers `-`, `!`, and `~`.
    Unary { op: UnaryOp, operand: InstRef },

    /// Reference construction (ADR-0062): `&x` (`is_mut = false`) or
    /// `&mut x` (`is_mut = true`). Operand must be an lvalue. Result type
    /// is `Ref(T)` or `MutRef(T)` where `T` is the operand's type.
    MakeRef { operand: InstRef, is_mut: bool },

    /// ADR-0064: a range-shaped subscript that wasn't borrowed by `&` or
    /// `&mut`. There is no slice value without a borrow, so this carries
    /// no operands; sema reports the error and continues.
    BareRangeSubscript,

    /// Slice construction by borrow over a range subscript (ADR-0064).
    ///
    /// Lowered from `&arr[range]` (`is_mut = false`) and
    /// `&mut arr[range]` (`is_mut = true`). The `base` must be an lvalue
    /// of array type. `lo` and `hi` are the range endpoints; `None` means
    /// the implicit default (`0` for `lo`, `arr.len()` for `hi`).
    MakeSlice {
        base: InstRef,
        lo: Option<InstRef>,
        hi: Option<InstRef>,
        is_mut: bool,
    },

    // Control flow
    /// Branch: if cond then then_block else else_block
    Branch {
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
        /// ADR-0079 follow-up: when set, sema evaluates `cond` at
        /// comptime and emits *only* the chosen branch — the
        /// discarded branch is never analyzed (so it's free to
        /// reference shapes that don't apply to the surrounding
        /// type, e.g. `@uninit(Self)` inside a struct-only branch
        /// when `Self` is enum). Source form: `comptime if cond { … }`.
        is_comptime: bool,
    },

    /// While loop: while cond { body }
    Loop { cond: InstRef, body: InstRef },

    /// For-in loop: for [mut] binding in iterable { body }
    For {
        /// The loop variable name
        binding: Spur,
        /// Whether the loop variable is mutable
        is_mut: bool,
        /// The iterable expression (array or @range result)
        iterable: InstRef,
        /// The loop body
        body: InstRef,
    },

    /// Infinite loop: loop { body }
    InfiniteLoop { body: InstRef },

    /// Match expression: match scrutinee { pattern => expr, ... }
    /// Arms are stored in the extra array using add_match_arms/get_match_arms.
    Match {
        /// The value being matched
        scrutinee: InstRef,
        /// Index into extra data where arms start
        arms_start: u32,
        /// Number of match arms
        arms_len: u32,
    },

    /// Break: exits the innermost loop
    Break,

    /// Continue: jumps to the next iteration of the innermost loop
    Continue,

    /// Function definition
    /// Contains: name symbol, parameters, return type symbol, body instruction ref
    /// Directives and params are stored in the extra array.
    FnDecl {
        /// Index into extra data where directives start
        directives_start: u32,
        /// Number of directives
        directives_len: u32,
        /// Whether this function is public (requires --preview modules)
        is_pub: bool,
        /// Whether this function is marked `unchecked` (can only be called from checked blocks)
        is_unchecked: bool,
        name: Spur,
        /// Index into extra data where params start
        params_start: u32,
        /// Number of parameters
        params_len: u32,
        return_type: Spur,
        body: InstRef,
        /// Whether this function/method takes `self` as a receiver.
        /// Only true for methods in impl blocks that have a self parameter.
        /// Used by sema to know to add the implicit self parameter.
        has_self: bool,
        /// Receiver shape for methods (ADR-0060, ADR-0076). Encoded as a
        /// byte: 0 = `self`/`self : Self` (by-value), 1 = `self : MutRef(Self)`,
        /// 2 = `self : Ref(Self)`. Ignored when `!has_self`.
        receiver_mode: u8,
    },

    /// Constant declaration
    /// Contains: name symbol, optional type, initializer expression ref
    /// Directives are stored in the extra array.
    /// Used for module re-exports: `pub const strings = @import("utils/strings.gruel");`
    ConstDecl {
        /// Index into extra data where directives start
        directives_start: u32,
        /// Number of directives
        directives_len: u32,
        /// Whether this constant is public (requires --preview modules)
        is_pub: bool,
        /// Constant name
        name: Spur,
        /// Optional type annotation (interned string, None if inferred)
        ty: Option<Spur>,
        /// Initializer expression
        init: InstRef,
    },

    /// Function call
    /// Args are stored in the extra array using add_call_args/get_call_args.
    Call {
        /// Function name
        name: Spur,
        /// Index into extra data where args start
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Intrinsic call with expression arguments (e.g., @dbg)
    /// Args are stored in the extra array using add_inst_refs/get_inst_refs.
    Intrinsic {
        /// Intrinsic name (without @)
        name: Spur,
        /// Index into extra data where args start
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Intrinsic call with a type argument (e.g., @size_of, @align_of)
    TypeIntrinsic {
        /// Intrinsic name (without @)
        name: Spur,
        /// Type argument (as an interned string, e.g., "i32", "Point", "[i32; 4]")
        type_arg: Spur,
    },

    /// Intrinsic call with a type argument and an interface argument
    /// (e.g., `@implements(T, Drop)`).
    ///
    /// `type_arg` is the interned name when the source has a bare
    /// type name or type expression. `type_inst`, when `Some`, is an
    /// arbitrary expression that comptime-evaluates to a `Type`
    /// value (e.g. `f.field_type` projecting out of `@type_info`).
    /// Sema prefers `type_inst` when set.
    TypeInterfaceIntrinsic {
        /// Intrinsic name (without @)
        name: Spur,
        /// Type argument (interned name).
        type_arg: Spur,
        /// Optional comptime-evaluable type expression. When set,
        /// supersedes `type_arg`.
        type_inst: Option<InstRef>,
        /// Interface argument (interned name).
        interface_arg: Spur,
    },

    /// Reference to a function parameter
    ParamRef {
        /// Parameter index (0-based)
        index: u32,
        /// Parameter name (for error messages)
        name: Spur,
    },

    /// Return value from function (None for `return;` in unit-returning functions)
    Ret(Option<InstRef>),

    /// Block of instructions (for function bodies)
    /// The result is the last instruction in the block
    Block {
        /// Index into extra data where instruction refs start
        extra_start: u32,
        /// Number of instructions in the block
        len: u32,
    },

    // Variable operations
    /// Local variable declaration: allocates storage and initializes
    /// If name is None, this is a wildcard pattern that discards the value
    /// Directives are stored in the extra array using add_directives/get_directives.
    Alloc {
        /// Index into extra data where directives start
        directives_start: u32,
        /// Number of directives
        directives_len: u32,
        /// Variable name (None for wildcard `_` pattern that discards the value)
        name: Option<Spur>,
        /// Whether the variable is mutable
        is_mut: bool,
        /// Optional type annotation
        ty: Option<Spur>,
        /// Initial value instruction
        init: InstRef,
    },

    /// Struct destructuring: `let TypeName { fields } = expr;`
    /// Fields are stored in the extra array as groups of 4 u32s:
    /// [field_name, binding_name (0 = shorthand), is_wildcard, is_mut]
    StructDestructure {
        /// Struct type name
        type_name: Spur,
        /// Index into extra data where field data starts
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
        /// Initializer expression
        init: InstRef,
    },

    /// Variable reference: reads the value of a variable
    VarRef {
        /// Variable name
        name: Spur,
    },

    /// Assignment: stores a value into a mutable variable
    Assign {
        /// Variable name
        name: Spur,
        /// Value to store
        value: InstRef,
    },

    // Struct operations
    /// Struct type declaration
    /// Directives, fields, and methods are stored in the extra array.
    StructDecl {
        /// Index into extra data where directives start
        directives_start: u32,
        /// Number of directives
        directives_len: u32,
        /// Whether this struct is public (requires --preview modules)
        is_pub: bool,
        /// Declared ownership posture (ADR-0080). `Affine` when neither
        /// `@mark(copy)` nor `@mark(linear)` appears in the directive list.
        posture: Posture,
        /// Struct name
        name: Spur,
        /// Index into extra data where fields start
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
        /// Index into extra data where method refs start
        methods_start: u32,
        /// Number of methods
        methods_len: u32,
    },

    /// Struct literal: creates a new struct instance
    /// Fields are stored in the extra array using add_field_inits/get_field_inits.
    StructInit {
        /// Optional module reference (for qualified struct literals like `module.Point { ... }`)
        /// If Some, the struct is looked up in the module's exports.
        module: Option<InstRef>,
        /// Struct type name
        type_name: Spur,
        /// Index into extra data where fields start
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
    },

    /// Field access: reads a field from a struct
    FieldGet {
        /// Base struct value
        base: InstRef,
        /// Field name
        field: Spur,
    },

    /// Field assignment: writes a value to a struct field
    FieldSet {
        /// Base struct value
        base: InstRef,
        /// Field name
        field: Spur,
        /// Value to store
        value: InstRef,
    },

    // Enum operations
    /// Enum type declaration
    /// Variants are stored in the extra array using add_enum_variant_decls/get_enum_variant_decls.
    /// Each variant encodes: [name_spur, field_count, field_type_0, ..., field_type_n].
    EnumDecl {
        /// Whether this enum is public (requires --preview modules)
        is_pub: bool,
        /// Declared ownership posture (ADR-0080). `Affine` when neither
        /// `@mark(copy)` nor `@mark(linear)` appears in the directive list.
        posture: Posture,
        /// Enum name
        name: Spur,
        /// Index into extra data where variants start
        variants_start: u32,
        /// Number of variants
        variants_len: u32,
        /// Index into extra data where method refs start
        methods_start: u32,
        /// Number of methods
        methods_len: u32,
        /// Index into extra data where directives start (ADR-0079).
        directives_start: u32,
        /// Number of directives.
        directives_len: u32,
    },

    /// Enum variant: creates a value of an enum type
    EnumVariant {
        /// Optional module reference (for qualified paths like `module.Color::Red`)
        /// If Some, the enum is looked up in the module's exports.
        module: Option<InstRef>,
        /// Enum type name
        type_name: Spur,
        /// Variant name
        variant: Spur,
    },

    /// Enum struct variant construction: `Enum::Variant { field: value, ... }`
    /// Fields are stored in the extra array using add_field_inits/get_field_inits.
    EnumStructVariant {
        /// Optional module reference (for qualified paths)
        module: Option<InstRef>,
        /// Enum type name
        type_name: Spur,
        /// Variant name
        variant: Spur,
        /// Start of field initializers in extra array
        fields_start: u32,
        /// Number of field initializers
        fields_len: u32,
    },

    // Array operations
    /// Array literal: creates a new array from element values
    /// Elements are stored in the extra array using add_inst_refs/get_inst_refs.
    ArrayInit {
        /// Index into extra data where elements start
        elems_start: u32,
        /// Number of elements
        elems_len: u32,
    },

    /// Array index read: reads an element from an array
    IndexGet {
        /// Base array value
        base: InstRef,
        /// Index expression
        index: InstRef,
    },

    /// Array index write: writes a value to an array element
    IndexSet {
        /// Base array value (must be a VarRef to a mutable variable)
        base: InstRef,
        /// Index expression
        index: InstRef,
        /// Value to store
        value: InstRef,
    },

    // Method operations
    /// Method call: receiver.method(args)
    /// Args are stored in the extra array using add_call_args/get_call_args.
    MethodCall {
        /// Receiver expression (the struct value)
        receiver: InstRef,
        /// Method name
        method: Spur,
        /// Index into extra data where args start
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Associated function call: Type::function(args)
    /// Args are stored in the extra array using add_call_args/get_call_args.
    AssocFnCall {
        /// Type name (e.g., Point)
        type_name: Spur,
        /// Function name (e.g., origin)
        function: Spur,
        /// Index into extra data where args start
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Interface declaration (ADR-0056): a structurally typed set of method
    /// requirements.
    ///
    /// Method signatures are stored as InstRefs to `InterfaceMethodSig`
    /// instructions in the inst-refs extra array.
    InterfaceDecl {
        /// Whether this interface is public.
        is_pub: bool,
        /// Interface name.
        name: Spur,
        /// Start of method-sig inst refs in extra data.
        methods_start: u32,
        /// Number of method signatures.
        methods_len: u32,
        /// Start of directives in extra data (ADR-0079).
        directives_start: u32,
        /// Number of directives.
        directives_len: u32,
    },

    /// A single method signature inside an `InterfaceDecl`.
    ///
    /// No body. The receiver is always present (interface methods cannot
    /// be associated functions in MVP); its surface shape (`self`,
    /// `self : MutRef(Self)`, or `self : Ref(Self)`) is encoded in
    /// `receiver_mode` using the byte mapping 0/1/2.
    InterfaceMethodSig {
        /// Method name.
        name: Spur,
        /// Start of params in extra data (excluding self).
        params_start: u32,
        /// Number of explicit parameters.
        params_len: u32,
        /// Return type symbol (`()` if none was written).
        return_type: Spur,
        /// Receiver mode encoded as `RirParamMode` (ADR-0060).
        receiver_mode: u8,
        /// ADR-0088: whether this signature was declared `@mark(unchecked)`.
        is_unchecked: bool,
    },

    /// Derive declaration (ADR-0058): `derive Name { fn ... }`.
    ///
    /// Holds a list of method declarations (each emitted as a `FnDecl`
    /// instruction with `has_self` set when the method takes `self`). When a
    /// `@derive(Name)` directive on a struct or enum names this derive, the
    /// methods are spliced into the host type's method list with `Self`
    /// bound to the host.
    DeriveDecl {
        /// Derive name (e.g., `Drop`).
        name: Spur,
        /// Start of method-decl inst refs in extra data.
        methods_start: u32,
        /// Number of methods.
        methods_len: u32,
    },

    /// Comptime block expression: comptime { expr }
    /// The inner expression must be evaluable at compile time.
    Comptime {
        /// The expression to evaluate at compile time
        expr: InstRef,
    },

    /// Comptime unroll for loop: comptime_unroll for binding in iterable { body }
    /// The iterable must be evaluable at compile time. The body is duplicated
    /// once per iteration with the binding replaced by each element's value.
    ComptimeUnrollFor {
        /// The loop variable name
        binding: Spur,
        /// The iterable expression (must be comptime-evaluable, e.g. @type_info fields)
        iterable: InstRef,
        /// The loop body (will be unrolled at compile time)
        body: InstRef,
    },

    /// Checked block expression: checked { expr }
    /// Unchecked operations (raw pointer manipulation, calling unchecked functions)
    /// are only allowed inside checked blocks.
    Checked {
        /// The expression inside the checked block
        expr: InstRef,
    },

    /// Type constant: a type used as a value expression (e.g., `i32` in `identity(i32, 42)`)
    /// The type_name is the symbol for the type (e.g., "i32", "bool").
    TypeConst {
        /// The type name symbol
        type_name: Spur,
    },

    /// Anonymous struct type: a struct type used as a value expression
    /// (e.g., `struct { first: T, second: T, fn method(self) -> T { ... } }` in comptime type construction)
    /// Fields are stored in the extra array using add_field_decls/get_field_decls.
    /// Methods are stored as InstRefs to FnDecl instructions in the extra array.
    AnonStructType {
        /// Index into extra data where directives start (ADR-0058 anon hosts).
        directives_start: u32,
        /// Number of directives.
        directives_len: u32,
        /// Index into extra data where fields start
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
        /// Index into extra data where method InstRefs start
        methods_start: u32,
        /// Number of methods (InstRefs to FnDecl instructions)
        methods_len: u32,
    },

    /// Anonymous enum type: an enum type used as a value expression
    /// (e.g., `enum { Some(T), None, fn method(self) -> T { ... } }` in comptime type construction)
    /// Variants are stored in the extra array using add_enum_variant_decls/get_enum_variant_decls.
    /// Methods are stored as InstRefs to FnDecl instructions in the extra array.
    AnonEnumType {
        /// Index into extra data where directives start (ADR-0058 anon hosts).
        directives_start: u32,
        /// Number of directives.
        directives_len: u32,
        /// Index into extra data where variants start
        variants_start: u32,
        /// Number of variants
        variants_len: u32,
        /// Index into extra data where method InstRefs start
        methods_start: u32,
        /// Number of methods (InstRefs to FnDecl instructions)
        methods_len: u32,
    },

    /// Anonymous interface type (ADR-0057): an interface type used as a
    /// value expression inside a comptime function body.
    ///
    /// The methods are stored as InstRefs to `InterfaceMethodSig` instructions
    /// in the inst-refs extra array, mirroring `InterfaceDecl` for named
    /// interfaces.
    AnonInterfaceType {
        /// Index into extra data where method-sig inst refs start.
        methods_start: u32,
        /// Number of method signatures.
        methods_len: u32,
    },

    /// Tuple literal: `(e0, e1, ..., eN-1)` (ADR-0048).
    /// Lowered in sema to a StructInit against a synthesised anon struct
    /// with field names "0", "1", ... and field types inferred from the elements.
    /// Elements are stored as raw u32 InstRefs in the extra array.
    ///
    /// Tuple field access `t.N` is *not* a distinct RIR instruction; astgen
    /// lowers it to a regular `FieldGet` whose `field` is the stringified index
    /// interned as a Spur. Struct field names are identifiers (never start with
    /// a digit), so synthetic tuple field names cannot collide with user code.
    TupleInit {
        /// Index into extra data where element InstRefs start
        elems_start: u32,
        /// Number of elements
        elems_len: u32,
    },

    /// Anonymous function value (ADR-0055). Lowered in sema to a fresh
    /// lambda-tagged anon struct with zero fields and one `__call` method,
    /// then instantiated as an empty StructInit. The struct type is unique
    /// per-site — sema skips structural dedup for lambda-origin structs so
    /// two same-signature `fn(...)` expressions produce distinct types.
    AnonFnValue {
        /// InstRef to the FnDecl for the synthesized `__call` method.
        method: InstRef,
    },
}

impl fmt::Display for InstRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::RirPrinter;
    use lasso::ThreadedRodeo;

    #[test]
    fn test_inst_ref_size() {
        assert_eq!(std::mem::size_of::<InstRef>(), 4);
    }

    #[test]
    fn test_add_and_get_inst() {
        let mut rir = Rir::new();
        let inst = Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        };
        let inst_ref = rir.add_inst(inst);

        let retrieved = rir.get(inst_ref);
        assert!(matches!(retrieved.data, InstData::IntConst(42)));
    }

    #[test]
    fn test_rir_is_empty() {
        let rir = Rir::new();
        assert!(rir.is_empty());
        assert_eq!(rir.len(), 0);
    }

    #[test]
    fn test_rir_extra_data() {
        let mut rir = Rir::new();
        let data = [1, 2, 3, 4, 5];
        let start = rir.add_extra(&data);
        assert_eq!(start, 0);

        let retrieved = rir.get_extra(start, 5);
        assert_eq!(retrieved, &data);

        // Add more extra data
        let data2 = [10, 20];
        let start2 = rir.add_extra(&data2);
        assert_eq!(start2, 5);
    }

    #[test]
    fn test_rir_iter() {
        let mut rir = Rir::new();
        rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(2, 3),
        });

        let items: Vec<_> = rir.iter().collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0.as_u32(), 0);
        assert_eq!(items[1].0.as_u32(), 1);
    }

    #[test]
    fn test_inst_ref_display() {
        let inst_ref = InstRef::from_raw(42);
        assert_eq!(format!("{}", inst_ref), "%42");
    }

    // RirPattern tests
    #[test]
    fn test_rir_pattern_wildcard_span() {
        let span = Span::new(10, 11);
        let pattern = RirPattern::Wildcard(span);
        assert_eq!(pattern.span(), span);
    }

    #[test]
    fn test_rir_pattern_int_span() {
        let span = Span::new(20, 22);
        let pattern = RirPattern::Int(42, span);
        assert_eq!(pattern.span(), span);

        // Test negative int
        let pattern_neg = RirPattern::Int(-100, span);
        assert_eq!(pattern_neg.span(), span);
    }

    #[test]
    fn test_rir_pattern_bool_span() {
        let span = Span::new(30, 34);
        let pattern = RirPattern::Bool(true, span);
        assert_eq!(pattern.span(), span);

        let pattern_false = RirPattern::Bool(false, span);
        assert_eq!(pattern_false.span(), span);
    }

    #[test]
    fn test_rir_pattern_path_span() {
        let span = Span::new(40, 50);
        let interner = ThreadedRodeo::new();
        let type_name = interner.get_or_intern("Color");
        let variant = interner.get_or_intern("Red");

        let pattern = RirPattern::Path {
            module: None,
            type_name,
            variant,
            span,
        };
        assert_eq!(pattern.span(), span);
    }

    // RirCallArg tests
    #[test]
    fn test_rir_call_arg_modes_distinct() {
        let arg_normal = RirCallArg {
            value: InstRef::from_raw(0),
            mode: RirArgMode::Normal,
        };
        let arg_mut_ref = RirCallArg {
            value: InstRef::from_raw(0),
            mode: RirArgMode::MutRef,
        };
        let arg_ref = RirCallArg {
            value: InstRef::from_raw(0),
            mode: RirArgMode::Ref,
        };
        assert_eq!(arg_normal.mode, RirArgMode::Normal);
        assert_eq!(arg_mut_ref.mode, RirArgMode::MutRef);
        assert_eq!(arg_ref.mode, RirArgMode::Ref);
        assert_ne!(arg_normal.mode, arg_mut_ref.mode);
        assert_ne!(arg_normal.mode, arg_ref.mode);
        assert_ne!(arg_mut_ref.mode, arg_ref.mode);
    }

    // RirPrinter tests
    fn create_printer_test_rir() -> (Rir, ThreadedRodeo) {
        let rir = Rir::new();
        let interner = ThreadedRodeo::new();
        (rir, interner)
    }

    #[test]
    fn test_printer_int_const() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const 42"));
    }

    #[test]
    fn test_printer_bool_const() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        rir.add_inst(Inst {
            data: InstData::BoolConst(false),
            span: Span::new(0, 5),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const true"));
        assert!(output.contains("%1 = const false"));
    }

    #[test]
    fn test_printer_string_const() {
        let (mut rir, interner) = create_printer_test_rir();
        let hello = interner.get_or_intern("hello world");
        rir.add_inst(Inst {
            data: InstData::StringConst(hello),
            span: Span::new(0, 13),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const \"hello world\""));
    }

    #[test]
    fn test_printer_unit_const() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("%0 = const ()"));
    }

    #[test]
    fn test_printer_binary_ops() {
        let (mut rir, interner) = create_printer_test_rir();
        let lhs = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let rhs = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(2, 3),
        });

        // Test all binary operations
        let ops = [
            BinOp::Add,
            BinOp::Sub,
            BinOp::Mul,
            BinOp::Div,
            BinOp::Mod,
            BinOp::Eq,
            BinOp::Ne,
            BinOp::Lt,
            BinOp::Gt,
            BinOp::Le,
            BinOp::Ge,
            BinOp::And,
            BinOp::Or,
            BinOp::BitAnd,
            BinOp::BitOr,
            BinOp::BitXor,
            BinOp::Shl,
            BinOp::Shr,
        ];
        let _ = (lhs, rhs);

        for op in ops {
            let mut test_rir = Rir::new();
            let lhs = test_rir.add_inst(Inst {
                data: InstData::IntConst(1),
                span: Span::new(0, 1),
            });
            let rhs = test_rir.add_inst(Inst {
                data: InstData::IntConst(2),
                span: Span::new(2, 3),
            });
            let data = InstData::Bin { op, lhs, rhs };
            let op_name = op.as_str();
            test_rir.add_inst(Inst {
                data,
                span: Span::new(0, 5),
            });

            let printer = RirPrinter::new(&test_rir, &interner);
            let output = printer.to_string();
            let expected = format!("%2 = {} %0, %1", op_name);
            assert!(
                output.contains(&expected),
                "Expected '{}' in output:\n{}",
                expected,
                output
            );
        }
    }

    #[test]
    fn test_printer_unary_ops() {
        let (mut rir, interner) = create_printer_test_rir();
        let operand = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        rir.add_inst(Inst {
            data: InstData::Unary {
                op: UnaryOp::Neg,
                operand,
            },
            span: Span::new(0, 3),
        });
        rir.add_inst(Inst {
            data: InstData::Unary {
                op: UnaryOp::Not,
                operand,
            },
            span: Span::new(0, 3),
        });
        rir.add_inst(Inst {
            data: InstData::Unary {
                op: UnaryOp::BitNot,
                operand,
            },
            span: Span::new(0, 3),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("neg %0"));
        assert!(output.contains("not %0"));
        assert!(output.contains("bit_not %0"));
    }

    #[test]
    fn test_printer_branch() {
        let (mut rir, interner) = create_printer_test_rir();
        let cond = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let then_block = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let else_block = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        // With else block
        rir.add_inst(Inst {
            data: InstData::Branch {
                cond,
                then_block,
                else_block: Some(else_block),
                is_comptime: false,
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("branch %0, %1, %2"));
    }

    #[test]
    fn test_printer_branch_no_else() {
        let (mut rir, interner) = create_printer_test_rir();
        let cond = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let then_block = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::Branch {
                cond,
                then_block,
                else_block: None,
                is_comptime: false,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        // Should not have the third argument
        assert!(output.contains("branch %0, %1\n"));
    }

    #[test]
    fn test_printer_loop() {
        let (mut rir, interner) = create_printer_test_rir();
        let cond = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::Loop { cond, body },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("loop %0, %1"));
    }

    #[test]
    fn test_printer_infinite_loop() {
        let (mut rir, interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::InfiniteLoop { body },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("infinite_loop %0"));
    }

    #[test]
    fn test_printer_break_continue() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::Break,
            span: Span::new(0, 5),
        });
        rir.add_inst(Inst {
            data: InstData::Continue,
            span: Span::new(0, 8),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("break\n"));
        assert!(output.contains("continue\n"));
    }

    #[test]
    fn test_printer_ret() {
        let (mut rir, interner) = create_printer_test_rir();
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        // Return with value
        rir.add_inst(Inst {
            data: InstData::Ret(Some(value)),
            span: Span::new(0, 10),
        });
        // Return without value
        rir.add_inst(Inst {
            data: InstData::Ret(None),
            span: Span::new(0, 6),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("ret %0"));
        assert!(output.contains("%2 = ret\n"));
    }

    #[test]
    fn test_printer_fn_decl() {
        let (mut rir, interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("main");
        let return_type = interner.get_or_intern("i32");
        let param_name = interner.get_or_intern("x");
        let param_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (params_start, params_len) = rir.add_params(&[RirParam {
            name: param_name,
            ty: param_type,
            mode: RirParamMode::Normal,
            is_comptime: false,
        }]);

        rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_unchecked: false,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self: false,
                receiver_mode: 0,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("fn main(x: i32) -> i32"));
    }

    #[test]
    fn test_printer_fn_decl_with_self() {
        let (mut rir, interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let name = interner.get_or_intern("get_x");
        let return_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (params_start, params_len) = rir.add_params(&[]);

        rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_unchecked: false,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self: true,
                receiver_mode: 0,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("fn get_x(self, ) -> i32"));
    }

    #[test]
    fn test_printer_fn_decl_param_modes() {
        let (mut rir, interner) = create_printer_test_rir();
        let body = rir.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("modify");
        let return_type = interner.get_or_intern("()");
        let param1_name = interner.get_or_intern("a");
        let param1_type = interner.get_or_intern("i32");
        let param2_name = interner.get_or_intern("b");
        let param2_type = interner.get_or_intern("i32");
        let param3_name = interner.get_or_intern("c");
        let param3_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (params_start, params_len) = rir.add_params(&[
            RirParam {
                name: param1_name,
                ty: param1_type,
                mode: RirParamMode::Normal,
                is_comptime: false,
            },
            RirParam {
                name: param2_name,
                ty: param2_type,
                mode: RirParamMode::MutRef,
                is_comptime: false,
            },
            RirParam {
                name: param3_name,
                ty: param3_type,
                mode: RirParamMode::Ref,
                is_comptime: false,
            },
        ]);

        rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_unchecked: false,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self: false,
                receiver_mode: 0,
            },
            span: Span::new(0, 50),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("a: i32"));
        assert!(output.contains("mut_ref b: i32"));
        assert!(output.contains("ref c: i32"));
    }

    #[test]
    fn test_printer_call() {
        let (mut rir, interner) = create_printer_test_rir();
        let arg = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("foo");

        let (args_start, args_len) = rir.add_call_args(&[RirCallArg {
            value: arg,
            mode: RirArgMode::Normal,
        }]);

        rir.add_inst(Inst {
            data: InstData::Call {
                name,
                args_start,
                args_len,
            },
            span: Span::new(0, 8),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("call foo(%0)"));
    }

    #[test]
    fn test_printer_call_with_arg_modes() {
        let (mut rir, interner) = create_printer_test_rir();
        let arg1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let arg2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let arg3 = rir.add_inst(Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 1),
        });

        let name = interner.get_or_intern("modify");

        let (args_start, args_len) = rir.add_call_args(&[
            RirCallArg {
                value: arg1,
                mode: RirArgMode::Normal,
            },
            RirCallArg {
                value: arg2,
                mode: RirArgMode::MutRef,
            },
            RirCallArg {
                value: arg3,
                mode: RirArgMode::Ref,
            },
        ]);

        rir.add_inst(Inst {
            data: InstData::Call {
                name,
                args_start,
                args_len,
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("call modify(%0, mut_ref %1, ref %2)"));
    }

    #[test]
    fn test_printer_intrinsic() {
        let (mut rir, interner) = create_printer_test_rir();
        let arg = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("dbg");

        let (args_start, args_len) = rir.add_call_args(&[RirCallArg {
            value: arg,
            mode: RirArgMode::Normal,
        }]);

        rir.add_inst(Inst {
            data: InstData::Intrinsic {
                name,
                args_start,
                args_len,
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("intrinsic @dbg(%0)"));
    }

    #[test]
    fn test_printer_type_intrinsic() {
        let (mut rir, interner) = create_printer_test_rir();
        let name = interner.get_or_intern("size_of");
        let type_arg = interner.get_or_intern("i32");

        rir.add_inst(Inst {
            data: InstData::TypeIntrinsic { name, type_arg },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("type_intrinsic @size_of(i32)"));
    }

    #[test]
    fn test_printer_param_ref() {
        let (mut rir, interner) = create_printer_test_rir();
        let name = interner.get_or_intern("x");

        rir.add_inst(Inst {
            data: InstData::ParamRef { index: 0, name },
            span: Span::new(0, 1),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("param 0 (x)"));
    }

    #[test]
    fn test_printer_block() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::Block {
                extra_start: 0,
                len: 3,
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("block(0, 3)"));
    }

    #[test]
    fn test_printer_alloc() {
        let (mut rir, interner) = create_printer_test_rir();
        let init = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("x");
        let ty = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);

        // Normal alloc with type
        rir.add_inst(Inst {
            data: InstData::Alloc {
                directives_start,
                directives_len,
                name: Some(name),
                is_mut: false,
                ty: Some(ty),
                init,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc x: i32= %0"));
    }

    #[test]
    fn test_printer_alloc_mut() {
        let (mut rir, interner) = create_printer_test_rir();
        let init = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("x");

        let (directives_start, directives_len) = rir.add_directives(&[]);

        rir.add_inst(Inst {
            data: InstData::Alloc {
                directives_start,
                directives_len,
                name: Some(name),
                is_mut: true,
                ty: None,
                init,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc mut x= %0"));
    }

    #[test]
    fn test_printer_alloc_wildcard() {
        let (mut rir, interner) = create_printer_test_rir();
        let init = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let (directives_start, directives_len) = rir.add_directives(&[]);

        rir.add_inst(Inst {
            data: InstData::Alloc {
                directives_start,
                directives_len,
                name: None,
                is_mut: false,
                ty: None,
                init,
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("alloc _= %0"));
    }

    #[test]
    fn test_printer_var_ref() {
        let (mut rir, interner) = create_printer_test_rir();
        let name = interner.get_or_intern("x");

        rir.add_inst(Inst {
            data: InstData::VarRef { name },
            span: Span::new(0, 1),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("var_ref x"));
    }

    #[test]
    fn test_printer_assign() {
        let (mut rir, interner) = create_printer_test_rir();
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        let name = interner.get_or_intern("x");

        rir.add_inst(Inst {
            data: InstData::Assign { name, value },
            span: Span::new(0, 6),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("assign x = %0"));
    }

    #[test]
    fn test_printer_struct_decl() {
        let (mut rir, interner) = create_printer_test_rir();
        let name = interner.get_or_intern("Point");
        let x_name = interner.get_or_intern("x");
        let y_name = interner.get_or_intern("y");
        let i32_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (fields_start, fields_len) =
            rir.add_field_decls(&[(x_name, i32_type), (y_name, i32_type)]);
        let (methods_start, methods_len) = rir.add_inst_refs(&[]);

        rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: false,
                posture: Posture::Affine,
                name,
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("struct Point { x: i32, y: i32 }"));
    }

    #[test]
    fn test_printer_struct_decl_with_directive() {
        let (mut rir, interner) = create_printer_test_rir();
        let name = interner.get_or_intern("Point");
        let x_name = interner.get_or_intern("x");
        let i32_type = interner.get_or_intern("i32");
        let directive_name = interner.get_or_intern("handle");

        let (directives_start, directives_len) = rir.add_directives(&[RirDirective {
            name: directive_name,
            args: vec![],
            span: Span::new(0, 7),
        }]);
        let (fields_start, fields_len) = rir.add_field_decls(&[(x_name, i32_type)]);
        let (methods_start, methods_len) = rir.add_inst_refs(&[]);

        rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: false,
                posture: Posture::Affine,
                name,
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("@handle struct Point { x: i32 }"));
    }

    #[test]
    fn test_printer_struct_init() {
        let (mut rir, interner) = create_printer_test_rir();
        let x_val = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });
        let y_val = rir.add_inst(Inst {
            data: InstData::IntConst(20),
            span: Span::new(0, 2),
        });

        let type_name = interner.get_or_intern("Point");
        let x_name = interner.get_or_intern("x");
        let y_name = interner.get_or_intern("y");

        let (fields_start, fields_len) = rir.add_field_inits(&[(x_name, x_val), (y_name, y_val)]);

        rir.add_inst(Inst {
            data: InstData::StructInit {
                module: None,
                type_name,
                fields_start,
                fields_len,
            },
            span: Span::new(0, 25),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("struct_init Point { x: %0, y: %1 }"));
    }

    #[test]
    fn test_printer_field_get() {
        let (mut rir, interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for a struct value
            span: Span::new(0, 1),
        });

        let field = interner.get_or_intern("x");

        rir.add_inst(Inst {
            data: InstData::FieldGet { base, field },
            span: Span::new(0, 5),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("field_get %0.x"));
    }

    #[test]
    fn test_printer_field_set() {
        let (mut rir, interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder
            span: Span::new(0, 1),
        });
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let field = interner.get_or_intern("x");

        rir.add_inst(Inst {
            data: InstData::FieldSet { base, field, value },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("field_set %0.x = %1"));
    }

    #[test]
    fn test_printer_enum_decl() {
        let (mut rir, interner) = create_printer_test_rir();
        let name = interner.get_or_intern("Color");
        let red = interner.get_or_intern("Red");
        let green = interner.get_or_intern("Green");
        let blue = interner.get_or_intern("Blue");

        // Unit variants: no fields
        let (variants_start, variants_len) = rir.add_enum_variant_decls(&[
            (red, vec![], vec![]),
            (green, vec![], vec![]),
            (blue, vec![], vec![]),
        ]);

        rir.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: false,
                posture: Posture::Affine,
                name,
                variants_start,
                variants_len,
                methods_start: 0,
                methods_len: 0,
                directives_start: 0,
                directives_len: 0,
            },
            span: Span::new(0, 35),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("enum Color { Red, Green, Blue }"));
    }

    #[test]
    fn test_printer_enum_decl_with_data() {
        let (mut rir, interner) = create_printer_test_rir();
        let name = interner.get_or_intern("IntOption");
        let none = interner.get_or_intern("None");
        let some = interner.get_or_intern("Some");
        let i32_ty = interner.get_or_intern("i32");

        let (variants_start, variants_len) =
            rir.add_enum_variant_decls(&[(none, vec![], vec![]), (some, vec![i32_ty], vec![])]);

        rir.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: false,
                posture: Posture::Affine,
                name,
                variants_start,
                variants_len,
                methods_start: 0,
                methods_len: 0,
                directives_start: 0,
                directives_len: 0,
            },
            span: Span::new(0, 35),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("enum IntOption { None, Some(i32) }"));
    }

    #[test]
    fn test_printer_enum_variant() {
        let (mut rir, interner) = create_printer_test_rir();
        let type_name = interner.get_or_intern("Color");
        let variant = interner.get_or_intern("Red");

        rir.add_inst(Inst {
            data: InstData::EnumVariant {
                module: None,
                type_name,
                variant,
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("enum_variant Color::Red"));
    }

    #[test]
    fn test_printer_array_init() {
        let (mut rir, interner) = create_printer_test_rir();
        let elem1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let elem2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let elem3 = rir.add_inst(Inst {
            data: InstData::IntConst(3),
            span: Span::new(0, 1),
        });

        let (elems_start, elems_len) = rir.add_inst_refs(&[elem1, elem2, elem3]);

        rir.add_inst(Inst {
            data: InstData::ArrayInit {
                elems_start,
                elems_len,
            },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("array_init [%0, %1, %2]"));
    }

    #[test]
    fn test_printer_index_get() {
        let (mut rir, interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for array
            span: Span::new(0, 1),
        });
        let index = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });

        rir.add_inst(Inst {
            data: InstData::IndexGet { base, index },
            span: Span::new(0, 5),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("index_get %0[%1]"));
    }

    #[test]
    fn test_printer_index_set() {
        let (mut rir, interner) = create_printer_test_rir();
        let base = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for array
            span: Span::new(0, 1),
        });
        let index = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let value = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        rir.add_inst(Inst {
            data: InstData::IndexSet { base, index, value },
            span: Span::new(0, 10),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("index_set %0[%1] = %2"));
    }

    // Struct with methods tests
    #[test]
    fn test_printer_struct_decl_with_methods() {
        let (mut rir, interner) = create_printer_test_rir();

        // Create a method first
        let method_body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });
        let method_name = interner.get_or_intern("get_x");
        let return_type = interner.get_or_intern("i32");

        let (directives_start, directives_len) = rir.add_directives(&[]);
        let (params_start, params_len) = rir.add_params(&[]);

        let method_ref = rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_unchecked: false,
                name: method_name,
                params_start,
                params_len,
                return_type,
                body: method_body,
                has_self: true,
                receiver_mode: 0,
            },
            span: Span::new(0, 30),
        });

        let struct_name = interner.get_or_intern("Point");
        let x_field = interner.get_or_intern("x");
        let i32_type = interner.get_or_intern("i32");

        let (fields_start, fields_len) = rir.add_field_decls(&[(x_field, i32_type)]);
        let (methods_start, methods_len) = rir.add_inst_refs(&[method_ref]);

        rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: false,
                posture: Posture::Affine,
                name: struct_name,
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            },
            span: Span::new(0, 50),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("struct Point { x: i32 } methods: [%1]"));
    }

    #[test]
    fn test_printer_method_call() {
        let (mut rir, interner) = create_printer_test_rir();
        let receiver = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for struct value
            span: Span::new(0, 1),
        });
        let arg = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        let method = interner.get_or_intern("add");

        let (args_start, args_len) = rir.add_call_args(&[RirCallArg {
            value: arg,
            mode: RirArgMode::Normal,
        }]);

        rir.add_inst(Inst {
            data: InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("method_call %0.add(%1)"));
    }

    #[test]
    fn test_printer_method_call_with_arg_modes() {
        let (mut rir, interner) = create_printer_test_rir();
        let receiver = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });
        let arg1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let arg2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });

        let method = interner.get_or_intern("modify");

        let (args_start, args_len) = rir.add_call_args(&[
            RirCallArg {
                value: arg1,
                mode: RirArgMode::MutRef,
            },
            RirCallArg {
                value: arg2,
                mode: RirArgMode::Ref,
            },
        ]);

        rir.add_inst(Inst {
            data: InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            },
            span: Span::new(0, 25),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("method_call %0.modify(mut_ref %1, ref %2)"));
    }

    #[test]
    fn test_printer_assoc_fn_call() {
        let (mut rir, interner) = create_printer_test_rir();

        let type_name = interner.get_or_intern("Point");
        let function = interner.get_or_intern("origin");

        let (args_start, args_len) = rir.add_call_args(&[]);

        rir.add_inst(Inst {
            data: InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            },
            span: Span::new(0, 15),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("assoc_fn_call Point::origin()"));
    }

    #[test]
    fn test_printer_assoc_fn_call_with_args() {
        let (mut rir, interner) = create_printer_test_rir();
        let arg1 = rir.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });
        let arg2 = rir.add_inst(Inst {
            data: InstData::IntConst(20),
            span: Span::new(0, 2),
        });

        let type_name = interner.get_or_intern("Point");
        let function = interner.get_or_intern("new");

        let (args_start, args_len) = rir.add_call_args(&[
            RirCallArg {
                value: arg1,
                mode: RirArgMode::Normal,
            },
            RirCallArg {
                value: arg2,
                mode: RirArgMode::Normal,
            },
        ]);

        rir.add_inst(Inst {
            data: InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("assoc_fn_call Point::new(%0, %1)"));
    }

    // Match and pattern tests
    #[test]
    fn test_printer_match_wildcard() {
        let (mut rir, interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });
        let body = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let (arms_start, arms_len) =
            rir.add_match_arms(&[(RirPattern::Wildcard(Span::new(0, 1)), body)]);

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 20),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { _ => %1 }"));
    }

    #[test]
    fn test_printer_match_int_pattern() {
        let (mut rir, interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });
        let body1 = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let body2 = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let body_default = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let (arms_start, arms_len) = rir.add_match_arms(&[
            (RirPattern::Int(1, Span::new(0, 1)), body1),
            (RirPattern::Int(-5, Span::new(0, 2)), body2),
            (RirPattern::Wildcard(Span::new(0, 1)), body_default),
        ]);

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { 1 => %1, -5 => %2, _ => %3 }"));
    }

    #[test]
    fn test_printer_match_bool_pattern() {
        let (mut rir, interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let body_true = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let body_false = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let (arms_start, arms_len) = rir.add_match_arms(&[
            (RirPattern::Bool(true, Span::new(0, 4)), body_true),
            (RirPattern::Bool(false, Span::new(0, 5)), body_false),
        ]);

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 30),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { true => %1, false => %2 }"));
    }

    #[test]
    fn test_printer_match_path_pattern() {
        let (mut rir, interner) = create_printer_test_rir();
        let scrutinee = rir.add_inst(Inst {
            data: InstData::IntConst(0), // placeholder for enum value
            span: Span::new(0, 1),
        });
        let body_red = rir.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let body_green = rir.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 1),
        });
        let body_default = rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });

        let color = interner.get_or_intern("Color");
        let red = interner.get_or_intern("Red");
        let green = interner.get_or_intern("Green");

        let (arms_start, arms_len) = rir.add_match_arms(&[
            (
                RirPattern::Path {
                    module: None,
                    type_name: color,
                    variant: red,
                    span: Span::new(0, 10),
                },
                body_red,
            ),
            (
                RirPattern::Path {
                    module: None,
                    type_name: color,
                    variant: green,
                    span: Span::new(0, 12),
                },
                body_green,
            ),
            (RirPattern::Wildcard(Span::new(0, 1)), body_default),
        ]);

        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 50),
        });

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();
        assert!(output.contains("match %0 { Color::Red => %1, Color::Green => %2, _ => %3 }"));
    }

    #[test]
    fn test_printer_display_trait() {
        let (mut rir, interner) = create_printer_test_rir();
        rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let printer = RirPrinter::new(&rir, &interner);
        // Test Display trait implementation
        let output = format!("{}", printer);
        assert!(output.contains("%0 = const 42"));
    }

    // ===== RIR merge tests =====

    #[test]
    fn test_merge_empty_rirs() {
        let merged = Rir::merge(&[]);
        assert!(merged.is_empty());
        assert!(merged.function_spans().is_empty());
    }

    #[test]
    fn test_merge_single_rir() {
        let mut rir = Rir::new();
        rir.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });
        rir.add_inst(Inst {
            data: InstData::BoolConst(true),
            span: Span::new(3, 7),
        });

        let merged = Rir::merge(&[rir]);
        assert_eq!(merged.len(), 2);

        // Check that instructions are preserved
        assert!(matches!(
            merged.get(InstRef::from_raw(0)).data,
            InstData::IntConst(42)
        ));
        assert!(matches!(
            merged.get(InstRef::from_raw(1)).data,
            InstData::BoolConst(true)
        ));
    }

    #[test]
    fn test_merge_two_rirs_simple() {
        // RIR 1: just an int constant
        let mut rir1 = Rir::new();
        rir1.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(0, 2),
        });

        // RIR 2: another int constant
        let mut rir2 = Rir::new();
        rir2.add_inst(Inst {
            data: InstData::IntConst(20),
            span: Span::new(5, 7),
        });

        let merged = Rir::merge(&[rir1, rir2]);
        assert_eq!(merged.len(), 2);

        // First instruction from rir1
        assert!(matches!(
            merged.get(InstRef::from_raw(0)).data,
            InstData::IntConst(10)
        ));
        // Second instruction from rir2 (renumbered to index 1)
        assert!(matches!(
            merged.get(InstRef::from_raw(1)).data,
            InstData::IntConst(20)
        ));
    }

    #[test]
    fn test_merge_renumbers_inst_refs() {
        // RIR 1: const and an add that references it
        let mut rir1 = Rir::new();
        let const1 = rir1.add_inst(Inst {
            data: InstData::IntConst(5),
            span: Span::new(0, 1),
        });
        rir1.add_inst(Inst {
            data: InstData::Bin {
                op: BinOp::Add,
                lhs: const1,
                rhs: const1,
            },
            span: Span::new(2, 5),
        });

        // RIR 2: const and an add that references it (local indices)
        let mut rir2 = Rir::new();
        let const2 = rir2.add_inst(Inst {
            data: InstData::IntConst(10),
            span: Span::new(10, 12),
        });
        rir2.add_inst(Inst {
            data: InstData::Bin {
                op: BinOp::Add,
                lhs: const2,
                rhs: const2,
            },
            span: Span::new(12, 16),
        });

        let merged = Rir::merge(&[rir1, rir2]);
        assert_eq!(merged.len(), 4);

        // Check rir1's add still references %0
        if let InstData::Bin {
            op: BinOp::Add,
            lhs,
            rhs,
        } = &merged.get(InstRef::from_raw(1)).data
        {
            assert_eq!(lhs.as_u32(), 0);
            assert_eq!(rhs.as_u32(), 0);
        } else {
            panic!("Expected Add instruction at index 1");
        }

        // Check rir2's add now references %2 (renumbered from %0)
        if let InstData::Bin {
            op: BinOp::Add,
            lhs,
            rhs,
        } = &merged.get(InstRef::from_raw(3)).data
        {
            assert_eq!(lhs.as_u32(), 2);
            assert_eq!(rhs.as_u32(), 2);
        } else {
            panic!("Expected Add instruction at index 3");
        }
    }

    #[test]
    fn test_merge_renumbers_extra_data() {
        // RIR 1: function call with args in extra
        let mut rir1 = Rir::new();
        let interner = ThreadedRodeo::new();
        let fn_name = interner.get_or_intern("foo");

        let const1 = rir1.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let (args_start, args_len) = rir1.add_call_args(&[RirCallArg {
            value: const1,
            mode: RirArgMode::Normal,
        }]);
        rir1.add_inst(Inst {
            data: InstData::Call {
                name: fn_name,
                args_start,
                args_len,
            },
            span: Span::new(2, 8),
        });

        // RIR 2: function call with args in extra
        let mut rir2 = Rir::new();
        let const2 = rir2.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(10, 11),
        });
        let (args_start2, args_len2) = rir2.add_call_args(&[RirCallArg {
            value: const2,
            mode: RirArgMode::Normal,
        }]);
        rir2.add_inst(Inst {
            data: InstData::Call {
                name: fn_name,
                args_start: args_start2,
                args_len: args_len2,
            },
            span: Span::new(12, 18),
        });

        let merged = Rir::merge(&[rir1, rir2]);
        assert_eq!(merged.len(), 4);

        // Check rir1's call still has correct args_start
        if let InstData::Call {
            args_start,
            args_len,
            ..
        } = &merged.get(InstRef::from_raw(1)).data
        {
            let args = merged.get_call_args(*args_start, *args_len);
            assert_eq!(args.len(), 1);
            assert_eq!(args[0].value.as_u32(), 0); // Still references const1 at %0
        } else {
            panic!("Expected Call instruction at index 1");
        }

        // Check rir2's call has updated args_start and renumbered arg value
        if let InstData::Call {
            args_start,
            args_len,
            ..
        } = &merged.get(InstRef::from_raw(3)).data
        {
            let args = merged.get_call_args(*args_start, *args_len);
            assert_eq!(args.len(), 1);
            assert_eq!(args[0].value.as_u32(), 2); // Now references const2 at %2
        } else {
            panic!("Expected Call instruction at index 3");
        }
    }

    #[test]
    fn test_merge_function_spans() {
        let interner = ThreadedRodeo::new();
        let main_name = interner.get_or_intern("main");
        let helper_name = interner.get_or_intern("helper");

        // RIR 1: main function
        let mut rir1 = Rir::new();
        let body_start1 = InstRef::from_raw(rir1.current_inst_index());
        let const1 = rir1.add_inst(Inst {
            data: InstData::IntConst(0),
            span: Span::new(0, 1),
        });
        let (params_start, params_len) = rir1.add_params(&[]);
        let (dirs_start, dirs_len) = rir1.add_directives(&[]);
        let ret_type = interner.get_or_intern("i32");
        let decl1 = rir1.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start: dirs_start,
                directives_len: dirs_len,
                is_pub: false,
                is_unchecked: false,
                name: main_name,
                params_start,
                params_len,
                return_type: ret_type,
                body: const1,
                has_self: false,
                receiver_mode: 0,
            },
            span: Span::new(0, 10),
        });
        rir1.add_function_span(FunctionSpan::new(main_name, body_start1, decl1));

        // RIR 2: helper function
        let mut rir2 = Rir::new();
        let body_start2 = InstRef::from_raw(rir2.current_inst_index());
        let const2 = rir2.add_inst(Inst {
            data: InstData::IntConst(42),
            span: Span::new(20, 22),
        });
        let (params_start2, params_len2) = rir2.add_params(&[]);
        let (dirs_start2, dirs_len2) = rir2.add_directives(&[]);
        let decl2 = rir2.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start: dirs_start2,
                directives_len: dirs_len2,
                is_pub: false,
                is_unchecked: false,
                name: helper_name,
                params_start: params_start2,
                params_len: params_len2,
                return_type: ret_type,
                body: const2,
                has_self: false,
                receiver_mode: 0,
            },
            span: Span::new(20, 35),
        });
        rir2.add_function_span(FunctionSpan::new(helper_name, body_start2, decl2));

        let merged = Rir::merge(&[rir1, rir2]);

        // Check we have 2 function spans
        assert_eq!(merged.function_spans().len(), 2);

        // Check main function span (from rir1, indices unchanged)
        let main_span = &merged.function_spans()[0];
        assert_eq!(main_span.name, main_name);
        assert_eq!(main_span.body_start.as_u32(), 0);
        assert_eq!(main_span.decl.as_u32(), 1);

        // Check helper function span (from rir2, indices shifted by 2)
        let helper_span = &merged.function_spans()[1];
        assert_eq!(helper_span.name, helper_name);
        assert_eq!(helper_span.body_start.as_u32(), 2); // Was 0, now 0 + 2 = 2
        assert_eq!(helper_span.decl.as_u32(), 3); // Was 1, now 1 + 2 = 3
    }

    #[test]
    fn test_merge_three_rirs() {
        let mut rir1 = Rir::new();
        rir1.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });

        let mut rir2 = Rir::new();
        rir2.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(10, 11),
        });

        let mut rir3 = Rir::new();
        rir3.add_inst(Inst {
            data: InstData::IntConst(3),
            span: Span::new(20, 21),
        });

        let merged = Rir::merge(&[rir1, rir2, rir3]);
        assert_eq!(merged.len(), 3);

        assert!(matches!(
            merged.get(InstRef::from_raw(0)).data,
            InstData::IntConst(1)
        ));
        assert!(matches!(
            merged.get(InstRef::from_raw(1)).data,
            InstData::IntConst(2)
        ));
        assert!(matches!(
            merged.get(InstRef::from_raw(2)).data,
            InstData::IntConst(3)
        ));
    }

    #[test]
    fn test_merge_preserves_spans() {
        let mut rir1 = Rir::new();
        rir1.add_inst(Inst {
            data: InstData::IntConst(1),
            span: Span::new(5, 10),
        });

        let mut rir2 = Rir::new();
        rir2.add_inst(Inst {
            data: InstData::IntConst(2),
            span: Span::new(100, 105),
        });

        let merged = Rir::merge(&[rir1, rir2]);

        // Spans should be preserved exactly
        assert_eq!(merged.get(InstRef::from_raw(0)).span, Span::new(5, 10));
        assert_eq!(merged.get(InstRef::from_raw(1)).span, Span::new(100, 105));
    }
}
