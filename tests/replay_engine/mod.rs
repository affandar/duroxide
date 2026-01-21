//! Replay Engine Tests
//!
//! This module contains comprehensive tests for the ReplayEngine,
//! verifying the core history-action matching, completion processing,
//! and determinism enforcement.

mod helpers;

mod fresh_execution;
mod replay_with_completions;
mod partial_completion;
mod sequential_progress;
mod completion_messages;
mod cancellation;
mod nondeterminism;
mod history_corruption;
mod composition;
mod event_allocation;
mod is_replaying;
mod sub_orchestration;
mod failure_handling;
mod panic_handling;
mod edge_cases;
