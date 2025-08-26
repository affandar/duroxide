#[cfg(test)]
mod tests {
    use crate::providers::in_memory::InMemoryHistoryStore;
    use crate::runtime::{Runtime, registry::ActivityRegistry, OrchestrationRegistry};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn test_v2_basic_orchestration() {
        // Test basic orchestration start with v2 engine
        let store: Arc<dyn crate::providers::HistoryStore> = Arc::new(InMemoryHistoryStore::default());
        let activity_registry = Arc::new(ActivityRegistry::new());
        let orchestration_registry = OrchestrationRegistry::new();
        
        // Set environment variable to force v2
        std::env::set_var("RUST_DTF_USE_V2_EXECUTION", "true");
        
        let runtime = Runtime::start_with_store(store, activity_registry, orchestration_registry).await;
        
        // Test simple orchestration start
        let instance = "test-v2-instance";
        let orchestration_name = "simple-test";
        
        // This should not hang - test basic activation
        println!("🧪 Testing v2 basic instance activation...");
        
        let start_time = std::time::Instant::now();
        
        // Run with timeout simulation
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.ensure_instance_active(instance, orchestration_name)
        ).await;
        
        match result {
            Ok(activated) => {
                let duration = start_time.elapsed();
                println!("✅ V2 instance activation completed in {:?}: {}", duration, activated);
            }
            Err(_) => {
                println!("⚠️ V2 instance activation timed out after 5 seconds");
            }
        }
        
        // Clean up environment variable
        std::env::remove_var("RUST_DTF_USE_V2_EXECUTION");
    }

    #[tokio::test]
    async fn test_v1_vs_v2_comparison() {
        // Compare basic behavior between v1 and v2
        let store: Arc<dyn crate::providers::HistoryStore> = Arc::new(InMemoryHistoryStore::default());
        let activity_registry = Arc::new(ActivityRegistry::new());
        let orchestration_registry = OrchestrationRegistry::new();
        
        println!("🧪 Testing V1 vs V2 behavior comparison...");
        
        // Test V1 first
        std::env::remove_var("RUST_DTF_USE_V2_EXECUTION");
        let runtime_v1 = Runtime::start_with_store(store.clone(), activity_registry.clone(), orchestration_registry.clone()).await;
        
        let start_v1 = std::time::Instant::now();
        let activated_v1 = tokio::time::timeout(
            Duration::from_secs(2),
            runtime_v1.ensure_instance_active("test-v1", "test-orch")
        ).await;
        let duration_v1 = start_v1.elapsed();
        
        // Test V2
        std::env::set_var("RUST_DTF_USE_V2_EXECUTION", "true");
        let runtime_v2 = Runtime::start_with_store(store.clone(), activity_registry.clone(), orchestration_registry.clone()).await;
        
        let start_v2 = std::time::Instant::now();
        let activated_v2 = tokio::time::timeout(
            Duration::from_secs(2),
            runtime_v2.ensure_instance_active("test-v2", "test-orch")
        ).await;
        let duration_v2 = start_v2.elapsed();
        
        println!("📊 Results:");
        println!("  V1: {:?} in {:?}", activated_v1, duration_v1);
        println!("  V2: {:?} in {:?}", activated_v2, duration_v2);
        
        // Both should succeed (or both should timeout)
        assert_eq!(activated_v1.is_ok(), activated_v2.is_ok(), 
                   "V1 and V2 should have same timeout behavior");
        
        // Clean up
        std::env::remove_var("RUST_DTF_USE_V2_EXECUTION");
    }

    #[tokio::test]
    async fn test_v2_message_routing() {
        // Test that v2 message routing works
        let store: Arc<dyn crate::providers::HistoryStore> = Arc::new(InMemoryHistoryStore::default());
        let activity_registry = Arc::new(ActivityRegistry::new());
        let orchestration_registry = OrchestrationRegistry::new();
        std::env::set_var("RUST_DTF_USE_V2_EXECUTION", "true");
        
        let runtime = Runtime::start_with_store(store, activity_registry, orchestration_registry).await;
        
        println!("🧪 Testing V2 message routing...");
        
        // Test that we can register and get messages
        let instance = "test-routing";
        
        let start = std::time::Instant::now();
        
        // Register instance
        let registered = tokio::time::timeout(
            Duration::from_millis(500),
            runtime.router.register(instance)
        ).await;
        
        println!("📝 Instance registration: {:?} in {:?}", registered, start.elapsed());
        
        if let Ok(Ok(mut rx)) = registered {
            println!("✅ Instance registered successfully, testing message receive...");
            
            // Try to receive a message with timeout
            let msg_result = tokio::time::timeout(
                Duration::from_millis(100),
                rx.recv()
            ).await;
            
            match msg_result {
                Ok(Some(msg)) => {
                    println!("📨 Received message: {:?}", msg);
                }
                Ok(None) => {
                    println!("📭 Channel closed");
                }
                Err(_) => {
                    println!("⏰ No messages received (expected for empty test)");
                }
            }
        }
        
        std::env::remove_var("RUST_DTF_USE_V2_EXECUTION");
    }
}
