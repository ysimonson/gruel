//! Place-base shared between AIR and CFG.
//!
//! Both IRs represent memory locations as a base (a local slot or a
//! parameter slot) plus a list of projections (field access, indexing).
//! The base half is identical in both — extracted here so it isn't
//! defined twice.
//!
//! The projection lists differ (AIR's `Index` carries an `AirRef`, CFG's
//! a `CfgValue`), so those stay per-IR for now.

/// Where a memory location starts: a local slot or a parameter slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlaceBase {
    /// Local variable slot.
    Local(u32),
    /// Parameter slot (for parameters, including `inout`).
    Param(u32),
}
