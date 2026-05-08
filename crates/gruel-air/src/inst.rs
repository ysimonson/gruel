//! AIR instruction definitions.
//!
//! Like RIR, instructions are stored densely and referenced by index.
//!
//! # Place Expressions (ADR-0030 Phase 8)
//!
//! Memory locations are represented using [`AirPlace`], which consists of:
//! - A base ([`AirPlaceBase`]): either a local variable slot or parameter slot
//! - A list of projections ([`AirProjection`]): field accesses and array indices
//!
//! This design follows Rust MIR's proven approach and eliminates redundant
//! Load instructions for nested access patterns like `arr[i].field`.

use std::fmt;

// Compile-time size assertions to prevent silent size growth during refactoring.
// These limits are set slightly above current sizes to allow minor changes,
// but will catch significant size regressions.
//
// Current sizes (as of 2025-12):
// - AirInst: 40 bytes (AirInstData + Type + Span)
// - AirInstData: 24 bytes
const _: () = assert!(std::mem::size_of::<AirInst>() <= 48);
const _: () = assert!(std::mem::size_of::<AirInstData>() <= 32);

use crate::types::{StructId, Type};
use gruel_util::{BinOp, Span, UnaryOp};
use lasso::{Key, Spur};

// ============================================================================
// Place Expressions (ADR-0030 Phase 8)
// ============================================================================

/// A reference to a place in AIR - stored as index into the places array.
///
/// This is a lightweight handle that can be copied and compared efficiently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct AirPlaceRef(u32);

impl AirPlaceRef {
    /// Create a new place reference from a raw index.
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

impl fmt::Display for AirPlaceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "place#{}", self.0)
    }
}

/// A memory location that can be read from or written to.
///
/// A place represents a path to a memory location, consisting of a base
/// (local variable or parameter) and zero or more projections (field access,
/// array indexing).
///
/// # Examples
///
/// - `x` → `AirPlace { base: Local(0), projections_start: 0, projections_len: 0 }`
/// - `arr[i]` → `AirPlace { base: Local(0), ... }` with `Index` projection
/// - `point.x` → `AirPlace { base: Local(0), ... }` with `Field` projection
/// - `arr[i].x` → `AirPlace { base: Local(0), ... }` with `Index` then `Field`
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AirPlace {
    /// The base of the place - either a local slot or parameter slot
    pub base: AirPlaceBase,
    /// Start index into Air's projections array
    pub projections_start: u32,
    /// Number of projections
    pub projections_len: u32,
}

impl AirPlace {
    /// Create a place for a local variable with no projections.
    #[inline]
    pub const fn local(slot: u32) -> Self {
        Self {
            base: AirPlaceBase::Local(slot),
            projections_start: 0,
            projections_len: 0,
        }
    }

    /// Create a place for a parameter with no projections.
    #[inline]
    pub const fn param(slot: u32) -> Self {
        Self {
            base: AirPlaceBase::Param(slot),
            projections_start: 0,
            projections_len: 0,
        }
    }

    /// Returns true if this place has no projections (is just a variable).
    #[inline]
    pub const fn is_simple(&self) -> bool {
        self.projections_len == 0
    }

    /// Returns the local slot if this is a simple local place with no projections.
    #[inline]
    pub const fn as_local(&self) -> Option<u32> {
        if self.projections_len == 0 {
            match self.base {
                AirPlaceBase::Local(slot) => Some(slot),
                AirPlaceBase::Param(_) => None,
            }
        } else {
            None
        }
    }

    /// Returns the param slot if this is a simple param place with no projections.
    #[inline]
    pub const fn as_param(&self) -> Option<u32> {
        if self.projections_len == 0 {
            match self.base {
                AirPlaceBase::Param(slot) => Some(slot),
                AirPlaceBase::Local(_) => None,
            }
        } else {
            None
        }
    }
}

/// The base of a place - where the memory location starts.
///
/// Re-export of [`gruel_util::PlaceBase`] under the AIR-flavoured name to
/// avoid a needless rename at every call site.
pub use gruel_util::PlaceBase as AirPlaceBase;

/// A projection applied to a place to reach a nested location.
///
/// Projections are stored in `Air::projections` and referenced by
/// `AirPlace::projections_start` and `AirPlace::projections_len`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AirProjection {
    /// Field access: `.field_name`
    ///
    /// The struct_id identifies the struct type, and field_index is the
    /// 0-based index of the field in declaration order.
    Field {
        struct_id: StructId,
        field_index: u32,
    },
    /// Array index: `[index]`
    ///
    /// The array_type is needed for bounds checking and element size calculation.
    /// The index is an AirRef that will be evaluated at runtime.
    Index { array_type: Type, index: AirRef },
}

/// Parameter passing mode in AIR.
///
/// Mirrors [`gruel_rir::RirParamMode`]. `MutRef` / `Ref` are the
/// vestigial-but-used by-pointer markers that survive ADR-0076 for
/// parameters whose declared type cannot itself be wrapped as
/// `MutRef(...)` / `Ref(...)` in the type pool (notably interface params).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AirParamMode {
    /// Normal pass-by-value parameter (or any reference whose ref-ness is
    /// already encoded in the parameter `Type`).
    #[default]
    Normal,
    /// Exclusive mutable borrow (interface-by-pointer ABI).
    MutRef,
    /// Shared immutable borrow (interface-by-pointer ABI).
    Ref,
}

impl AirParamMode {
    /// Returns true if the parameter is passed by reference per the legacy
    /// mode mechanism. Type-driven `Ref(T)` / `MutRef(T)` parameters are
    /// not detected here; callers must additionally inspect the parameter
    /// `Type`.
    #[inline]
    pub fn is_by_ref(self) -> bool {
        matches!(self, AirParamMode::MutRef | AirParamMode::Ref)
    }

    /// Returns true if the parameter is an exclusive mutable borrow per
    /// the legacy mode mechanism.
    #[inline]
    pub fn is_mut_ref(self) -> bool {
        matches!(self, AirParamMode::MutRef)
    }

    /// Returns true if the parameter is a shared immutable borrow per the
    /// legacy mode mechanism.
    #[inline]
    pub fn is_ref(self) -> bool {
        matches!(self, AirParamMode::Ref)
    }
}

impl From<gruel_rir::RirParamMode> for AirParamMode {
    fn from(mode: gruel_rir::RirParamMode) -> Self {
        match mode {
            gruel_rir::RirParamMode::MutRef => AirParamMode::MutRef,
            gruel_rir::RirParamMode::Ref => AirParamMode::Ref,
            // Comptime params are erased by the time they reach codegen;
            // treat them as normal pass-by-value.
            gruel_rir::RirParamMode::Normal | gruel_rir::RirParamMode::Comptime => {
                AirParamMode::Normal
            }
        }
    }
}

/// Argument passing mode in AIR. Mirrors [`AirParamMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AirArgMode {
    /// Normal pass-by-value argument.
    #[default]
    Normal,
    /// Exclusive mutable reborrow (by-pointer ABI).
    MutRef,
    /// Shared immutable reborrow (by-pointer ABI).
    Ref,
}

impl AirArgMode {
    /// Convert to u32 for storage in extra array.
    #[inline]
    pub fn as_u32(self) -> u32 {
        match self {
            AirArgMode::Normal => 0,
            AirArgMode::MutRef => 1,
            AirArgMode::Ref => 2,
        }
    }

    /// Convert from u32 stored in extra array.
    #[inline]
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => AirArgMode::Normal,
            1 => AirArgMode::MutRef,
            2 => AirArgMode::Ref,
            _ => panic!("invalid AirArgMode value: {}", v),
        }
    }
}

impl From<gruel_rir::RirArgMode> for AirArgMode {
    fn from(mode: gruel_rir::RirArgMode) -> Self {
        match mode {
            gruel_rir::RirArgMode::Normal => AirArgMode::Normal,
            gruel_rir::RirArgMode::MutRef => AirArgMode::MutRef,
            gruel_rir::RirArgMode::Ref => AirArgMode::Ref,
        }
    }
}

/// An argument in a function call (AIR level).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AirCallArg {
    /// The argument expression
    pub value: AirRef,
    /// The passing mode for this argument
    pub mode: AirArgMode,
}

impl AirCallArg {
    /// Returns true if this argument is passed as an exclusive mutable
    /// reborrow per the legacy mode mechanism.
    pub fn is_mut_ref(&self) -> bool {
        self.mode == AirArgMode::MutRef
    }

    /// Returns true if this argument is passed as a shared immutable
    /// reborrow per the legacy mode mechanism.
    pub fn is_ref(&self) -> bool {
        self.mode == AirArgMode::Ref
    }
}

impl fmt::Display for AirCallArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.mode {
            AirArgMode::MutRef => write!(f, "mut_ref {}", self.value),
            AirArgMode::Ref => write!(f, "ref {}", self.value),
            AirArgMode::Normal => write!(f, "{}", self.value),
        }
    }
}

/// A pattern in a match expression (AIR level - typed).
///
/// Recursive shape introduced by ADR-0051. `Wildcard`, `Int`, `Bool`, and
/// `EnumVariant` are the flat variants produced by the pre-ADR-0051 sema
/// path; `Bind`, `Tuple`, `Struct`, `EnumDataVariant`, `EnumStructVariant`,
/// and `EnumUnitVariant` are the recursive variants produced by the new
/// lowering. During Phases 1-3 both encodings coexist; Phase 4 drops the
/// flat `EnumVariant`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AirPattern {
    /// Wildcard pattern `_` - matches anything, binds nothing.
    Wildcard,
    /// Integer literal pattern (can be positive or negative).
    Int(i64),
    /// Boolean literal pattern.
    Bool(bool),
    /// Legacy flat enum-variant pattern produced by the pre-ADR-0051
    /// lowering path. Kept for backward compatibility until Phase 4.
    EnumVariant {
        enum_id: crate::types::EnumId,
        variant_index: u32,
    },
    /// Bind the scrutinee (or its projection) to a local. `inner` is the
    /// sub-pattern applied to the same value; `None` is a bare binding,
    /// `Some(p)` is `name @ p` (not yet exposed in surface syntax).
    Bind {
        name: lasso::Spur,
        is_mut: bool,
        inner: Option<Box<AirPattern>>,
    },
    /// Tuple dispatch; `elems[i]` applies to projection `i`.
    Tuple { elems: Vec<AirPattern> },
    /// Named-struct dispatch. Unlisted fields (from `..`) become explicit
    /// `Wildcard` entries at sema lowering time.
    Struct {
        struct_id: StructId,
        fields: Vec<(u32, AirPattern)>,
    },
    /// Enum data-variant dispatch. `fields[i]` applies to positional
    /// field `i` of the variant.
    EnumDataVariant {
        enum_id: crate::types::EnumId,
        variant_index: u32,
        fields: Vec<AirPattern>,
    },
    /// Enum struct-variant dispatch. Same shape as `Struct` but tagged by
    /// variant.
    EnumStructVariant {
        enum_id: crate::types::EnumId,
        variant_index: u32,
        fields: Vec<(u32, AirPattern)>,
    },
    /// Unit enum variant. Recursive-lowering counterpart of the legacy
    /// `EnumVariant`.
    EnumUnitVariant {
        enum_id: crate::types::EnumId,
        variant_index: u32,
    },
}

/// Pattern type tags for extra array encoding.
const PATTERN_WILDCARD: u32 = 0;
const PATTERN_INT: u32 = 1;
const PATTERN_BOOL: u32 = 2;
const PATTERN_ENUM_VARIANT: u32 = 3;
const PATTERN_BIND: u32 = 4;
const PATTERN_TUPLE: u32 = 5;
const PATTERN_STRUCT: u32 = 6;
const PATTERN_ENUM_DATA: u32 = 7;
const PATTERN_ENUM_STRUCT: u32 = 8;
const PATTERN_ENUM_UNIT: u32 = 9;

impl AirPattern {
    /// Encode this arm as `[body_ref, ...pattern_tree]` appended to `out`.
    /// The pattern tree is self-describing: decoding consumes exactly the
    /// words this function produced (see `decode_pattern_tree`).
    pub fn encode(&self, body: AirRef, out: &mut Vec<u32>) {
        out.push(body.as_u32());
        encode_pattern_tree(self, out);
    }
}

fn encode_pattern_tree(pattern: &AirPattern, out: &mut Vec<u32>) {
    match pattern {
        AirPattern::Wildcard => out.push(PATTERN_WILDCARD),
        AirPattern::Int(n) => {
            out.push(PATTERN_INT);
            out.push(*n as u32);
            out.push((*n >> 32) as u32);
        }
        AirPattern::Bool(b) => {
            out.push(PATTERN_BOOL);
            out.push(if *b { 1 } else { 0 });
        }
        AirPattern::EnumVariant {
            enum_id,
            variant_index,
        } => {
            out.push(PATTERN_ENUM_VARIANT);
            out.push(enum_id.0);
            out.push(*variant_index);
        }
        AirPattern::Bind {
            name,
            is_mut,
            inner,
        } => {
            out.push(PATTERN_BIND);
            out.push(name.into_usize() as u32);
            let flags = (if *is_mut { 1u32 } else { 0 }) | (if inner.is_some() { 2u32 } else { 0 });
            out.push(flags);
            if let Some(inner) = inner {
                encode_pattern_tree(inner, out);
            }
        }
        AirPattern::Tuple { elems } => {
            out.push(PATTERN_TUPLE);
            out.push(elems.len() as u32);
            for e in elems {
                encode_pattern_tree(e, out);
            }
        }
        AirPattern::Struct { struct_id, fields } => {
            out.push(PATTERN_STRUCT);
            out.push(struct_id.0);
            out.push(fields.len() as u32);
            for (idx, p) in fields {
                out.push(*idx);
                encode_pattern_tree(p, out);
            }
        }
        AirPattern::EnumDataVariant {
            enum_id,
            variant_index,
            fields,
        } => {
            out.push(PATTERN_ENUM_DATA);
            out.push(enum_id.0);
            out.push(*variant_index);
            out.push(fields.len() as u32);
            for p in fields {
                encode_pattern_tree(p, out);
            }
        }
        AirPattern::EnumStructVariant {
            enum_id,
            variant_index,
            fields,
        } => {
            out.push(PATTERN_ENUM_STRUCT);
            out.push(enum_id.0);
            out.push(*variant_index);
            out.push(fields.len() as u32);
            for (idx, p) in fields {
                out.push(*idx);
                encode_pattern_tree(p, out);
            }
        }
        AirPattern::EnumUnitVariant {
            enum_id,
            variant_index,
        } => {
            out.push(PATTERN_ENUM_UNIT);
            out.push(enum_id.0);
            out.push(*variant_index);
        }
    }
}

/// Decode a single pattern tree from `data`, returning the pattern and
/// the number of u32s consumed.
fn decode_pattern_tree(data: &[u32]) -> (AirPattern, usize) {
    let tag = data[0];
    match tag {
        PATTERN_WILDCARD => (AirPattern::Wildcard, 1),
        PATTERN_INT => {
            let lo = data[1] as i64;
            let hi = (data[2] as i64) << 32;
            (AirPattern::Int(lo | hi), 3)
        }
        PATTERN_BOOL => (AirPattern::Bool(data[1] != 0), 2),
        PATTERN_ENUM_VARIANT => {
            let enum_id = crate::types::EnumId(data[1]);
            let variant_index = data[2];
            (
                AirPattern::EnumVariant {
                    enum_id,
                    variant_index,
                },
                3,
            )
        }
        PATTERN_BIND => {
            let name = lasso::Spur::try_from_usize(data[1] as usize)
                .expect("invalid Spur encoding in pattern");
            let flags = data[2];
            let is_mut = (flags & 1) != 0;
            let has_inner = (flags & 2) != 0;
            let mut offset = 3;
            let inner = if has_inner {
                let (p, consumed) = decode_pattern_tree(&data[offset..]);
                offset += consumed;
                Some(Box::new(p))
            } else {
                None
            };
            (
                AirPattern::Bind {
                    name,
                    is_mut,
                    inner,
                },
                offset,
            )
        }
        PATTERN_TUPLE => {
            let n = data[1] as usize;
            let mut offset = 2;
            let mut elems = Vec::with_capacity(n);
            for _ in 0..n {
                let (p, consumed) = decode_pattern_tree(&data[offset..]);
                elems.push(p);
                offset += consumed;
            }
            (AirPattern::Tuple { elems }, offset)
        }
        PATTERN_STRUCT => {
            let struct_id = StructId(data[1]);
            let n = data[2] as usize;
            let mut offset = 3;
            let mut fields = Vec::with_capacity(n);
            for _ in 0..n {
                let field_index = data[offset];
                offset += 1;
                let (p, consumed) = decode_pattern_tree(&data[offset..]);
                offset += consumed;
                fields.push((field_index, p));
            }
            (AirPattern::Struct { struct_id, fields }, offset)
        }
        PATTERN_ENUM_DATA => {
            let enum_id = crate::types::EnumId(data[1]);
            let variant_index = data[2];
            let n = data[3] as usize;
            let mut offset = 4;
            let mut fields = Vec::with_capacity(n);
            for _ in 0..n {
                let (p, consumed) = decode_pattern_tree(&data[offset..]);
                offset += consumed;
                fields.push(p);
            }
            (
                AirPattern::EnumDataVariant {
                    enum_id,
                    variant_index,
                    fields,
                },
                offset,
            )
        }
        PATTERN_ENUM_STRUCT => {
            let enum_id = crate::types::EnumId(data[1]);
            let variant_index = data[2];
            let n = data[3] as usize;
            let mut offset = 4;
            let mut fields = Vec::with_capacity(n);
            for _ in 0..n {
                let field_index = data[offset];
                offset += 1;
                let (p, consumed) = decode_pattern_tree(&data[offset..]);
                offset += consumed;
                fields.push((field_index, p));
            }
            (
                AirPattern::EnumStructVariant {
                    enum_id,
                    variant_index,
                    fields,
                },
                offset,
            )
        }
        PATTERN_ENUM_UNIT => {
            let enum_id = crate::types::EnumId(data[1]);
            let variant_index = data[2];
            (
                AirPattern::EnumUnitVariant {
                    enum_id,
                    variant_index,
                },
                3,
            )
        }
        _ => panic!("invalid pattern tag: {}", tag),
    }
}

/// Iterator for reading match arms from the extra array.
pub struct MatchArmIterator<'a> {
    data: &'a [u32],
    remaining: usize,
}

impl Iterator for MatchArmIterator<'_> {
    type Item = (AirPattern, AirRef);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;

        let body = AirRef::from_raw(self.data[0]);
        let (pattern, consumed) = decode_pattern_tree(&self.data[1..]);
        self.data = &self.data[1 + consumed..];
        Some((pattern, body))
    }
}

impl fmt::Display for AirPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AirPattern::Wildcard => write!(f, "_"),
            AirPattern::Int(n) => write!(f, "{}", n),
            AirPattern::Bool(b) => write!(f, "{}", b),
            AirPattern::EnumVariant {
                enum_id,
                variant_index,
            }
            | AirPattern::EnumUnitVariant {
                enum_id,
                variant_index,
            } => write!(f, "enum#{}::{}", enum_id.0, variant_index),
            AirPattern::Bind {
                name,
                is_mut,
                inner,
            } => {
                if *is_mut {
                    write!(f, "mut ")?;
                }
                write!(f, "${}", name.into_usize())?;
                if let Some(inner) = inner {
                    write!(f, " @ {}", inner)?;
                }
                Ok(())
            }
            AirPattern::Tuple { elems } => {
                write!(f, "(")?;
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", e)?;
                }
                write!(f, ")")
            }
            AirPattern::Struct { struct_id, fields } => {
                write!(f, "struct#{} {{ ", struct_id.0)?;
                for (i, (idx, p)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, ".{}: {}", idx, p)?;
                }
                write!(f, " }}")
            }
            AirPattern::EnumDataVariant {
                enum_id,
                variant_index,
                fields,
            } => {
                write!(f, "enum#{}::{}(", enum_id.0, variant_index)?;
                for (i, p) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p)?;
                }
                write!(f, ")")
            }
            AirPattern::EnumStructVariant {
                enum_id,
                variant_index,
                fields,
            } => {
                write!(f, "enum#{}::{} {{ ", enum_id.0, variant_index)?;
                for (i, (idx, p)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, ".{}: {}", idx, p)?;
                }
                write!(f, " }}")
            }
        }
    }
}

/// A reference to an instruction in the AIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct AirRef(u32);

impl AirRef {
    #[inline]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// The complete AIR for a function.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Air {
    instructions: Vec<AirInst>,
    /// Extra data for variable-length instruction payloads (args, elements, etc.)
    extra: Vec<u32>,
    /// The return type of this function
    return_type: Type,
    /// Storage for place projections (ADR-0030 Phase 8).
    /// AirPlace instructions store (start, len) indices into this array.
    projections: Vec<AirProjection>,
    /// Storage for places (ADR-0030 Phase 8).
    /// AirPlaceRef values are indices into this array.
    places: Vec<AirPlace>,
    /// Comptime value arguments captured at each `CallGeneric` site, keyed by
    /// the `CallGeneric` instruction's index. Populated when the call has
    /// `comptime n: i32` (or other value comptime) parameters; the
    /// specialization pass reads these to build a unique
    /// `(name, type_args, value_args)` key per call so per-call `comptime
    /// if`/`@compile_error` checks fire only for the values they apply to.
    #[serde(default)]
    comptime_value_args: rustc_hash::FxHashMap<u32, Vec<crate::sema::ConstValue>>,
}

impl Air {
    /// Create a new empty AIR.
    pub fn new(return_type: Type) -> Self {
        Self {
            instructions: Vec::new(),
            extra: Vec::new(),
            return_type,
            projections: Vec::new(),
            places: Vec::new(),
            comptime_value_args: rustc_hash::FxHashMap::default(),
        }
    }

    /// Record the comptime value arguments captured at a `CallGeneric` site.
    /// Indexed by the `CallGeneric` instruction's index in `instructions`.
    pub fn set_comptime_value_args(
        &mut self,
        inst_index: u32,
        value_args: Vec<crate::sema::ConstValue>,
    ) {
        if !value_args.is_empty() {
            self.comptime_value_args.insert(inst_index, value_args);
        }
    }

    /// Retrieve the comptime value arguments captured at the given
    /// `CallGeneric` site, or an empty slice if none were recorded.
    pub fn comptime_value_args(&self, inst_index: u32) -> &[crate::sema::ConstValue] {
        self.comptime_value_args
            .get(&inst_index)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Add an instruction and return its reference.
    pub fn add_inst(&mut self, inst: AirInst) -> AirRef {
        let index = self.instructions.len() as u32;
        self.instructions.push(inst);
        AirRef::from_raw(index)
    }

    /// Get an instruction by reference.
    #[inline]
    pub fn get(&self, inst_ref: AirRef) -> &AirInst {
        &self.instructions[inst_ref.0 as usize]
    }

    /// The return type of this function.
    #[inline]
    pub fn return_type(&self) -> Type {
        self.return_type
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
    pub fn iter(&self) -> impl Iterator<Item = (AirRef, &AirInst)> {
        self.instructions
            .iter()
            .enumerate()
            .map(|(i, inst)| (AirRef::from_raw(i as u32), inst))
    }

    /// Add extra data and return the start index.
    pub fn add_extra(&mut self, data: &[u32]) -> u32 {
        // Debug assertions for u32 overflow
        debug_assert!(
            self.extra.len() <= u32::MAX as usize,
            "AIR extra data overflow: {} entries exceeds u32::MAX",
            self.extra.len()
        );
        debug_assert!(
            self.extra.len().saturating_add(data.len()) <= u32::MAX as usize,
            "AIR extra data would overflow: {} + {} exceeds u32::MAX",
            self.extra.len(),
            data.len()
        );

        let start = self.extra.len() as u32;
        self.extra.extend_from_slice(data);
        start
    }

    /// Get extra data slice by start index and length.
    #[inline]
    pub fn get_extra(&self, start: u32, len: u32) -> &[u32] {
        let start = start as usize;
        let end = start + len as usize;
        &self.extra[start..end]
    }

    // Helper methods for reading structured data from extra array

    /// Get AirRefs from extra array (for blocks, array elements, intrinsic args, etc.).
    #[inline]
    pub fn get_air_refs(&self, start: u32, len: u32) -> impl Iterator<Item = AirRef> + '_ {
        self.get_extra(start, len)
            .iter()
            .map(|&v| AirRef::from_raw(v))
    }

    /// Get call arguments from extra array.
    /// Each call arg is encoded as 2 u32s: (air_ref, mode).
    #[inline]
    pub fn get_call_args(&self, start: u32, len: u32) -> impl Iterator<Item = AirCallArg> + '_ {
        let data = self.get_extra(start, len * 2);
        data.chunks_exact(2).map(|chunk| AirCallArg {
            value: AirRef::from_raw(chunk[0]),
            mode: AirArgMode::from_u32(chunk[1]),
        })
    }

    /// Get match arms from extra array.
    /// Each match arm is encoded based on pattern type plus the body AirRef.
    #[inline]
    pub fn get_match_arms(
        &self,
        start: u32,
        len: u32,
    ) -> impl Iterator<Item = (AirPattern, AirRef)> + '_ {
        MatchArmIterator {
            data: &self.extra[start as usize..],
            remaining: len as usize,
        }
    }

    /// Get struct init data from extra array.
    /// Returns (field_refs_iterator, source_order_iterator).
    #[inline]
    pub fn get_struct_init(
        &self,
        fields_start: u32,
        fields_len: u32,
        source_order_start: u32,
    ) -> (
        impl Iterator<Item = AirRef> + '_,
        impl Iterator<Item = usize> + '_,
    ) {
        let fields = self.get_air_refs(fields_start, fields_len);
        let source_order = self
            .get_extra(source_order_start, fields_len)
            .iter()
            .map(|&v| v as usize);
        (fields, source_order)
    }

    /// Remap string constant IDs using the provided mapping function.
    ///
    /// This is used after parallel function analysis to convert local string IDs
    /// (per-function) to global string IDs (across all functions). The mapping
    /// function takes a local string ID and returns the global string ID.
    pub fn remap_string_ids<F>(&mut self, map_fn: F)
    where
        F: Fn(u32) -> u32,
    {
        for inst in &mut self.instructions {
            if let AirInstData::StringConst(ref mut id) = inst.data {
                *id = map_fn(*id);
            }
        }
    }

    /// Remap byte-blob IDs after merging per-function bytes pools into a
    /// single global pool (mirrors `remap_string_ids`).
    pub fn remap_bytes_ids<F>(&mut self, map_fn: F)
    where
        F: Fn(u32) -> u32,
    {
        for inst in &mut self.instructions {
            if let AirInstData::BytesConst(ref mut id) = inst.data {
                *id = map_fn(*id);
            }
        }
    }

    /// Get a reference to all instructions.
    #[inline]
    pub fn instructions(&self) -> &[AirInst] {
        &self.instructions
    }

    /// Rewrite the data of an instruction at a given index.
    ///
    /// This is used by the specialization pass to rewrite `CallGeneric` to `Call`.
    /// The type and span are preserved.
    pub fn rewrite_inst_data(&mut self, index: usize, new_data: AirInstData) {
        self.instructions[index].data = new_data;
    }

    // ========================================================================
    // Place operations (ADR-0030 Phase 8)
    // ========================================================================

    /// Add projections to the projections array and return (start, len).
    ///
    /// Used for PlaceRead and PlaceWrite instructions.
    pub fn push_projections(
        &mut self,
        projs: impl IntoIterator<Item = AirProjection>,
    ) -> (u32, u32) {
        let start = self.projections.len() as u32;
        self.projections.extend(projs);
        let len = self.projections.len() as u32 - start;
        (start, len)
    }

    /// Get a slice from the projections array.
    #[inline]
    pub fn get_projections(&self, start: u32, len: u32) -> &[AirProjection] {
        &self.projections[start as usize..(start + len) as usize]
    }

    /// Get projections for a place.
    #[inline]
    pub fn get_place_projections(&self, place: &AirPlace) -> &[AirProjection] {
        self.get_projections(place.projections_start, place.projections_len)
    }

    /// Create a place with the given base and projections.
    ///
    /// This adds the projections to the projections array and returns a PlaceRef
    /// that references the place.
    pub fn make_place(
        &mut self,
        base: AirPlaceBase,
        projs: impl IntoIterator<Item = AirProjection>,
    ) -> AirPlaceRef {
        let (projections_start, projections_len) = self.push_projections(projs);
        let place = AirPlace {
            base,
            projections_start,
            projections_len,
        };
        let index = self.places.len() as u32;
        self.places.push(place);
        AirPlaceRef::from_raw(index)
    }

    /// Get a place by reference.
    #[inline]
    pub fn get_place(&self, place_ref: AirPlaceRef) -> &AirPlace {
        &self.places[place_ref.0 as usize]
    }

    /// Get all places.
    #[inline]
    pub fn places(&self) -> &[AirPlace] {
        &self.places
    }

    /// Get all projections.
    #[inline]
    pub fn projections(&self) -> &[AirProjection] {
        &self.projections
    }
}

/// A single AIR instruction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AirInst {
    pub data: AirInstData,
    pub ty: Type,
    pub span: Span,
}

/// AIR instruction data - fully typed operations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AirInstData {
    /// Integer constant (typed)
    Const(u64),

    /// Floating-point constant, stored as f64 bits via `f64::to_bits()`.
    FloatConst(u64),

    /// Boolean constant
    BoolConst(bool),

    /// String constant (index into string table)
    StringConst(u32),

    /// Byte-blob constant (index into the bytes table). Typed as `Slice(u8)`;
    /// the slice borrows from a binary-baked global at codegen.
    BytesConst(u32),

    /// Unit constant
    UnitConst,

    /// Type constant - a compile-time type value.
    /// This is used for comptime type parameters (e.g., passing `i32` to `fn foo(comptime T: type)`).
    /// The contained Type is the type being passed as a value.
    /// This instruction has type `Type::COMPTIME_TYPE` and is erased during specialization.
    TypeConst(crate::Type),

    /// Binary operation: arithmetic, comparison, logical, or bitwise.
    /// Logical `And`/`Or` are short-circuiting; they are lowered to
    /// control flow during CFG construction.
    Bin(BinOp, AirRef, AirRef),

    /// Unary operation: `-`, `!`, or `~`.
    Unary(UnaryOp, AirRef),

    /// Reference construction (ADR-0062): `&x` (`is_mut = false`) or
    /// `&mut x` (`is_mut = true`). Operand must be an lvalue. Lowers to
    /// the address of the operand's slot.
    MakeRef { operand: AirRef, is_mut: bool },

    /// Slice construction by borrow over a range subscript (ADR-0064).
    ///
    /// Lowered from `&arr[range]` / `&mut arr[range]`. `base` must
    /// designate an array place. `lo` defaults to `0`, `hi` defaults to
    /// `arr.len()` when absent.
    MakeSlice {
        base: AirRef,
        lo: Option<AirRef>,
        hi: Option<AirRef>,
        is_mut: bool,
    },

    // Control flow
    /// Conditional branch
    Branch {
        cond: AirRef,
        then_value: AirRef,
        else_value: Option<AirRef>,
    },

    /// While loop
    Loop { cond: AirRef, body: AirRef },

    /// Infinite loop (produces Never type)
    InfiniteLoop { body: AirRef },

    /// Match expression
    Match {
        /// The value being matched (scrutinee)
        scrutinee: AirRef,
        /// Start index into extra array for match arms
        arms_start: u32,
        /// Number of match arms
        arms_len: u32,
    },

    /// Break: exits the innermost loop
    Break,

    /// Continue: jumps to the next iteration of the innermost loop
    Continue,

    // Variable operations
    /// Allocate local variable with initial value
    /// Returns the slot index
    Alloc {
        /// Local variable slot index (0, 1, 2, ...)
        slot: u32,
        /// Initial value
        init: AirRef,
    },

    /// Load value from local variable
    Load {
        /// Local variable slot index
        slot: u32,
    },

    /// Store value to local variable
    Store {
        /// Local variable slot index
        slot: u32,
        /// Value to store
        value: AirRef,
        /// True if the slot held a live (non-moved) value before this assignment.
        /// When true, the old value must be dropped before the new value is written.
        /// When false (value was moved or this is initial allocation), no drop is needed.
        had_live_value: bool,
    },

    /// Store value to a parameter (for inout params)
    ParamStore {
        /// Parameter's ABI slot (relative to params, not locals)
        param_slot: u32,
        /// Value to store
        value: AirRef,
    },

    /// Bare-name write-through for a `MutRef(T)`-typed local binding
    /// (ADR-0076 Phase 3). Loads the pointer held in the local's slot and
    /// stores `value` (typed as the referent `T`) through that pointer.
    RefStore {
        /// Slot index of the local (whose stored value is the pointer)
        slot: u32,
        /// Value to store at the pointee
        value: AirRef,
    },

    /// Return from function (None for `return;` in unit-returning functions)
    Ret(Option<AirRef>),

    /// Function call
    Call {
        /// Function name (interned symbol)
        name: Spur,
        /// Start index into extra array for arguments
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Generic function call - requires specialization before codegen.
    ///
    /// This is emitted when calling a function with `comptime T: type` parameters.
    /// During a post-analysis specialization pass, this is rewritten to a regular
    /// `Call` to a specialized version of the function (e.g., `identity__i32`).
    ///
    /// The type_args are encoded in the extra array as raw Type discriminant values.
    /// The runtime args (non-comptime) are also in the extra array, after type_args.
    CallGeneric {
        /// Base function name (interned symbol)
        name: Spur,
        /// Start index into extra array for type arguments (raw Type values)
        type_args_start: u32,
        /// Number of type arguments
        type_args_len: u32,
        /// Start index into extra array for runtime arguments
        args_start: u32,
        /// Number of runtime arguments
        args_len: u32,
    },

    /// Intrinsic call (e.g., @dbg)
    Intrinsic {
        /// Intrinsic name (without @, interned)
        name: Spur,
        /// Start index into extra array for arguments
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Reference to a function parameter
    Param {
        /// Parameter index (0-based)
        index: u32,
    },

    /// Block expression with statements and final value.
    /// Used to group side-effect statements with their result value,
    /// enabling demand-driven lowering for short-circuit evaluation.
    Block {
        /// Start index into extra array for statement refs
        stmts_start: u32,
        /// Number of statements
        stmts_len: u32,
        /// The block's resulting value
        value: AirRef,
    },

    // Struct operations
    /// Create a new struct instance with initialized fields
    StructInit {
        /// The struct type being created
        struct_id: StructId,
        /// Start index into extra array for field refs (in declaration order)
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
        /// Start index into extra array for source order indices
        /// Each entry is an index into fields, specifying evaluation order
        source_order_start: u32,
    },

    /// Load a field from a struct value
    FieldGet {
        /// The struct value
        base: AirRef,
        /// The struct type
        struct_id: StructId,
        /// Field index (0-based, in declaration order)
        field_index: u32,
    },

    /// Store a value to a struct field (for local variables)
    FieldSet {
        /// The struct variable slot
        slot: u32,
        /// The struct type
        struct_id: StructId,
        /// Field index (0-based, in declaration order)
        field_index: u32,
        /// Value to store
        value: AirRef,
    },

    /// Store a value to a struct field (for parameters, including inout)
    ParamFieldSet {
        /// The parameter's ABI slot (relative to params, not locals)
        param_slot: u32,
        /// Offset within the struct for nested field access (e.g., p.inner.x)
        inner_offset: u32,
        /// The struct type containing the field being set
        struct_id: StructId,
        /// Field index (0-based, in declaration order)
        field_index: u32,
        /// Value to store
        value: AirRef,
    },

    // Array operations
    /// Create a new array with initialized elements.
    /// The array type is stored in `AirInst.ty` as `Type::new_array(...)`.
    ArrayInit {
        /// Start index into extra array for element refs
        elems_start: u32,
        /// Number of elements
        elems_len: u32,
    },

    /// Load an element from an array.
    /// The array type is stored in `AirInst.ty`.
    IndexGet {
        /// The array value
        base: AirRef,
        /// The array type (for bounds checking and element size)
        array_type: Type,
        /// Index expression
        index: AirRef,
    },

    /// Store a value to an array element.
    /// The array type is stored in `AirInst.ty`.
    IndexSet {
        /// The array variable slot
        slot: u32,
        /// The array type (for bounds checking and element size)
        array_type: Type,
        /// Index expression
        index: AirRef,
        /// Value to store
        value: AirRef,
    },

    /// Store a value to an array element of an inout parameter.
    /// The array type is stored in `AirInst.ty`.
    ParamIndexSet {
        /// The parameter's ABI slot (relative to params, not locals)
        param_slot: u32,
        /// The array type (for bounds checking and element size)
        array_type: Type,
        /// Index expression
        index: AirRef,
        /// Value to store
        value: AirRef,
    },

    // Place operations (ADR-0030 Phase 8)
    /// Read a value from a memory location.
    ///
    /// This unifies Load, IndexGet, and FieldGet into a single instruction
    /// that can handle arbitrarily nested access patterns like `arr[i].field`.
    /// Eventually, the separate FieldGet/IndexGet instructions will be removed.
    PlaceRead {
        /// Reference to the place to read from
        place: AirPlaceRef,
    },

    /// Write a value to a memory location.
    ///
    /// This unifies Store, IndexSet, ParamIndexSet, FieldSet, and ParamFieldSet
    /// into a single instruction that can handle nested writes.
    /// Eventually, the separate *Set instructions will be removed.
    PlaceWrite {
        /// Reference to the place to write to
        place: AirPlaceRef,
        /// Value to write
        value: AirRef,
    },

    // Enum operations
    /// Create an enum variant value (unit variant or any variant of a unit-only enum)
    EnumVariant {
        /// The enum type ID
        enum_id: crate::types::EnumId,
        /// The variant index (0-based)
        variant_index: u32,
    },

    /// Create a data enum variant value with associated field values.
    /// Used when the enum has at least one data variant.
    /// Field AirRefs are stored in the extra array at [fields_start..fields_start+fields_len].
    EnumCreate {
        /// The enum type ID
        enum_id: crate::types::EnumId,
        /// The variant index (0-based)
        variant_index: u32,
        /// Start index into extra array for field values
        fields_start: u32,
        /// Number of field values
        fields_len: u32,
    },

    /// Extract a field value from an enum variant's payload.
    /// Used in data variant match arm bodies to bind pattern variables.
    EnumPayloadGet {
        /// The enum value to extract from
        base: AirRef,
        /// The variant index (must match the enclosing arm's pattern)
        variant_index: u32,
        /// The field index within the variant
        field_index: u32,
    },

    // Type conversion operations
    /// Integer cast: convert between integer types with runtime range check.
    /// Panics if the value cannot be represented in the target type.
    /// The target type is stored in AirInst.ty.
    IntCast {
        /// The value to cast
        value: AirRef,
        /// The source type (for determining signedness and size)
        from_ty: Type,
    },

    /// Float cast: convert between floating-point types (fptrunc/fpext).
    /// The target type is stored in AirInst.ty.
    FloatCast {
        /// The value to cast
        value: AirRef,
        /// The source float type
        from_ty: Type,
    },

    /// Integer to float conversion (sitofp/uitofp).
    /// The target type is stored in AirInst.ty.
    IntToFloat {
        /// The integer value to convert
        value: AirRef,
        /// The source integer type (for determining signedness)
        from_ty: Type,
    },

    /// Float to integer conversion (fptosi/fptoui) with runtime range check.
    /// Panics if the value is NaN or out of range of the target integer type.
    /// The target type is stored in AirInst.ty.
    FloatToInt {
        /// The float value to convert
        value: AirRef,
        /// The source float type
        from_ty: Type,
    },

    // Drop/destructor operations
    /// Drop a value, running its destructor if the type has one.
    /// For trivially droppable types, this is a no-op.
    /// The type is stored in the AirInst.ty field.
    Drop {
        /// The value to drop
        value: AirRef,
    },

    // Storage liveness operations (for drop elaboration)
    /// Marks that a local slot becomes live (storage allocated).
    /// Emitted when a variable binding is created.
    /// The type is stored in AirInst.ty for drop elaboration.
    StorageLive {
        /// The slot that becomes live
        slot: u32,
    },

    /// Marks that a local slot becomes dead (storage can be deallocated).
    /// Emitted at scope exit for variables declared in that scope.
    /// The type is stored in AirInst.ty for drop elaboration.
    /// Drop elaboration will insert a Drop before this if the type needs drop
    /// and the value wasn't moved.
    StorageDead {
        /// The slot that becomes dead
        slot: u32,
    },

    /// Coerce a concrete value to an interface fat pointer (ADR-0056).
    ///
    /// `value` is a place of concrete type `Foo`. The result is a fat
    /// pointer `(data: &Foo, vtable: &VTable_Foo_Iface)`. Codegen (Phase 4d)
    /// materializes the data pointer and the vtable global.
    ///
    /// The result type (`AirInst.ty`) is `Type::new_interface(interface_id)`.
    MakeInterfaceRef {
        /// The concrete value being coerced. Must be a place expression
        /// (parameter ref, var ref, etc.) — codegen takes its address.
        value: AirRef,
        /// The concrete struct ID of `value`. Used by codegen to locate the
        /// `(struct_id, interface_id)` vtable.
        struct_id: crate::types::StructId,
        /// The target interface.
        interface_id: crate::types::InterfaceId,
    },

    /// Dynamic-dispatch method call on an interface receiver (ADR-0056).
    ///
    /// `recv` is a value of type `Type::new_interface(iid)` (a fat pointer).
    /// Codegen loads function-pointer slot `slot` from `recv`'s vtable and
    /// calls it, passing `recv.data_ptr` as the receiver and the regular
    /// `args` afterwards.
    ///
    /// The result type is the interface method's declared return type.
    MethodCallDyn {
        /// The interface being dispatched on (used to type the vtable).
        interface_id: crate::types::InterfaceId,
        /// The vtable slot index (corresponds to the method's declaration
        /// order in the interface).
        slot: u32,
        /// The fat-pointer receiver (must have type `Type::new_interface(iid)`).
        recv: AirRef,
        /// Start of additional args (excluding the receiver) in the extra
        /// array. Encoded as `[value, mode]` pairs like ordinary `Call`.
        args_start: u32,
        /// Number of additional args.
        args_len: u32,
    },
}

impl fmt::Display for AirRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

impl fmt::Display for Air {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "air (return_type: {}) {{", self.return_type.name())?;
        for (inst_ref, inst) in self.iter() {
            write!(f, "    {} : {} = ", inst_ref, inst.ty.name())?;
            match &inst.data {
                AirInstData::Const(v) => writeln!(f, "const {}", v)?,
                AirInstData::FloatConst(bits) => writeln!(f, "const {}", f64::from_bits(*bits))?,
                AirInstData::BoolConst(v) => writeln!(f, "const {}", v)?,
                AirInstData::StringConst(idx) => writeln!(f, "string_const @{}", idx)?,
                AirInstData::BytesConst(idx) => writeln!(f, "bytes_const @{}", idx)?,
                AirInstData::UnitConst => writeln!(f, "const ()")?,
                AirInstData::TypeConst(ty) => writeln!(f, "type_const {}", ty.name())?,
                AirInstData::Bin(op, lhs, rhs) => writeln!(f, "{} {}, {}", op, lhs, rhs)?,
                AirInstData::Unary(op, operand) => writeln!(f, "{} {}", op, operand)?,
                AirInstData::MakeRef { operand, is_mut } => writeln!(
                    f,
                    "make_ref{} {}",
                    if *is_mut { "_mut" } else { "" },
                    operand
                )?,
                AirInstData::MakeSlice {
                    base,
                    lo,
                    hi,
                    is_mut,
                } => {
                    write!(
                        f,
                        "make_slice{} {}",
                        if *is_mut { "_mut" } else { "" },
                        base
                    )?;
                    if let Some(lo) = lo {
                        write!(f, ", lo={}", lo)?;
                    }
                    if let Some(hi) = hi {
                        write!(f, ", hi={}", hi)?;
                    }
                    writeln!(f)?;
                }
                AirInstData::Branch {
                    cond,
                    then_value,
                    else_value,
                } => {
                    if let Some(else_v) = else_value {
                        writeln!(f, "branch {}, {}, {}", cond, then_value, else_v)?
                    } else {
                        writeln!(f, "branch {}, {}", cond, then_value)?
                    }
                }
                AirInstData::Loop { cond, body } => writeln!(f, "loop {}, {}", cond, body)?,
                AirInstData::InfiniteLoop { body } => writeln!(f, "infinite_loop {}", body)?,
                AirInstData::Match {
                    scrutinee,
                    arms_start,
                    arms_len,
                } => {
                    write!(f, "match {} {{ ", scrutinee)?;
                    for (i, (pat, body)) in self.get_match_arms(*arms_start, *arms_len).enumerate()
                    {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{} => {}", pat, body)?;
                    }
                    writeln!(f, " }}")?;
                }
                AirInstData::Break => writeln!(f, "break")?,
                AirInstData::Continue => writeln!(f, "continue")?,
                AirInstData::Alloc { slot, init } => writeln!(f, "alloc ${} = {}", slot, init)?,
                AirInstData::Load { slot } => writeln!(f, "load ${}", slot)?,
                AirInstData::Store { slot, value, .. } => {
                    writeln!(f, "store ${} = {}", slot, value)?
                }
                AirInstData::ParamStore { param_slot, value } => {
                    writeln!(f, "param_store %{} = {}", param_slot, value)?
                }
                AirInstData::RefStore { slot, value } => {
                    writeln!(f, "ref_store ${} = {}", slot, value)?
                }
                AirInstData::Ret(inner) => {
                    if let Some(inner) = inner {
                        writeln!(f, "ret {}", inner)?
                    } else {
                        writeln!(f, "ret")?
                    }
                }
                AirInstData::Call {
                    name,
                    args_start,
                    args_len,
                } => {
                    write!(f, "call @{}(", name.into_usize())?;
                    for (i, arg) in self.get_call_args(*args_start, *args_len).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::CallGeneric {
                    name,
                    type_args_start,
                    type_args_len,
                    args_start,
                    args_len,
                } => {
                    write!(f, "call_generic @{}<", name.into_usize())?;
                    // Show type arguments
                    for i in 0..*type_args_len {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        let type_val = self.extra[(*type_args_start + i) as usize];
                        write!(f, "type#{}", type_val)?;
                    }
                    write!(f, ">(")?;
                    // Show runtime arguments
                    for (i, arg) in self.get_call_args(*args_start, *args_len).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::Intrinsic {
                    name,
                    args_start,
                    args_len,
                } => {
                    write!(f, "intrinsic @sym:{}(", name.into_usize())?;
                    for (i, arg) in self.get_air_refs(*args_start, *args_len).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::Param { index } => writeln!(f, "param {}", index)?,
                AirInstData::Block {
                    stmts_start,
                    stmts_len,
                    value,
                } => {
                    write!(f, "block [")?;
                    for (i, s) in self.get_air_refs(*stmts_start, *stmts_len).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", s)?;
                    }
                    writeln!(f, "], {}", value)?;
                }
                AirInstData::StructInit {
                    struct_id,
                    fields_start,
                    fields_len,
                    source_order_start,
                } => {
                    write!(f, "struct_init #{} {{", struct_id.0)?;
                    let (fields, source_order) =
                        self.get_struct_init(*fields_start, *fields_len, *source_order_start);
                    for (i, field) in fields.enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", field)?;
                    }
                    write!(f, "}} eval_order=[")?;
                    for (i, idx) in source_order.enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", idx)?;
                    }
                    writeln!(f, "]")?;
                }
                AirInstData::FieldGet {
                    base,
                    struct_id,
                    field_index,
                } => {
                    writeln!(f, "field_get {}.#{}.{}", base, struct_id.0, field_index)?;
                }
                AirInstData::FieldSet {
                    slot,
                    struct_id,
                    field_index,
                    value,
                } => {
                    writeln!(
                        f,
                        "field_set ${}.#{}.{} = {}",
                        slot, struct_id.0, field_index, value
                    )?;
                }
                AirInstData::ParamFieldSet {
                    param_slot,
                    inner_offset,
                    struct_id,
                    field_index,
                    value,
                } => {
                    writeln!(
                        f,
                        "param_field_set %{}+{}.#{}.{} = {}",
                        param_slot, inner_offset, struct_id.0, field_index, value
                    )?;
                }
                AirInstData::ArrayInit {
                    elems_start,
                    elems_len,
                } => {
                    write!(f, "array_init [")?;
                    for (i, elem) in self.get_air_refs(*elems_start, *elems_len).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", elem)?;
                    }
                    writeln!(f, "]")?;
                }
                AirInstData::IndexGet {
                    base,
                    array_type,
                    index,
                } => {
                    writeln!(f, "index_get {}({})[{}]", base, array_type.name(), index)?;
                }
                AirInstData::IndexSet {
                    slot,
                    array_type,
                    index,
                    value,
                } => {
                    writeln!(
                        f,
                        "index_set ${}({})[{}] = {}",
                        slot,
                        array_type.name(),
                        index,
                        value
                    )?;
                }
                AirInstData::ParamIndexSet {
                    param_slot,
                    array_type,
                    index,
                    value,
                } => {
                    writeln!(
                        f,
                        "param_index_set param{}({})[{}] = {}",
                        param_slot,
                        array_type.name(),
                        index,
                        value
                    )?;
                }
                AirInstData::PlaceRead { place } => {
                    write!(f, "place_read ")?;
                    self.fmt_place(f, *place)?;
                    writeln!(f)?;
                }
                AirInstData::PlaceWrite { place, value } => {
                    write!(f, "place_write ")?;
                    self.fmt_place(f, *place)?;
                    writeln!(f, " = {}", value)?;
                }
                AirInstData::EnumVariant {
                    enum_id,
                    variant_index,
                } => {
                    writeln!(f, "enum_variant #{}::{}", enum_id.0, variant_index)?;
                }
                AirInstData::EnumCreate {
                    enum_id,
                    variant_index,
                    fields_start,
                    fields_len,
                } => {
                    let field_strs: Vec<String> = self
                        .get_air_refs(*fields_start, *fields_len)
                        .map(|r| format!("{}", r))
                        .collect();
                    writeln!(
                        f,
                        "enum_create #{}::{}({})",
                        enum_id.0,
                        variant_index,
                        field_strs.join(", ")
                    )?;
                }
                AirInstData::EnumPayloadGet {
                    base,
                    variant_index,
                    field_index,
                } => {
                    writeln!(
                        f,
                        "enum_payload_get {} variant={} field={}",
                        base, variant_index, field_index
                    )?;
                }
                AirInstData::IntCast { value, from_ty } => {
                    writeln!(f, "intcast {} from {}", value, from_ty.name())?;
                }
                AirInstData::FloatCast { value, from_ty } => {
                    writeln!(f, "floatcast {} from {}", value, from_ty.name())?;
                }
                AirInstData::IntToFloat { value, from_ty } => {
                    writeln!(f, "int_to_float {} from {}", value, from_ty.name())?;
                }
                AirInstData::FloatToInt { value, from_ty } => {
                    writeln!(f, "float_to_int {} from {}", value, from_ty.name())?;
                }
                AirInstData::Drop { value } => {
                    writeln!(f, "drop {}", value)?;
                }
                AirInstData::StorageLive { slot } => {
                    writeln!(f, "storage_live ${}", slot)?;
                }
                AirInstData::StorageDead { slot } => {
                    writeln!(f, "storage_dead ${}", slot)?;
                }
                AirInstData::MakeInterfaceRef {
                    value,
                    struct_id,
                    interface_id,
                } => {
                    writeln!(
                        f,
                        "make_interface_ref {} (struct=#{}, iface=#{})",
                        value, struct_id.0, interface_id.0
                    )?;
                }
                AirInstData::MethodCallDyn {
                    interface_id,
                    slot,
                    recv,
                    args_len,
                    ..
                } => {
                    writeln!(
                        f,
                        "method_call_dyn iface=#{} slot={} recv={} (+{} args)",
                        interface_id.0, slot, recv, args_len
                    )?;
                }
            }
        }
        writeln!(f, "}}")
    }
}

impl Air {
    /// Format a place for display, showing the base and projections.
    fn fmt_place(&self, f: &mut fmt::Formatter<'_>, place_ref: AirPlaceRef) -> fmt::Result {
        let place = self.get_place(place_ref);

        // Write the base
        match place.base {
            AirPlaceBase::Local(slot) => write!(f, "${}", slot)?,
            AirPlaceBase::Param(slot) => write!(f, "param%{}", slot)?,
        }

        // Write the projections
        let projections = self.get_place_projections(place);
        for proj in projections {
            match proj {
                AirProjection::Field {
                    struct_id,
                    field_index,
                } => {
                    write!(f, ".#{}.{}", struct_id.0, field_index)?;
                }
                AirProjection::Index { array_type, index } => {
                    write!(f, "({})[{}]", array_type.name(), index)?;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_air_ref_size() {
        assert_eq!(std::mem::size_of::<AirRef>(), 4);
    }

    #[test]
    fn test_air_inst_size() {
        // Document actual sizes for future reference.
        // If this test fails, update the const assertions at the top of this file.
        let air_inst_size = std::mem::size_of::<AirInst>();
        let air_inst_data_size = std::mem::size_of::<AirInstData>();

        // These assertions document the current sizes.
        // If the layout changes, update both these values and the const assertions.
        assert!(
            air_inst_size <= 48,
            "AirInst grew beyond 48 bytes: {}",
            air_inst_size
        );
        assert!(
            air_inst_data_size <= 32,
            "AirInstData grew beyond 32 bytes: {}",
            air_inst_data_size
        );
    }

    #[test]
    fn test_add_and_get_inst() {
        let mut air = Air::new(Type::I32);
        let inst = AirInst {
            data: AirInstData::Const(42),
            ty: Type::I32,
            span: Span::new(0, 2),
        };
        let inst_ref = air.add_inst(inst);

        let retrieved = air.get(inst_ref);
        assert!(matches!(retrieved.data, AirInstData::Const(42)));
        assert_eq!(retrieved.ty, Type::I32);
    }

    // ADR-0051 Phase 1: round-trip encode/decode coverage for every
    // AirPattern shape, including nested recursive ones.
    mod pattern_encoding {
        use super::*;
        use crate::types::EnumId;
        use lasso::{Key, Spur};

        fn roundtrip(pattern: AirPattern) {
            let body = AirRef::from_raw(0xDEAD_BEEF);
            let mut buf = Vec::new();
            pattern.encode(body, &mut buf);
            let mut iter = MatchArmIterator {
                data: &buf,
                remaining: 1,
            };
            let (decoded, decoded_body) = iter.next().expect("one arm");
            assert_eq!(decoded_body.as_u32(), body.as_u32());
            assert!(iter.next().is_none(), "exactly one arm consumed");
            // Re-encode the decoded pattern and check bytes match.
            let mut buf2 = Vec::new();
            decoded.encode(body, &mut buf2);
            assert_eq!(buf, buf2, "round-trip differs; pattern = {:?}", pattern);
        }

        fn spur(n: usize) -> Spur {
            Spur::try_from_usize(n).unwrap()
        }

        #[test]
        fn wildcard() {
            roundtrip(AirPattern::Wildcard);
        }

        #[test]
        fn int_positive_and_negative() {
            roundtrip(AirPattern::Int(42));
            roundtrip(AirPattern::Int(-1));
            roundtrip(AirPattern::Int(i64::MIN));
            roundtrip(AirPattern::Int(i64::MAX));
        }

        #[test]
        fn bool_both() {
            roundtrip(AirPattern::Bool(true));
            roundtrip(AirPattern::Bool(false));
        }

        #[test]
        fn enum_variant_legacy() {
            roundtrip(AirPattern::EnumVariant {
                enum_id: EnumId(7),
                variant_index: 3,
            });
        }

        #[test]
        fn enum_unit_variant() {
            roundtrip(AirPattern::EnumUnitVariant {
                enum_id: EnumId(7),
                variant_index: 3,
            });
        }

        #[test]
        fn bind_bare() {
            roundtrip(AirPattern::Bind {
                name: spur(5),
                is_mut: false,
                inner: None,
            });
            roundtrip(AirPattern::Bind {
                name: spur(5),
                is_mut: true,
                inner: None,
            });
        }

        #[test]
        fn bind_at_inner() {
            roundtrip(AirPattern::Bind {
                name: spur(9),
                is_mut: false,
                inner: Some(Box::new(AirPattern::Int(7))),
            });
        }

        #[test]
        fn tuple_flat_and_nested() {
            roundtrip(AirPattern::Tuple {
                elems: vec![
                    AirPattern::Int(1),
                    AirPattern::Wildcard,
                    AirPattern::Bool(true),
                ],
            });
            roundtrip(AirPattern::Tuple {
                elems: vec![
                    AirPattern::Tuple {
                        elems: vec![AirPattern::Int(1), AirPattern::Int(2)],
                    },
                    AirPattern::Wildcard,
                ],
            });
        }

        #[test]
        fn struct_pattern() {
            roundtrip(AirPattern::Struct {
                struct_id: StructId(2),
                fields: vec![
                    (0, AirPattern::Int(1)),
                    (1, AirPattern::Wildcard),
                    (2, AirPattern::Bool(false)),
                ],
            });
        }

        #[test]
        fn enum_data_variant_nested() {
            // Some(Some(42))
            roundtrip(AirPattern::EnumDataVariant {
                enum_id: EnumId(1),
                variant_index: 0,
                fields: vec![AirPattern::EnumDataVariant {
                    enum_id: EnumId(1),
                    variant_index: 0,
                    fields: vec![AirPattern::Int(42)],
                }],
            });
        }

        #[test]
        fn enum_struct_variant_nested() {
            roundtrip(AirPattern::EnumStructVariant {
                enum_id: EnumId(3),
                variant_index: 1,
                fields: vec![
                    (0, AirPattern::Int(5)),
                    (
                        1,
                        AirPattern::Bind {
                            name: spur(11),
                            is_mut: false,
                            inner: None,
                        },
                    ),
                ],
            });
        }

        #[test]
        fn multiple_arms_round_trip() {
            // Ensure sequential decode tracks variable-width arms correctly.
            let arms = vec![
                (AirPattern::Int(1), AirRef::from_raw(10)),
                (
                    AirPattern::Tuple {
                        elems: vec![AirPattern::Int(1), AirPattern::Wildcard],
                    },
                    AirRef::from_raw(11),
                ),
                (AirPattern::Wildcard, AirRef::from_raw(12)),
                (
                    AirPattern::EnumDataVariant {
                        enum_id: EnumId(0),
                        variant_index: 0,
                        fields: vec![AirPattern::Bind {
                            name: spur(1),
                            is_mut: false,
                            inner: None,
                        }],
                    },
                    AirRef::from_raw(13),
                ),
            ];
            let mut buf = Vec::new();
            for (p, b) in &arms {
                p.encode(*b, &mut buf);
            }
            let iter = MatchArmIterator {
                data: &buf,
                remaining: arms.len(),
            };
            let decoded: Vec<_> = iter.collect();
            assert_eq!(decoded.len(), arms.len());
            for ((orig_p, orig_b), (dec_p, dec_b)) in arms.iter().zip(decoded.iter()) {
                assert_eq!(orig_b.as_u32(), dec_b.as_u32());
                // Compare by re-encoding (patterns are not PartialEq).
                let mut a = Vec::new();
                let mut b = Vec::new();
                orig_p.encode(AirRef::from_raw(0), &mut a);
                dec_p.encode(AirRef::from_raw(0), &mut b);
                assert_eq!(a, b);
            }
        }

        #[test]
        fn display_renders_surface_shapes() {
            let p = AirPattern::EnumDataVariant {
                enum_id: EnumId(2),
                variant_index: 0,
                fields: vec![AirPattern::EnumUnitVariant {
                    enum_id: EnumId(2),
                    variant_index: 1,
                }],
            };
            assert_eq!(format!("{}", p), "enum#2::0(enum#2::1)");

            let t = AirPattern::Tuple {
                elems: vec![AirPattern::Int(1), AirPattern::Wildcard],
            };
            assert_eq!(format!("{}", t), "(1, _)");
        }
    }
}
