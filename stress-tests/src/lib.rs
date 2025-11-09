//! Duroxide Stress Testing Library
//!
//! This library provides infrastructure for running stress tests on Duroxide providers.
//! It can be used with built-in providers (SQLite) or custom provider implementations.
//!
//! This crate now re-exports the stress test infrastructure from the main duroxide crate
//! (behind the `provider-test` feature), providing backward compatibility while allowing
//! external provider implementations to use the same infrastructure directly.

pub mod parallel_orchestrations;

// Re-export the core stress test infrastructure from the main crate
pub use duroxide::provider_stress_tests::{
    create_default_activities, create_default_orchestrations, print_comparison_table, run_stress_test,
    StressTestConfig, StressTestResult,
};
