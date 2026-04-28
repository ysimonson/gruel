//! Type checking and resolution helpers for semantic analysis.
//!
//! This module contains helper functions for:
//! - Resolving type symbols to concrete types
//! - Type checking (is_copy, format_type_name)
//! - ABI slot calculations
//! - Type conversions between AIR types and inference types

use gruel_error::{CompileError, CompileResult, ErrorKind};
use gruel_span::Span;
use lasso::Spur;

use super::Sema;
use crate::inference::InferType;
use crate::types::{
    ArrayTypeId, IfaceTy, Type, TypeKind, parse_array_type_syntax, parse_tuple_type_syntax,
    parse_type_call_syntax,
};

impl<'a> Sema<'a> {
    /// Get a human-readable name for a type.
    pub(crate) fn format_type_name(&self, ty: Type) -> String {
        self.type_pool.format_type_name(ty)
    }

    /// Check if a type is a Copy type.
    /// This differs from Type::is_copy() because it can look up struct definitions
    /// to check if a struct is marked with @copy.
    pub(crate) fn is_type_copy(&self, ty: Type) -> bool {
        match ty.kind() {
            // Primitive Copy types
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Isize
            | TypeKind::Usize
            | TypeKind::F16
            | TypeKind::F32
            | TypeKind::F64
            | TypeKind::Bool
            | TypeKind::Unit => true,
            // Enum types are Copy (they're small discriminant values)
            TypeKind::Enum(_) => true,
            // ComptimeInt is Copy (like ComptimeStr)
            TypeKind::ComptimeInt => true,
            // Never and Error are Copy for convenience
            TypeKind::Never | TypeKind::Error => true,
            // Struct types: check if marked with @copy
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def.is_copy
            }
            // Note: String is now handled via TypeKind::Struct with is_builtin
            // Arrays are Copy if their element type is Copy
            TypeKind::Array(array_id) => {
                let (element_type, _length) = self.type_pool.array_def(array_id);
                self.is_type_copy(element_type)
            }
            // Module types are Copy (they're just compile-time namespace references)
            TypeKind::Module(_) => true,
            // ComptimeType and ComptimeStr are Copy (only exist at comptime anyway)
            TypeKind::ComptimeType | TypeKind::ComptimeStr => true,
            // Pointer types are Copy (they're just addresses)
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => true,
            // References (ADR-0062) are Copy — they're scope-bound aliases,
            // not owning handles. Borrow-checking enforces exclusivity, not
            // affinity.
            TypeKind::Ref(_) | TypeKind::MutRef(_) => true,
            // Interface types (ADR-0056): the fat pointer is two pointer-
            // sized values. Bitwise-copying it is safe — it just produces a
            // second reference to the same underlying data via the data
            // pointer, which is the same ownership posture as the original
            // borrow. Treating as Copy lets the receiver be used as a method
            // call argument without triggering "move out of borrow" errors.
            TypeKind::Interface(_) => true,
            // Slices (ADR-0064) are Copy — they're scope-bound fat pointers
            // (ptr + len). Bitwise-copying is safe; borrow-checking enforces
            // exclusivity.
            TypeKind::Slice(_) | TypeKind::MutSlice(_) => true,
        }
    }

    /// Convert a fully-resolved InferType to a concrete Type.
    ///
    /// This handles the conversion of InferType::Array to Type::new_array(id)
    /// by using the array type registry.
    pub(crate) fn infer_type_to_type(&mut self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::ERROR,   // Unbound variable
            InferType::IntLiteral => Type::I32, // Default (shouldn't happen after resolution)
            InferType::FloatLiteral => Type::F64, // Default (shouldn't happen after resolution)
            InferType::Array { element, length } => {
                // Recursively convert element type
                let elem_ty = self.infer_type_to_type(element);
                if elem_ty == Type::ERROR {
                    return Type::ERROR;
                }
                // Get or create the array type ID
                let array_type_id = self.get_or_create_array_type(elem_ty, *length);
                Type::new_array(array_type_id)
            }
        }
    }

    /// Convert a concrete Type to InferType for use in constraint generation.
    ///
    /// This handles the conversion of Type::new_array(id) to InferType::Array
    /// by looking up the array definition to get element type and length.
    pub(crate) fn type_to_infer_type(&self, ty: Type) -> InferType {
        match ty.kind() {
            TypeKind::Array(array_id) => {
                let (element_type, length) = self.type_pool.array_def(array_id);
                let element_infer = self.type_to_infer_type(element_type);
                InferType::Array {
                    element: Box::new(element_infer),
                    length,
                }
            }
            // ComptimeInt coerces to any integer type (like an integer literal)
            TypeKind::ComptimeInt => InferType::IntLiteral,
            // All other types wrap directly
            _ => InferType::Concrete(ty),
        }
    }
    /// Resolve a parameter type symbol, accepting interface names when the
    /// parameter mode is `inout` or `borrow` (ADR-0056 Phase 4).
    ///
    /// Outside of parameter positions, callers should use `resolve_type`
    /// directly — interfaces are not yet legal as field types, return
    /// types, or local-binding types.
    pub(crate) fn resolve_param_type(
        &mut self,
        type_sym: Spur,
        mode: gruel_rir::RirParamMode,
        span: Span,
    ) -> CompileResult<Type> {
        if let Some(&interface_id) = self.interfaces.get(&type_sym) {
            match mode {
                gruel_rir::RirParamMode::Inout | gruel_rir::RirParamMode::Borrow => {
                    Ok(Type::new_interface(interface_id))
                }
                _ => {
                    let name = self.interner.resolve(&type_sym).to_string();
                    Err(CompileError::new(
                        ErrorKind::UnknownType(name.clone()),
                        span,
                    )
                    .with_help(format!(
                        "interface-typed parameters require a borrow mode: use `borrow t: {}` or `inout t: {}`. By-value `t: {}` is not supported (ADR-0056).",
                        name, name, name
                    )))
                }
            }
        } else {
            self.resolve_type(type_sym, span)
        }
    }

    /// Resolve a type slot inside an interface method signature (ADR-0060).
    ///
    /// Recognizes the symbol `Self` and returns `IfaceTy::SelfType`.
    /// All other symbols flow through `resolve_type` and are wrapped in
    /// `IfaceTy::Concrete`.
    pub(crate) fn resolve_iface_ty(
        &mut self,
        type_sym: Spur,
        span: Span,
    ) -> CompileResult<IfaceTy> {
        if self.interner.resolve(&type_sym) == "Self" {
            return Ok(IfaceTy::SelfType);
        }
        Ok(IfaceTy::Concrete(self.resolve_type(type_sym, span)?))
    }

    /// Resolve a type symbol to a Type.
    ///
    /// Handles array types with the syntax "[T; N]".
    pub(crate) fn resolve_type(&mut self, type_sym: Spur, span: Span) -> CompileResult<Type> {
        let type_name = self.interner.resolve(&type_sym);

        // Check primitive types first.
        // Note: String is handled below via struct lookup (it's a builtin struct).
        match type_name {
            "i8" => return Ok(Type::I8),
            "i16" => return Ok(Type::I16),
            "i32" => return Ok(Type::I32),
            "i64" => return Ok(Type::I64),
            "u8" => return Ok(Type::U8),
            "u16" => return Ok(Type::U16),
            "u32" => return Ok(Type::U32),
            "u64" => return Ok(Type::U64),
            "isize" => return Ok(Type::ISIZE),
            "usize" => return Ok(Type::USIZE),
            "f16" => return Ok(Type::F16),
            "f32" => return Ok(Type::F32),
            "f64" => return Ok(Type::F64),
            "bool" => return Ok(Type::BOOL),
            "()" => return Ok(Type::UNIT),
            "!" => return Ok(Type::NEVER),
            // The type of types - used for comptime type parameters
            "type" => return Ok(Type::COMPTIME_TYPE),
            _ => {}
        }

        if let Some(&struct_id) = self.structs.get(&type_sym) {
            Ok(Type::new_struct(struct_id))
        } else if let Some(&enum_id) = self.enums.get(&type_sym) {
            Ok(Type::new_enum(enum_id))
        } else if self.interfaces.contains_key(&type_sym) {
            // ADR-0056: interfaces are usable as RUNTIME types only in
            // parameter positions with `borrow`/`inout` mode (Phase 4).
            // The general `resolve_type` path rejects them — callers that
            // accept them (`collect_function_signature` / method gather)
            // call `resolve_param_type` instead.
            Err(CompileError::new(
                ErrorKind::UnknownType(type_name.to_string()),
                span,
            )
            .with_help(format!(
                "`{}` is an interface, not a value type. Use `comptime T: {}` for compile-time generics, or `borrow t: {}` / `inout t: {}` in a parameter position for runtime dispatch.",
                type_name, type_name, type_name, type_name
            )))
        } else if let Some((callee_name, arg_strs)) = parse_type_call_syntax(type_name) {
            // ADR-0061: built-in parameterized types (`Ptr(T)`, `MutPtr(T)`).
            // These short-circuit the comptime-evaluation path because they
            // lower to fixed `TypeKind` variants rather than running through
            // a `fn ... -> type` body.
            if let Some(constructor) = gruel_builtins::get_builtin_type_constructor(&callee_name) {
                use gruel_builtins::BuiltinTypeConstructorKind;
                if arg_strs.len() != constructor.arity {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: constructor.arity,
                            found: arg_strs.len(),
                        },
                        span,
                    ));
                }
                let mut arg_types: Vec<Type> = Vec::with_capacity(arg_strs.len());
                for arg_str in &arg_strs {
                    let arg_sym = self.interner.get_or_intern(arg_str);
                    let arg_ty = self.resolve_type(arg_sym, span)?;
                    arg_types.push(arg_ty);
                }
                return match constructor.kind {
                    BuiltinTypeConstructorKind::Ptr => {
                        let ptr_id = self.type_pool.intern_ptr_const_from_type(arg_types[0]);
                        Ok(Type::new_ptr_const(ptr_id))
                    }
                    BuiltinTypeConstructorKind::MutPtr => {
                        let ptr_id = self.type_pool.intern_ptr_mut_from_type(arg_types[0]);
                        Ok(Type::new_ptr_mut(ptr_id))
                    }
                    BuiltinTypeConstructorKind::Ref => {
                        let ref_id = self.type_pool.intern_ref_from_type(arg_types[0]);
                        Ok(Type::new_ref(ref_id))
                    }
                    BuiltinTypeConstructorKind::MutRef => {
                        let ref_id = self.type_pool.intern_mut_ref_from_type(arg_types[0]);
                        Ok(Type::new_mut_ref(ref_id))
                    }
                    BuiltinTypeConstructorKind::Slice => {
                        let slice_id = self.type_pool.intern_slice_from_type(arg_types[0]);
                        Ok(Type::new_slice(slice_id))
                    }
                    BuiltinTypeConstructorKind::MutSlice => {
                        let slice_id = self.type_pool.intern_mut_slice_from_type(arg_types[0]);
                        Ok(Type::new_mut_slice(slice_id))
                    }
                };
            }

            // ADR-0057: parameterized type call `Name(arg1, arg2, ...)` in
            // type position. Evaluate the callee at comptime with the
            // resolved arguments substituted in for its comptime params.
            let callee_sym = self.interner.get_or_intern(&callee_name);
            let fn_info = match self.functions.get(&callee_sym).copied() {
                Some(info) => info,
                None => {
                    return Err(CompileError::new(
                        ErrorKind::UnknownType(type_name.to_string()),
                        span,
                    )
                    .with_help(format!(
                        "`{}` is not a function in scope. Parameterized type calls require a `fn ... -> type` declaration.",
                        callee_name
                    )));
                }
            };
            // Resolve each argument to a Type. Args are themselves type
            // expressions, so we recursively resolve them.
            let mut arg_types: Vec<Type> = Vec::with_capacity(arg_strs.len());
            for arg_str in &arg_strs {
                let arg_sym = self.interner.get_or_intern(arg_str);
                let arg_ty = self.resolve_type(arg_sym, span)?;
                arg_types.push(arg_ty);
            }
            // Build a substitution map from the callee's comptime param
            // names to the resolved argument types. The arg list must
            // match the comptime-typed param list in count.
            let param_comptime = self.param_arena.comptime(fn_info.params).to_vec();
            let param_names = self.param_arena.names(fn_info.params).to_vec();
            let comptime_param_names: Vec<lasso::Spur> = param_names
                .iter()
                .zip(param_comptime.iter())
                .filter_map(|(n, &c)| if c { Some(*n) } else { None })
                .collect();
            if comptime_param_names.len() != arg_types.len() {
                return Err(CompileError::new(
                    ErrorKind::WrongArgumentCount {
                        expected: comptime_param_names.len(),
                        found: arg_types.len(),
                    },
                    span,
                ));
            }
            let mut subst: std::collections::HashMap<lasso::Spur, Type> =
                std::collections::HashMap::new();
            for (n, t) in comptime_param_names.iter().zip(arg_types.iter()) {
                subst.insert(*n, *t);
            }
            // Evaluate the function body at comptime with the substitution.
            // The body must produce a `Type` value.
            let value_subst: std::collections::HashMap<lasso::Spur, super::ConstValue> =
                std::collections::HashMap::new();
            match self.try_evaluate_const_with_subst(fn_info.body, &subst, &value_subst) {
                Some(super::ConstValue::Type(t)) => Ok(t),
                _ => Err(
                    CompileError::new(ErrorKind::UnknownType(type_name.to_string()), span)
                        .with_help(format!(
                            "`{}` did not evaluate to a `type` value at compile time.",
                            type_name
                        )),
                ),
            }
        } else {
            // Check for array type syntax: [T; N]
            if let Some((element_type, length)) = parse_array_type_syntax(type_name) {
                // Resolve the element type first
                let element_sym = self.interner.get_or_intern(&element_type);
                let element_ty = self.resolve_type(element_sym, span)?;
                // Get or create the array type
                let array_type_id = self.get_or_create_array_type(element_ty, length);
                Ok(Type::new_array(array_type_id))
            } else if let Some(pointee_type_str) = type_name.strip_prefix("ptr const ") {
                // Pointer type syntax: ptr const T
                let pointee_sym = self.interner.get_or_intern(pointee_type_str);
                let pointee_ty = self.resolve_type(pointee_sym, span)?;
                let ptr_type_id = self.type_pool.intern_ptr_const_from_type(pointee_ty);
                Ok(Type::new_ptr_const(ptr_type_id))
            } else if let Some(pointee_type_str) = type_name.strip_prefix("ptr mut ") {
                // Pointer type syntax: ptr mut T
                let pointee_sym = self.interner.get_or_intern(pointee_type_str);
                let pointee_ty = self.resolve_type(pointee_sym, span)?;
                let ptr_type_id = self.type_pool.intern_ptr_mut_from_type(pointee_ty);
                Ok(Type::new_ptr_mut(ptr_type_id))
            } else if let Some(elems) = parse_tuple_type_syntax(type_name) {
                // Tuple type syntax: (T, U, ...) — ADR-0048.
                // Resolve to an anonymous struct with fields "0", "1", ...
                let mut struct_fields = Vec::with_capacity(elems.len());
                for (i, elem_str) in elems.iter().enumerate() {
                    let elem_sym = self.interner.get_or_intern(elem_str);
                    let elem_ty = self.resolve_type(elem_sym, span)?;
                    struct_fields.push(crate::types::StructField {
                        name: i.to_string(),
                        ty: elem_ty,
                    });
                }
                let (ty, _is_new) = self.find_or_create_anon_struct(
                    &struct_fields,
                    &[],
                    &std::collections::HashMap::new(),
                );
                Ok(ty)
            } else {
                Err(CompileError::new(
                    ErrorKind::UnknownType(type_name.to_string()),
                    span,
                ))
            }
        }
    }

    /// Resolve a type symbol to a Type, returning None if the type is unknown.
    ///
    /// This is used in comptime evaluation where we can't produce a compile error.
    pub(crate) fn resolve_type_for_comptime(&mut self, type_sym: Spur) -> Option<Type> {
        self.resolve_type_for_comptime_with_subst(type_sym, &std::collections::HashMap::new())
    }

    /// Resolve a type symbol to a Type with type parameter substitution.
    ///
    /// This is used in comptime evaluation of generic functions where type parameters
    /// need to be substituted with their concrete types. For example, when evaluating
    /// `fn Pair(comptime T: type) -> type { struct { first: T, second: T } }` with T=i32,
    /// we need to resolve `T` to `i32`.
    pub(crate) fn resolve_type_for_comptime_with_subst(
        &mut self,
        type_sym: Spur,
        type_subst: &std::collections::HashMap<Spur, Type>,
    ) -> Option<Type> {
        // First check the substitution map for type parameters
        if let Some(&ty) = type_subst.get(&type_sym) {
            return Some(ty);
        }

        let type_name = self.interner.resolve(&type_sym);

        // Check primitive types first
        match type_name {
            "i8" => return Some(Type::I8),
            "i16" => return Some(Type::I16),
            "i32" => return Some(Type::I32),
            "i64" => return Some(Type::I64),
            "isize" => return Some(Type::ISIZE),
            "u8" => return Some(Type::U8),
            "u16" => return Some(Type::U16),
            "u32" => return Some(Type::U32),
            "u64" => return Some(Type::U64),
            "usize" => return Some(Type::USIZE),
            "f16" => return Some(Type::F16),
            "f32" => return Some(Type::F32),
            "f64" => return Some(Type::F64),
            "bool" => return Some(Type::BOOL),
            "()" => return Some(Type::UNIT),
            "!" => return Some(Type::NEVER),
            "type" => return Some(Type::COMPTIME_TYPE),
            _ => {}
        }

        if let Some(&struct_id) = self.structs.get(&type_sym) {
            Some(Type::new_struct(struct_id))
        } else if let Some(&enum_id) = self.enums.get(&type_sym) {
            Some(Type::new_enum(enum_id))
        } else if let Some((element_type, length)) = parse_array_type_syntax(type_name) {
            // Resolve the element type first
            let element_sym = self.interner.get_or_intern(&element_type);
            let element_ty = self.resolve_type_for_comptime_with_subst(element_sym, type_subst)?;
            // Get or create the array type
            let array_type_id = self.get_or_create_array_type(element_ty, length);
            Some(Type::new_array(array_type_id))
        } else if let Some(pointee_type_str) = type_name.strip_prefix("ptr const ") {
            // Pointer type syntax: ptr const T
            let pointee_sym = self.interner.get_or_intern(pointee_type_str);
            let pointee_ty = self.resolve_type_for_comptime_with_subst(pointee_sym, type_subst)?;
            let ptr_type_id = self.type_pool.intern_ptr_const_from_type(pointee_ty);
            Some(Type::new_ptr_const(ptr_type_id))
        } else if let Some(pointee_type_str) = type_name.strip_prefix("ptr mut ") {
            // Pointer type syntax: ptr mut T
            let pointee_sym = self.interner.get_or_intern(pointee_type_str);
            let pointee_ty = self.resolve_type_for_comptime_with_subst(pointee_sym, type_subst)?;
            let ptr_type_id = self.type_pool.intern_ptr_mut_from_type(pointee_ty);
            Some(Type::new_ptr_mut(ptr_type_id))
        } else {
            None // Unknown type
        }
    }

    /// Get or create an array type for the given element type and length.
    pub(crate) fn get_or_create_array_type(
        &mut self,
        element_type: Type,
        length: u64,
    ) -> ArrayTypeId {
        self.type_pool.intern_array_from_type(element_type, length)
    }

    /// Pre-create array types from a resolved InferType.
    ///
    /// This walks the InferType recursively and ensures all array types that will
    /// be needed during `infer_type_to_type` conversion are created beforehand.
    /// This separation enables future parallelization of function analysis, where
    /// all mutations happen in this pre-collection phase.
    pub(crate) fn pre_create_array_types_from_infer_type(&mut self, ty: &InferType) {
        match ty {
            InferType::Array { element, length } => {
                // First recursively process nested array types (e.g., [[i32; 3]; 4])
                self.pre_create_array_types_from_infer_type(element);

                // Convert the element type to get the concrete Type
                // (This is safe because we processed nested arrays first)
                let elem_ty = self.infer_type_to_concrete_type_for_key(element);
                if elem_ty != Type::ERROR {
                    // Pre-create this array type
                    self.get_or_create_array_type(elem_ty, *length);
                }
            }
            InferType::Concrete(_)
            | InferType::Var(_)
            | InferType::IntLiteral
            | InferType::FloatLiteral => {
                // Non-array types don't need pre-creation
            }
        }
    }

    /// Convert an InferType to a concrete Type for use as an array element key.
    ///
    /// This is a helper for `pre_create_array_types_from_infer_type` that converts
    /// the element type without mutating `self.array_types` (since we're in a
    /// pre-creation context where the array type may not exist yet).
    pub(crate) fn infer_type_to_concrete_type_for_key(&self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::ERROR,   // Unbound variable
            InferType::IntLiteral => Type::I32, // Default
            InferType::FloatLiteral => Type::F64, // Default
            InferType::Array { element, length } => {
                // For nested arrays, look up or create the array type
                let elem_ty = self.infer_type_to_concrete_type_for_key(element);
                if elem_ty == Type::ERROR {
                    return Type::ERROR;
                }
                // Get or create the array type in the pool
                let id = self.type_pool.intern_array_from_type(elem_ty, *length);
                Type::new_array(id)
            }
        }
    }

    /// Get the number of ABI slots required for a type.
    /// Scalar types (i8, i16, i32, i64, u8, u16, u32, u64, bool) use 1 slot,
    /// structs use 1 slot per field, arrays use 1 slot per element.
    /// Zero-sized types (unit, never, empty structs, zero-length arrays) use 0 slots.
    pub(crate) fn abi_slot_count(&self, ty: Type) -> u32 {
        match ty.kind() {
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Isize
            | TypeKind::Usize
            | TypeKind::F16
            | TypeKind::F32
            | TypeKind::F64
            | TypeKind::Bool
            | TypeKind::Error => 1,
            // Zero-sized types use 0 slots
            // ComptimeType/ComptimeStr/ComptimeInt are comptime-only and use 0 runtime slots
            TypeKind::Unit
            | TypeKind::Never
            | TypeKind::ComptimeType
            | TypeKind::ComptimeStr
            | TypeKind::ComptimeInt => 0,
            // Enums are represented as their discriminant type (a scalar), so 1 slot
            TypeKind::Enum(_) => 1,
            // Struct uses sum of all field slots (includes builtin String with 3 fields)
            TypeKind::Struct(struct_id) => {
                // Sum the slot counts of all fields (handles arrays, nested structs, and builtins)
                // Empty structs naturally get 0 slots here
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def
                    .fields
                    .iter()
                    .map(|f| self.abi_slot_count(f.ty))
                    .sum()
            }
            TypeKind::Array(array_type_id) => {
                // Zero-length arrays naturally get 0 slots (0 * element_slots)
                let (element_type, length) = self.type_pool.array_def(array_type_id);
                let element_slots = self.abi_slot_count(element_type);
                element_slots * length as u32
            }
            // Module types don't take ABI slots (they're compile-time only)
            TypeKind::Module(_) => 0,
            // Pointer types take 1 slot (64-bit address)
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => 1,
            // References (ADR-0062) lower like borrows — single pointer slot.
            TypeKind::Ref(_) | TypeKind::MutRef(_) => 1,
            // Interface types (ADR-0056): runtime fat pointer occupies two
            // pointer-sized ABI slots — `(data_ptr, vtable_ptr)`. Comptime
            // usage is erased before codegen, so this only fires for
            // runtime-dispatched interface params.
            TypeKind::Interface(_) => 2,
            // Slices (ADR-0064): fat pointer `{ptr, len}` occupies 2 slots.
            TypeKind::Slice(_) | TypeKind::MutSlice(_) => 2,
        }
    }
}
