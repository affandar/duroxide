//! Macro-based Workflow Example
//!
//! This example demonstrates how to use duroxide's proc macros to write
//! durable workflows that look like normal async Rust code.
//!
//! Run with: `cargo run --example macro_workflow --features "sqlite,macros"`

use duroxide::providers::sqlite::SqliteProvider;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{self};
use duroxide::{activity, ActivityContext, Client, OrchestrationContext, OrchestrationRegistry};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ============================================================================
// Data Structures
// ============================================================================

/// User data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    id: String,
    name: String,
    email: String,
}

/// Order data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Order {
    id: String,
    user_id: String,
    items: Vec<String>,
    total: f64,
}

/// Payment info for process_payment activity
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PaymentRequest {
    order_id: String,
    amount: f64,
}

/// Notification request
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NotificationRequest {
    email: String,
    message: String,
}

// ============================================================================
// Define Activities - These are the units of work that can fail and retry
// ============================================================================

/// Activity: Fetch user by ID (input is JSON-encoded string)
async fn fetch_user(_ctx: ActivityContext, input: String) -> Result<String, String> {
    // Parse the JSON-encoded user_id
    let user_id: String = serde_json::from_str(&input).map_err(|e| e.to_string())?;
    
    // Simulate fetching user from database
    let user = User {
        id: user_id.clone(),
        name: format!("User {}", user_id),
        email: format!("user{}@example.com", user_id),
    };
    serde_json::to_string(&user).map_err(|e| e.to_string())
}

/// Activity: Validate order - takes Order as typed input
async fn validate_order(_ctx: ActivityContext, order_json: String) -> Result<String, String> {
    let order: Order = serde_json::from_str(&order_json).map_err(|e| e.to_string())?;

    // Simulate order validation
    if order.total <= 0.0 {
        return Err("Order total must be positive".to_string());
    }

    if order.items.is_empty() {
        return Err("Order must have at least one item".to_string());
    }

    Ok("validated".to_string())
}

/// Activity: Process payment
async fn process_payment(_ctx: ActivityContext, request_json: String) -> Result<String, String> {
    let request: PaymentRequest = serde_json::from_str(&request_json).map_err(|e| e.to_string())?;

    // Simulate payment processing
    if request.amount > 10000.0 {
        return Err("Amount exceeds limit".to_string());
    }

    Ok(format!("payment-{}-{:.2}", request.order_id, request.amount))
}

/// Activity: Ship order (input is JSON-encoded string)
async fn ship_order(_ctx: ActivityContext, input: String) -> Result<String, String> {
    // Parse the JSON-encoded order_id
    let order_id: String = serde_json::from_str(&input).map_err(|e| e.to_string())?;
    
    // Simulate shipping
    Ok(format!("tracking-{}", order_id))
}

/// Activity: Send notification
async fn send_notification(_ctx: ActivityContext, request_json: String) -> Result<String, String> {
    let request: NotificationRequest = serde_json::from_str(&request_json).map_err(|e| e.to_string())?;

    // Simulate sending notification
    println!("Sending notification to {}: {}", request.email, request.message);
    Ok("sent".to_string())
}

// ============================================================================
// Define Orchestration - The workflow that coordinates activities
// ============================================================================

/// Order processing workflow using the activity! macro for clean syntax
async fn order_processing_workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    // Parse the order from input
    let order: Order = serde_json::from_str(&input).map_err(|e| format!("Invalid order: {}", e))?;

    ctx.trace_info(format!("Processing order {} for user {}", order.id, order.user_id));

    // Step 1: Fetch user details - looks like a normal function call!
    let user_json = activity!(ctx, fetch_user(order.user_id.clone())).await?;
    let user: User = serde_json::from_str(&user_json).map_err(|e| e.to_string())?;

    ctx.trace_info(format!("Found user: {}", user.name));

    // Step 2: Validate the order - serialize the order struct for the activity
    let order_json = serde_json::to_string(&order).map_err(|e| e.to_string())?;
    // Use schedule_activity directly when you have pre-serialized data
    let _validation = ctx.schedule_activity("validate_order", order_json).into_activity().await?;

    ctx.trace_info("Order validated successfully");

    // Step 3: Process payment - using a typed request struct
    let payment_request = PaymentRequest {
        order_id: order.id.clone(),
        amount: order.total,
    };
    let payment_id = activity!(ctx, process_payment(payment_request)).await?;

    ctx.trace_info(format!("Payment processed: {}", payment_id));

    // Step 4: Ship the order
    let tracking_number = activity!(ctx, ship_order(order.id.clone())).await?;

    ctx.trace_info(format!("Order shipped: {}", tracking_number));

    // Step 5: Notify the user
    let notification = NotificationRequest {
        email: user.email.clone(),
        message: format!("Your order {} has shipped! Tracking: {}", order.id, tracking_number),
    };
    let _ = activity!(ctx, send_notification(notification)).await?;

    ctx.trace_info("Notification sent");

    // Return the tracking number
    Ok(tracking_number)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Create a temporary SQLite database
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("macro_workflow.db");
    std::fs::File::create(&db_path)?;
    let db_url = format!("sqlite:{}", db_path.to_str().unwrap());
    let store = Arc::new(SqliteProvider::new(&db_url, None).await?);

    // Register activities
    let activities = ActivityRegistry::builder()
        .register("fetch_user", fetch_user)
        .register("validate_order", validate_order)
        .register("process_payment", process_payment)
        .register("ship_order", ship_order)
        .register("send_notification", send_notification)
        .build();

    // Register the orchestration
    let orchestrations = OrchestrationRegistry::builder()
        .register("OrderProcessing", order_processing_workflow)
        .build();

    // Start the runtime
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;

    // Create a client
    let client = Client::new(store);

    // Create a test order
    let order = Order {
        id: "ORD-001".to_string(),
        user_id: "USR-123".to_string(),
        items: vec!["Widget A".to_string(), "Widget B".to_string()],
        total: 99.99,
    };

    let order_json = serde_json::to_string(&order)?;

    // Start the orchestration
    let instance_id = "order-processing-1";
    client
        .start_orchestration(instance_id, "OrderProcessing", order_json)
        .await?;

    println!("Started order processing workflow");

    // Wait for completion
    match client
        .wait_for_orchestration(instance_id, std::time::Duration::from_secs(30))
        .await
        .map_err(|e| format!("Wait error: {e:?}"))?
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            println!("Order processing completed!");
            println!("Tracking number: {}", output);
        }
        duroxide::OrchestrationStatus::Failed { details } => {
            println!("Order processing failed: {}", details.display_message());
        }
        status => {
            println!("Unexpected status: {:?}", status);
        }
    }

    rt.shutdown(None).await;
    Ok(())
}
