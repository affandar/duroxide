use rust_dtf::Event;
use rust_dtf::providers::HistoryStore;
use std::sync::Arc as StdArc;
use std::time::{Duration, Instant};

#[allow(dead_code)]
pub async fn wait_for_history<F>(store: StdArc<dyn HistoryStore>, instance: &str, predicate: F, timeout_ms: u64) -> bool
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
    store: StdArc<dyn HistoryStore>,
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
    store: StdArc<dyn HistoryStore>,
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
