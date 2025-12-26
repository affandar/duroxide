//! Memory and CPU footprint comparison with configurable options
//!
//! Environment variables:
//!   MODE              - sqlite, pg, pg_opt
//!   TOKIO_THREADS     - 0 for current_thread, N for multi_thread with N workers
//!   ORCH_CONCURRENCY  - number of orchestration workers (default 2)
//!   WORKER_CONCURRENCY - number of activity workers (default 2)
//!   DUROXIDE_PG_POOL_MAX - PG pool size (default 10)
//!   IDLE_SECONDS      - how long to stay alive (default 999999)

use std::sync::Arc;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::var("MODE").unwrap_or_else(|_| "help".into());
    let tokio_threads: usize = std::env::var("TOKIO_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0); // 0 = current_thread
    
    let rt = if tokio_threads == 0 {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
    } else {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(tokio_threads)
            .enable_all()
            .build()?
    };

    match mode.as_str() {
        "sqlite" => rt.block_on(run_sqlite())?,
        "pg" => rt.block_on(run_pg())?,
        "pg_opt" => rt.block_on(run_pg_opt())?,
        _ => {
            eprintln!("Configurable memory/CPU footprint test");
            eprintln!();
            eprintln!("Environment variables:");
            eprintln!("  MODE=sqlite|pg|pg_opt");
            eprintln!("  TOKIO_THREADS=0 (current_thread) or N (multi_thread with N workers)");
            eprintln!("  ORCH_CONCURRENCY=N (orchestration workers, default 2)");
            eprintln!("  WORKER_CONCURRENCY=N (activity workers, default 2)");
            eprintln!("  DUROXIDE_PG_POOL_MAX=N (PG pool size, default 10)");
            eprintln!("  IDLE_SECONDS=N (how long to stay alive, default 999999)");
        }
    }

    Ok(())
}

fn get_runtime_options() -> duroxide::runtime::RuntimeOptions {
    use duroxide::runtime::RuntimeOptions;
    
    let orch_concurrency: usize = std::env::var("ORCH_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    
    let worker_concurrency: usize = std::env::var("WORKER_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    
    RuntimeOptions {
        orchestration_concurrency: orch_concurrency,
        worker_concurrency: worker_concurrency,
        ..Default::default()
    }
}

fn get_idle_seconds() -> u64 {
    std::env::var("IDLE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(999999)
}

async fn run_sqlite() -> Result<(), Box<dyn std::error::Error>> {
    use duroxide::providers::sqlite::SqliteProvider;
    use duroxide::runtime::registry::ActivityRegistry;
    use duroxide::runtime;
    use duroxide::OrchestrationRegistry;

    let options = get_runtime_options();
    eprintln!("[sqlite] orch={} worker={}", 
        options.orchestration_concurrency, 
        options.worker_concurrency);
    
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();

    let store = Arc::new(SqliteProvider::new_in_memory().await?);
    let activities = ActivityRegistry::builder().build();
    let orchestrations = OrchestrationRegistry::builder().build();
    let _rt = runtime::Runtime::start_with_options(
        store, 
        Arc::new(activities), 
        orchestrations,
        options
    ).await;

    tokio::time::sleep(Duration::from_secs(get_idle_seconds())).await;
    Ok(())
}

async fn run_pg() -> Result<(), Box<dyn std::error::Error>> {
    use duroxide_pg::PostgresProvider;
    use duroxide::runtime::registry::ActivityRegistry;
    use duroxide::runtime;
    use duroxide::OrchestrationRegistry;

    let options = get_runtime_options();
    let pool_max = std::env::var("DUROXIDE_PG_POOL_MAX").unwrap_or_else(|_| "10".into());
    eprintln!("[pg] orch={} worker={} pool={}", 
        options.orchestration_concurrency, 
        options.worker_concurrency,
        pool_max);
    
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();
    let _ = dotenvy::from_path("/Users/affandar/workshop/duroxide-pg/.env");
    
    let db_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");
    
    let store = Arc::new(PostgresProvider::new(&db_url).await?);
    let activities = ActivityRegistry::builder().build();
    let orchestrations = OrchestrationRegistry::builder().build();
    let _rt = runtime::Runtime::start_with_options(
        store, 
        Arc::new(activities), 
        orchestrations,
        options
    ).await;

    tokio::time::sleep(Duration::from_secs(get_idle_seconds())).await;
    Ok(())
}

async fn run_pg_opt() -> Result<(), Box<dyn std::error::Error>> {
    use duroxide_pg_opt::PostgresProvider;
    use duroxide::runtime::registry::ActivityRegistry;
    use duroxide::runtime;
    use duroxide::OrchestrationRegistry;

    let options = get_runtime_options();
    let pool_max = std::env::var("DUROXIDE_PG_POOL_MAX").unwrap_or_else(|_| "10".into());
    eprintln!("[pg_opt] orch={} worker={} pool={}", 
        options.orchestration_concurrency, 
        options.worker_concurrency,
        pool_max);
    
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();
    let _ = dotenvy::from_path("/Users/affandar/workshop/duroxide-pg/.env");
    
    let db_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");
    
    let store = Arc::new(PostgresProvider::new(&db_url).await?);
    let activities = ActivityRegistry::builder().build();
    let orchestrations = OrchestrationRegistry::builder().build();
    let _rt = runtime::Runtime::start_with_options(
        store, 
        Arc::new(activities), 
        orchestrations,
        options
    ).await;

    tokio::time::sleep(Duration::from_secs(get_idle_seconds())).await;
    Ok(())
}
