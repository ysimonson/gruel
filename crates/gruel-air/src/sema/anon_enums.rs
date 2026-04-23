//! Anonymous enum handling.
//!
//! This module implements structural type equality for anonymous enums.
//! Two anonymous enums with the same variant names/types (in order) AND
//! the same method signatures AND the same captured comptime values are the same type.

use std::collections::HashMap;

use lasso::Spur;

use crate::sema::context::ConstValue;
use crate::types::{EnumDef, EnumVariantDef, Type};

use super::Sema;
use super::info::AnonMethodSig;

impl Sema<'_> {
    /// Find an existing anonymous enum with the same variants, methods, and captured values, or create a new one.
    ///
    /// This implements structural type equality for anonymous enums: two anonymous
    /// enums with the same variant names/types (in the same order) AND the same method
    /// signatures AND the same captured comptime values are the same type.
    ///
    /// Returns a tuple of (Type, is_new) where is_new indicates whether the enum was
    /// newly created (true) or an existing match was found (false). Callers should only
    /// register methods for newly created enums.
    pub(crate) fn find_or_create_anon_enum(
        &mut self,
        variants: &[EnumVariantDef],
        method_sigs: &[AnonMethodSig],
        captured_values: &HashMap<Spur, ConstValue>,
    ) -> (Type, bool) {
        // Check if an equivalent anonymous enum already exists
        // Anonymous enums have names starting with "__anon_enum_"
        for enum_id in self.type_pool.all_enum_ids() {
            let enum_def = self.type_pool.enum_def(enum_id);
            if enum_def.name.starts_with("__anon_enum_") {
                // Check variants match
                if enum_def.variants.len() != variants.len() {
                    continue;
                }
                let mut variants_match = true;
                for (def_var, new_var) in enum_def.variants.iter().zip(variants.iter()) {
                    if def_var.name != new_var.name
                        || def_var.fields != new_var.fields
                        || def_var.field_names != new_var.field_names
                    {
                        variants_match = false;
                        break;
                    }
                }
                if !variants_match {
                    continue;
                }

                // Check method signatures match
                let existing_sigs = self
                    .anon_enum_method_sigs
                    .get(&enum_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                if existing_sigs.len() != method_sigs.len() {
                    continue;
                }
                let mut methods_match = true;
                for (existing, new) in existing_sigs.iter().zip(method_sigs.iter()) {
                    if existing != new {
                        methods_match = false;
                        break;
                    }
                }
                if !methods_match {
                    continue;
                }

                // Check captured comptime values match
                let empty_map = HashMap::new();
                let existing_captures = self
                    .anon_enum_captured_values
                    .get(&enum_id)
                    .unwrap_or(&empty_map);
                if existing_captures.len() != captured_values.len() {
                    continue;
                }
                let mut captures_match = true;
                for (key, new_val) in captured_values.iter() {
                    if let Some(existing_val) = existing_captures.get(key) {
                        if existing_val != new_val {
                            captures_match = false;
                            break;
                        }
                    } else {
                        captures_match = false;
                        break;
                    }
                }
                if captures_match {
                    // Found a matching enum - return it with is_new=false
                    return (Type::new_enum(enum_id), false);
                }
            }
        }

        // No matching enum found - create a new one
        let name = format!("__anon_enum_{}", self.type_pool.all_enum_ids().len());
        let name_spur = self.interner.get_or_intern(&name);

        let enum_def = EnumDef {
            name,
            variants: variants.to_vec(),
            is_pub: false,
            file_id: gruel_span::FileId::new(0),
            destructor: None,
        };

        let (enum_id, _) = self.type_pool.register_enum(name_spur, enum_def);

        // Store method signatures for future structural equality checks
        if !method_sigs.is_empty() {
            self.anon_enum_method_sigs
                .insert(enum_id, method_sigs.to_vec());
        }

        // Store captured comptime values for future structural equality checks
        if !captured_values.is_empty() {
            self.anon_enum_captured_values
                .insert(enum_id, captured_values.clone());
        }

        // Register in enum lookup
        self.enums.insert(name_spur, enum_id);

        // Return with is_new=true
        (Type::new_enum(enum_id), true)
    }
}
