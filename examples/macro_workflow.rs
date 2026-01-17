//! Macro-based Workflow Example
//!
//! This example demonstrates how to use duroxide's `activity!` macro to write
//! durable workflows that feel like normal async Rust code.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    id: String,
    name: String,
    email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Order {
    id: String,
    user_id: String,
    items: Vec<String>,
    total: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PaymentRequest {
    order_id: String,
    amount: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NotificationRequest {
    email: String,
    message: String,
}

// ============================================================================
// Activities - Business logic units
// ============================================================================

async fn fetch_user(_ctx: ActivityContext, input: String) -> Result<String, String> {
    let user_id: String = serde_json::from_str(&input).map_err(|e| e.to_string())?;
    let user = User {
        id: user_id.clone(),
        name: format!("User {}", user_id),
        email: format!("user{}@example.com", user_id),
    };
    serde_json::to_string(&user).map_err(|e| e.to_string())
}

async fn validate_order(_ctx: ActivityContext, input: String) -> Result<String, String> {
    let order: Order = serde_json::from_str(&input).map_err(|e| e.to_string())?;
    if order.total <= 0.0 {
        return Err("Order total must be positive".to_string());
    }
    if order.items.is_empty() {
        return Err("Order must have at least one item".to_string());
    }
    Ok("validated".to_string())
}

async fn process_payment(_ctx: ActivityContext, input: String) -> Result<String, String> {
    let request: PaymentRequest = serde_json::from_str(&input).map_err(|e| e.to_string())?;
    if request.amount > 10000.0 {
        return Err("Amount exceeds limit".to_string());
    }
    Ok(format!("payment-{}-{:.2}", request.order_id, request.amount))
}

async fn ship_order(_ctx: ActivityContext, input: String) -> Result<String, String> {
    let order_id: String = serde_json::from_str(&input).map_err(|e| e.to_string())?;
    Ok(format!("tracking-{}", order_id))
}

async fn send_notification(_ctx: ActivityContext, input: String) -> Result<String, String> {
    let request: NotificationRequest = serde_json::from_str(&input).map_err(|e| e.to_string())?;
    println!("📧 Sending to {}: {}", request.email, request.message);
    Ok("sent".to_string())
}

// ============================================================================
// Orchestration - Clean activity calls with method-like syntax!
// ============================================================================

/// Order processing workflow.
/// 
/// Notice how clean the activity calls are with the method-like syntax:
/// `activity!(ctx.func(arg))` reads naturally as "ctx dot function"
async fn order_workflow(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    let order: Order = serde_json::from_str(&input).map_err(|e| format!("Invalid order: {}", e))?;

    ctx.trace_info(format!("Processing order {}", order.id));

    // Activity calls look like method calls on ctx!
    let user_json = activity!(ctx.fetch_user(order.user_id.clone())).await?;
    let user: User = serde_json::from_str(&user_json).unwrap();
    
    ctx.trace_info(format!("User: {}", user.name));

    // Struct inputs work naturally - just pass them in
    let _ = activity!(ctx.validate_order(order.clone())).await?;
    ctx.trace_info("Order validated");

    let payment = PaymentRequest {
        order_id: order.id.clone(),
        amount: order.total,
    };
    let payment_id = activity!(ctx.process_payment(payment)).await?;
    ctx.trace_info(format!("Payment: {}", payment_id));

    let tracking = activity!(ctx.ship_order(order.id.clone())).await?;
    ctx.trace_info(format!("Shipped: {}", tracking));

    let notification = NotificationRequest {
        email: user.email,
        message: format!("Order {} shipped! Tracking: {}", order.id, tracking),
    };
    let _ = activity!(ctx.send_notification(notification)).await?;

    Ok(tracking)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Setup database
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("workflow.db");
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

    // Register orchestration
    let orchestrations = OrchestrationRegistry::builder()
        .register("OrderWorkflow", order_workflow)
        .build();

    // Start runtime
    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
    let client = Client::new(store);

    // Create and run an order
    let order = Order {
        id: "ORD-001".to_string(),
        user_id: "USR-123".to_string(),
        items: vec!["Widget".to_string(), "Gadget".to_string()],
        total: 99.99,
    };

    println!("🚀 Starting order workflow...\n");
    
    client
        .start_orchestration("order-1", "OrderWorkflow", serde_json::to_string(&order)?)
        .await?;

    match client
        .wait_for_orchestration("order-1", std::time::Duration::from_secs(30))
        .await
        .map_err(|e| format!("{e:?}"))?
    {
        duroxide::OrchestrationStatus::Completed { output } => {
            println!("\n✅ Order completed! Tracking: {}", output);
        }
        duroxide::OrchestrationStatus::Failed { details } => {
            println!("\n❌ Failed: {}", details.display_message());
        }
        _ => println!("\n⏳ Still running..."),
    }

    rt.shutdown(None).await;
    Ok(())
}
