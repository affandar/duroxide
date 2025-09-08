// Example implementation of management APIs for Duroxide Runtime
// This shows how the proposed APIs would be used in practice

use duroxide::runtime::{Runtime, InstanceFilter, InstanceSummary, OrchestrationStatus};
use duroxide::providers::sqlite::SqliteProvider;
use std::sync::Arc;
use chrono::{DateTime, Utc};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup runtime with SQLite provider
    let store = Arc::new(SqliteProvider::new("sqlite:./management_demo.db").await?);
    let activities = duroxide::runtime::registry::ActivityRegistry::builder().build();
    let orchestrations = duroxide::OrchestrationRegistry::builder().build();
    
    let runtime = Runtime::start_with_store(store, Arc::new(activities), orchestrations).await;

    // === EXAMPLE 1: Instance Discovery & Monitoring ===
    
    // List all running instances
    let running_filter = InstanceFilter {
        status: Some(vec![OrchestrationStatus::Running]),
        orchestration_name: None,
        created_after: None,
        created_before: None,
        has_parent: None,
        parent_instance: None,
    };
    
    let running_instances = runtime.list_instances(Some(running_filter)).await;
    println!("Found {} running instances", running_instances.len());
    
    for instance in &running_instances {
        println!("  {} - {} ({})", 
            instance.instance_id, 
            instance.orchestration_name,
            instance.status
        );
    }

    // === EXAMPLE 2: Bulk Status Checking ===
    
    let instance_ids: Vec<String> = running_instances
        .iter()
        .map(|i| i.instance_id.clone())
        .collect();
    
    let statuses = runtime.get_instances_status(instance_ids).await;
    println!("\nBulk status check:");
    for (instance_id, status) in statuses {
        println!("  {}: {:?}", instance_id, status);
    }

    // === EXAMPLE 3: Detailed Instance Analysis ===
    
    if let Some(instance) = running_instances.first() {
        if let Some(details) = runtime.get_instance_details(&instance.instance_id).await {
            println!("\nDetailed analysis for {}:", instance.instance_id);
            println!("  Executions: {}", details.executions.len());
            println!("  Total activities: {}", details.statistics.total_activities_scheduled);
            println!("  Completed activities: {}", details.statistics.total_activities_completed);
            
            if let Some(current) = details.current_execution {
                println!("  Current execution:");
                println!("    Pending activities: {}", current.pending_activities.len());
                println!("    Pending timers: {}", current.pending_timers.len());
                println!("    Pending events: {}", current.pending_events.len());
            }
        }
    }

    // === EXAMPLE 4: Runtime Health Monitoring ===
    
    let metrics = runtime.get_runtime_metrics().await;
    println!("\nRuntime Health:");
    println!("  Uptime: {} seconds", metrics.uptime_seconds);
    println!("  Active instances: {}", metrics.active_instances);
    println!("  Total completed: {}", metrics.total_instances_completed);
    println!("  Success rate: {:.2}%", 
        (metrics.total_instances_completed as f64 / 
         (metrics.total_instances_completed + metrics.total_instances_failed) as f64) * 100.0
    );
    println!("  Orchestrator healthy: {}", metrics.orchestrator_dispatcher_healthy);
    println!("  Worker healthy: {}", metrics.worker_dispatcher_healthy);
    println!("  Timer healthy: {}", metrics.timer_dispatcher_healthy);

    // === EXAMPLE 5: Provider Diagnostics ===
    
    let provider_diag = runtime.get_provider_diagnostics().await;
    println!("\nProvider Diagnostics:");
    println!("  Type: {}", provider_diag.provider_type);
    println!("  Connection healthy: {}", provider_diag.connection_healthy);
    println!("  Total instances: {}", provider_diag.total_instances_stored);
    println!("  Total events: {}", provider_diag.total_history_events);
    println!("  Avg read latency: {:.2}ms", provider_diag.average_read_latency_ms);
    println!("  Avg write latency: {:.2}ms", provider_diag.average_write_latency_ms);

    // === EXAMPLE 6: Queue Health ===
    
    let queue_metrics = runtime.get_queue_metrics().await;
    println!("\nQueue Health:");
    println!("  Orchestrator queue: {} items", queue_metrics.orchestrator_queue_size);
    println!("  Worker queue: {} items", queue_metrics.worker_queue_size);
    println!("  Timer queue: {} items", queue_metrics.timer_queue_size);
    
    if let Some(oldest_age) = queue_metrics.orchestrator_queue_oldest_item_age_ms {
        println!("  Oldest orchestrator item: {}ms old", oldest_age);
    }

    // === EXAMPLE 7: Orchestration Type Analytics ===
    
    let orch_metrics = runtime.get_orchestration_metrics().await;
    println!("\nOrchestration Type Metrics:");
    for metric in orch_metrics {
        println!("  {}:", metric.orchestration_name);
        println!("    Total started: {}", metric.total_started);
        println!("    Success rate: {:.2}%", metric.success_rate * 100.0);
        println!("    Avg duration: {:.2}ms", metric.average_duration_ms);
        println!("    Currently active: {}", metric.active_count);
    }

    // === EXAMPLE 8: Bulk Operations ===
    
    // Find failed instances to potentially retry
    let failed_filter = InstanceFilter {
        status: Some(vec![OrchestrationStatus::Failed { error: "".to_string() }]),
        created_after: Some(Utc::now() - chrono::Duration::hours(1)), // Last hour
        ..Default::default()
    };
    
    let failed_instances = runtime.list_instances(Some(failed_filter)).await;
    
    if !failed_instances.is_empty() {
        println!("\nFound {} failed instances in the last hour", failed_instances.len());
        
        // Could implement bulk retry logic here
        // let retry_request = BulkRetryRequest {
        //     instances: failed_instances.iter().map(|i| i.instance_id.clone()).collect(),
        //     reason: "Automatic retry after failure".to_string(),
        // };
        // let retry_response = runtime.retry_instances(retry_request).await;
    }

    // === EXAMPLE 9: Real-time Monitoring (Conceptual) ===
    
    // This would be used in a monitoring dashboard
    /*
    let event_stream = runtime.stream_instance_events(None).await;
    tokio::spawn(async move {
        while let Some(event) = event_stream.next().await {
            match event.event_type {
                InstanceEventType::Failed => {
                    eprintln!("ALERT: Instance {} failed: {:?}", 
                        event.instance_id, event.details);
                }
                InstanceEventType::Completed => {
                    println!("✅ Instance {} completed successfully", event.instance_id);
                }
                _ => {}
            }
        }
    });
    */

    // === EXAMPLE 10: Dependency Analysis ===
    
    // Analyze parent-child relationships for complex workflows
    if let Some(instance) = running_instances.first() {
        let dependencies = runtime.get_instance_dependencies(&instance.instance_id).await;
        println!("\nDependency Analysis for {}:", instance.instance_id);
        println!("  Total descendants: {}", dependencies.total_descendants);
        
        if let Some(parent) = dependencies.parent {
            println!("  Parent: {} ({})", parent.instance_id, parent.orchestration_name);
        }
        
        if !dependencies.children.is_empty() {
            println!("  Children:");
            for child in dependencies.children {
                println!("    {} - {} ({}) - {} descendants", 
                    child.instance_id, 
                    child.orchestration_name,
                    child.status,
                    child.child_count
                );
            }
        }
    }

    runtime.shutdown().await;
    Ok(())
}

// Example monitoring dashboard function
async fn monitoring_dashboard(runtime: &Runtime) {
    loop {
        let metrics = runtime.get_runtime_metrics().await;
        let queue_metrics = runtime.get_queue_metrics().await;
        
        // Check for alerts
        if metrics.active_instances > 1000 {
            eprintln!("ALERT: High instance count: {}", metrics.active_instances);
        }
        
        if queue_metrics.orchestrator_queue_size > 100 {
            eprintln!("ALERT: Orchestrator queue backlog: {}", queue_metrics.orchestrator_queue_size);
        }
        
        if let Some(oldest_age) = queue_metrics.orchestrator_queue_oldest_item_age_ms {
            if oldest_age > 60_000 { // 1 minute
                eprintln!("ALERT: Stale queue items detected: {}ms old", oldest_age);
            }
        }
        
        // Update dashboard metrics
        println!("Dashboard Update: {} active, {} queued", 
            metrics.active_instances, 
            queue_metrics.orchestrator_queue_size
        );
        
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
    }
}

// Example bulk operations for operational management
async fn operational_management(runtime: &Runtime) {
    // Cancel all instances of a specific orchestration type that are stuck
    let stuck_filter = InstanceFilter {
        orchestration_name: Some("ProblematicOrchestration".to_string()),
        status: Some(vec![OrchestrationStatus::Running]),
        created_before: Some(Utc::now() - chrono::Duration::hours(24)), // Older than 24h
        ..Default::default()
    };
    
    let cancel_request = BulkCancelRequest {
        instances: None,
        filter: Some(stuck_filter),
        reason: "Cancelling stuck instances older than 24 hours".to_string(),
        max_instances: Some(100), // Safety limit
    };
    
    let cancel_response = runtime.cancel_instances(cancel_request).await;
    println!("Cancelled {} stuck instances", cancel_response.cancelled_instances.len());
    
    // Raise maintenance events to all running instances
    let maintenance_request = BulkEventRequest {
        instances: runtime.list_instances(Some(InstanceFilter {
            status: Some(vec![OrchestrationStatus::Running]),
            ..Default::default()
        })).await.iter().map(|i| i.instance_id.clone()).collect(),
        event_name: "MaintenanceWindow".to_string(),
        event_data: serde_json::json!({
            "start_time": Utc::now(),
            "duration_minutes": 30,
            "message": "Scheduled maintenance window"
        }).to_string(),
    };
    
    let event_response = runtime.raise_event_bulk(maintenance_request).await;
    println!("Sent maintenance notifications to {} instances", 
        event_response.delivered_instances.len());
}
