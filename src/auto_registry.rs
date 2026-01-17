//! Compile-time auto-registration via `inventory`.
//!
//! This enables "no manual registry wiring" setups:
//! - annotate activities/orchestrations with proc macros
//! - call `duroxide::auto_activities()` / `duroxide::auto_orchestrations()`
//!
//! This module is intentionally small and purely additive to the existing API.

use crate::runtime::registry::{ActivityRegistry, ActivityRegistryBuilder, OrchestrationRegistry, OrchestrationRegistryBuilder};
use std::sync::Arc;

/// Inventory item for registering an activity into an `ActivityRegistryBuilder`.
pub struct ActivityAutoReg {
    pub register: fn(ActivityRegistryBuilder) -> ActivityRegistryBuilder,
}

/// Inventory item for registering an orchestration into an `OrchestrationRegistryBuilder`.
pub struct OrchestrationAutoReg {
    pub register: fn(OrchestrationRegistryBuilder) -> OrchestrationRegistryBuilder,
}

crate::inventory::collect!(ActivityAutoReg);
crate::inventory::collect!(OrchestrationAutoReg);

/// Build an `ActivityRegistry` from all `#[duroxide::activity]` registrations in the binary.
pub fn auto_activities() -> Arc<ActivityRegistry> {
    let builder = ActivityRegistry::builder();
    let builder = crate::inventory::iter::<ActivityAutoReg>
        .into_iter()
        .fold(builder, |b, item| (item.register)(b));
    Arc::new(builder.build())
}

/// Build an `OrchestrationRegistry` from all `#[duroxide::orchestration]` registrations in the binary.
pub fn auto_orchestrations() -> OrchestrationRegistry {
    let builder = OrchestrationRegistry::builder();
    let builder = crate::inventory::iter::<OrchestrationAutoReg>
        .into_iter()
        .fold(builder, |b, item| (item.register)(b));
    builder.build()
}

