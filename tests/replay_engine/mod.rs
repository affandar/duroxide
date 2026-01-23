//! Replay Engine Tests
//!
//! This module contains comprehensive tests for the ReplayEngine,
//! verifying the core history-action matching, completion processing,
//! and determinism enforcement.

mod helpers;

mod cancellation;
mod completion_messages;
mod composition;
mod edge_cases;
mod event_allocation;
mod failure_handling;
mod fresh_execution;
mod history_corruption;
mod is_replaying;
mod nondeterminism;
mod panic_handling;
mod partial_completion;
mod replay_with_completions;
mod sequential_progress;
mod sub_orchestration;
mod unobserved_futures;
