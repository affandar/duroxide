//! Scenario tests derived from real-world usage patterns
//!
//! This module contains regression tests that model specific scenarios found in actual
//! production usage of duroxide. These tests validate complex orchestration patterns,
//! long-running workflows, and edge cases discovered during real-world deployments.
//!
//! Examples include:
//! - Instance actor patterns with health checks
//! - Long continue-as-new chains
//! - Concurrent orchestration execution
//! - Complex activity workflows
//! - Single-thread runtime mode (for embedded hosts)

#[path = "scenarios/toygres.rs"]
mod toygres;

#[path = "scenarios/single_thread.rs"]
mod single_thread;
