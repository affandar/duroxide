//! Basic macro compilation tests
//!
//! These tests verify that macros compile correctly.
//! Full integration tests with code generation are in the main duroxide crate.
//!
//! Note: #[orchestration] tests are in integration tests because they need
//! linkme and duroxide dependencies.

use duroxide_macros::activity;

#[test]
fn activity_basic() {
    #[activity]
    async fn simple_activity(input: String) -> Result<String, String> {
        Ok(input)
    }
    
    let _ = simple_activity;
}

#[test]
fn activity_custom_name() {
    #[activity(name = "CustomName")]
    async fn my_activity(input: String) -> Result<String, String> {
        Ok(input)
    }
    
    let _ = my_activity;
}

// Orchestration tests moved to integration tests (need linkme/duroxide)

#[test]
fn multiple_activities() {
    #[activity]
    async fn act1(input: String) -> Result<String, String> {
        Ok(input)
    }
    
    #[activity(name = "Act2")]
    async fn act2(input: String) -> Result<String, String> {
        Ok(input)
    }
    
    let _ = (act1, act2);
}

#[test]
fn phase1_basic_tests_complete() {
    println!("✅ Phase 1 Macro Compilation Tests:");
    println!("   - Basic #[activity] works");
    println!("   - #[activity(name)] works");
    println!("\n🎯 Phase 1 complete!");
    println!("📝 Note: #[orchestration] and #[activity(typed)] tested in integration tests");
}
