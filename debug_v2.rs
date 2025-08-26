use std::sync::Arc;
use tokio;

#[tokio::main]
async fn main() {
    println!("🧪 Testing V2 Integration");
    
    // Set the flag
    std::env::set_var("RUST_DTF_USE_V2_EXECUTION", "true");
    
    // Create runtime
    let store = Arc::new(rust_dtf::providers::in_memory::InMemoryHistoryStore::default());
    let activity_registry = Arc::new(rust_dtf::runtime::registry::ActivityRegistry::default());
    let orchestration_registry = rust_dtf::runtime::OrchestrationRegistry::default();
    
    println!("🧪 Creating runtime...");
    let runtime = rust_dtf::runtime::Runtime::start_with_store(
        store,
        activity_registry,
        orchestration_registry
    ).await;
    
    println!("🧪 Runtime created successfully");
    
    // Try to ensure instance active
    println!("🧪 Testing ensure_instance_active...");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.ensure_instance_active("test-instance", "test-orch")
    ).await;
    
    match result {
        Ok(active) => println!("🧪 Instance active: {}", active),
        Err(_) => println!("🧪 ensure_instance_active timed out"),
    }
    
    println!("🧪 Test completed");
}
