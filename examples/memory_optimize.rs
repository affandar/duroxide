//! Memory optimization experiments.
//!
//! Tests various configurations to find the smallest functional footprint.
//!
//! cargo build --example memory_optimize --features sqlite --release
//! Then run with MODE=<variant>

use std::sync::Arc;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::var("MODE").unwrap_or_else(|_| "help".into());

    match mode.as_str() {
        // ============================================================
        // TOKIO RUNTIME VARIANTS (no duroxide, just runtime overhead)
        // ============================================================
        "mt_default" => {
            // Default multi-threaded runtime (uses all CPUs)
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                eprintln!("[mt_default] multi-thread default");
                tokio::time::sleep(Duration::from_secs(999999)).await;
            });
        }

        "mt_2workers" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?;
            rt.block_on(async {
                eprintln!("[mt_2workers] multi-thread 2 workers");
                tokio::time::sleep(Duration::from_secs(999999)).await;
            });
        }

        "mt_1worker" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()?;
            rt.block_on(async {
                eprintln!("[mt_1worker] multi-thread 1 worker");
                tokio::time::sleep(Duration::from_secs(999999)).await;
            });
        }

        "current_thread" => {
            // Single-threaded runtime (smallest possible)
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(async {
                eprintln!("[current_thread] current-thread runtime");
                tokio::time::sleep(Duration::from_secs(999999)).await;
            });
        }

        // ============================================================
        // SQLITE POOL SIZE VARIANTS
        // ============================================================
        "ct_sqlite_pool1" => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(async {
                eprintln!("[ct_sqlite_pool1] current-thread + sqlite pool=1");
                let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();
                
                use sqlx::sqlite::SqlitePoolOptions;
                let pool = SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect("sqlite::memory:")
                    .await?;
                
                // Run migrations to match what SqliteProvider does
                sqlx::query("CREATE TABLE IF NOT EXISTS test (id INTEGER PRIMARY KEY)")
                    .execute(&pool)
                    .await?;
                
                tokio::time::sleep(Duration::from_secs(999999)).await;
                Ok::<_, Box<dyn std::error::Error>>(())
            })?;
        }

        "ct_sqlite_pool5" => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(async {
                eprintln!("[ct_sqlite_pool5] current-thread + sqlite pool=5");
                let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();
                
                use sqlx::sqlite::SqlitePoolOptions;
                let pool = SqlitePoolOptions::new()
                    .max_connections(5)
                    .connect("sqlite::memory:")
                    .await?;
                
                sqlx::query("CREATE TABLE IF NOT EXISTS test (id INTEGER PRIMARY KEY)")
                    .execute(&pool)
                    .await?;
                
                tokio::time::sleep(Duration::from_secs(999999)).await;
                Ok::<_, Box<dyn std::error::Error>>(())
            })?;
        }

        "mt_sqlite_pool1" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?;
            rt.block_on(async {
                eprintln!("[mt_sqlite_pool1] multi-thread 2 workers + sqlite pool=1");
                let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();
                
                use sqlx::sqlite::SqlitePoolOptions;
                let pool = SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect("sqlite::memory:")
                    .await?;
                
                sqlx::query("CREATE TABLE IF NOT EXISTS test (id INTEGER PRIMARY KEY)")
                    .execute(&pool)
                    .await?;
                
                tokio::time::sleep(Duration::from_secs(999999)).await;
                Ok::<_, Box<dyn std::error::Error>>(())
            })?;
        }

        // ============================================================
        // FULL RUNTIME VARIANTS
        // ============================================================
        "mt_full" => {
            // Current default behavior
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(run_full_runtime("[mt_full] multi-thread default + full runtime"))?;
        }

        "mt_2w_full" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?;
            rt.block_on(run_full_runtime("[mt_2w_full] multi-thread 2 workers + full runtime"))?;
        }

        "ct_full" => {
            // Single-threaded with full runtime
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(run_full_runtime("[ct_full] current-thread + full runtime"))?;
        }

        _ => {
            eprintln!("Memory optimization experiment variants:");
            eprintln!();
            eprintln!("  Tokio runtime only:");
            eprintln!("    MODE=mt_default       - multi-thread (all CPUs)");
            eprintln!("    MODE=mt_2workers      - multi-thread (2 workers)");
            eprintln!("    MODE=mt_1worker       - multi-thread (1 worker)");
            eprintln!("    MODE=current_thread   - current-thread (smallest)");
            eprintln!();
            eprintln!("  With SQLite pool (isolate pool overhead):");
            eprintln!("    MODE=ct_sqlite_pool1  - current-thread + pool=1");
            eprintln!("    MODE=ct_sqlite_pool5  - current-thread + pool=5");
            eprintln!("    MODE=mt_sqlite_pool1  - multi-thread 2w + pool=1");
            eprintln!();
            eprintln!("  Full runtime:");
            eprintln!("    MODE=mt_full          - multi-thread default + full runtime");
            eprintln!("    MODE=mt_2w_full       - multi-thread 2 workers + full runtime");
            eprintln!("    MODE=ct_full          - current-thread + full runtime");
            eprintln!();
            eprintln!("Usage: MODE=<variant> ./target/release/examples/memory_optimize");
            eprintln!("Then: ps -p <pid> -o rss=");
        }
    }

    Ok(())
}

async fn run_full_runtime(label: &str) -> Result<(), Box<dyn std::error::Error>> {
    use duroxide::providers::sqlite::SqliteProvider;
    use duroxide::runtime::registry::ActivityRegistry;
    use duroxide::runtime;
    use duroxide::OrchestrationRegistry;

    eprintln!("{}", label);
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();

    let store = Arc::new(SqliteProvider::new_in_memory().await?);
    let activities = ActivityRegistry::builder().build();
    let orchestrations = OrchestrationRegistry::builder().build();
    let _rt = runtime::Runtime::start_with_store(store, Arc::new(activities), orchestrations).await;

    tokio::time::sleep(Duration::from_secs(999999)).await;
    Ok(())
}
