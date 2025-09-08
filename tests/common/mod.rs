use duroxide::Event;
use duroxide::providers::Provider;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::Client;
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
pub async fn wait_for_subscription(
    store: StdArc<dyn Provider>,
    instance: &str,
    name: &str,
    timeout_ms: u64,
) -> bool {
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
    let store = StdArc::new(SqliteProvider::new(&db_url).await.unwrap()) as StdArc<dyn Provider>;
    (store, td)
}

#[allow(dead_code)]
pub async fn create_sqlite_store_memory() -> StdArc<dyn Provider> {
    let store = SqliteProvider::new_in_memory().await.unwrap();
    StdArc::new(store) as StdArc<dyn Provider>
}

#[allow(dead_code)]
pub fn create_client(store: StdArc<dyn Provider>) -> Client {
    Client::new(store)
}
