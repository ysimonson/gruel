//! Type intern pool for efficient type representation.
//!
//! This module implements a unified type interning system inspired by Zig's `InternPool`.
//! All types become 32-bit indices into a canonical pool, enabling:
//!
//! - O(1) type equality (u32 comparison)
//! - Efficient memory usage
//! - Clean parallel compilation (no per-function type merging)
//! - Foundation for future generics
//!
//! # Architecture
//!
//! The `TypeInternPool` serves as a canonical repository for all composite types:
//! - **Structs and enums** are nominal types (same name = same type)
//! - **Arrays** are structural types (same element type + length = same type)
//!
//! Primitive types (i8-i64, u8-u64, bool, unit, never, error) are encoded directly
//! in the `InternedType` index using reserved indices 0-15, requiring no pool lookup.
//!
//! # Migration Strategy (ADR-0024)
//!
//! This module is part of Phase 1 of the Type Intern Pool migration:
//! - Phase 1: Introduce pool alongside existing system (this module)
//! - Phase 2: Migrate array types to the pool
//! - Phase 3: Migrate struct/enum IDs to pool indices
//! - Phase 4: Unify Type representation to `InternedType(u32)`
//!
//! During Phase 1, the pool coexists with the existing `Type` enum, `StructId`,
//! `EnumId`, and `ArrayTypeId`. The pool is populated during declaration collection
//! but not yet used for type operations.
//!
//! # Thread Safety
//!
//! The pool uses `RwLock` for thread-safe access during parallel compilation:
//! - Read lock for lookups (common case)
//! - Write lock for insertions (rare, during declaration gathering)

use rustc_hash::FxHashMap as HashMap;
use std::sync::{PoisonError, RwLock};

use gruel_builtins::Posture;
use lasso::Spur;

use crate::layout::Layout;
use crate::types::{
    ArrayTypeId, EnumDef, EnumId, MutRefTypeId, MutSliceTypeId, PtrConstTypeId, PtrMutTypeId,
    RefTypeId, SliceTypeId, StructDef, StructId, Type, TypeKind, VecTypeId,
};

/// Interned type index - 32 bits, Copy, cheap comparison.
///
/// Reserved indices 0-15 are primitives (no lookup needed).
/// Index 16+ are composite types stored in the pool.
///
/// # Primitive Encoding
///
/// The following indices are reserved for primitive types:
/// - 0: i8
/// - 1: i16
/// - 2: i32
/// - 3: i64
/// - 4: u8
/// - 5: u16
/// - 6: u32
/// - 7: u64
/// - 8: isize
/// - 9: usize
/// - 10: f16
/// - 11: f32
/// - 12: f64
/// - 13: bool
/// - 14: unit
/// - 15: never
/// - 16: error
/// - 17-18: reserved for future primitives
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub struct InternedType(u32);

impl InternedType {
    // Reserved indices for primitives
    pub const I8: InternedType = InternedType(0);
    pub const I16: InternedType = InternedType(1);
    pub const I32: InternedType = InternedType(2);
    pub const I64: InternedType = InternedType(3);
    pub const U8: InternedType = InternedType(4);
    pub const U16: InternedType = InternedType(5);
    pub const U32: InternedType = InternedType(6);
    pub const U64: InternedType = InternedType(7);
    pub const ISIZE: InternedType = InternedType(8);
    pub const USIZE: InternedType = InternedType(9);
    pub const F16: InternedType = InternedType(10);
    pub const F32: InternedType = InternedType(11);
    pub const F64: InternedType = InternedType(12);
    pub const BOOL: InternedType = InternedType(13);
    pub const UNIT: InternedType = InternedType(14);
    pub const NEVER: InternedType = InternedType(15);
    pub const ERROR: InternedType = InternedType(16);
    /// ADR-0071: Unicode scalar value (`char`).
    pub const CHAR: InternedType = InternedType(20);

    // ADR-0086: C named primitive types. Slot numbers match the `Type` tag
    // encoding for these variants (21-33).
    pub const C_SCHAR: InternedType = InternedType(21);
    pub const C_SHORT: InternedType = InternedType(22);
    pub const C_INT: InternedType = InternedType(23);
    pub const C_LONG: InternedType = InternedType(24);
    pub const C_LONGLONG: InternedType = InternedType(25);
    pub const C_UCHAR: InternedType = InternedType(26);
    pub const C_USHORT: InternedType = InternedType(27);
    pub const C_UINT: InternedType = InternedType(28);
    pub const C_ULONG: InternedType = InternedType(29);
    pub const C_ULONGLONG: InternedType = InternedType(30);
    pub const C_FLOAT: InternedType = InternedType(31);
    pub const C_DOUBLE: InternedType = InternedType(32);
    pub const C_VOID: InternedType = InternedType(33);

    const PRIMITIVE_COUNT: u32 = 34;

    /// Check if this is a primitive type (no pool lookup needed).
    #[inline]
    pub fn is_primitive(self) -> bool {
        self.0 < Self::PRIMITIVE_COUNT
    }

    /// Get the raw index value.
    #[inline]
    pub fn index(self) -> u32 {
        self.0
    }

    /// Create an InternedType from a raw index.
    ///
    /// # Safety
    ///
    /// The caller must ensure the index is valid (either a primitive index 0-15,
    /// or a composite index that exists in the pool).
    #[inline]
    pub fn from_raw(index: u32) -> Self {
        InternedType(index)
    }

    /// Create an InternedType for a composite type from its pool index.
    ///
    /// The pool index is offset by `PRIMITIVE_COUNT` to produce the final index.
    #[inline]
    fn from_pool_index(pool_index: u32) -> Self {
        InternedType(pool_index + Self::PRIMITIVE_COUNT)
    }

    /// Get the pool index for a composite type.
    ///
    /// Returns `None` for primitive types.
    #[inline]
    pub fn pool_index(self) -> Option<u32> {
        if self.is_primitive() {
            None
        } else {
            Some(self.0 - Self::PRIMITIVE_COUNT)
        }
    }
}

impl std::fmt::Debug for InternedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_primitive() {
            let name = match self.0 {
                0 => "i8",
                1 => "i16",
                2 => "i32",
                3 => "i64",
                4 => "u8",
                5 => "u16",
                6 => "u32",
                7 => "u64",
                8 => "bool",
                9 => "()",
                10 => "!",
                11 => "<error>",
                _ => "<reserved>",
            };
            write!(f, "InternedType({name})")
        } else {
            write!(f, "InternedType(pool:{})", self.0 - Self::PRIMITIVE_COUNT)
        }
    }
}

/// Type data stored in the intern pool.
///
/// This is NOT Copy - it lives in the pool. You work with `InternedType` indices.
///
/// # Type Categories
///
/// - **Struct** and **Enum** are nominal types: identity comes from the name
/// - **Array**, **PtrConst**, and **PtrMut** are structural types: identity comes from element/pointee type
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum TypeData {
    /// User-defined struct (nominal type).
    ///
    /// Two structs with the same fields but different names are different types.
    Struct(StructData),

    /// User-defined enum (nominal type).
    ///
    /// Two enums with the same variants but different names are different types.
    Enum(EnumData),

    /// Fixed-size array (structural type).
    ///
    /// Arrays with the same element type and length are the same type,
    /// regardless of where they were defined.
    Array { element: InternedType, len: u64 },

    /// Raw const pointer (structural type).
    ///
    /// `ptr const T` - pointer to immutable data.
    PtrConst { pointee: InternedType },

    /// Raw mut pointer (structural type).
    ///
    /// `ptr mut T` - pointer to mutable data.
    PtrMut { pointee: InternedType },

    /// Immutable reference (structural type, ADR-0062).
    ///
    /// `Ref(T)` - scope-bound non-mutating borrow.
    Ref { referent: InternedType },

    /// Mutable reference (structural type, ADR-0062).
    ///
    /// `MutRef(T)` - scope-bound exclusive mutating borrow.
    MutRef { referent: InternedType },

    /// Immutable slice (structural type, ADR-0064).
    ///
    /// `Slice(T)` - scope-bound fat pointer `{ptr, len}`.
    Slice { element: InternedType },

    /// Mutable slice (structural type, ADR-0064).
    ///
    /// `MutSlice(T)` - scope-bound exclusive fat pointer `{ptr, len}`.
    MutSlice { element: InternedType },

    /// Owned, growable vector (structural type, ADR-0066).
    ///
    /// `Vec(T)` - heap-allocated `{ptr, len, cap}`.
    Vec { element: InternedType },
}

/// Data for a struct type in the intern pool.
///
/// During Phase 1, this mirrors the existing `StructDef` to verify correctness.
/// In later phases, `StructDef` will be replaced by this.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StructData {
    /// The name symbol (interned string).
    pub name: Spur,
    /// Reference to the full struct definition.
    /// During Phase 1, we keep a clone of the StructDef for verification.
    /// In later phases, the pool will be the canonical source.
    pub def: StructDef,
}

/// Data for an enum type in the intern pool.
///
/// During Phase 1, this mirrors the existing `EnumDef` to verify correctness.
/// In later phases, `EnumDef` will be replaced by this.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnumData {
    /// The name symbol (interned string).
    pub name: Spur,
    /// Reference to the full enum definition.
    /// During Phase 1, we keep a clone of the EnumDef for verification.
    /// In later phases, the pool will be the canonical source.
    pub def: EnumDef,
}

/// Thread-safe intern pool for all composite types.
///
/// The pool is designed to be built during declaration gathering (sequential)
/// and then queried during function body analysis (potentially parallel).
///
/// # Thread Safety
///
/// Uses `RwLock` for interior mutability:
/// - Read lock for lookups (most common)
/// - Write lock for insertions (only during declaration gathering)
///
/// # Usage
///
/// ```ignore
/// let pool = TypeInternPool::new();
///
/// // Register nominal types (structs/enums)
/// let (struct_type, is_new) = pool.register_struct(name_spur, struct_def);
///
/// // Intern structural types (arrays)
/// let array_type = pool.intern_array(element_type, 10);
///
/// // Look up type data
/// if let Some(data) = pool.try_get(some_type) {
///     match data {
///         TypeData::Struct(s) => println!("struct {}", s.def.name),
///         TypeData::Enum(e) => println!("enum {}", e.def.name),
///         TypeData::Array { element, len } => println!("array of {:?}; {}", element, len),
///     }
/// }
/// ```
#[derive(Debug)]
pub struct TypeInternPool {
    inner: RwLock<TypeInternPoolInner>,
}

// ADR-0074 Phase 4: serialize / deserialize TypeInternPool by snapshotting
// only its canonical `types: Vec<TypeData>`. The structural-dedup HashMaps
// (array_map, ptr_const_map, etc.) are reconstructed from `types` on load
// because they are pure caches over its contents. The lazy layout_cache
// starts empty after deserialization and re-populates on demand.
//
// IMPORTANT: cached InternedType values index into `types` and are stable
// only against the snapshotted pool. Loading a pool replaces — does not
// merge — the build's TypeInternPool. Any cross-file use of cached AIR
// requires a TypeId remap walker (Phase 4 follow-up).
#[derive(serde::Serialize, serde::Deserialize)]
struct TypeInternPoolWire {
    types: Vec<TypeData>,
}

impl serde::Serialize for TypeInternPool {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let wire = TypeInternPoolWire {
            types: inner.types.clone(),
        };
        wire.serialize(ser)
    }
}

impl<'de> serde::Deserialize<'de> for TypeInternPool {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let wire = TypeInternPoolWire::deserialize(de)?;
        // Reconstruct the dedup maps by walking `types` in order. Each
        // TypeData index becomes the InternedType key (offset by the
        // primitive count via try_from_index, identical to the original
        // intern path).
        let mut inner = TypeInternPoolInner {
            types: Vec::with_capacity(wire.types.len()),
            array_map: HashMap::default(),
            ptr_const_map: HashMap::default(),
            ptr_mut_map: HashMap::default(),
            ref_map: HashMap::default(),
            mut_ref_map: HashMap::default(),
            slice_map: HashMap::default(),
            mut_slice_map: HashMap::default(),
            vec_map: HashMap::default(),
            struct_by_name: HashMap::default(),
            enum_by_name: HashMap::default(),
            layout_cache: HashMap::default(),
        };
        for data in wire.types {
            // The interned index is `PRIMITIVE_COUNT + types.len()` BEFORE
            // pushing the new entry, matching the original intern path.
            let idx = InternedType(InternedType::PRIMITIVE_COUNT + inner.types.len() as u32);
            match &data {
                TypeData::Struct(s) => {
                    inner.struct_by_name.insert(s.name, idx);
                }
                TypeData::Enum(e) => {
                    inner.enum_by_name.insert(e.name, idx);
                }
                TypeData::Array { element, len } => {
                    inner.array_map.insert((*element, *len), idx);
                }
                TypeData::PtrConst { pointee } => {
                    inner.ptr_const_map.insert(*pointee, idx);
                }
                TypeData::PtrMut { pointee } => {
                    inner.ptr_mut_map.insert(*pointee, idx);
                }
                TypeData::Ref { referent } => {
                    inner.ref_map.insert(*referent, idx);
                }
                TypeData::MutRef { referent } => {
                    inner.mut_ref_map.insert(*referent, idx);
                }
                TypeData::Slice { element } => {
                    inner.slice_map.insert(*element, idx);
                }
                TypeData::MutSlice { element } => {
                    inner.mut_slice_map.insert(*element, idx);
                }
                TypeData::Vec { element } => {
                    inner.vec_map.insert(*element, idx);
                }
            }
            inner.types.push(data);
        }
        Ok(Self {
            inner: RwLock::new(inner),
        })
    }
}

#[derive(Debug)]
struct TypeInternPoolInner {
    /// All composite type data, indexed by (InternedType.0 - PRIMITIVE_COUNT).
    types: Vec<TypeData>,

    /// Structural type deduplication: (element, len) -> InternedType for arrays.
    array_map: HashMap<(InternedType, u64), InternedType>,

    /// Structural type deduplication: pointee -> InternedType for ptr const.
    ptr_const_map: HashMap<InternedType, InternedType>,

    /// Structural type deduplication: pointee -> InternedType for ptr mut.
    ptr_mut_map: HashMap<InternedType, InternedType>,

    /// Structural type deduplication: referent -> InternedType for `Ref(T)`.
    ref_map: HashMap<InternedType, InternedType>,

    /// Structural type deduplication: referent -> InternedType for `MutRef(T)`.
    mut_ref_map: HashMap<InternedType, InternedType>,

    /// Structural type deduplication: element -> InternedType for `Slice(T)`.
    slice_map: HashMap<InternedType, InternedType>,

    /// Structural type deduplication: element -> InternedType for `MutSlice(T)`.
    mut_slice_map: HashMap<InternedType, InternedType>,

    /// Structural type deduplication: element -> InternedType for `Vec(T)` (ADR-0066).
    vec_map: HashMap<InternedType, InternedType>,

    /// Nominal type lookup: name -> InternedType for structs.
    struct_by_name: HashMap<Spur, InternedType>,

    /// Nominal type lookup: name -> InternedType for enums.
    enum_by_name: HashMap<Spur, InternedType>,

    /// Cached layouts (ADR-0069). Populated lazily by `layout::layout_of`.
    /// Keyed by `Type` (a u32 index); since types are interned, the layout is
    /// a pure function of the key.
    layout_cache: HashMap<Type, Layout>,
}

impl TypeInternPool {
    /// Clone the pool by snapshotting its canonical types and
    /// reconstructing the structural-dedup HashMaps. Used by
    /// ADR-0074 Phase 4's AIR cache write to capture sema's pool
    /// state without taking ownership of it. Behavior matches
    /// serde round-trip — both produce a pool whose
    /// `intern_*(...)` calls return the same `InternedType`s as
    /// the original.
    pub fn clone_snapshot(&self) -> Self {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let mut new_inner = TypeInternPoolInner {
            types: Vec::with_capacity(inner.types.len()),
            array_map: HashMap::default(),
            ptr_const_map: HashMap::default(),
            ptr_mut_map: HashMap::default(),
            ref_map: HashMap::default(),
            mut_ref_map: HashMap::default(),
            slice_map: HashMap::default(),
            mut_slice_map: HashMap::default(),
            vec_map: HashMap::default(),
            struct_by_name: HashMap::default(),
            enum_by_name: HashMap::default(),
            layout_cache: HashMap::default(),
        };
        for data in &inner.types {
            let idx = InternedType(InternedType::PRIMITIVE_COUNT + new_inner.types.len() as u32);
            match data {
                TypeData::Struct(s) => {
                    new_inner.struct_by_name.insert(s.name, idx);
                }
                TypeData::Enum(e) => {
                    new_inner.enum_by_name.insert(e.name, idx);
                }
                TypeData::Array { element, len } => {
                    new_inner.array_map.insert((*element, *len), idx);
                }
                TypeData::PtrConst { pointee } => {
                    new_inner.ptr_const_map.insert(*pointee, idx);
                }
                TypeData::PtrMut { pointee } => {
                    new_inner.ptr_mut_map.insert(*pointee, idx);
                }
                TypeData::Ref { referent } => {
                    new_inner.ref_map.insert(*referent, idx);
                }
                TypeData::MutRef { referent } => {
                    new_inner.mut_ref_map.insert(*referent, idx);
                }
                TypeData::Slice { element } => {
                    new_inner.slice_map.insert(*element, idx);
                }
                TypeData::MutSlice { element } => {
                    new_inner.mut_slice_map.insert(*element, idx);
                }
                TypeData::Vec { element } => {
                    new_inner.vec_map.insert(*element, idx);
                }
            }
            new_inner.types.push(data.clone());
        }
        Self {
            inner: RwLock::new(new_inner),
        }
    }

    /// Create a new empty pool.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(TypeInternPoolInner {
                types: Vec::new(),
                array_map: HashMap::default(),
                ptr_const_map: HashMap::default(),
                ptr_mut_map: HashMap::default(),
                ref_map: HashMap::default(),
                mut_ref_map: HashMap::default(),
                slice_map: HashMap::default(),
                mut_slice_map: HashMap::default(),
                vec_map: HashMap::default(),
                struct_by_name: HashMap::default(),
                enum_by_name: HashMap::default(),
                layout_cache: HashMap::default(),
            }),
        }
    }

    /// Look up a cached layout, if any (ADR-0069).
    pub(crate) fn cached_layout(&self, ty: Type) -> Option<Layout> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.layout_cache.get(&ty).cloned()
    }

    /// Insert a layout into the cache (ADR-0069).
    pub(crate) fn cache_layout(&self, ty: Type, layout: Layout) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        inner.layout_cache.insert(ty, layout);
    }

    /// Register a new struct (nominal - no deduplication).
    ///
    /// Returns the `StructId` (containing the pool index) and whether it was newly inserted.
    /// If a struct with this name already exists, returns the existing StructId.
    pub fn register_struct(&self, name: Spur, def: StructDef) -> (StructId, bool) {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.struct_by_name.get(&name) {
                // Convert InternedType back to StructId via pool_index
                let pool_index = existing.pool_index().expect("struct must have pool index");
                return (StructId::from_pool_index(pool_index), false);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.struct_by_name.get(&name) {
            let pool_index = existing.pool_index().expect("struct must have pool index");
            return (StructId::from_pool_index(pool_index), false);
        }

        // Create new struct type
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::Struct(StructData { name, def }));
        inner.struct_by_name.insert(name, interned);

        (StructId::from_pool_index(pool_index), true)
    }

    /// Reserve a struct ID without registering the full definition yet.
    ///
    /// This is used for anonymous structs where we need to know the ID before
    /// we can construct the name (which includes the ID). Call `complete_struct_registration`
    /// with the reserved ID to finish registration.
    ///
    /// # Returns
    ///
    /// Returns the reserved `StructId`. The caller MUST call `complete_struct_registration`
    /// with this ID before any other pool operations that might read this entry.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let struct_id = pool.reserve_struct_id();
    /// let name = format!("__anon_struct_{}", struct_id.0);
    /// let name_spur = interner.get_or_intern(&name);
    /// let def = StructDef { name: name.clone(), ... };
    /// pool.complete_struct_registration(struct_id, name_spur, def);
    /// ```
    pub fn reserve_struct_id(&self) -> StructId {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Reserve a slot by pushing a placeholder
        // We use a placeholder Struct with empty data that will be overwritten
        let pool_index = inner.types.len() as u32;

        // Push a placeholder - this reserves the index
        // The placeholder will be replaced by complete_struct_registration
        inner.types.push(TypeData::Struct(StructData {
            name: Spur::default(),
            def: StructDef {
                name: String::new(),
                fields: vec![],
                posture: Posture::Affine,
                is_clone: false,
                thread_safety: gruel_builtins::ThreadSafety::Sync,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: gruel_util::FileId::DEFAULT,
                is_c_layout: false,
            },
        }));

        StructId::from_pool_index(pool_index)
    }

    /// Complete the registration of a previously reserved struct ID.
    ///
    /// This must be called after `reserve_struct_id` to fill in the actual struct data.
    /// The struct will be registered with the provided name for lookup purposes.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - The struct_id wasn't created by `reserve_struct_id`
    /// - The slot at struct_id doesn't contain a placeholder struct
    /// - A struct with the given name already exists
    pub fn complete_struct_registration(&self, struct_id: StructId, name: Spur, def: StructDef) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        let pool_index = struct_id.0 as usize;

        // Verify this is a valid reserved slot
        assert!(
            pool_index < inner.types.len(),
            "Invalid reserved struct ID: index {} out of bounds (len {})",
            pool_index,
            inner.types.len()
        );

        // Check that a struct with this name doesn't already exist
        assert!(
            !inner.struct_by_name.contains_key(&name),
            "Struct with this name already exists"
        );

        // Update the placeholder with actual data
        inner.types[pool_index] = TypeData::Struct(StructData { name, def });

        // Register in the name lookup
        let interned = InternedType::from_pool_index(pool_index as u32);
        inner.struct_by_name.insert(name, interned);
    }

    /// Register a new enum (nominal - no deduplication).
    ///
    /// Returns the `EnumId` (containing the pool index) and whether it was newly inserted.
    /// If an enum with this name already exists, returns the existing EnumId.
    pub fn register_enum(&self, name: Spur, def: EnumDef) -> (EnumId, bool) {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.enum_by_name.get(&name) {
                let pool_index = existing.pool_index().expect("enum must have pool index");
                return (EnumId::from_pool_index(pool_index), false);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.enum_by_name.get(&name) {
            let pool_index = existing.pool_index().expect("enum must have pool index");
            return (EnumId::from_pool_index(pool_index), false);
        }

        // Create new enum type
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::Enum(EnumData { name, def }));
        inner.enum_by_name.insert(name, interned);

        (EnumId::from_pool_index(pool_index), true)
    }

    /// Intern an array type (structural - deduplicates).
    ///
    /// Returns the canonical `InternedType` for arrays with this element type and length.
    /// If an identical array type already exists, returns the existing type.
    pub fn intern_array(&self, element: InternedType, len: u64) -> InternedType {
        let key = (element, len);

        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.array_map.get(&key) {
                return existing;
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.array_map.get(&key) {
            return existing;
        }

        // Create new array type
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::Array { element, len });
        inner.array_map.insert(key, interned);

        interned
    }

    /// Intern a ptr const type (structural - deduplicates).
    ///
    /// Returns the canonical `InternedType` for pointers to this pointee type.
    /// If an identical pointer type already exists, returns the existing type.
    pub fn intern_ptr_const(&self, pointee: InternedType) -> InternedType {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.ptr_const_map.get(&pointee) {
                return existing;
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.ptr_const_map.get(&pointee) {
            return existing;
        }

        // Create new pointer type
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::PtrConst { pointee });
        inner.ptr_const_map.insert(pointee, interned);

        interned
    }

    /// Intern a ptr mut type (structural - deduplicates).
    ///
    /// Returns the canonical `InternedType` for mutable pointers to this pointee type.
    /// If an identical pointer type already exists, returns the existing type.
    pub fn intern_ptr_mut(&self, pointee: InternedType) -> InternedType {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.ptr_mut_map.get(&pointee) {
                return existing;
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.ptr_mut_map.get(&pointee) {
            return existing;
        }

        // Create new pointer type
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::PtrMut { pointee });
        inner.ptr_mut_map.insert(pointee, interned);

        interned
    }

    /// Intern a `Ref(T)` type (structural - deduplicates).
    pub fn intern_ref(&self, referent: InternedType) -> InternedType {
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.ref_map.get(&referent) {
                return existing;
            }
        }
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(&existing) = inner.ref_map.get(&referent) {
            return existing;
        }
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);
        inner.types.push(TypeData::Ref { referent });
        inner.ref_map.insert(referent, interned);
        interned
    }

    /// Intern a `MutRef(T)` type (structural - deduplicates).
    pub fn intern_mut_ref(&self, referent: InternedType) -> InternedType {
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.mut_ref_map.get(&referent) {
                return existing;
            }
        }
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(&existing) = inner.mut_ref_map.get(&referent) {
            return existing;
        }
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);
        inner.types.push(TypeData::MutRef { referent });
        inner.mut_ref_map.insert(referent, interned);
        interned
    }

    /// Intern a `Slice(T)` type (structural - deduplicates).
    pub fn intern_slice(&self, element: InternedType) -> InternedType {
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.slice_map.get(&element) {
                return existing;
            }
        }
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(&existing) = inner.slice_map.get(&element) {
            return existing;
        }
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);
        inner.types.push(TypeData::Slice { element });
        inner.slice_map.insert(element, interned);
        interned
    }

    /// Intern a `MutSlice(T)` type (structural - deduplicates).
    pub fn intern_mut_slice(&self, element: InternedType) -> InternedType {
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.mut_slice_map.get(&element) {
                return existing;
            }
        }
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(&existing) = inner.mut_slice_map.get(&element) {
            return existing;
        }
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);
        inner.types.push(TypeData::MutSlice { element });
        inner.mut_slice_map.insert(element, interned);
        interned
    }

    /// Intern a `Vec(T)` type (structural - deduplicates) (ADR-0066).
    pub fn intern_vec(&self, element: InternedType) -> InternedType {
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.vec_map.get(&element) {
                return existing;
            }
        }
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(&existing) = inner.vec_map.get(&element) {
            return existing;
        }
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);
        inner.types.push(TypeData::Vec { element });
        inner.vec_map.insert(element, interned);
        interned
    }

    /// Look up a struct by name.
    ///
    pub fn get_struct_by_name(&self, name: Spur) -> Option<InternedType> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.struct_by_name.get(&name).copied()
    }

    /// Look up an enum by name.
    pub fn get_enum_by_name(&self, name: Spur) -> Option<InternedType> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.enum_by_name.get(&name).copied()
    }

    /// Look up an array type by element and length.
    pub fn get_array(&self, element: InternedType, len: u64) -> Option<InternedType> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.array_map.get(&(element, len)).copied()
    }

    /// Get type data for a composite type.
    ///
    /// Returns `None` for primitive types (use `InternedType::is_primitive()` first).
    ///
    /// # Panics
    ///
    /// Panics if the index is invalid.
    pub fn get(&self, ty: InternedType) -> Option<TypeData> {
        if ty.is_primitive() {
            return None;
        }

        let pool_index = ty.pool_index().expect("non-primitive must have pool index");
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        Some(inner.types[pool_index as usize].clone())
    }

    /// Check if this is a struct type.
    pub fn is_struct(&self, ty: InternedType) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(self.get(ty), Some(TypeData::Struct(_)))
    }

    /// Check if this is an enum type.
    pub fn is_enum(&self, ty: InternedType) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(self.get(ty), Some(TypeData::Enum(_)))
    }

    /// Check if this is an array type.
    pub fn is_array(&self, ty: InternedType) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(self.get(ty), Some(TypeData::Array { .. }))
    }

    /// Get the struct definition if this is a struct type.
    pub fn get_struct_def(&self, ty: InternedType) -> Option<StructDef> {
        match self.get(ty)? {
            TypeData::Struct(data) => Some(data.def),
            _ => None,
        }
    }

    /// Get the enum definition if this is an enum type.
    pub fn get_enum_def(&self, ty: InternedType) -> Option<EnumDef> {
        match self.get(ty)? {
            TypeData::Enum(data) => Some(data.def),
            _ => None,
        }
    }

    /// Get array info (element type, length) if this is an array type.
    pub fn get_array_info(&self, ty: InternedType) -> Option<(InternedType, u64)> {
        match self.get(ty)? {
            TypeData::Array { element, len } => Some((element, len)),
            _ => None,
        }
    }

    // ========================================================================
    // Phase 3 helpers: Direct StructId/EnumId access
    // ========================================================================
    //
    // These methods allow accessing struct and enum definitions directly via
    // StructId/EnumId, which now store pool indices instead of vector indices.

    /// Get a struct definition by StructId.
    ///
    /// The StructId contains a pool index. This method looks up the struct
    /// in the pool and returns a clone of its definition.
    ///
    /// # Panics
    ///
    /// Panics if the StructId doesn't correspond to a struct in the pool.
    pub fn struct_def(&self, struct_id: StructId) -> StructDef {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let pool_index = struct_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::Struct(data) => data.def.clone(),
            other => panic!(
                "Expected struct at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Get an enum definition by EnumId.
    ///
    /// The EnumId contains a pool index. This method looks up the enum
    /// in the pool and returns a clone of its definition.
    ///
    /// # Panics
    ///
    /// Panics if the EnumId doesn't correspond to an enum in the pool.
    pub fn enum_def(&self, enum_id: EnumId) -> EnumDef {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let pool_index = enum_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::Enum(data) => data.def.clone(),
            other => panic!(
                "Expected enum at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Update a struct definition in the pool.
    ///
    /// This is used during semantic analysis when struct fields are resolved
    /// after the struct is initially registered.
    ///
    /// # Panics
    ///
    /// Panics if the StructId doesn't correspond to a struct in the pool.
    pub fn update_struct_def(&self, struct_id: StructId, new_def: StructDef) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        let pool_index = struct_id.0 as usize;
        match &mut inner.types[pool_index] {
            TypeData::Struct(data) => data.def = new_def,
            other => panic!(
                "Expected struct at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Update an enum definition in the pool.
    ///
    /// This is used during semantic analysis when enum variants are resolved
    /// after the enum is initially registered.
    ///
    /// # Panics
    ///
    /// Panics if the EnumId doesn't correspond to an enum in the pool.
    pub fn update_enum_def(&self, enum_id: EnumId, new_def: EnumDef) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        let pool_index = enum_id.0 as usize;
        match &mut inner.types[pool_index] {
            TypeData::Enum(data) => data.def = new_def,
            other => panic!(
                "Expected enum at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Convert a StructId to an InternedType.
    ///
    /// Since StructId now contains a pool index, we just add the primitive offset.
    #[inline]
    pub fn struct_id_to_interned(&self, struct_id: StructId) -> InternedType {
        InternedType::from_pool_index(struct_id.0)
    }

    /// Convert an EnumId to an InternedType.
    ///
    /// Since EnumId now contains a pool index, we just add the primitive offset.
    #[inline]
    pub fn enum_id_to_interned(&self, enum_id: EnumId) -> InternedType {
        InternedType::from_pool_index(enum_id.0)
    }

    /// Get an array type definition by ArrayTypeId.
    ///
    /// The ArrayTypeId contains a pool index. This method looks up the array
    /// in the pool and returns its element type and length as a tuple.
    ///
    /// # Returns
    ///
    /// Returns `(element_type, length)` where `element_type` is the array's element type
    /// and `length` is the array's fixed size.
    ///
    /// # Panics
    ///
    /// Panics if the ArrayTypeId doesn't correspond to an array in the pool.
    pub fn array_def(&self, array_id: ArrayTypeId) -> (Type, u64) {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let pool_index = array_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::Array { element, len } => {
                // Convert InternedType back to Type
                let element_type = Self::interned_to_type_recursive(*element, &inner);
                (element_type, *len)
            }
            other => panic!(
                "Expected array at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Intern an array type from a Type element.
    ///
    /// This is a helper method that converts the Type to InternedType
    /// and then interns the array.
    ///
    /// # Panics
    ///
    /// Panics if the element type contains a struct/enum that isn't in the pool.
    pub fn intern_array_from_type(&self, element_type: Type, len: u64) -> ArrayTypeId {
        let element_interned = Self::type_to_interned_recursive(element_type);
        let array_interned = self.intern_array(element_interned, len);
        ArrayTypeId::from_pool_index(
            array_interned
                .pool_index()
                .expect("array must have pool index"),
        )
    }

    /// Look up an array type by Type element and length.
    ///
    /// Returns None if no such array exists in the pool.
    pub fn get_array_by_type(&self, element_type: Type, len: u64) -> Option<ArrayTypeId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let element_interned = Self::type_to_interned_recursive(element_type);
        let array_interned = inner.array_map.get(&(element_interned, len))?;
        Some(ArrayTypeId::from_pool_index(
            array_interned
                .pool_index()
                .expect("array must have pool index"),
        ))
    }

    /// Intern a ptr const type from a Type pointee.
    ///
    /// # Panics
    ///
    /// Panics if the pointee type contains a struct/enum that isn't in the pool.
    pub fn intern_ptr_const_from_type(&self, pointee_type: Type) -> PtrConstTypeId {
        let pointee_interned = Self::type_to_interned_recursive(pointee_type);
        let ptr_interned = self.intern_ptr_const(pointee_interned);
        PtrConstTypeId::from_pool_index(
            ptr_interned
                .pool_index()
                .expect("ptr const must have pool index"),
        )
    }

    /// Intern a ptr mut type from a Type pointee.
    ///
    /// # Panics
    ///
    /// Panics if the pointee type contains a struct/enum that isn't in the pool.
    pub fn intern_ptr_mut_from_type(&self, pointee_type: Type) -> PtrMutTypeId {
        let pointee_interned = Self::type_to_interned_recursive(pointee_type);
        let ptr_interned = self.intern_ptr_mut(pointee_interned);
        PtrMutTypeId::from_pool_index(
            ptr_interned
                .pool_index()
                .expect("ptr mut must have pool index"),
        )
    }

    /// Get ptr const pointee type if this is a ptr const type.
    pub fn ptr_const_def(&self, ptr_id: PtrConstTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let pool_index = ptr_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::PtrConst { pointee } => Self::interned_to_type_recursive(*pointee, &inner),
            other => panic!(
                "Expected ptr const at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Get ptr mut pointee type if this is a ptr mut type.
    pub fn ptr_mut_def(&self, ptr_id: PtrMutTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let pool_index = ptr_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::PtrMut { pointee } => Self::interned_to_type_recursive(*pointee, &inner),
            other => panic!(
                "Expected ptr mut at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Intern a `Ref(T)` type from a Type referent.
    pub fn intern_ref_from_type(&self, referent_type: Type) -> RefTypeId {
        let referent_interned = Self::type_to_interned_recursive(referent_type);
        let ref_interned = self.intern_ref(referent_interned);
        RefTypeId::from_pool_index(ref_interned.pool_index().expect("ref must have pool index"))
    }

    /// Intern a `MutRef(T)` type from a Type referent.
    pub fn intern_mut_ref_from_type(&self, referent_type: Type) -> MutRefTypeId {
        let referent_interned = Self::type_to_interned_recursive(referent_type);
        let mut_ref_interned = self.intern_mut_ref(referent_interned);
        MutRefTypeId::from_pool_index(
            mut_ref_interned
                .pool_index()
                .expect("mut ref must have pool index"),
        )
    }

    /// Get the referent type of a `Ref(T)`.
    pub fn ref_def(&self, ref_id: RefTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let pool_index = ref_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::Ref { referent } => Self::interned_to_type_recursive(*referent, &inner),
            other => panic!("Expected ref at pool index {}, got {:?}", pool_index, other),
        }
    }

    /// Get the referent type of a `MutRef(T)`.
    pub fn mut_ref_def(&self, ref_id: MutRefTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let pool_index = ref_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::MutRef { referent } => Self::interned_to_type_recursive(*referent, &inner),
            other => panic!(
                "Expected mut ref at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Intern a `Slice(T)` type from a Type element.
    pub fn intern_slice_from_type(&self, element_type: Type) -> SliceTypeId {
        let element_interned = Self::type_to_interned_recursive(element_type);
        let slice_interned = self.intern_slice(element_interned);
        SliceTypeId::from_pool_index(
            slice_interned
                .pool_index()
                .expect("slice must have pool index"),
        )
    }

    /// Intern a `MutSlice(T)` type from a Type element.
    pub fn intern_mut_slice_from_type(&self, element_type: Type) -> MutSliceTypeId {
        let element_interned = Self::type_to_interned_recursive(element_type);
        let mut_slice_interned = self.intern_mut_slice(element_interned);
        MutSliceTypeId::from_pool_index(
            mut_slice_interned
                .pool_index()
                .expect("mut slice must have pool index"),
        )
    }

    /// Get the element type of a `Slice(T)`.
    pub fn slice_def(&self, slice_id: SliceTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let pool_index = slice_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::Slice { element } => Self::interned_to_type_recursive(*element, &inner),
            other => panic!(
                "Expected slice at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Get the element type of a `MutSlice(T)`.
    pub fn mut_slice_def(&self, slice_id: MutSliceTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let pool_index = slice_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::MutSlice { element } => Self::interned_to_type_recursive(*element, &inner),
            other => panic!(
                "Expected mut slice at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Intern a `Vec(T)` type from a Type element (ADR-0066).
    pub fn intern_vec_from_type(&self, element_type: Type) -> VecTypeId {
        let element_interned = Self::type_to_interned_recursive(element_type);
        let vec_interned = self.intern_vec(element_interned);
        VecTypeId::from_pool_index(vec_interned.pool_index().expect("vec must have pool index"))
    }

    /// Get the element type of a `Vec(T)` (ADR-0066).
    pub fn vec_def(&self, vec_id: VecTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let pool_index = vec_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::Vec { element } => Self::interned_to_type_recursive(*element, &inner),
            other => panic!("Expected vec at pool index {}, got {:?}", pool_index, other),
        }
    }

    /// ADR-0084: thread-safety classification for any type.
    ///
    /// Returns one of `Unsend < Send < Sync`:
    ///
    /// - **Built-in negative facts.** Raw pointers (`Ptr(T)` /
    ///   `MutPtr(T)`) are intrinsically `Unsend` regardless of `T`.
    /// - **Built-in positive facts.** Primitive integer / float / bool
    ///   / char / unit / never types are intrinsically `Sync`.
    /// - **Composites.** Arrays, slices, vectors, references, and
    ///   pointer-to-`T` chains take the classification of their
    ///   element/referent. Struct / enum types read the
    ///   `thread_safety` field on their definition (computed by
    ///   `validate_consistency` as the structural minimum over members,
    ///   then optionally overridden by a `@mark(unsend)` /
    ///   `@mark(checked_send)` / `@mark(checked_sync)` directive).
    ///
    /// Module / interface / comptime-only types fall through to
    /// `Sync` — they have no runtime presence so the classification
    /// doesn't constrain anything.
    pub fn is_thread_safety_type(&self, ty: Type) -> gruel_builtins::ThreadSafety {
        use gruel_builtins::ThreadSafety;
        match ty.kind() {
            // Built-in negative facts: raw pointers can't safely cross
            // a thread boundary on their own. The user can override
            // with `@mark(checked_send)` / `@mark(checked_sync)` on
            // the containing struct (ADR-0084 § "checked_" naming).
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => ThreadSafety::Unsend,

            // Built-in positive facts: primitives are intrinsically
            // Sync. There's no shared mutable state behind a value of
            // these types.
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
            | TypeKind::Char
            | TypeKind::Unit
            | TypeKind::Never
            | TypeKind::Error
            // ADR-0086: C named primitive types are Sync — same reasoning
            // as the native primitives. `c_void` is an incomplete type with
            // no runtime values, but classifying it as Sync is harmless
            // because no value of it can exist to cross a thread boundary.
            | TypeKind::CSchar
            | TypeKind::CShort
            | TypeKind::CInt
            | TypeKind::CLong
            | TypeKind::CLonglong
            | TypeKind::CUchar
            | TypeKind::CUshort
            | TypeKind::CUint
            | TypeKind::CUlong
            | TypeKind::CUlonglong
            | TypeKind::CFloat
            | TypeKind::CDouble
            | TypeKind::CVoid => ThreadSafety::Sync,

            // Composites delegate structurally.
            TypeKind::Array(array_id) => {
                let (element_type, _length) = self.array_def(array_id);
                self.is_thread_safety_type(element_type)
            }
            TypeKind::Slice(slice_id) => {
                let element_type = self.slice_def(slice_id);
                self.is_thread_safety_type(element_type)
            }
            TypeKind::MutSlice(slice_id) => {
                let element_type = self.mut_slice_def(slice_id);
                self.is_thread_safety_type(element_type)
            }
            TypeKind::Vec(vec_id) => {
                let element_type = self.vec_def(vec_id);
                self.is_thread_safety_type(element_type)
            }
            TypeKind::Ref(ref_id) => {
                let referent = self.ref_def(ref_id);
                self.is_thread_safety_type(referent)
            }
            TypeKind::MutRef(ref_id) => {
                let referent = self.mut_ref_def(ref_id);
                self.is_thread_safety_type(referent)
            }

            TypeKind::Struct(struct_id) => self.struct_def(struct_id).thread_safety,
            TypeKind::Enum(enum_id) => self.enum_def(enum_id).thread_safety,

            // Module / interface / comptime-only types have no runtime
            // representation; treat as Sync (no constraint on caller).
            TypeKind::Module(_)
            | TypeKind::Interface(_)
            | TypeKind::ComptimeType
            | TypeKind::ComptimeStr
            | TypeKind::ComptimeInt => ThreadSafety::Sync,
        }
    }

    /// Check if a type is linear (must be explicitly consumed, can't be
    /// implicitly dropped — ADR-0008).
    ///
    /// Linearity propagates through compound types (ADR-0067):
    /// - `Struct(S)` is linear iff `S` was declared `linear struct`.
    /// - `[T; N]` is linear iff `N > 0` and `T` is linear.
    /// - `Vec(T)` is linear iff `T` is linear.
    /// - `Enum(E)` is linear iff any variant payload is linear.
    /// - All other types are non-linear.
    pub fn is_type_linear(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => self.struct_def(struct_id).posture == Posture::Linear,
            TypeKind::Array(array_id) => {
                let (element_type, length) = self.array_def(array_id);
                length > 0 && self.is_type_linear(element_type)
            }
            TypeKind::Vec(vec_id) => {
                let element_type = self.vec_def(vec_id);
                self.is_type_linear(element_type)
            }
            // ADR-0080: enums declared `linear` are linear directly.
            // For anonymous enums with no keyword (Option / Result and
            // similar generic helpers), `find_or_create_anon_enum`
            // infers the posture from members; this method then sees
            // it set. The propagation fallback below covers
            // pre-specialization paths where method bodies are
            // type-checked before the host enum has been constructed
            // through that codepath.
            TypeKind::Enum(enum_id) => {
                let def = self.enum_def(enum_id);
                def.posture == Posture::Linear
                    || def
                        .variants
                        .iter()
                        .any(|v| v.fields.iter().any(|f| self.is_type_linear(*f)))
            }
            _ => false,
        }
    }

    /// Get all `Vec(T)` type IDs registered in the pool (ADR-0066).
    pub fn all_vec_ids(&self) -> Vec<VecTypeId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .types
            .iter()
            .enumerate()
            .filter_map(|(i, td)| match td {
                TypeData::Vec { .. } => Some(VecTypeId::from_pool_index(i as u32)),
                _ => None,
            })
            .collect()
    }

    /// Convert InternedType to Type recursively (handles composite types).
    ///
    /// This is used to convert array element types from InternedType back to Type.
    fn interned_to_type_recursive(ty: InternedType, inner: &TypeInternPoolInner) -> Type {
        if ty.is_primitive() {
            return match ty.0 {
                0 => Type::I8,
                1 => Type::I16,
                2 => Type::I32,
                3 => Type::I64,
                4 => Type::U8,
                5 => Type::U16,
                6 => Type::U32,
                7 => Type::U64,
                8 => Type::ISIZE,
                9 => Type::USIZE,
                10 => Type::F16,
                11 => Type::F32,
                12 => Type::F64,
                13 => Type::BOOL,
                14 => Type::UNIT,
                15 => Type::NEVER,
                16 => Type::ERROR,
                _ => panic!("Unknown primitive index: {}", ty.0),
            };
        }

        let pool_index = ty.pool_index().expect("non-primitive must have pool index");
        match &inner.types[pool_index as usize] {
            TypeData::Struct(_) => Type::new_struct(StructId::from_pool_index(pool_index)),
            TypeData::Enum(_) => Type::new_enum(EnumId::from_pool_index(pool_index)),
            TypeData::Array { .. } => Type::new_array(ArrayTypeId::from_pool_index(pool_index)),
            TypeData::PtrConst { .. } => {
                Type::new_ptr_const(PtrConstTypeId::from_pool_index(pool_index))
            }
            TypeData::PtrMut { .. } => Type::new_ptr_mut(PtrMutTypeId::from_pool_index(pool_index)),
            TypeData::Ref { .. } => Type::new_ref(RefTypeId::from_pool_index(pool_index)),
            TypeData::MutRef { .. } => Type::new_mut_ref(MutRefTypeId::from_pool_index(pool_index)),
            TypeData::Slice { .. } => Type::new_slice(SliceTypeId::from_pool_index(pool_index)),
            TypeData::MutSlice { .. } => {
                Type::new_mut_slice(MutSliceTypeId::from_pool_index(pool_index))
            }
            TypeData::Vec { .. } => Type::new_vec(VecTypeId::from_pool_index(pool_index)),
        }
    }

    /// Convert Type to InternedType recursively (handles composite types).
    ///
    /// This is used during Phase 2B migration to convert Type to InternedType
    /// for array interning.
    fn type_to_interned_recursive(ty: Type) -> InternedType {
        match ty.kind() {
            TypeKind::I8 => InternedType::I8,
            TypeKind::I16 => InternedType::I16,
            TypeKind::I32 => InternedType::I32,
            TypeKind::I64 => InternedType::I64,
            TypeKind::U8 => InternedType::U8,
            TypeKind::U16 => InternedType::U16,
            TypeKind::U32 => InternedType::U32,
            TypeKind::U64 => InternedType::U64,
            TypeKind::Isize => InternedType::ISIZE,
            TypeKind::Usize => InternedType::USIZE,
            TypeKind::F16 => InternedType::F16,
            TypeKind::F32 => InternedType::F32,
            TypeKind::F64 => InternedType::F64,
            TypeKind::Bool => InternedType::BOOL,
            TypeKind::Char => InternedType::CHAR,
            TypeKind::Unit => InternedType::UNIT,
            TypeKind::Never => InternedType::NEVER,
            TypeKind::Error => InternedType::ERROR,
            // ADR-0086 C named primitive types.
            TypeKind::CSchar => InternedType::C_SCHAR,
            TypeKind::CShort => InternedType::C_SHORT,
            TypeKind::CInt => InternedType::C_INT,
            TypeKind::CLong => InternedType::C_LONG,
            TypeKind::CLonglong => InternedType::C_LONGLONG,
            TypeKind::CUchar => InternedType::C_UCHAR,
            TypeKind::CUshort => InternedType::C_USHORT,
            TypeKind::CUint => InternedType::C_UINT,
            TypeKind::CUlong => InternedType::C_ULONG,
            TypeKind::CUlonglong => InternedType::C_ULONGLONG,
            TypeKind::CFloat => InternedType::C_FLOAT,
            TypeKind::CDouble => InternedType::C_DOUBLE,
            TypeKind::CVoid => InternedType::C_VOID,
            TypeKind::Struct(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::Enum(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::Array(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::PtrConst(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::PtrMut(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::Ref(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::MutRef(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::Slice(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::MutSlice(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::Vec(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::Module(_) => panic!("Cannot intern module types"),
            TypeKind::Interface(_) => panic!("Cannot intern interface types"),
            TypeKind::ComptimeType => panic!("Cannot intern comptime types"),
            TypeKind::ComptimeStr => panic!("Cannot intern comptime_str types"),
            TypeKind::ComptimeInt => panic!("Cannot intern comptime_int types"),
        }
    }

    /// Convert an ArrayTypeId to an InternedType.
    ///
    /// Since ArrayTypeId now contains a pool index, we just add the primitive offset.
    #[inline]
    pub fn array_id_to_interned(&self, array_id: ArrayTypeId) -> InternedType {
        InternedType::from_pool_index(array_id.0)
    }

    /// Get all struct IDs registered in the pool.
    ///
    /// Returns a vector of all StructId values, useful for iterating over all
    /// structs (e.g., for drop glue synthesis).
    pub fn all_struct_ids(&self) -> Vec<StructId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .struct_by_name
            .values()
            .map(|interned| {
                let pool_index = interned.pool_index().expect("struct must have pool index");
                StructId::from_pool_index(pool_index)
            })
            .collect()
    }

    /// Get all enum IDs registered in the pool.
    ///
    /// Returns a vector of all EnumId values, useful for iterating over all
    /// enums.
    pub fn all_enum_ids(&self) -> Vec<EnumId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .enum_by_name
            .values()
            .map(|interned| {
                let pool_index = interned.pool_index().expect("enum must have pool index");
                EnumId::from_pool_index(pool_index)
            })
            .collect()
    }

    /// Get all array IDs registered in the pool.
    ///
    /// Returns a vector of all ArrayTypeId values, useful for iterating over all
    /// arrays (e.g., for drop glue synthesis).
    pub fn all_array_ids(&self) -> Vec<ArrayTypeId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .types
            .iter()
            .enumerate()
            .filter_map(|(idx, data)| match data {
                TypeData::Array { .. } => Some(ArrayTypeId(idx as u32)),
                _ => None,
            })
            .collect()
    }

    /// Get the number of composite types in the pool.
    pub fn len(&self) -> usize {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.types.len()
    }

    /// Check if the pool is empty (no composite types).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get statistics about the pool contents.
    pub fn stats(&self) -> TypeInternPoolStats {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let mut struct_count = 0;
        let mut enum_count = 0;
        let mut array_count = 0;

        for data in &inner.types {
            match data {
                TypeData::Struct(_) => struct_count += 1,
                TypeData::Enum(_) => enum_count += 1,
                TypeData::Array { .. } => array_count += 1,
                TypeData::PtrConst { .. }
                | TypeData::PtrMut { .. }
                | TypeData::Ref { .. }
                | TypeData::MutRef { .. }
                | TypeData::Slice { .. }
                | TypeData::MutSlice { .. }
                | TypeData::Vec { .. } => {
                    // Pointer/reference/slice/vec types are not counted separately in stats
                }
            }
        }

        TypeInternPoolStats {
            struct_count,
            enum_count,
            array_count,
            total: inner.types.len(),
        }
    }

    // ========================================================================
    // Conversion helpers for migration (Phase 1)
    // ========================================================================

    /// Convert an old-style `Type` to an `InternedType`.
    ///
    /// This is a temporary helper for Phase 1 migration. It converts the
    /// existing `Type` enum to the new interned representation.
    ///
    /// # Note
    ///
    /// For struct/enum types, the corresponding type must already be registered
    /// in the pool. For array types, this returns an error since array interning
    /// requires the pool to already have the element type interned.
    pub fn type_to_interned(&self, ty: Type) -> Option<InternedType> {
        match ty.kind() {
            TypeKind::I8 => Some(InternedType::I8),
            TypeKind::I16 => Some(InternedType::I16),
            TypeKind::I32 => Some(InternedType::I32),
            TypeKind::I64 => Some(InternedType::I64),
            TypeKind::U8 => Some(InternedType::U8),
            TypeKind::U16 => Some(InternedType::U16),
            TypeKind::U32 => Some(InternedType::U32),
            TypeKind::U64 => Some(InternedType::U64),
            TypeKind::Isize => Some(InternedType::ISIZE),
            TypeKind::Usize => Some(InternedType::USIZE),
            TypeKind::F16 => Some(InternedType::F16),
            TypeKind::F32 => Some(InternedType::F32),
            TypeKind::F64 => Some(InternedType::F64),
            TypeKind::Bool => Some(InternedType::BOOL),
            TypeKind::Char => Some(InternedType::CHAR),
            TypeKind::Unit => Some(InternedType::UNIT),
            TypeKind::Never => Some(InternedType::NEVER),
            TypeKind::Error => Some(InternedType::ERROR),
            // ADR-0086 C named primitive types.
            TypeKind::CSchar => Some(InternedType::C_SCHAR),
            TypeKind::CShort => Some(InternedType::C_SHORT),
            TypeKind::CInt => Some(InternedType::C_INT),
            TypeKind::CLong => Some(InternedType::C_LONG),
            TypeKind::CLonglong => Some(InternedType::C_LONGLONG),
            TypeKind::CUchar => Some(InternedType::C_UCHAR),
            TypeKind::CUshort => Some(InternedType::C_USHORT),
            TypeKind::CUint => Some(InternedType::C_UINT),
            TypeKind::CUlong => Some(InternedType::C_ULONG),
            TypeKind::CUlonglong => Some(InternedType::C_ULONGLONG),
            TypeKind::CFloat => Some(InternedType::C_FLOAT),
            TypeKind::CDouble => Some(InternedType::C_DOUBLE),
            TypeKind::CVoid => Some(InternedType::C_VOID),
            // Struct, enum, array, pointer, and module require pool lookup by ID - we need the name
            // to find the interned type. This conversion is not straightforward
            // without additional context. Return None to indicate we can't convert.
            TypeKind::Struct(_)
            | TypeKind::Enum(_)
            | TypeKind::Array(_)
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_)
            | TypeKind::Ref(_)
            | TypeKind::MutRef(_)
            | TypeKind::Slice(_)
            | TypeKind::MutSlice(_)
            | TypeKind::Vec(_)
            | TypeKind::Module(_)
            | TypeKind::Interface(_) => None,
            // Comptime-only types cannot be interned for runtime
            TypeKind::ComptimeType | TypeKind::ComptimeStr | TypeKind::ComptimeInt => None,
        }
    }

    /// Convert an `InternedType` back to the old-style `Type`.
    ///
    /// This is a temporary helper for Phase 1 migration to verify correctness.
    /// Returns `None` for composite types since we need the old IDs.
    pub fn interned_to_type(&self, ty: InternedType) -> Option<Type> {
        if !ty.is_primitive() {
            return None;
        }

        Some(match ty.0 {
            0 => Type::I8,
            1 => Type::I16,
            2 => Type::I32,
            3 => Type::I64,
            4 => Type::U8,
            5 => Type::U16,
            6 => Type::U32,
            7 => Type::U64,
            8 => Type::ISIZE,
            9 => Type::USIZE,
            10 => Type::F16,
            11 => Type::F32,
            12 => Type::F64,
            13 => Type::BOOL,
            14 => Type::UNIT,
            15 => Type::NEVER,
            16 => Type::ERROR,
            20 => Type::CHAR,
            // ADR-0086 C named primitive types.
            21 => Type::C_SCHAR,
            22 => Type::C_SHORT,
            23 => Type::C_INT,
            24 => Type::C_LONG,
            25 => Type::C_LONGLONG,
            26 => Type::C_UCHAR,
            27 => Type::C_USHORT,
            28 => Type::C_UINT,
            29 => Type::C_ULONG,
            30 => Type::C_ULONGLONG,
            31 => Type::C_FLOAT,
            32 => Type::C_DOUBLE,
            33 => Type::C_VOID,
            _ => return None,
        })
    }

    /// Return the number of ABI slots that `ty` occupies when passed as a function argument.
    ///
    /// - Scalars (integers, bool, enum, pointer) → 1
    /// - Struct → sum of field slot counts (flattened)
    /// - Array → element slot count × length
    /// - Zero-sized types (unit, never, comptime-only, module) → 0
    pub fn abi_slot_count(&self, ty: Type) -> u32 {
        match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self.struct_def(id);
                def.fields.iter().map(|f| self.abi_slot_count(f.ty)).sum()
            }
            TypeKind::Array(id) => {
                let (elem, len) = self.array_def(id);
                self.abi_slot_count(elem) * len as u32
            }
            TypeKind::Unit
            | TypeKind::Never
            | TypeKind::ComptimeType
            | TypeKind::ComptimeStr
            | TypeKind::ComptimeInt
            | TypeKind::Module(_) => 0,
            // Fat pointers: 2 slots (ptr + len or ptr + vtable)
            TypeKind::Slice(_) | TypeKind::MutSlice(_) | TypeKind::Interface(_) => 2,
            // Vec(T) (ADR-0066): 3 slots (ptr + len + cap).
            TypeKind::Vec(_) => 3,
            _ => 1,
        }
    }

    /// Return the human-readable name of `ty`, suitable for error messages.
    ///
    /// Examples: `"i32"`, `"bool"`, `"MyStruct"`, `"[i32; 4]"`, `"Ptr(i32)"`.
    /// Pointer types are formatted in the ADR-0061 surface form
    /// (`Ptr(T)` / `MutPtr(T)`) regardless of which syntax the user wrote;
    /// the old `ptr const T` / `ptr mut T` form remains accepted by the
    /// parser during the migration but is not produced by diagnostics.
    pub fn format_type_name(&self, ty: Type) -> String {
        match ty.kind() {
            TypeKind::I8 => "i8".to_string(),
            TypeKind::I16 => "i16".to_string(),
            TypeKind::I32 => "i32".to_string(),
            TypeKind::I64 => "i64".to_string(),
            TypeKind::U8 => "u8".to_string(),
            TypeKind::U16 => "u16".to_string(),
            TypeKind::U32 => "u32".to_string(),
            TypeKind::U64 => "u64".to_string(),
            TypeKind::Isize => "isize".to_string(),
            TypeKind::Usize => "usize".to_string(),
            TypeKind::F16 => "f16".to_string(),
            TypeKind::F32 => "f32".to_string(),
            TypeKind::F64 => "f64".to_string(),
            TypeKind::Bool => "bool".to_string(),
            TypeKind::Char => "char".to_string(),
            TypeKind::Unit => "()".to_string(),
            TypeKind::Never => "!".to_string(),
            TypeKind::Error => "<error>".to_string(),
            // ADR-0086 C named primitive types.
            TypeKind::CSchar => "c_schar".to_string(),
            TypeKind::CShort => "c_short".to_string(),
            TypeKind::CInt => "c_int".to_string(),
            TypeKind::CLong => "c_long".to_string(),
            TypeKind::CLonglong => "c_longlong".to_string(),
            TypeKind::CUchar => "c_uchar".to_string(),
            TypeKind::CUshort => "c_ushort".to_string(),
            TypeKind::CUint => "c_uint".to_string(),
            TypeKind::CUlong => "c_ulong".to_string(),
            TypeKind::CUlonglong => "c_ulonglong".to_string(),
            TypeKind::CFloat => "c_float".to_string(),
            TypeKind::CDouble => "c_double".to_string(),
            TypeKind::CVoid => "c_void".to_string(),
            TypeKind::ComptimeType => "type".to_string(),
            TypeKind::ComptimeStr => "comptime_str".to_string(),
            TypeKind::ComptimeInt => "comptime_int".to_string(),
            TypeKind::Struct(id) => self.struct_def(id).name.clone(),
            TypeKind::Enum(id) => self.enum_def(id).name.clone(),
            TypeKind::Array(id) => {
                let (elem, len) = self.array_def(id);
                format!("[{}; {}]", self.format_type_name(elem), len)
            }
            TypeKind::PtrConst(id) => {
                let pointee = self.ptr_const_def(id);
                format!("Ptr({})", self.format_type_name(pointee))
            }
            TypeKind::PtrMut(id) => {
                let pointee = self.ptr_mut_def(id);
                format!("MutPtr({})", self.format_type_name(pointee))
            }
            TypeKind::Ref(id) => {
                let referent = self.ref_def(id);
                format!("Ref({})", self.format_type_name(referent))
            }
            TypeKind::MutRef(id) => {
                let referent = self.mut_ref_def(id);
                format!("MutRef({})", self.format_type_name(referent))
            }
            TypeKind::Slice(id) => {
                let element = self.slice_def(id);
                format!("Slice({})", self.format_type_name(element))
            }
            TypeKind::MutSlice(id) => {
                let element = self.mut_slice_def(id);
                format!("MutSlice({})", self.format_type_name(element))
            }
            TypeKind::Vec(id) => {
                let element = self.vec_def(id);
                format!("Vec({})", self.format_type_name(element))
            }
            TypeKind::Module(_) => "<module>".to_string(),
            // Interface names are stored on `Sema::interfaces`, not in the
            // type pool. Caller-side error messages should resolve them via
            // `Sema`; this fallback is for cases where only the pool is
            // available.
            TypeKind::Interface(id) => format!("<interface#{}>", id.0),
        }
    }
}

impl Default for TypeInternPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for TypeInternPool {
    /// Clone the pool by copying all type data into a new pool.
    ///
    /// This is used when building `SemaContext` from `Sema`, as the context
    /// needs its own copy of the pool for thread-safe sharing.
    fn clone(&self) -> Self {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        Self {
            inner: RwLock::new(TypeInternPoolInner {
                types: inner.types.clone(),
                array_map: inner.array_map.clone(),
                ptr_const_map: inner.ptr_const_map.clone(),
                ptr_mut_map: inner.ptr_mut_map.clone(),
                ref_map: inner.ref_map.clone(),
                mut_ref_map: inner.mut_ref_map.clone(),
                slice_map: inner.slice_map.clone(),
                mut_slice_map: inner.mut_slice_map.clone(),
                vec_map: inner.vec_map.clone(),
                struct_by_name: inner.struct_by_name.clone(),
                enum_by_name: inner.enum_by_name.clone(),
                layout_cache: inner.layout_cache.clone(),
            }),
        }
    }
}

/// Statistics about the intern pool contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeInternPoolStats {
    pub struct_count: usize,
    pub enum_count: usize,
    pub array_count: usize,
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EnumVariantDef;
    use lasso::ThreadedRodeo;

    // ========================================================================
    // InternedType tests
    // ========================================================================

    #[test]
    fn type_intern_pool_round_trips_through_serde() {
        // ADR-0074 Phase 4: snapshot a pool, serialize via bincode, load
        // back, verify all interned types resolve to the same data.
        let pool = TypeInternPool::new();
        let inner_array = pool.intern_array(InternedType::I32, 4);
        let outer_array = pool.intern_array(inner_array, 2);
        let ptr = pool.intern_ptr_const(InternedType::I64);

        let bytes =
            bincode::serde::encode_to_vec(&pool, bincode::config::standard()).expect("serialize");
        let (restored, _): (TypeInternPool, _) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .expect("deserialize");

        // The restored pool's structural-dedup maps should be reconstructed:
        // re-interning identical types should return the same InternedType
        // values that the original pool produced.
        assert_eq!(restored.intern_array(InternedType::I32, 4), inner_array);
        assert_eq!(restored.intern_array(inner_array, 2), outer_array);
        assert_eq!(restored.intern_ptr_const(InternedType::I64), ptr);
    }

    #[test]
    fn test_interned_type_primitives() {
        assert!(InternedType::I8.is_primitive());
        assert!(InternedType::I16.is_primitive());
        assert!(InternedType::I32.is_primitive());
        assert!(InternedType::I64.is_primitive());
        assert!(InternedType::U8.is_primitive());
        assert!(InternedType::U16.is_primitive());
        assert!(InternedType::U32.is_primitive());
        assert!(InternedType::U64.is_primitive());
        assert!(InternedType::BOOL.is_primitive());
        assert!(InternedType::UNIT.is_primitive());
        assert!(InternedType::NEVER.is_primitive());
        assert!(InternedType::ERROR.is_primitive());
    }

    #[test]
    fn test_interned_type_indices() {
        assert_eq!(InternedType::I8.index(), 0);
        assert_eq!(InternedType::I16.index(), 1);
        assert_eq!(InternedType::I32.index(), 2);
        assert_eq!(InternedType::I64.index(), 3);
        assert_eq!(InternedType::U8.index(), 4);
        assert_eq!(InternedType::ISIZE.index(), 8);
        assert_eq!(InternedType::USIZE.index(), 9);
        assert_eq!(InternedType::F16.index(), 10);
        assert_eq!(InternedType::F32.index(), 11);
        assert_eq!(InternedType::F64.index(), 12);
        assert_eq!(InternedType::BOOL.index(), 13);
        assert_eq!(InternedType::UNIT.index(), 14);
    }

    #[test]
    fn test_interned_type_pool_index() {
        // Primitives don't have pool indices
        assert_eq!(InternedType::I32.pool_index(), None);
        assert_eq!(InternedType::BOOL.pool_index(), None);

        // Composite types have pool indices
        let composite = InternedType::from_pool_index(0);
        assert_eq!(composite.pool_index(), Some(0));
        assert!(!composite.is_primitive());

        let composite2 = InternedType::from_pool_index(42);
        assert_eq!(composite2.pool_index(), Some(42));
    }

    #[test]
    fn test_interned_type_equality() {
        assert_eq!(InternedType::I32, InternedType::I32);
        assert_ne!(InternedType::I32, InternedType::I64);
        assert_ne!(InternedType::I32, InternedType::from_pool_index(0));
    }

    #[test]
    fn test_interned_type_debug() {
        let i32_str = format!("{:?}", InternedType::I32);
        assert!(i32_str.contains("i32"));

        let composite_str = format!("{:?}", InternedType::from_pool_index(5));
        assert!(composite_str.contains("pool:5"));
    }

    // ========================================================================
    // TypeInternPool tests
    // ========================================================================

    #[test]
    fn test_pool_new() {
        let pool = TypeInternPool::new();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_pool_register_struct() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Point");

        let def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };

        let (struct_id, is_new) = pool.register_struct(name, def.clone());
        assert!(is_new);
        assert_eq!(struct_id.pool_index(), 0); // First entry in pool
        assert_eq!(pool.len(), 1);

        // Registering the same name returns the existing type
        let (struct_id2, is_new2) = pool.register_struct(name, def);
        assert!(!is_new2);
        assert_eq!(struct_id, struct_id2);
        assert_eq!(pool.len(), 1); // No new type added
    }

    #[test]
    fn test_pool_register_enum() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Color");

        let def = EnumDef {
            name: "Color".to_string(),
            variants: vec![
                EnumVariantDef::unit("Red"),
                EnumVariantDef::unit("Green"),
                EnumVariantDef::unit("Blue"),
            ],
            posture: Posture::Affine,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            destructor: None,
        };

        let (enum_id, is_new) = pool.register_enum(name, def.clone());
        assert!(is_new);
        assert_eq!(enum_id.pool_index(), 0); // First entry in pool
        assert_eq!(pool.len(), 1);

        // Registering the same name returns the existing type
        let (enum_id2, is_new2) = pool.register_enum(name, def);
        assert!(!is_new2);
        assert_eq!(enum_id, enum_id2);
    }

    #[test]
    fn test_pool_intern_array() {
        let pool = TypeInternPool::new();

        // Intern [i32; 5]
        let arr1 = pool.intern_array(InternedType::I32, 5);
        assert!(!arr1.is_primitive());
        assert_eq!(pool.len(), 1);

        // Interning the same array returns the same type
        let arr2 = pool.intern_array(InternedType::I32, 5);
        assert_eq!(arr1, arr2);
        assert_eq!(pool.len(), 1);

        // Different length is a different type
        let arr3 = pool.intern_array(InternedType::I32, 10);
        assert_ne!(arr1, arr3);
        assert_eq!(pool.len(), 2);

        // Different element type is a different type
        let arr4 = pool.intern_array(InternedType::I64, 5);
        assert_ne!(arr1, arr4);
        assert_eq!(pool.len(), 3);
    }

    #[test]
    fn test_pool_get_struct_by_name() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Point");

        assert!(pool.get_struct_by_name(name).is_none());

        let def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };

        let (struct_id, _) = pool.register_struct(name, def);
        // get_struct_by_name returns InternedType, convert StructId for comparison
        let expected = pool.struct_id_to_interned(struct_id);
        assert_eq!(pool.get_struct_by_name(name), Some(expected));
    }

    #[test]
    fn test_pool_get_enum_by_name() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Status");

        assert!(pool.get_enum_by_name(name).is_none());

        let def = EnumDef {
            name: "Status".to_string(),
            variants: vec![
                EnumVariantDef::unit("Active"),
                EnumVariantDef::unit("Inactive"),
            ],
            posture: Posture::Affine,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            destructor: None,
        };

        let (enum_id, _) = pool.register_enum(name, def);
        // get_enum_by_name returns InternedType, convert EnumId for comparison
        let expected = pool.enum_id_to_interned(enum_id);
        assert_eq!(pool.get_enum_by_name(name), Some(expected));
    }

    #[test]
    fn test_pool_get_array() {
        let pool = TypeInternPool::new();

        assert!(pool.get_array(InternedType::I32, 5).is_none());

        let arr = pool.intern_array(InternedType::I32, 5);
        assert_eq!(pool.get_array(InternedType::I32, 5), Some(arr));
        assert!(pool.get_array(InternedType::I32, 10).is_none());
    }

    #[test]
    fn test_pool_get_type_data() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        // Primitive types return None
        assert!(pool.get(InternedType::I32).is_none());

        // Register a struct
        let struct_name = interner.get_or_intern("Point");
        let struct_def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };
        let (struct_id, _) = pool.register_struct(struct_name, struct_def);
        let struct_ty = pool.struct_id_to_interned(struct_id);

        // Get struct data
        let data = pool.get(struct_ty).expect("should get struct data");
        assert!(matches!(data, TypeData::Struct(_)));

        // Intern an array
        let arr_ty = pool.intern_array(InternedType::I32, 10);
        let arr_data = pool.get(arr_ty).expect("should get array data");
        match arr_data {
            TypeData::Array { element, len } => {
                assert_eq!(element, InternedType::I32);
                assert_eq!(len, 10);
            }
            _ => panic!("expected array data"),
        }
    }

    #[test]
    fn test_pool_type_checks() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let struct_name = interner.get_or_intern("Point");
        let struct_def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };
        let (struct_id, _) = pool.register_struct(struct_name, struct_def);
        let struct_ty = pool.struct_id_to_interned(struct_id);

        let enum_name = interner.get_or_intern("Color");
        let enum_def = EnumDef {
            name: "Color".to_string(),
            variants: vec![EnumVariantDef::unit("Red")],
            posture: Posture::Affine,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            destructor: None,
        };
        let (enum_id, _) = pool.register_enum(enum_name, enum_def);
        let enum_ty = pool.enum_id_to_interned(enum_id);

        let array_ty = pool.intern_array(InternedType::I32, 5);

        // Check is_struct
        assert!(pool.is_struct(struct_ty));
        assert!(!pool.is_struct(enum_ty));
        assert!(!pool.is_struct(array_ty));
        assert!(!pool.is_struct(InternedType::I32));

        // Check is_enum
        assert!(!pool.is_enum(struct_ty));
        assert!(pool.is_enum(enum_ty));
        assert!(!pool.is_enum(array_ty));
        assert!(!pool.is_enum(InternedType::I32));

        // Check is_array
        assert!(!pool.is_array(struct_ty));
        assert!(!pool.is_array(enum_ty));
        assert!(pool.is_array(array_ty));
        assert!(!pool.is_array(InternedType::I32));
    }

    #[test]
    fn test_pool_get_struct_def() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let name = interner.get_or_intern("Point");
        let def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            posture: Posture::Copy,
            is_clone: false,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };
        let (struct_id, _) = pool.register_struct(name, def.clone());

        // Test the new Phase 3 struct_def() method that takes StructId directly
        let retrieved = pool.struct_def(struct_id);
        assert_eq!(retrieved.name, def.name);
        assert_eq!(retrieved.posture, def.posture);

        // Test the old get_struct_def() that takes InternedType
        let interned = pool.struct_id_to_interned(struct_id);
        let retrieved2 = pool
            .get_struct_def(interned)
            .expect("should get struct def");
        assert_eq!(retrieved2.name, def.name);

        // Non-struct returns None for get_struct_def
        let array_ty = pool.intern_array(InternedType::I32, 5);
        assert!(pool.get_struct_def(array_ty).is_none());
        assert!(pool.get_struct_def(InternedType::I32).is_none());
    }

    #[test]
    fn test_pool_get_enum_def() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let name = interner.get_or_intern("Status");
        let def = EnumDef {
            name: "Status".to_string(),
            variants: vec![EnumVariantDef::unit("A"), EnumVariantDef::unit("B")],
            posture: Posture::Affine,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            destructor: None,
        };
        let (enum_id, _) = pool.register_enum(name, def.clone());

        // Test the new Phase 3 enum_def() method that takes EnumId directly
        let retrieved = pool.enum_def(enum_id);
        assert_eq!(retrieved.name, def.name);
        assert_eq!(retrieved.variants.len(), 2);

        // Test the old get_enum_def() that takes InternedType
        let interned = pool.enum_id_to_interned(enum_id);
        let retrieved2 = pool.get_enum_def(interned).expect("should get enum def");
        assert_eq!(retrieved2.name, def.name);

        // Non-enum returns None for get_enum_def
        let array_ty = pool.intern_array(InternedType::I32, 5);
        assert!(pool.get_enum_def(array_ty).is_none());
        assert!(pool.get_enum_def(InternedType::I32).is_none());
    }

    #[test]
    fn test_pool_get_array_info() {
        let pool = TypeInternPool::new();

        let array_ty = pool.intern_array(InternedType::I64, 100);
        let (element, len) = pool
            .get_array_info(array_ty)
            .expect("should get array info");
        assert_eq!(element, InternedType::I64);
        assert_eq!(len, 100);

        // Non-array returns None
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("X");
        let def = StructDef {
            name: "X".to_string(),
            fields: vec![],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };
        let (struct_id, _) = pool.register_struct(name, def);
        let struct_ty = pool.struct_id_to_interned(struct_id);
        assert!(pool.get_array_info(struct_ty).is_none());
        assert!(pool.get_array_info(InternedType::I32).is_none());
    }

    #[test]
    fn test_pool_stats() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let stats = pool.stats();
        assert_eq!(stats.struct_count, 0);
        assert_eq!(stats.enum_count, 0);
        assert_eq!(stats.array_count, 0);
        assert_eq!(stats.total, 0);

        // Add some types
        let s1 = interner.get_or_intern("S1");
        let s2 = interner.get_or_intern("S2");
        let e1 = interner.get_or_intern("E1");

        let def = StructDef {
            name: "S1".to_string(),
            fields: vec![],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };
        pool.register_struct(s1, def.clone());
        pool.register_struct(
            s2,
            StructDef {
                name: "S2".to_string(),
                ..def
            },
        );

        pool.register_enum(
            e1,
            EnumDef {
                name: "E1".to_string(),
                variants: vec![],
                posture: Posture::Affine,
                thread_safety: gruel_builtins::ThreadSafety::Sync,
                is_pub: false,
                file_id: gruel_util::FileId::DEFAULT,
                destructor: None,
            },
        );

        pool.intern_array(InternedType::I32, 5);
        pool.intern_array(InternedType::I32, 10);
        pool.intern_array(InternedType::BOOL, 3);

        let stats = pool.stats();
        assert_eq!(stats.struct_count, 2);
        assert_eq!(stats.enum_count, 1);
        assert_eq!(stats.array_count, 3);
        assert_eq!(stats.total, 6);
    }

    #[test]
    fn test_pool_nested_arrays() {
        let pool = TypeInternPool::new();

        // Create [i32; 3]
        let inner = pool.intern_array(InternedType::I32, 3);

        // Create [[i32; 3]; 4]
        let outer = pool.intern_array(inner, 4);

        // Verify structure
        let (outer_elem, outer_len) = pool.get_array_info(outer).expect("outer array info");
        assert_eq!(outer_elem, inner);
        assert_eq!(outer_len, 4);

        let (inner_elem, inner_len) = pool.get_array_info(inner).expect("inner array info");
        assert_eq!(inner_elem, InternedType::I32);
        assert_eq!(inner_len, 3);
    }

    #[test]
    fn test_pool_type_to_interned() {
        let pool = TypeInternPool::new();

        // Primitive types convert correctly
        assert_eq!(pool.type_to_interned(Type::I8), Some(InternedType::I8));
        assert_eq!(pool.type_to_interned(Type::I16), Some(InternedType::I16));
        assert_eq!(pool.type_to_interned(Type::I32), Some(InternedType::I32));
        assert_eq!(pool.type_to_interned(Type::I64), Some(InternedType::I64));
        assert_eq!(pool.type_to_interned(Type::U8), Some(InternedType::U8));
        assert_eq!(pool.type_to_interned(Type::U16), Some(InternedType::U16));
        assert_eq!(pool.type_to_interned(Type::U32), Some(InternedType::U32));
        assert_eq!(pool.type_to_interned(Type::U64), Some(InternedType::U64));
        assert_eq!(pool.type_to_interned(Type::BOOL), Some(InternedType::BOOL));
        assert_eq!(pool.type_to_interned(Type::UNIT), Some(InternedType::UNIT));
        assert_eq!(
            pool.type_to_interned(Type::NEVER),
            Some(InternedType::NEVER)
        );
        assert_eq!(
            pool.type_to_interned(Type::ERROR),
            Some(InternedType::ERROR)
        );

        // Composite types return None (need name lookup)
        assert!(
            pool.type_to_interned(Type::new_struct(crate::types::StructId(0)))
                .is_none()
        );
        assert!(
            pool.type_to_interned(Type::new_enum(crate::types::EnumId(0)))
                .is_none()
        );
        assert!(
            pool.type_to_interned(Type::new_array(crate::types::ArrayTypeId(0)))
                .is_none()
        );
    }

    #[test]
    fn test_pool_interned_to_type() {
        let pool = TypeInternPool::new();

        // Primitive types convert back correctly
        assert_eq!(pool.interned_to_type(InternedType::I8), Some(Type::I8));
        assert_eq!(pool.interned_to_type(InternedType::I16), Some(Type::I16));
        assert_eq!(pool.interned_to_type(InternedType::I32), Some(Type::I32));
        assert_eq!(pool.interned_to_type(InternedType::I64), Some(Type::I64));
        assert_eq!(pool.interned_to_type(InternedType::U8), Some(Type::U8));
        assert_eq!(pool.interned_to_type(InternedType::U16), Some(Type::U16));
        assert_eq!(pool.interned_to_type(InternedType::U32), Some(Type::U32));
        assert_eq!(pool.interned_to_type(InternedType::U64), Some(Type::U64));
        assert_eq!(pool.interned_to_type(InternedType::BOOL), Some(Type::BOOL));
        assert_eq!(pool.interned_to_type(InternedType::UNIT), Some(Type::UNIT));
        assert_eq!(
            pool.interned_to_type(InternedType::NEVER),
            Some(Type::NEVER)
        );
        assert_eq!(
            pool.interned_to_type(InternedType::ERROR),
            Some(Type::ERROR)
        );

        // Composite types return None
        assert!(
            pool.interned_to_type(InternedType::from_pool_index(0))
                .is_none()
        );
    }

    // ========================================================================
    // Thread safety tests
    // ========================================================================

    #[test]
    fn test_pool_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let pool = Arc::new(TypeInternPool::new());
        let interner = Arc::new(ThreadedRodeo::default());

        // Pre-register names for thread safety
        let names: Vec<Spur> = (0..100)
            .map(|i| interner.get_or_intern(format!("Type{}", i)))
            .collect();

        let handles: Vec<_> = (0..10)
            .map(|thread_id| {
                let pool = Arc::clone(&pool);
                let names = names.clone();
                thread::spawn(move || {
                    // Each thread registers 10 types
                    for i in 0..10 {
                        let idx = thread_id * 10 + i;
                        let name = names[idx];
                        let def = StructDef {
                            name: format!("Type{}", idx),
                            fields: vec![],
                            posture: Posture::Affine,
                            is_clone: false,
                            thread_safety: gruel_builtins::ThreadSafety::Sync,
                            destructor: None,
                            is_builtin: false,
                            is_pub: false,
                            file_id: gruel_util::FileId::DEFAULT,
                            is_c_layout: false,
                        };
                        pool.register_struct(name, def);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("thread panicked");
        }

        // All 100 types should be registered
        assert_eq!(pool.len(), 100);

        // Each name should map to a valid type
        for name in &names {
            assert!(pool.get_struct_by_name(*name).is_some());
        }
    }

    #[test]
    fn test_pool_concurrent_array_interning() {
        use std::sync::Arc;
        use std::thread;

        let pool = Arc::new(TypeInternPool::new());

        // Multiple threads try to intern the same array type
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || pool.intern_array(InternedType::I32, 42))
            })
            .collect();

        let results: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect();

        // All threads should get the same type
        let first = results[0];
        for result in &results {
            assert_eq!(*result, first);
        }

        // Only one array type should be in the pool
        assert_eq!(pool.stats().array_count, 1);
    }

    // ========================================================================
    // Struct ID reservation tests
    // ========================================================================

    #[test]
    fn test_pool_reserve_and_complete_struct() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        // Reserve an ID
        let struct_id = pool.reserve_struct_id();
        assert_eq!(struct_id.pool_index(), 0);
        assert_eq!(pool.len(), 1); // Placeholder was pushed

        // Use the ID to create a name
        let name_str = format!("__anon_struct_{}", struct_id.0);
        let name = interner.get_or_intern(&name_str);

        let def = StructDef {
            name: name_str.clone(),
            fields: vec![],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: gruel_builtins::ThreadSafety::Sync,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };

        // Complete registration
        pool.complete_struct_registration(struct_id, name, def);

        // Verify registration succeeded
        assert_eq!(pool.len(), 1); // No new entry, just updated
        assert!(pool.get_struct_by_name(name).is_some());

        // Can retrieve the struct definition
        let retrieved = pool.struct_def(struct_id);
        assert_eq!(retrieved.name, name_str);
    }

    #[test]
    fn test_pool_reserve_multiple_structs() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        // Reserve multiple IDs
        let id1 = pool.reserve_struct_id();
        let id2 = pool.reserve_struct_id();
        let id3 = pool.reserve_struct_id();

        assert_eq!(id1.pool_index(), 0);
        assert_eq!(id2.pool_index(), 1);
        assert_eq!(id3.pool_index(), 2);
        assert_eq!(pool.len(), 3);

        // Complete them in any order (here: reverse)
        for (i, id) in [(2, id3), (1, id2), (0, id1)] {
            let name_str = format!("__anon_struct_{}", i);
            let name = interner.get_or_intern(&name_str);
            let def = StructDef {
                name: name_str,
                fields: vec![],
                posture: Posture::Affine,
                is_clone: false,
                thread_safety: gruel_builtins::ThreadSafety::Sync,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: gruel_util::FileId::DEFAULT,
                is_c_layout: false,
            };
            pool.complete_struct_registration(id, name, def);
        }

        // All three should be registered
        assert_eq!(pool.stats().struct_count, 3);
    }

    // Compile-time assertion that TypeInternPool is Send + Sync
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn test_pool_is_send_sync() {
        assert_send_sync::<TypeInternPool>();
    }

    // ========================================================================
    // ADR-0084: thread-safety inference tests
    // ========================================================================

    /// Phase 2 verification: every primitive integer / float / bool /
    /// char / unit / never type is intrinsically `Sync`.
    #[test]
    fn thread_safety_primitives_are_sync() {
        use gruel_builtins::ThreadSafety;
        let pool = TypeInternPool::new();
        for ty in [
            Type::I8,
            Type::I16,
            Type::I32,
            Type::I64,
            Type::U8,
            Type::U16,
            Type::U32,
            Type::U64,
            Type::ISIZE,
            Type::USIZE,
            Type::F16,
            Type::F32,
            Type::F64,
            Type::BOOL,
            Type::UNIT,
            Type::NEVER,
        ] {
            assert_eq!(
                pool.is_thread_safety_type(ty),
                ThreadSafety::Sync,
                "{:?} should be Sync",
                ty
            );
        }
    }

    /// Phase 2 verification: raw pointers are intrinsically `Unsend`,
    /// regardless of their pointee.
    #[test]
    fn thread_safety_raw_ptr_is_unsend() {
        use gruel_builtins::ThreadSafety;
        let pool = TypeInternPool::new();
        let ptr = pool.intern_ptr_const_from_type(Type::I32);
        let mutptr = pool.intern_ptr_mut_from_type(Type::U8);
        assert_eq!(
            pool.is_thread_safety_type(Type::new_ptr_const(ptr)),
            ThreadSafety::Unsend
        );
        assert_eq!(
            pool.is_thread_safety_type(Type::new_ptr_mut(mutptr)),
            ThreadSafety::Unsend
        );
    }

    /// Phase 2 verification: arrays propagate the element's
    /// classification structurally — `[MutPtr(i32); 4]` is `Unsend`.
    #[test]
    fn thread_safety_array_of_ptr_is_unsend() {
        use gruel_builtins::ThreadSafety;
        let pool = TypeInternPool::new();
        let mutptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32));
        let arr_id = pool.intern_array_from_type(mutptr_ty, 4);
        assert_eq!(
            pool.is_thread_safety_type(Type::new_array(arr_id)),
            ThreadSafety::Unsend
        );
    }

    /// Phase 2 verification: arrays of primitives stay `Sync`.
    #[test]
    fn thread_safety_array_of_i32_is_sync() {
        use gruel_builtins::ThreadSafety;
        let pool = TypeInternPool::new();
        let arr_id = pool.intern_array_from_type(Type::I32, 8);
        assert_eq!(
            pool.is_thread_safety_type(Type::new_array(arr_id)),
            ThreadSafety::Sync
        );
    }

    /// Phase 2 verification: `Vec(T)` propagates the element's
    /// classification.
    #[test]
    fn thread_safety_vec_of_ptr_is_unsend() {
        use gruel_builtins::ThreadSafety;
        let pool = TypeInternPool::new();
        let mutptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32));
        let vec_id = pool.intern_vec_from_type(mutptr_ty);
        assert_eq!(
            pool.is_thread_safety_type(Type::new_vec(vec_id)),
            ThreadSafety::Unsend
        );
    }

    /// Phase 2 verification: a single raw pointer field on an otherwise
    /// `Sync` struct poisons the structural minimum to `Unsend`. The
    /// inference is what `validate_consistency` would write into the
    /// struct's `thread_safety` field.
    #[test]
    fn thread_safety_struct_with_ptr_field_infers_unsend() {
        use crate::types::StructField;
        use gruel_builtins::ThreadSafety;

        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let mutptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));

        // Build a struct manually with the structural minimum
        // pre-computed (mirroring what `validate_consistency` does).
        let inferred = [Type::I32, mutptr_ty, Type::USIZE]
            .iter()
            .map(|t| pool.is_thread_safety_type(*t))
            .min()
            .unwrap();
        assert_eq!(inferred, ThreadSafety::Unsend);

        let name = interner.get_or_intern("Buf");
        let def = StructDef {
            name: "Buf".to_string(),
            fields: vec![
                StructField {
                    name: "len".into(),
                    ty: Type::USIZE,
                    is_pub: false,
                },
                StructField {
                    name: "ptr".into(),
                    ty: mutptr_ty,
                    is_pub: false,
                },
            ],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: inferred,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };
        let (sid, _) = pool.register_struct(name, def);
        assert_eq!(
            pool.is_thread_safety_type(Type::new_struct(sid)),
            ThreadSafety::Unsend
        );
    }

    /// Phase 2 verification: nested struct-with-pointer propagates
    /// `Unsend` through the outer struct via `is_thread_safety_type`.
    #[test]
    fn thread_safety_nested_struct_with_ptr_is_unsend() {
        use crate::types::StructField;
        use gruel_builtins::ThreadSafety;

        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let mutptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));

        // Inner struct holds the pointer — Unsend.
        let inner_name = interner.get_or_intern("Inner");
        let inner_def = StructDef {
            name: "Inner".to_string(),
            fields: vec![StructField {
                name: "ptr".into(),
                ty: mutptr_ty,
                is_pub: false,
            }],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: ThreadSafety::Unsend,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };
        let (inner_sid, _) = pool.register_struct(inner_name, inner_def);
        let inner_ty = Type::new_struct(inner_sid);

        // Outer wraps inner — should also be Unsend by structural fold.
        let outer_inferred = [Type::I32, inner_ty]
            .iter()
            .map(|t| pool.is_thread_safety_type(*t))
            .min()
            .unwrap();
        assert_eq!(outer_inferred, ThreadSafety::Unsend);

        let outer_name = interner.get_or_intern("Outer");
        let outer_def = StructDef {
            name: "Outer".to_string(),
            fields: vec![
                StructField {
                    name: "tag".into(),
                    ty: Type::I32,
                    is_pub: false,
                },
                StructField {
                    name: "inner".into(),
                    ty: inner_ty,
                    is_pub: false,
                },
            ],
            posture: Posture::Affine,
            is_clone: false,
            thread_safety: outer_inferred,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: gruel_util::FileId::DEFAULT,
            is_c_layout: false,
        };
        let (outer_sid, _) = pool.register_struct(outer_name, outer_def);
        assert_eq!(
            pool.is_thread_safety_type(Type::new_struct(outer_sid)),
            ThreadSafety::Unsend
        );
    }

    /// Phase 2 verification: ref/mut-ref types inherit the referent's
    /// classification. A `Ref(MutPtr(T))` is `Unsend`.
    #[test]
    fn thread_safety_ref_inherits_referent() {
        use gruel_builtins::ThreadSafety;
        let pool = TypeInternPool::new();
        let mutptr_ty = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::U8));
        let ref_id = pool.intern_ref_from_type(mutptr_ty);
        assert_eq!(
            pool.is_thread_safety_type(Type::new_ref(ref_id)),
            ThreadSafety::Unsend
        );

        let ref_i32 = pool.intern_ref_from_type(Type::I32);
        assert_eq!(
            pool.is_thread_safety_type(Type::new_ref(ref_i32)),
            ThreadSafety::Sync
        );
    }
}
