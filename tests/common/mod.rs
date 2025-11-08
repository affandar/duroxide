use duroxide::Event;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::providers::{ExecutionMetadata, Provider, WorkItem};
use std::sync::Arc as StdArc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[allow(dead_code)]
pub async fn wait_for_history<F>(store: StdArc<dyn Provider>, instance: &str, predicate: F, timeout_ms: u64) -> bool
where
    F: Fn(&Vec<Event>) -> bool,
{
    wait_for_history_event(
        store,
        instance,
        |hist| if predicate(hist) { Some(()) } else { None },
        timeout_ms,
    )
    .await
    .is_some()
}

#[allow(dead_code)]
pub async fn wait_for_subscription(store: StdArc<dyn Provider>, instance: &str, name: &str, timeout_ms: u64) -> bool {
    wait_for_history(
        store,
        instance,
        |hist| {
            hist.iter()
                .any(|e| matches!(e, Event::ExternalSubscribed { name: n, .. } if n == name))
        },
        timeout_ms,
    )
    .await
}

#[allow(dead_code)]
pub async fn wait_for_history_event<T, F>(
    store: StdArc<dyn Provider>,
    instance: &str,
    selector: F,
    timeout_ms: u64,
) -> Option<T>
where
    T: Clone,
    F: Fn(&Vec<Event>) -> Option<T>,
{
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let hist = store.read(instance).await;
        if let Some(e) = selector(&hist) {
            return Some(e);
        }
        if Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[allow(dead_code)]
pub async fn create_sqlite_store_disk() -> (StdArc<dyn Provider>, TempDir) {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("test.db");
    std::fs::File::create(&db_path).unwrap();
    let db_url = format!("sqlite:{}", db_path.display());
    let store = StdArc::new(SqliteProvider::new(&db_url, None).await.unwrap()) as StdArc<dyn Provider>;
    (store, td)
}

/// Test helper to create a new orchestration instance with initial history.
///
/// This replicates what the runtime does in production by using real provider APIs:
/// 1. Enqueues StartOrchestration work item
/// 2. Fetches it to get a lock token
/// 3. Acks with OrchestrationStarted event
///
/// Use this to seed test state without spinning up a full runtime.
#[allow(dead_code)]
pub async fn test_create_execution(
    provider: &dyn Provider,
    instance: &str,
    orchestration: &str,
    version: &str,
    input: &str,
    parent_instance: Option<&str>,
    parent_id: Option<u64>,
) -> Result<u64, String> {
    // Calculate next execution ID (max + 1, or INITIAL if none exist)
    let execs = provider.list_executions(instance).await;
    let next_execution_id = if execs.is_empty() {
        duroxide::INITIAL_EXECUTION_ID
    } else {
        execs.iter().max().copied().unwrap() + 1
    };

    // Enqueue StartOrchestration work item with calculated execution_id
    provider
        .enqueue_orchestrator_work(
            WorkItem::StartOrchestration {
                instance: instance.to_string(),
                orchestration: orchestration.to_string(),
                version: Some(version.to_string()),
                input: input.to_string(),
                parent_instance: parent_instance.map(|s| s.to_string()),
                parent_id,
                execution_id: next_execution_id,
            },
            None,
        )
        .await?;

    // Fetch to get lock token
    let item = provider
        .fetch_orchestration_item()
        .await
        .ok_or_else(|| "Failed to fetch orchestration item".to_string())?;

    // The fetched item should have the execution_id we enqueued
    let execution_id = next_execution_id;

    // Ack with OrchestrationStarted event and proper metadata
    provider
        .ack_orchestration_item(
            &item.lock_token,
            execution_id,
            vec![Event::OrchestrationStarted {
                event_id: duroxide::INITIAL_EVENT_ID,
                name: orchestration.to_string(),
                version: version.to_string(),
                input: input.to_string(),
                parent_instance: parent_instance.map(|s| s.to_string()),
                parent_id,
            }],
            vec![], // no worker items
            vec![], // no orchestrator items
            ExecutionMetadata {
                orchestration_name: Some(orchestration.to_string()),
                orchestration_version: Some(version.to_string()),
                ..Default::default()
            },
        )
        .await?;

    Ok(execution_id)
}
