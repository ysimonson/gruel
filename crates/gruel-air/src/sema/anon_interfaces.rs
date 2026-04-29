//! Anonymous interface handling (ADR-0057).
//!
//! Implements structural type equality and comptime construction for
//! anonymous interface types — `interface { fn name(self) -> T; ... }`
//! expressions evaluated inside `fn ... -> type` comptime bodies.
//!
//! Two anonymous interfaces with the same method requirements (name +
//! param types + return type, in declaration order) are the same type.
//! Names are synthetic (`__anon_iface_<n>`) but never affect identity.

use rustc_hash::FxHashMap as HashMap;

use gruel_error::{CompileError, CompileResult, ErrorKind};
use gruel_rir::InstData;
use gruel_span::Span;
use lasso::Spur;

use super::Sema;
use crate::types::{IfaceTy, InterfaceDef, InterfaceId, InterfaceMethodReq, ReceiverMode, Type};

/// Decode a `RirParamMode`-style byte (0/1/2) into a [`ReceiverMode`].
/// Falls back to `ByValue` for unrecognized values. Shared by interface
/// validation and method gather paths (ADR-0060).
pub(crate) fn decode_receiver_mode(byte: u8) -> ReceiverMode {
    match byte {
        1 => ReceiverMode::Inout,
        2 => ReceiverMode::Borrow,
        _ => ReceiverMode::ByValue,
    }
}

impl<'a> Sema<'a> {
    /// Resolve the methods of an anonymous interface (the method-sig
    /// instructions referenced from `methods_start..+methods_len`) under
    /// the supplied comptime substitution map, producing a list of
    /// `InterfaceMethodReq`.
    ///
    /// `subst` substitutes type parameter names (e.g. `T` → `i32`) in the
    /// method's parameter and return types. Pass an empty map for
    /// non-comptime contexts.
    pub(crate) fn build_anon_interface_def(
        &mut self,
        methods_start: u32,
        methods_len: u32,
        span: Span,
        subst: &HashMap<Spur, Type>,
    ) -> CompileResult<Vec<InterfaceMethodReq>> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len).to_vec();
        let mut out: Vec<InterfaceMethodReq> = Vec::with_capacity(method_refs.len());
        let mut seen: rustc_hash::FxHashSet<String> = {
            let mut s = rustc_hash::FxHashSet::default();
            s.reserve(method_refs.len());
            s
        };
        for method_ref in method_refs {
            let m = self.rir.get(method_ref);
            let m_span = m.span;
            let InstData::InterfaceMethodSig {
                name,
                params_start,
                params_len,
                return_type,
                receiver_mode,
            } = &m.data
            else {
                return Err(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "anonymous interface method ref does not point at \
                         InterfaceMethodSig: {:?}",
                        m.data
                    )),
                    m_span,
                ));
            };
            let name = *name;
            let params_start = *params_start;
            let params_len = *params_len;
            let return_type_sym = *return_type;
            let receiver = decode_receiver_mode(*receiver_mode);

            let name_str = self.interner.resolve(&name).to_string();
            if !seen.insert(name_str.clone()) {
                return Err(CompileError::new(
                    ErrorKind::DuplicateMethod {
                        type_name: "anonymous interface".to_string(),
                        method_name: name_str,
                    },
                    m_span,
                ));
            }

            let params: Vec<(Spur, Spur)> = self
                .rir
                .get_params(params_start, params_len)
                .into_iter()
                .map(|p| (p.name, p.ty))
                .collect();
            let mut param_types = Vec::with_capacity(params.len());
            for (_pname, ty_sym) in &params {
                param_types.push(self.resolve_iface_with_subst(*ty_sym, span, subst)?);
            }
            let return_type = self.resolve_iface_with_subst(return_type_sym, span, subst)?;
            out.push(InterfaceMethodReq {
                name: name_str,
                receiver,
                param_types,
                return_type,
            });
        }
        Ok(out)
    }

    /// Resolve a type symbol, applying the comptime substitution map first.
    /// Falls back to `resolve_type` when no substitution applies.
    fn resolve_with_subst(
        &mut self,
        ty_sym: Spur,
        span: Span,
        subst: &HashMap<Spur, Type>,
    ) -> CompileResult<Type> {
        if let Some(&ty) = subst.get(&ty_sym) {
            return Ok(ty);
        }
        // Try the comptime resolver first (it handles `T` symbols that name
        // a comptime type variable). Fall back to the regular resolver.
        if let Some(ty) = self.resolve_type_for_comptime_with_subst(ty_sym, subst) {
            return Ok(ty);
        }
        self.resolve_type(ty_sym, span)
    }

    /// Resolve a type slot inside an anonymous interface signature, mirroring
    /// `resolve_with_subst` but yielding `IfaceTy` so that `Self` survives
    /// resolution unwrapped (ADR-0060).
    fn resolve_iface_with_subst(
        &mut self,
        ty_sym: Spur,
        span: Span,
        subst: &HashMap<Spur, Type>,
    ) -> CompileResult<IfaceTy> {
        if self.interner.resolve(&ty_sym) == "Self" {
            return Ok(IfaceTy::SelfType);
        }
        Ok(IfaceTy::Concrete(
            self.resolve_with_subst(ty_sym, span, subst)?,
        ))
    }

    /// Look up an existing anonymous interface that matches the supplied
    /// method requirements structurally, or register a fresh one and return
    /// its id.
    pub(crate) fn find_or_create_anon_interface(
        &mut self,
        methods: Vec<InterfaceMethodReq>,
    ) -> InterfaceId {
        // Search existing anonymous interfaces. We restrict the scan to
        // synthetic names — anonymous interfaces always start with the
        // marker prefix. Named interfaces are nominal and never collapse
        // with anonymous ones.
        for (i, def) in self.interface_defs.iter().enumerate() {
            if !def.name.starts_with("__anon_iface_") {
                continue;
            }
            if def.methods.len() != methods.len() {
                continue;
            }
            let mut all_match = true;
            for (a, b) in def.methods.iter().zip(methods.iter()) {
                if a.name != b.name
                    || a.return_type != b.return_type
                    || a.param_types != b.param_types
                {
                    all_match = false;
                    break;
                }
            }
            if all_match {
                return InterfaceId(i as u32);
            }
        }

        let id = InterfaceId(self.interface_defs.len() as u32);
        let name = format!("__anon_iface_{}", id.0);
        self.interface_defs.push(InterfaceDef {
            name,
            methods,
            is_pub: false,
            file_id: gruel_span::FileId::new(0),
        });
        id
    }
}
