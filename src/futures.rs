//! Futures utilities for duroxide.
//!
//! This module contains helper functions for durable orchestrations.

use std::time::{SystemTime, UNIX_EPOCH};

/// Generate a deterministic GUID for use in orchestrations.
///
/// Uses timestamp + thread-local counter for uniqueness.
pub(crate) fn generate_guid() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    // Thread-local counter for uniqueness within the same timestamp
    thread_local! {
        static COUNTER: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    let counter = COUNTER.with(|c| {
        let val = c.get();
        c.set(val.wrapping_add(1));
        val
    });

    // Format as UUID-like string
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (timestamp >> 96) as u32,
        ((timestamp >> 80) & 0xFFFF) as u16,
        (counter & 0xFFFF) as u16,
        ((timestamp >> 64) & 0xFFFF) as u16,
        (timestamp & 0xFFFFFFFFFFFF) as u64
    )
}
