//! Lang-item registry (ADR-0079).
//!
//! Stores the resolved interface/enum IDs for the compiler-recognized
//! lang items (`drop`, `clone`, `handle`, `op_eq`, `op_cmp`,
//! `ordering`). Populated from `@lang("…")` directives on prelude
//! declarations during `resolve_declarations`. Every compiler-side
//! behavior that historically matched on a hardcoded type name (the
//! `Drop`/`Clone`/`Eq`/`Ord` strings) consults this registry instead.
//!
//! ADR-0080 retired the `copy` lang item: posture is declared on the
//! type (`copy struct`/`copy enum`) and queried via `@ownership(T)`,
//! never dispatched, so it no longer needs an interface binding.

use gruel_builtins::{LangEnumItem, LangFnItem, LangInterfaceItem, LangItemKind};
use gruel_rir::InstData;
use gruel_util::{CompileError, CompileResult, ErrorKind, Span};
use lasso::Spur;

use super::Sema;
use super::file_paths::is_prelude_path;
use crate::types::{EnumId, InterfaceId};

/// Lang items resolved against the prelude. Each option is `Some` once
/// the prelude tags the corresponding declaration with `@lang("…")`.
/// Missing entries surface lazily — every call site that needs a lang
/// item logs through `Sema::lang_items()` and tolerates an absent entry
/// in the same way it tolerated a missing interface name pre-ADR-0079.
#[derive(Debug, Default, Clone)]
pub struct LangItems {
    pub(crate) drop: Option<InterfaceId>,
    pub(crate) clone: Option<InterfaceId>,
    pub(crate) handle: Option<InterfaceId>,
    pub(crate) op_eq: Option<InterfaceId>,
    pub(crate) op_cmp: Option<InterfaceId>,
    pub(crate) ordering: Option<EnumId>,
    /// `@lang("vec")` — the prelude `Vec(comptime T: type) -> type`
    /// function whose instantiations are recognized as the canonical
    /// owned-buffer vector (ADR-0082).
    pub(crate) vec_fn: Option<Spur>,
}

impl LangItems {
    pub fn drop(&self) -> Option<InterfaceId> {
        self.drop
    }
    pub fn clone(&self) -> Option<InterfaceId> {
        self.clone
    }
    pub fn handle(&self) -> Option<InterfaceId> {
        self.handle
    }
    pub fn op_eq(&self) -> Option<InterfaceId> {
        self.op_eq
    }
    pub fn op_cmp(&self) -> Option<InterfaceId> {
        self.op_cmp
    }
    pub fn ordering(&self) -> Option<EnumId> {
        self.ordering
    }
    pub fn vec_fn(&self) -> Option<Spur> {
        self.vec_fn
    }
}

impl<'a> Sema<'a> {
    /// Public read access to the lang-item registry.
    pub fn lang_items(&self) -> &LangItems {
        &self.lang_items
    }

    /// ADR-0082: if `ty` is `TypeKind::Struct` for an instance of the
    /// `@lang("vec")` function (i.e. it was produced by evaluating
    /// `Vec(elem_ty)` through the prelude declaration), return the
    /// element type. Returns `None` for plain user structs and for any
    /// type that's not a struct.
    pub(crate) fn as_vec_instance(&self, ty: crate::types::Type) -> Option<crate::types::Type> {
        let crate::types::TypeKind::Struct(struct_id) = ty.kind() else {
            return None;
        };
        self.vec_instance_registry.get(&struct_id).copied()
    }

    /// ADR-0082: reverse lookup — given an element type, return the
    /// `StructId` of the `@lang("vec")` instance for that element.
    /// Returns `None` if `populate_vec_instance` has not yet been
    /// called for `elem_ty` (or the lang-item Vec is unbound).
    pub(crate) fn vec_instance_for_elem(
        &self,
        elem_ty: crate::types::Type,
    ) -> Option<crate::types::StructId> {
        self.vec_instance_registry
            .iter()
            .find_map(|(sid, t)| if *t == elem_ty { Some(*sid) } else { None })
    }

    /// ADR-0082: ensure the prelude `@lang("vec")` function's
    /// instantiation for `elem_ty` has been evaluated and its
    /// `StructId` registered. Idempotent — early-returns if the
    /// element type is already represented in the registry. Errors
    /// are swallowed: the legacy `TypeKind::Vec(_)` path stays valid,
    /// so a failed prelude evaluation only means the registry stays
    /// empty for that element type. Phase 3's bridge consumers fall
    /// back to the legacy path when the registry has no entry.
    pub(crate) fn populate_vec_instance(&mut self, elem_ty: crate::types::Type) {
        // Skip if any existing entry already maps to this element.
        if self
            .vec_instance_registry
            .values()
            .any(|t| *t == elem_ty)
        {
            return;
        }
        let Some(vec_fn_sym) = self.lang_items.vec_fn() else {
            return;
        };
        let Some(fn_info) = self.functions.get(&vec_fn_sym).copied() else {
            return;
        };
        let param_names = self.param_arena.names(fn_info.params).to_vec();
        if param_names.is_empty() {
            return;
        }
        let mut type_subst: rustc_hash::FxHashMap<lasso::Spur, crate::types::Type> =
            rustc_hash::FxHashMap::default();
        type_subst.insert(param_names[0], elem_ty);
        let value_subst: rustc_hash::FxHashMap<lasso::Spur, super::ConstValue> =
            rustc_hash::FxHashMap::default();
        let saved_ctor = self.comptime_ctor_fn.replace(vec_fn_sym);
        let _ = self.try_evaluate_const_with_subst(fn_info.body, &type_subst, &value_subst);
        self.comptime_ctor_fn = saved_ctor;
    }

    /// ADR-0079: walk every interface and enum declaration in the RIR,
    /// resolve `@lang("…")` directives against the closed lang-item set,
    /// and record the binding on `self.lang_items`.
    ///
    /// Errors:
    /// - `@lang(...)` outside a prelude file (`InvalidLangItem`).
    /// - Wrong arg shape (zero, multiple, or non-string) at the
    ///   directive site.
    /// - Unknown lang-item name.
    /// - Lang-item kind mismatched with declaration kind (e.g.
    ///   `@lang("ordering")` on an interface).
    /// - Two declarations both claim the same lang item.
    pub(crate) fn populate_lang_items(&mut self) -> CompileResult<()> {
        // Collect raw bindings first; we look up interface/enum IDs from
        // already-populated maps and only mutate `self.lang_items` after
        // all directives validate.
        struct Pending {
            kind: PendingKind,
            site: Span,
        }
        enum PendingKind {
            Interface(LangInterfaceItem, InterfaceId),
            Enum(LangEnumItem, EnumId),
            Fn(LangFnItem, Spur),
        }
        let mut pending: Vec<Pending> = Vec::new();

        for (_, inst) in self.rir.iter() {
            let (name, directives_start, directives_len, kind) = match &inst.data {
                InstData::InterfaceDecl {
                    name,
                    directives_start,
                    directives_len,
                    ..
                } => (
                    *name,
                    *directives_start,
                    *directives_len,
                    DeclKind::Interface,
                ),
                InstData::EnumDecl {
                    name,
                    directives_start,
                    directives_len,
                    ..
                } => (*name, *directives_start, *directives_len, DeclKind::Enum),
                InstData::FnDecl {
                    name,
                    directives_start,
                    directives_len,
                    ..
                } => (*name, *directives_start, *directives_len, DeclKind::Fn),
                _ => continue,
            };
            if directives_len == 0 {
                continue;
            }
            // Use the host inst's span for privilege/diagnostic
            // reporting. RIR directive storage drops the directive's
            // file_id (its `span` carries `(start, len)` only) so the
            // host span is the reliable file-of-origin signal.
            let host_span = inst.span;
            let directives = self.rir.get_directives(directives_start, directives_len);
            for d in &directives {
                if self.interner.resolve(&d.name) != "lang" {
                    continue;
                }
                // Privilege gate: `@lang(...)` is only valid inside the
                // prelude.
                if !self.is_directive_in_prelude(host_span) {
                    return Err(CompileError::new(
                        ErrorKind::InvalidLangItem {
                            reason: "`@lang(...)` is only valid in the prelude (under `prelude/`)"
                                .to_string(),
                        },
                        host_span,
                    ));
                }
                if d.args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::InvalidLangItem {
                            reason: format!(
                                "`@lang(...)` expects one string argument, got {}",
                                d.args.len()
                            ),
                        },
                        host_span,
                    ));
                }
                let lang_name = self.interner.resolve(&d.args[0]).to_string();
                let lang_kind = match LangItemKind::from_str(&lang_name) {
                    Some(k) => k,
                    None => {
                        let known = gruel_builtins::all_lang_item_names().join(", ");
                        return Err(CompileError::new(
                            ErrorKind::InvalidLangItem {
                                reason: format!(
                                    "unknown lang item `{}`; known: {}",
                                    lang_name, known
                                ),
                            },
                            host_span,
                        ));
                    }
                };
                match (kind, lang_kind) {
                    (DeclKind::Interface, LangItemKind::Interface(item)) => {
                        let id = match self.interfaces.get(&name) {
                            Some(&id) => id,
                            None => continue,
                        };
                        pending.push(Pending {
                            kind: PendingKind::Interface(item, id),
                            site: host_span,
                        });
                    }
                    (DeclKind::Enum, LangItemKind::Enum(item)) => {
                        let id = match self.enums.get(&name) {
                            Some(&id) => id,
                            None => continue,
                        };
                        pending.push(Pending {
                            kind: PendingKind::Enum(item, id),
                            site: host_span,
                        });
                    }
                    (DeclKind::Fn, LangItemKind::Fn(item)) => {
                        // ADR-0082: bind the function's name `Spur` so
                        // `Sema::lang_items().vec_fn()` returns it; the
                        // function body lives in `self.functions[name]`.
                        if !self.functions.contains_key(&name) {
                            continue;
                        }
                        pending.push(Pending {
                            kind: PendingKind::Fn(item, name),
                            site: host_span,
                        });
                    }
                    (DeclKind::Interface, LangItemKind::Enum(_))
                    | (DeclKind::Interface, LangItemKind::Fn(_)) => {
                        return Err(CompileError::new(
                            ErrorKind::InvalidLangItem {
                                reason: format!(
                                    "lang item `{}` cannot appear on an interface declaration",
                                    lang_name
                                ),
                            },
                            host_span,
                        ));
                    }
                    (DeclKind::Enum, LangItemKind::Interface(_))
                    | (DeclKind::Enum, LangItemKind::Fn(_)) => {
                        return Err(CompileError::new(
                            ErrorKind::InvalidLangItem {
                                reason: format!(
                                    "lang item `{}` cannot appear on an enum declaration",
                                    lang_name
                                ),
                            },
                            host_span,
                        ));
                    }
                    (DeclKind::Fn, LangItemKind::Interface(_))
                    | (DeclKind::Fn, LangItemKind::Enum(_)) => {
                        return Err(CompileError::new(
                            ErrorKind::InvalidLangItem {
                                reason: format!(
                                    "lang item `{}` cannot appear on a function declaration",
                                    lang_name
                                ),
                            },
                            host_span,
                        ));
                    }
                }
            }
        }

        for p in pending {
            match p.kind {
                PendingKind::Interface(item, id) => {
                    let slot: &mut Option<InterfaceId> = match item {
                        LangInterfaceItem::Drop => &mut self.lang_items.drop,
                        LangInterfaceItem::Clone => &mut self.lang_items.clone,
                        LangInterfaceItem::Handle => &mut self.lang_items.handle,
                        LangInterfaceItem::OpEq => &mut self.lang_items.op_eq,
                        LangInterfaceItem::OpCmp => &mut self.lang_items.op_cmp,
                    };
                    if let Some(_existing) = *slot {
                        return Err(CompileError::new(
                            ErrorKind::InvalidLangItem {
                                reason: format!("lang item `{}` is bound twice", item.name()),
                            },
                            p.site,
                        ));
                    }
                    *slot = Some(id);
                }
                PendingKind::Enum(item, id) => {
                    let slot: &mut Option<EnumId> = match item {
                        LangEnumItem::Ordering => &mut self.lang_items.ordering,
                    };
                    if let Some(_existing) = *slot {
                        return Err(CompileError::new(
                            ErrorKind::InvalidLangItem {
                                reason: format!("lang item `{}` is bound twice", item.name()),
                            },
                            p.site,
                        ));
                    }
                    *slot = Some(id);
                }
                PendingKind::Fn(item, sym) => {
                    let slot: &mut Option<Spur> = match item {
                        LangFnItem::Vec => &mut self.lang_items.vec_fn,
                    };
                    if let Some(_existing) = *slot {
                        return Err(CompileError::new(
                            ErrorKind::InvalidLangItem {
                                reason: format!("lang item `{}` is bound twice", item.name()),
                            },
                            p.site,
                        ));
                    }
                    *slot = Some(sym);
                }
            }
        }
        Ok(())
    }

    /// Return true iff the directive's source file is part of the
    /// prelude (top-level `prelude/` directory). Recognizes:
    ///
    /// - `FileId::PRELUDE` itself (the prelude root).
    /// - File IDs in the high reserved band (0xFFFF_F000 ..=
    ///   FileId::PRELUDE) that the prelude loader assigns to submodules
    ///   counting down from PRELUDE. Test fixtures inline submodules
    ///   without registering paths, but the file IDs they use sit in
    ///   this band.
    /// - Any registered path that satisfies `is_prelude_path`.
    fn is_directive_in_prelude(&self, span: Span) -> bool {
        let id = span.file_id.index();
        if id >= 0xFFFF_F000 {
            return true;
        }
        match self.file_paths.get(&span.file_id) {
            Some(path) => is_prelude_path(path),
            // Without a registered path, default to denying — user
            // files always have a path; absence implies a virtual or
            // ungated source we don't trust to use `@lang(...)`.
            None => false,
        }
    }
}

#[derive(Clone, Copy)]
enum DeclKind {
    Interface,
    Enum,
    Fn,
}
