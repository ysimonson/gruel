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
        // ADR-0071: char is a 32-bit Unicode scalar with two niche ranges:
        //   - surrogates U+D800..=U+DFFF
        //   - codepoints > U+10FFFF (i.e., 0x110000..=0xFFFFFFFF)
        // Niche-filling enums consume these to elide their discriminant.
        TypeKind::Char => Layout {
            size: 4,
            align: 4,
            niches: vec![
                NicheRange {
                    offset: 0,
                    width: 4,
                    start: 0xD800,
                    end: 0xDFFF,
                },
                NicheRange {
                    offset: 0,
                    width: 4,
                    start: 0x110000,
                    end: 0xFFFFFFFF,
                },
            ],
            discriminant: None,
        },
        // Pointer-sized: 64-bit target.
        TypeKind::Isize | TypeKind::Usize => Layout::scalar(8, 8),
        // ADR-0086: C named arithmetic primitive types. Sizes match the
        // underlying Gruel type on every blessed LP64 target.
        TypeKind::CSchar | TypeKind::CUchar => Layout::scalar(1, 1),
        TypeKind::CShort | TypeKind::CUshort => Layout::scalar(2, 2),
        TypeKind::CInt | TypeKind::CUint => Layout::scalar(4, 4),
        TypeKind::CLong | TypeKind::CUlong | TypeKind::CLonglong | TypeKind::CUlonglong => {
            Layout::scalar(8, 8)
        }
        TypeKind::CFloat => Layout::scalar(4, 4),
        TypeKind::CDouble => Layout::scalar(8, 8),
        // ADR-0086: c_void is an incomplete type with no values. Treated
        // as zero-sized for layout purposes — sema rejects c_void in any
        // value-bearing position before this query runs.
        TypeKind::CVoid => Layout::zero_sized(),
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
            let mut niches = Vec::new();
            for f in &def.fields {
                let field_layout = layout_of(pool, f.ty);
                if field_layout.size == 0 {
                    continue;
                }
                max_align = max_align.max(field_layout.align);
                offset = align_up(offset, field_layout.align);
                // Inherit field niches with offset adjusted to the field's
                // offset within the struct (ADR-0069 phase 7).
                for n in &field_layout.niches {
                    niches.push(NicheRange {
                        offset: n.offset + offset as u32,
                        width: n.width,
                        start: n.start,
                        end: n.end,
                    });
                }
                offset += field_layout.size;
            }
            if max_align > 1 {
                offset = align_up(offset, max_align);
            }
            Layout {
                size: offset,
                align: max_align,
                niches,
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
            if let Some(niche_layout) = try_niche_encoded_enum_layout(pool, &def) {
                return niche_layout;
            }
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

/// Try to compute a niche-encoded layout for an enum (ADR-0069 phase 5).
///
/// Returns `Some(layout)` when the enum is "Option-shaped":
///   - Exactly one unit variant.
///   - Exactly one data variant whose payload type exposes a usable niche.
///
/// Otherwise returns `None` and the caller falls back to
/// [`enum_layout_separate`].
fn try_niche_encoded_enum_layout(pool: &TypeInternPool, def: &EnumDef) -> Option<Layout> {
    if def.variants.len() != 2 {
        return None;
    }
    let (unit_idx, data_idx) = match (def.variants[0].has_data(), def.variants[1].has_data()) {
        (false, true) => (0u32, 1u32),
        (true, false) => (1u32, 0u32),
        _ => return None,
    };
    let data_variant = &def.variants[data_idx as usize];
    if data_variant.fields.len() != 1 {
        // V1 only handles a single-field data variant (e.g. `Some(T)`).
        return None;
    }
    let payload_ty = data_variant.fields[0];
    let payload_layout = layout_of(pool, payload_ty);
    let niche = payload_layout.niches.first()?;
    // Reserve niche.start for the unit variant; expose the rest to enclosing
    // types so the optimization composes (Phase 7).
    let niche_value = niche.start;
    let remaining_start = niche.start + 1;
    let mut composed_niches = Vec::new();
    if remaining_start <= niche.end {
        composed_niches.push(NicheRange {
            offset: niche.offset,
            width: niche.width,
            start: remaining_start,
            end: niche.end,
        });
    }
    Some(Layout {
        size: payload_layout.size,
        align: payload_layout.align,
        niches: composed_niches,
        discriminant: Some(DiscriminantStrategy::Niche {
            unit_variant: unit_idx,
            data_variant: data_idx,
            niche_offset: niche.offset,
            niche_width: niche.width,
            niche_value,
        }),
    })
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
    use gruel_builtins::Posture;
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
            posture: Posture::Affine,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            is_pub: false,
            file_id: FileId::DEFAULT,
            destructor: None,
            is_c_layout: false,
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

                    is_pub: true,
                },
                StructField {
                    name: "b".into(),
                    ty: Type::U32,

                    is_pub: true,
                },
                StructField {
                    name: "c".into(),
                    ty: Type::U8,

                    is_pub: true,
                },
            ],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: FileId::DEFAULT,
            is_c_layout: false,
        };
        let (sid, _) = p.register_struct(name, def);
        let ty = Type::new_struct(sid);
        let layout = layout_of(&p, ty);
        // a@0..1, pad@1..4, b@4..8, c@8..9, tail-pad to 12.
        assert_eq!(layout.size, 12);
        assert_eq!(layout.align, 4);
        assert!(layout.niches.is_empty());
    }

    fn make_option_bool(p: &TypeInternPool, name: &str) -> Type {
        let mut rodeo = Rodeo::default();
        let s = rodeo.get_or_intern(name);
        let mut some = EnumVariantDef::unit("Some");
        some.fields = vec![Type::BOOL];
        let def = crate::EnumDef {
            name: name.to_string(),
            variants: vec![EnumVariantDef::unit("None"), some],
            posture: Posture::Affine,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            is_pub: false,
            file_id: FileId::DEFAULT,
            destructor: None,
            is_c_layout: false,
        };
        let (eid, _) = p.register_enum(s, def);
        Type::new_enum(eid)
    }

    #[test]
    fn option_bool_collapses_to_one_byte() {
        let p = fresh_pool();
        let ty = make_option_bool(&p, "OptB2");
        let layout = layout_of(&p, ty);
        assert_eq!(layout.size, 1, "Option(bool) should collapse to 1 byte");
        assert_eq!(layout.align, 1);
        match layout.discriminant_strategy().unwrap() {
            DiscriminantStrategy::Niche {
                niche_offset,
                niche_width,
                niche_value,
                unit_variant,
                data_variant,
            } => {
                assert_eq!(niche_offset, 0);
                assert_eq!(niche_width, 1);
                assert_eq!(niche_value, 2, "first reserved niche value");
                assert_eq!(unit_variant, 0);
                assert_eq!(data_variant, 1);
            }
            other => panic!("expected Niche, got {other:?}"),
        }
        // Remaining niche values [3..=255] are exposed for further nesting.
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
    fn nested_option_bool_collapses_recursively() {
        let p = fresh_pool();
        // Build Option(Option(Option(bool))) by hand.
        let inner = make_option_bool(&p, "OptB_inner");
        let mut rodeo = Rodeo::default();
        let mid_name = rodeo.get_or_intern("OptOptB");
        let mut some_mid = EnumVariantDef::unit("Some");
        some_mid.fields = vec![inner];
        let mid_def = crate::EnumDef {
            name: "OptOptB".into(),
            variants: vec![EnumVariantDef::unit("None"), some_mid],
            posture: Posture::Affine,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            is_pub: false,
            file_id: FileId::DEFAULT,
            destructor: None,
            is_c_layout: false,
        };
        let (mid_eid, _) = p.register_enum(mid_name, mid_def);
        let mid = Type::new_enum(mid_eid);
        let outer_name = rodeo.get_or_intern("OptOptOptB");
        let mut some_outer = EnumVariantDef::unit("Some");
        some_outer.fields = vec![mid];
        let outer_def = crate::EnumDef {
            name: "OptOptOptB".into(),
            variants: vec![EnumVariantDef::unit("None"), some_outer],
            posture: Posture::Affine,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            is_pub: false,
            file_id: FileId::DEFAULT,
            destructor: None,
            is_c_layout: false,
        };
        let (outer_eid, _) = p.register_enum(outer_name, outer_def);
        let outer = Type::new_enum(outer_eid);
        let layout = layout_of(&p, outer);
        assert_eq!(
            layout.size, 1,
            "Option(Option(Option(bool))) should collapse to 1 byte"
        );
    }

    #[test]
    fn struct_inherits_field_niches() {
        let p = fresh_pool();
        let mut rodeo = Rodeo::default();
        let name = rodeo.get_or_intern("Wrap");
        // struct Wrap { _pad: u8, b: bool }
        let def = StructDef {
            name: "Wrap".into(),
            fields: vec![
                StructField {
                    name: "pad".into(),
                    ty: Type::U8,

                    is_pub: true,
                },
                StructField {
                    name: "b".into(),
                    ty: Type::BOOL,

                    is_pub: true,
                },
            ],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: FileId::DEFAULT,
            is_c_layout: false,
        };
        let (sid, _) = p.register_struct(name, def);
        let layout = layout_of(&p, Type::new_struct(sid));
        // The bool's niche should be inherited at offset 1 (the bool field offset).
        assert_eq!(
            layout.niches,
            vec![NicheRange {
                offset: 1,
                width: 1,
                start: 2,
                end: 255,
            }]
        );
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
            posture: Posture::Affine,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            is_pub: false,
            file_id: FileId::DEFAULT,
            destructor: None,
            is_c_layout: false,
        };
        let (eid, _) = p.register_enum(name, def);
        let ty = Type::new_enum(eid);
        let layout = layout_of(&p, ty);
        assert_eq!(layout.size, 1);
        assert_eq!(layout.align, 1);
    }
}
