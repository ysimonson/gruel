//! ADR-0051 Phase 5: Maranget-style usefulness algorithm for match
//! exhaustiveness and unreachable-arm detection over recursive
//! `AirPattern`s.
//!
//! Given a list of arm patterns, compute:
//! - **Missing witnesses** — concrete patterns that no arm covers; the
//!   arm list is exhaustive iff this is empty.
//! - **Useless arms** — arms that do not contribute new coverage because
//!   earlier arms already subsume them; surfaced as
//!   `WarningKind::UnreachablePattern` by the caller.
//!
//! The algorithm operates on a matrix `P` of pattern rows plus a "query"
//! pattern row `q`. `is_useful(P, q)` returns `Some(witness)` when `q`
//! covers at least one value no row in `P` covers, and `None` otherwise.
//! Exhaustiveness is `is_useful(P, [_])` against a single-column
//! wildcard; reachability is `is_useful(P_{<i}, [arm_i])` for each arm.
//!
//! Reference: Luc Maranget, "Warnings for pattern matching" (JFP 2007).

use crate::inst::AirPattern;
use crate::intern_pool::TypeInternPool;
use crate::sema::Sema;
use crate::types::{EnumId, StructId, Type};

/// Head constructor of a pattern for usefulness bookkeeping. `Wildcard`
/// and `Bind` both reduce to [`Ctor::Wildcard`]; literal / variant /
/// tuple / struct heads each get a distinct concrete constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Ctor {
    /// Anonymous wildcard: matches every value of the head type.
    Wildcard,
    Int(i64),
    Bool(bool),
    EnumVariant {
        enum_id: EnumId,
        variant_index: u32,
    },
    Tuple {
        arity: u32,
    },
    Struct {
        struct_id: StructId,
    },
}

/// Return the head constructor of a pattern.
fn head_ctor(pattern: &AirPattern) -> Ctor {
    match pattern {
        AirPattern::Wildcard => Ctor::Wildcard,
        AirPattern::Bind { inner: None, .. } => Ctor::Wildcard,
        AirPattern::Bind {
            inner: Some(inner), ..
        } => head_ctor(inner),
        AirPattern::Int(n) => Ctor::Int(*n),
        AirPattern::Bool(b) => Ctor::Bool(*b),
        AirPattern::EnumVariant {
            enum_id,
            variant_index,
        }
        | AirPattern::EnumUnitVariant {
            enum_id,
            variant_index,
        }
        | AirPattern::EnumDataVariant {
            enum_id,
            variant_index,
            ..
        }
        | AirPattern::EnumStructVariant {
            enum_id,
            variant_index,
            ..
        } => Ctor::EnumVariant {
            enum_id: *enum_id,
            variant_index: *variant_index,
        },
        AirPattern::Tuple { elems } => Ctor::Tuple {
            arity: elems.len() as u32,
        },
        AirPattern::Struct { struct_id, .. } => Ctor::Struct {
            struct_id: *struct_id,
        },
    }
}

/// Arity of a constructor (number of sub-patterns it owns).
fn ctor_arity(ctor: Ctor, type_pool: &TypeInternPool) -> u32 {
    match ctor {
        Ctor::Wildcard | Ctor::Int(_) | Ctor::Bool(_) => 0,
        Ctor::EnumVariant {
            enum_id,
            variant_index,
        } => {
            let def = type_pool.enum_def(enum_id);
            def.variants[variant_index as usize].fields.len() as u32
        }
        Ctor::Tuple { arity } => arity,
        Ctor::Struct { struct_id } => type_pool.struct_def(struct_id).fields.len() as u32,
    }
}

/// Types of a constructor's sub-patterns, in positional order.
fn ctor_sub_types(ctor: Ctor, type_pool: &TypeInternPool, head_ty: Type) -> Vec<Type> {
    match ctor {
        Ctor::Wildcard | Ctor::Int(_) | Ctor::Bool(_) => Vec::new(),
        Ctor::EnumVariant {
            enum_id,
            variant_index,
        } => {
            let def = type_pool.enum_def(enum_id);
            def.variants[variant_index as usize].fields.clone()
        }
        Ctor::Tuple { .. } | Ctor::Struct { .. } => {
            // Both tuple and struct use the scrutinee's struct fields.
            if let Some(sid) = head_ty.as_struct() {
                type_pool
                    .struct_def(sid)
                    .fields
                    .iter()
                    .map(|f| f.ty)
                    .collect()
            } else {
                Vec::new()
            }
        }
    }
}

/// Expand a pattern for specialisation on constructor `ctor`:
/// returns `Some(sub_patterns)` if the pattern matches `ctor`, or
/// `None` if it doesn't. For wildcards, returns `Some(vec of wildcards
/// of ctor's arity)`. For `Bind`, treat like wildcard (binding matches).
fn specialize_pattern(pattern: &AirPattern, ctor: Ctor, arity: u32) -> Option<Vec<AirPattern>> {
    match (pattern, ctor) {
        (AirPattern::Wildcard, _) => Some(vec![AirPattern::Wildcard; arity as usize]),
        (AirPattern::Bind { inner: None, .. }, _) => {
            Some(vec![AirPattern::Wildcard; arity as usize])
        }
        (
            AirPattern::Bind {
                inner: Some(inner), ..
            },
            _,
        ) => specialize_pattern(inner, ctor, arity),
        (AirPattern::Int(n), Ctor::Int(m)) if *n == m => Some(Vec::new()),
        (AirPattern::Bool(b), Ctor::Bool(c)) if *b == c => Some(Vec::new()),
        (
            AirPattern::EnumVariant {
                variant_index: vi, ..
            }
            | AirPattern::EnumUnitVariant {
                variant_index: vi, ..
            },
            Ctor::EnumVariant { variant_index, .. },
        ) if *vi == variant_index => {
            // A "unit" variant pattern written against a data variant
            // still covers the entire constructor; treat the missing
            // fields as wildcards for specialisation purposes.
            Some(vec![AirPattern::Wildcard; arity as usize])
        }
        (
            AirPattern::EnumDataVariant {
                variant_index: vi,
                fields,
                ..
            },
            Ctor::EnumVariant { variant_index, .. },
        ) if *vi == variant_index => Some(fields.clone()),
        (
            AirPattern::EnumStructVariant {
                variant_index: vi,
                fields,
                ..
            },
            Ctor::EnumVariant { variant_index, .. },
        ) if *vi == variant_index => {
            // Fields are (field_index, pattern) in declaration order; the
            // matrix reads them positionally.
            Some(fields.iter().map(|(_, p)| p.clone()).collect())
        }
        (AirPattern::Tuple { elems }, Ctor::Tuple { arity: a }) if elems.len() as u32 == a => {
            Some(elems.clone())
        }
        (
            AirPattern::Struct {
                struct_id: sid,
                fields,
            },
            Ctor::Struct { struct_id },
        ) if *sid == struct_id => {
            // Fields are (field_index, pattern); positional.
            Some(fields.iter().map(|(_, p)| p.clone()).collect())
        }
        _ => None,
    }
}

/// Specialise matrix `rows` against constructor `ctor`: keep only the
/// rows whose head matches, replacing the head column with the
/// constructor's sub-patterns (and appending the rest of the row).
fn specialize_rows(rows: &[Vec<AirPattern>], ctor: Ctor, arity: u32) -> Vec<Vec<AirPattern>> {
    let mut out = Vec::new();
    for row in rows {
        let Some((head, tail)) = row.split_first() else {
            continue;
        };
        let Some(sub) = specialize_pattern(head, ctor, arity) else {
            continue;
        };
        let mut new_row = sub;
        new_row.extend_from_slice(tail);
        out.push(new_row);
    }
    out
}

/// Default matrix `D(P)`: keep only rows whose head is a wildcard (or
/// a Bind with no refutable inner); drop the head column.
fn default_rows(rows: &[Vec<AirPattern>]) -> Vec<Vec<AirPattern>> {
    rows.iter()
        .filter(|row| {
            row.first()
                .is_some_and(|h| matches!(head_ctor(h), Ctor::Wildcard))
        })
        .map(|row| row[1..].to_vec())
        .collect()
}

/// Return the full signature of a head type, or `None` if the
/// signature is open (integer literals). Tuples and structs have a
/// single-constructor signature; bools have two; enums have one
/// variant per enum entry.
fn type_signature(ty: Type, type_pool: &TypeInternPool) -> Option<Vec<Ctor>> {
    if ty == Type::BOOL {
        return Some(vec![Ctor::Bool(true), Ctor::Bool(false)]);
    }
    if let Some(enum_id) = ty.as_enum() {
        let def = type_pool.enum_def(enum_id);
        return Some(
            (0..def.variants.len())
                .map(|i| Ctor::EnumVariant {
                    enum_id,
                    variant_index: i as u32,
                })
                .collect(),
        );
    }
    if let Some(struct_id) = ty.as_struct() {
        let def = type_pool.struct_def(struct_id);
        // Tuple-shaped structs use the Tuple constructor so that
        // `(1, 2)` arms and nested tuple patterns share the same
        // constructor family.
        if def.is_tuple_shaped() {
            return Some(vec![Ctor::Tuple {
                arity: def.fields.len() as u32,
            }]);
        }
        return Some(vec![Ctor::Struct { struct_id }]);
    }
    if ty.is_integer() {
        // Integer literals form an infinite signature; only wildcard
        // exhausts, so we report as "no signature" → default-matrix
        // codepath with `_` witness.
        return None;
    }
    None
}

/// Reconstruct a witness pattern from a specialisation witness. Given
/// a constructor `ctor` and its sub-witnesses (one per field), build
/// the `AirPattern` that represents them.
fn build_witness(ctor: Ctor, subs: Vec<AirPattern>, type_pool: &TypeInternPool) -> AirPattern {
    match ctor {
        Ctor::Wildcard => AirPattern::Wildcard,
        Ctor::Int(n) => AirPattern::Int(n),
        Ctor::Bool(b) => AirPattern::Bool(b),
        Ctor::EnumVariant {
            enum_id,
            variant_index,
        } => {
            if subs.is_empty() {
                AirPattern::EnumUnitVariant {
                    enum_id,
                    variant_index,
                }
            } else {
                AirPattern::EnumDataVariant {
                    enum_id,
                    variant_index,
                    fields: subs,
                }
            }
        }
        Ctor::Tuple { .. } => AirPattern::Tuple { elems: subs },
        Ctor::Struct { struct_id } => {
            let def = type_pool.struct_def(struct_id);
            let fields = subs
                .into_iter()
                .enumerate()
                .take(def.fields.len())
                .map(|(i, p)| (i as u32, p))
                .collect();
            AirPattern::Struct { struct_id, fields }
        }
    }
}

/// Outcome of `is_useful`: when useful, provide a concrete witness
/// row that demonstrates which value(s) `q` covers that `P` does not.
#[derive(Debug, Clone)]
enum Usefulness {
    NotUseful,
    Useful { witnesses: Vec<Vec<AirPattern>> },
}

/// Core usefulness recursion. `rows` is matrix `P`, each row with
/// `column_types.len()` columns; `query` is pattern row `q` with the
/// same width. Returns `Useful { witnesses }` when `q` covers values
/// no row in `P` does, otherwise `NotUseful`. Witnesses are full
/// rows whose first column values aren't covered.
fn is_useful(
    rows: &[Vec<AirPattern>],
    query: &[AirPattern],
    column_types: &[Type],
    type_pool: &TypeInternPool,
) -> Usefulness {
    if query.is_empty() {
        // Empty query: useful iff `P` has no rows.
        return if rows.is_empty() {
            Usefulness::Useful {
                witnesses: vec![Vec::new()],
            }
        } else {
            Usefulness::NotUseful
        };
    }

    let head_ty = column_types[0];
    let rest_tys = &column_types[1..];
    let q_head = &query[0];

    match head_ctor(q_head) {
        Ctor::Wildcard => {
            // Check completeness of the head type signature in P.
            let used: rustc_hash::FxHashSet<Ctor> = rows
                .iter()
                .filter_map(|r| r.first().map(head_ctor))
                .filter(|c| !matches!(c, Ctor::Wildcard))
                .collect();

            let sig = type_signature(head_ty, type_pool);
            match sig {
                None => {
                    // Open signature (integers): fall back to default
                    // matrix; witness is `_`.
                    let inner = is_useful(&default_rows(rows), &query[1..], rest_tys, type_pool);
                    match inner {
                        Usefulness::NotUseful => Usefulness::NotUseful,
                        Usefulness::Useful { witnesses } => Usefulness::Useful {
                            witnesses: witnesses
                                .into_iter()
                                .map(|w| {
                                    let mut v = vec![AirPattern::Wildcard];
                                    v.extend(w);
                                    v
                                })
                                .collect(),
                        },
                    }
                }
                Some(ctors) => {
                    let missing: Vec<Ctor> = ctors
                        .iter()
                        .copied()
                        .filter(|c| !used.contains(c))
                        .collect();
                    if missing.is_empty() {
                        // All constructors present: check usefulness
                        // per constructor and union the witnesses.
                        let mut all_witnesses: Vec<Vec<AirPattern>> = Vec::new();
                        for c in &ctors {
                            let arity = ctor_arity(*c, type_pool);
                            let specialised_rows = specialize_rows(rows, *c, arity);
                            let mut specialised_query =
                                specialize_pattern(q_head, *c, arity).unwrap_or_default();
                            specialised_query.extend_from_slice(&query[1..]);
                            let mut sub_tys = ctor_sub_types(*c, type_pool, head_ty);
                            sub_tys.extend_from_slice(rest_tys);
                            match is_useful(
                                &specialised_rows,
                                &specialised_query,
                                &sub_tys,
                                type_pool,
                            ) {
                                Usefulness::Useful { witnesses } => {
                                    for w in witnesses {
                                        let arity_usize = arity as usize;
                                        if w.len() < arity_usize {
                                            continue;
                                        }
                                        let (subs, tail) = w.split_at(arity_usize);
                                        let mut row =
                                            vec![build_witness(*c, subs.to_vec(), type_pool)];
                                        row.extend_from_slice(tail);
                                        all_witnesses.push(row);
                                    }
                                }
                                Usefulness::NotUseful => {}
                            }
                        }
                        if all_witnesses.is_empty() {
                            Usefulness::NotUseful
                        } else {
                            Usefulness::Useful {
                                witnesses: all_witnesses,
                            }
                        }
                    } else {
                        // Some constructors missing: default matrix
                        // covers the "other" rows; witness for each
                        // missing ctor is ctor(_ * arity).
                        let inner =
                            is_useful(&default_rows(rows), &query[1..], rest_tys, type_pool);
                        match inner {
                            Usefulness::NotUseful => Usefulness::NotUseful,
                            Usefulness::Useful { witnesses } => {
                                let mut all = Vec::new();
                                for c in &missing {
                                    let arity = ctor_arity(*c, type_pool);
                                    let stub = vec![AirPattern::Wildcard; arity as usize];
                                    for w in &witnesses {
                                        let mut row =
                                            vec![build_witness(*c, stub.clone(), type_pool)];
                                        row.extend_from_slice(w);
                                        all.push(row);
                                    }
                                }
                                Usefulness::Useful { witnesses: all }
                            }
                        }
                    }
                }
            }
        }
        ctor => {
            let arity = ctor_arity(ctor, type_pool);
            let specialised_rows = specialize_rows(rows, ctor, arity);
            let mut specialised_query = specialize_pattern(q_head, ctor, arity).unwrap_or_default();
            specialised_query.extend_from_slice(&query[1..]);
            let mut sub_tys = ctor_sub_types(ctor, type_pool, head_ty);
            sub_tys.extend_from_slice(rest_tys);
            match is_useful(&specialised_rows, &specialised_query, &sub_tys, type_pool) {
                Usefulness::NotUseful => Usefulness::NotUseful,
                Usefulness::Useful { witnesses } => {
                    let arity_usize = arity as usize;
                    let folded = witnesses
                        .into_iter()
                        .filter_map(|w| {
                            if w.len() < arity_usize {
                                return None;
                            }
                            let (subs, tail) = w.split_at(arity_usize);
                            let mut row = vec![build_witness(ctor, subs.to_vec(), type_pool)];
                            row.extend_from_slice(tail);
                            Some(row)
                        })
                        .collect::<Vec<_>>();
                    if folded.is_empty() {
                        Usefulness::NotUseful
                    } else {
                        Usefulness::Useful { witnesses: folded }
                    }
                }
            }
        }
    }
}

/// Compute missing-pattern witnesses for a list of arm patterns. An
/// empty return value means the match is exhaustive.
pub(crate) fn exhaustiveness_witnesses(
    arms: &[AirPattern],
    scrutinee_type: Type,
    sema: &Sema<'_>,
) -> Vec<AirPattern> {
    let rows: Vec<Vec<AirPattern>> = arms.iter().map(|a| vec![a.clone()]).collect();
    let query = vec![AirPattern::Wildcard];
    match is_useful(&rows, &query, &[scrutinee_type], &sema.type_pool) {
        Usefulness::NotUseful => Vec::new(),
        Usefulness::Useful { witnesses } => {
            witnesses.into_iter().filter_map(|mut r| r.pop()).collect()
        }
    }
}

/// For each arm, report whether it is reachable given the preceding
/// arms. An arm is unreachable when the earlier arms already cover
/// every value it matches. Returns a boolean vector parallel to
/// `arms`; `false` means unreachable.
///
/// Not wired into the diagnostic pipeline yet; sema still tracks
/// unreachable-literal / unreachable-variant warnings via the
/// simpler per-kind bookkeeping in `analyze_match`. This hook lives
/// here so a follow-up can surface `WarningKind::UnreachablePattern`
/// for nested arms that the bookkeeping misses.
pub(crate) fn arm_reachability(
    arms: &[AirPattern],
    scrutinee_type: Type,
    sema: &Sema<'_>,
) -> Vec<bool> {
    let mut reach = Vec::with_capacity(arms.len());
    let mut seen: Vec<Vec<AirPattern>> = Vec::new();
    for a in arms {
        let query = vec![a.clone()];
        let useful = matches!(
            is_useful(&seen, &query, &[scrutinee_type], &sema.type_pool),
            Usefulness::Useful { .. }
        );
        reach.push(useful);
        seen.push(vec![a.clone()]);
    }
    reach
}

/// Render an `AirPattern` witness into a user-facing string using the
/// type pool for enum / struct names. Falls back to the flat
/// `Display` impl when metadata isn't available.
pub(crate) fn render_witness(pattern: &AirPattern, type_pool: &TypeInternPool) -> String {
    match pattern {
        AirPattern::Wildcard => "_".to_string(),
        AirPattern::Int(n) => n.to_string(),
        AirPattern::Bool(b) => b.to_string(),
        AirPattern::EnumVariant {
            enum_id,
            variant_index,
        }
        | AirPattern::EnumUnitVariant {
            enum_id,
            variant_index,
        } => {
            let def = type_pool.enum_def(*enum_id);
            let v = def
                .variants
                .get(*variant_index as usize)
                .map(|v| v.name.as_str())
                .unwrap_or("?");
            format!("{}::{}", def.name, v)
        }
        AirPattern::EnumDataVariant {
            enum_id,
            variant_index,
            fields,
        } => {
            let def = type_pool.enum_def(*enum_id);
            let v = def
                .variants
                .get(*variant_index as usize)
                .map(|v| v.name.as_str())
                .unwrap_or("?");
            let parts: Vec<String> = fields
                .iter()
                .map(|f| render_witness(f, type_pool))
                .collect();
            format!("{}::{}({})", def.name, v, parts.join(", "))
        }
        AirPattern::EnumStructVariant {
            enum_id,
            variant_index,
            fields,
        } => {
            let def = type_pool.enum_def(*enum_id);
            let v_def = def.variants.get(*variant_index as usize);
            let v_name = v_def.map(|v| v.name.as_str()).unwrap_or("?");
            let field_names = v_def.map(|v| &v.field_names).cloned().unwrap_or_default();
            let parts: Vec<String> = fields
                .iter()
                .map(|(idx, p)| {
                    let name = field_names
                        .get(*idx as usize)
                        .cloned()
                        .unwrap_or_else(|| idx.to_string());
                    format!("{}: {}", name, render_witness(p, type_pool))
                })
                .collect();
            format!("{}::{} {{ {} }}", def.name, v_name, parts.join(", "))
        }
        AirPattern::Tuple { elems } => {
            let parts: Vec<String> = elems.iter().map(|e| render_witness(e, type_pool)).collect();
            format!("({})", parts.join(", "))
        }
        AirPattern::Struct { struct_id, fields } => {
            let def = type_pool.struct_def(*struct_id);
            // Tuple-shaped structs render with tuple syntax.
            if def.is_tuple_shaped() {
                let mut parts = vec![String::new(); def.fields.len()];
                for (idx, p) in fields {
                    if let Some(slot) = parts.get_mut(*idx as usize) {
                        *slot = render_witness(p, type_pool);
                    }
                }
                for (i, slot) in parts.iter_mut().enumerate() {
                    if slot.is_empty() {
                        // Any unlisted field inherits `_` (the
                        // witness algorithm always lists all fields,
                        // but be defensive).
                        let _ = i;
                        *slot = "_".to_string();
                    }
                }
                return format!("({})", parts.join(", "));
            }
            let parts: Vec<String> = fields
                .iter()
                .map(|(idx, p)| {
                    let name = def
                        .fields
                        .get(*idx as usize)
                        .map(|f| f.name.clone())
                        .unwrap_or_else(|| idx.to_string());
                    format!("{}: {}", name, render_witness(p, type_pool))
                })
                .collect();
            format!("{} {{ {} }}", def.name, parts.join(", "))
        }
        AirPattern::Bind { .. } => "_".to_string(),
    }
}
