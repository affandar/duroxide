//! Runtime limits and constants.
//!
//! Collect all hard limits in one place so they're easy to find, document,
//! and reference from both runtime code and provider validators.

/// Maximum number of unmatched persistent events that can be carried forward
/// across a `continue_as_new()` boundary.
///
/// When the list exceeds this limit the oldest events (by history order) are
/// dropped and a warning is logged.
pub const MAX_CARRY_FORWARD_EVENTS: usize = 20;

/// Maximum size in bytes for the custom status string set via
/// `ctx.set_custom_status()`.
///
/// If the orchestration sets a custom status that exceeds this limit, the
/// runtime will fail the orchestration with an `Infrastructure` error
/// before the ack is committed.
///
/// 256 KiB — generous for progress/status strings while preventing unbounded
/// growth in the execution metadata row.
pub const MAX_CUSTOM_STATUS_BYTES: usize = 256 * 1024;
