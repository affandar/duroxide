//! Convenient imports for using duroxide.
//!
//! # Example
//! ```
//! use duroxide::prelude::*;
//! ```

pub use crate::{
    Client,
    OrchestrationContext,
    Event,
};

pub use crate::runtime::{
    Runtime,
    RuntimeOptions,
    OrchestrationStatus,
    OrchestrationRegistry,
    registry::ActivityRegistry,
};

pub use crate::providers::sqlite::SqliteProvider;

// Re-export macros when feature is enabled
#[cfg(feature = "macros")]
pub use duroxide_macros::{
    activity,
    orchestration,
    durable,
    durable_trace_info,
    durable_trace_warn,
    durable_trace_error,
    durable_trace_debug,
    durable_newguid,
    durable_utcnow,
};

// Common standard library imports
pub use std::sync::Arc;
pub use std::time::Duration;

