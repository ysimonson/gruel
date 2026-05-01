//! Type layout abstraction (ADR-0069).
//!
//! Provides a single source of truth for size/alignment/niche information.
//! All in-tree callers that previously computed sizes or alignments ad-hoc
//! should consult [`layout_of`] instead.
//!
//! Phase 1 of the ADR: types and `layout_of` exist; `niches` is always empty.
//! Later phases populate niches and add the niche-encoded enum layout.

use crate::{EnumDef, Type, TypeInternPool, TypeKind};

/// A forbidden bit-pattern range within a value of some type.
///
/// Reading the `width` bytes at `offset` (interpreted as a little-endian
/// unsigned integer) from any valid value will never yield a value in
/// `[start, end]` (inclusive). Surrounding contexts (e.g. an enclosing enum)
/// can reuse those bit patterns to encode tag information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NicheRange {
    /// Byte offset within the type where the niche-bearing bytes live.
    pub offset: u32,
    /// Width of the niche-bearing region, in bytes.
    pub width: u8,
    /// Inclusive start of the forbidden range.
    pub start: u128,
    /// Inclusive end of the forbidden range.
    pub end: u128,
}

impl NicheRange {
    /// Number of forbidden bit patterns in this niche.
    pub fn count(&self) -> u128 {
        self.end - self.start + 1
    }

    /// Maximum value representable in `width` bytes (inclusive).
    pub fn max_for_width(width: u8) -> u128 {
        if width >= 16 {
            u128::MAX
        } else {
            (1u128 << (width as u32 * 8)) - 1
        }
    }
}

/// How an enum encodes its discriminant within its storage.
///
/// Returned by [`Layout::discriminant_strategy`] for enum types. The constructor
/// and match-dispatch in codegen consult this to decide where to read/write the
/// tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscriminantStrategy {
    /// Standard tagged union: discriminant lives in its own slot at
    /// `tag_offset`, with payload following at `payload_offset`.
    ///
    /// In LLVM this is the `{ discrim, [N x i8] }` struct shape used today
    /// (`tag_offset = 0`, `payload_offset = tag_width`).
    Separate {
        /// Byte offset of the discriminant tag within the enum's storage.
        tag_offset: u32,
        /// Width of the discriminant tag in bytes.
        tag_width: u8,
        /// Byte offset where variant payload bytes start.
        payload_offset: u32,
    },
    /// Niche-encoded: no separate discriminant slot. The unit variant is
    /// identified by reading `niche_width` bytes at `niche_offset` and seeing
    /// `niche_value`; any other bit pattern means the data variant.
    ///
    /// Reserved for Phase 5+ of ADR-0069.
    Niche {
        /// Index of the (single) unit variant.
        unit_variant: u32,
        /// Index of the (single) data variant.
        data_variant: u32,
        /// Byte offset of the niche bytes within the enum's storage.
        niche_offset: u32,
        /// Niche width in bytes.
        niche_width: u8,
        /// Bit pattern (little-endian) that encodes the unit variant.
        niche_value: u128,
    },
}

/// The layout of a Gruel type: size, alignment, niches, and (for enums) its
/// discriminant strategy.
///
/// `Layout` is a pure function of the type. Because types are interned, the
/// pool caches the result of [`layout_of`] keyed by [`Type`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// ABI size in bytes (matches LLVM `store` width).
    pub size: u64,
    /// ABI alignment in bytes.
    pub align: u64,
    /// Forbidden bit-pattern ranges within a value of this type.
    pub niches: Vec<NicheRange>,
    /// For enum types: how the discriminant is encoded. `None` for non-enums.
    pub discriminant: Option<DiscriminantStrategy>,
}

impl Layout {
    /// A layout with the given size and alignment, no niches, no enum repr.
    pub fn scalar(size: u64, align: u64) -> Self {
        Self {
            size,
            align,
            niches: Vec::new(),
            discriminant: None,
        }
    }

    /// A zero-sized layout.
    pub fn zero_sized() -> Self {
        Self {
            size: 0,
            align: 1,
            niches: Vec::new(),
            discriminant: None,
        }
    }

    /// Discriminant strategy for enum types; `None` for non-enum types.
    pub fn discriminant_strategy(&self) -> Option<DiscriminantStrategy> {
        self.discriminant
    }
}

/// Compute (or look up the cached) layout of `ty`.
pub fn layout_of(pool: &TypeInternPool, ty: Type) -> Layout {
    if let Some(cached) = pool.cached_layout(ty) {
        return cached;
    }
    let computed = compute_layout(pool, ty);
    pool.cache_layout(ty, computed.clone());
    computed
}

fn compute_layout(pool: &TypeInternPool, ty: Type) -> Layout {
    match ty.kind() {
        TypeKind::Bool => Layout {
            size: 1,
            align: 1,
            // `bool` storage byte holds either 0 or 1; values 2..=255 are
            // forbidden bit patterns. (ADR-0069 phase 4.)
            niches: vec![NicheRange {
                offset: 0,
                width: 1,
                start: 2,
                end: 255,
            }],
            discriminant: None,
        },
        TypeKind::I8 | TypeKind::U8 => Layout::scalar(1, 1),
        TypeKind::I16 | TypeKind::U16 | TypeKind::F16 => Layout::scalar(2, 2),
        TypeKind::I32 | TypeKind::U32 | TypeKind::F32 => Layout::scalar(4, 4),
        TypeKind::I64 | TypeKind::U64 | TypeKind::F64 => Layout::scalar(8, 8),
        // Pointer-sized: 64-bit target.
        TypeKind::Isize | TypeKind::Usize => Layout::scalar(8, 8),
        TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => Layout::scalar(8, 8),
        TypeKind::Ref(_) | TypeKind::MutRef(_) => Layout::scalar(8, 8),
        // Slice: fat pointer { ptr, i64 } — 16 bytes, 8-byte aligned.
        TypeKind::Slice(_) | TypeKind::MutSlice(_) => Layout::scalar(16, 8),
        // Vec: { ptr, i64, i64 } — 24 bytes, 8-byte aligned.
        TypeKind::Vec(_) => Layout::scalar(24, 8),
        // Interface: { ptr, ptr } — 16 bytes, 8-byte aligned.
        TypeKind::Interface(_) => Layout::scalar(16, 8),

        TypeKind::Struct(id) => {
            let def = pool.struct_def(id);
            let mut offset = 0u64;
            let mut max_align = 1u64;
            for f in &def.fields {
                let field_layout = layout_of(pool, f.ty);
                if field_layout.size == 0 {
                    continue;
                }
                max_align = max_align.max(field_layout.align);
                offset = align_up(offset, field_layout.align);
                offset += field_layout.size;
            }
            if max_align > 1 {
                offset = align_up(offset, max_align);
            }
            Layout {
                size: offset,
                align: max_align,
                niches: Vec::new(),
                discriminant: None,
            }
        }

        TypeKind::Array(id) => {
            let (elem_ty, len) = pool.array_def(id);
            let elem = layout_of(pool, elem_ty);
            Layout {
                size: elem.size * len,
                align: elem.align,
                niches: Vec::new(),
                discriminant: None,
            }
        }

        TypeKind::Enum(id) => {
            let def = pool.enum_def(id);
            enum_layout_separate(pool, &def)
        }

        // Zero-sized / non-codegen types.
        TypeKind::Unit
        | TypeKind::Never
        | TypeKind::Error
        | TypeKind::ComptimeType
        | TypeKind::ComptimeStr
        | TypeKind::ComptimeInt
        | TypeKind::Module(_) => Layout::zero_sized(),
    }
}

/// Compute the standard tagged-union layout for an enum: `{ discriminant, [N x i8] }`.
///
/// This is the pre-niche layout used in Phase 1–4. Niche-encoded enums override
/// this in Phase 5+.
fn enum_layout_separate(pool: &TypeInternPool, def: &EnumDef) -> Layout {
    let discrim_layout = layout_of(pool, def.discriminant_type());
    let tag_width = discrim_layout.size as u8;
    let strategy = DiscriminantStrategy::Separate {
        tag_offset: 0,
        tag_width,
        payload_offset: discrim_layout.size as u32,
    };
    if def.is_unit_only() {
        // Unit-only enum: the storage holds the discriminant directly.
        // Discriminant values >= variant_count are forbidden bit patterns,
        // exposed as a niche so an enclosing enum (Phase 5+) can re-niche us.
        let variant_count = def.variants.len() as u128;
        let max = NicheRange::max_for_width(tag_width);
        let niches = if variant_count > 0 && variant_count <= max {
            vec![NicheRange {
                offset: 0,
                width: tag_width,
                start: variant_count,
                end: max,
            }]
        } else {
            Vec::new()
        };
        return Layout {
            size: discrim_layout.size,
            align: discrim_layout.align,
            niches,
            discriminant: Some(strategy),
        };
    }
    // Tagged union { discrim, [max_payload x i8] }
    let max_payload: u64 = def
        .variants
        .iter()
        .map(|v| {
            v.fields
                .iter()
                .map(|f| layout_of(pool, *f).size)
                .sum::<u64>()
        })
        .max()
        .unwrap_or(0);
    if max_payload == 0 {
        return Layout {
            size: discrim_layout.size,
            align: discrim_layout.align,
            niches: Vec::new(),
            discriminant: Some(strategy),
        };
    }
    let total = discrim_layout.size + max_payload;
    let size = align_up(total, discrim_layout.align);
    Layout {
        size,
        align: discrim_layout.align,
        niches: Vec::new(),
        discriminant: Some(strategy),
    }
}

#[inline]
fn align_up(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EnumVariantDef, StructDef, StructField};
    use gruel_util::FileId;
    use lasso::Rodeo;

    fn fresh_pool() -> TypeInternPool {
        TypeInternPool::new()
    }

    #[test]
    fn bool_has_niche_2_through_255() {
        let p = fresh_pool();
        let layout = layout_of(&p, Type::BOOL);
        assert_eq!(layout.size, 1);
        assert_eq!(layout.align, 1);
        assert_eq!(
            layout.niches,
            vec![NicheRange {
                offset: 0,
                width: 1,
                start: 2,
                end: 255,
            }]
        );
    }

    #[test]
    fn unit_enum_exposes_unused_discriminant_as_niche() {
        let p = fresh_pool();
        let mut rodeo = Rodeo::default();
        let name = rodeo.get_or_intern("E3");
        let def = crate::EnumDef {
            name: "E3".into(),
            variants: vec![
                EnumVariantDef::unit("A"),
                EnumVariantDef::unit("B"),
                EnumVariantDef::unit("C"),
            ],
            is_pub: false,
            file_id: FileId::DEFAULT,
            destructor: None,
        };
        let (eid, _) = p.register_enum(name, def);
        let layout = layout_of(&p, Type::new_enum(eid));
        assert_eq!(layout.size, 1);
        assert_eq!(
            layout.niches,
            vec![NicheRange {
                offset: 0,
                width: 1,
                start: 3,
                end: 255,
            }]
        );
    }

    #[test]
    fn primitive_sizes() {
        let p = fresh_pool();
        assert_eq!(layout_of(&p, Type::I8), Layout::scalar(1, 1));
        assert_eq!(layout_of(&p, Type::I16), Layout::scalar(2, 2));
        assert_eq!(layout_of(&p, Type::I32), Layout::scalar(4, 4));
        assert_eq!(layout_of(&p, Type::I64), Layout::scalar(8, 8));
        assert_eq!(layout_of(&p, Type::U8), Layout::scalar(1, 1));
        assert_eq!(layout_of(&p, Type::U16), Layout::scalar(2, 2));
        assert_eq!(layout_of(&p, Type::U32), Layout::scalar(4, 4));
        assert_eq!(layout_of(&p, Type::U64), Layout::scalar(8, 8));
        assert_eq!(layout_of(&p, Type::ISIZE), Layout::scalar(8, 8));
        assert_eq!(layout_of(&p, Type::USIZE), Layout::scalar(8, 8));
        assert_eq!(layout_of(&p, Type::F16), Layout::scalar(2, 2));
        assert_eq!(layout_of(&p, Type::F32), Layout::scalar(4, 4));
        assert_eq!(layout_of(&p, Type::F64), Layout::scalar(8, 8));
        // bool is size/align 1 but carries a niche — covered by `bool_has_niche_2_through_255`.
        let bool_layout = layout_of(&p, Type::BOOL);
        assert_eq!(bool_layout.size, 1);
        assert_eq!(bool_layout.align, 1);
    }

    #[test]
    fn unit_and_never_are_zero_sized() {
        let p = fresh_pool();
        assert_eq!(layout_of(&p, Type::UNIT).size, 0);
        assert_eq!(layout_of(&p, Type::NEVER).size, 0);
    }

    #[test]
    fn struct_layout_packs_with_padding() {
        let p = fresh_pool();
        let mut rodeo = Rodeo::default();
        let name = rodeo.get_or_intern("S");
        let def = StructDef {
            name: "S".into(),
            fields: vec![
                StructField {
                    name: "a".into(),
                    ty: Type::U8,
                },
                StructField {
                    name: "b".into(),
                    ty: Type::U32,
                },
                StructField {
                    name: "c".into(),
                    ty: Type::U8,
                },
            ],
            is_copy: false,
            is_clone: false,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: FileId::DEFAULT,
        };
        let (sid, _) = p.register_struct(name, def);
        let ty = Type::new_struct(sid);
        let layout = layout_of(&p, ty);
        // a@0..1, pad@1..4, b@4..8, c@8..9, tail-pad to 12.
        assert_eq!(layout.size, 12);
        assert_eq!(layout.align, 4);
        assert!(layout.niches.is_empty());
    }

    #[test]
    fn unit_only_enum_is_discriminant_sized() {
        let p = fresh_pool();
        let mut rodeo = Rodeo::default();
        let name = rodeo.get_or_intern("Color");
        let def = crate::EnumDef {
            name: "Color".into(),
            variants: vec![
                EnumVariantDef::unit("R"),
                EnumVariantDef::unit("G"),
                EnumVariantDef::unit("B"),
            ],
            is_pub: false,
            file_id: FileId::DEFAULT,
            destructor: None,
        };
        let (eid, _) = p.register_enum(name, def);
        let ty = Type::new_enum(eid);
        let layout = layout_of(&p, ty);
        assert_eq!(layout.size, 1);
        assert_eq!(layout.align, 1);
    }
}
