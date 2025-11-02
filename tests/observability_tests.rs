//! Observability-focused tests covering tracing for activities and orchestrations.

use async_trait::async_trait;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::providers::{ExecutionMetadata, ManagementCapability, OrchestrationItem, Provider, WorkItem};
use duroxide::runtime;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{LogFormat, ObservabilityConfig, RuntimeOptions};
use duroxide::{ActivityContext, Client, Event, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Dispatch, Event as TracingEvent, Level, Subscriber, dispatcher};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context as LayerContext, Layer};
use tracing_subscriber::prelude::*;

#[derive(Debug, Clone)]
struct RecordedEvent {
    level: Level,
    target: String,
    fields: BTreeMap<String, String>,
}

struct RecordingLayer {
    events: Arc<Mutex<Vec<RecordedEvent>>>,
}

struct FieldVisitor<'a> {
    fields: &'a mut BTreeMap<String, String>,
}

impl<'a> Visit for FieldVisitor<'a> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields.insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.fields.insert(field.name().to_string(), format!("{value:?}"));
    }
}

impl<S> Layer<S> for RecordingLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &TracingEvent<'_>, _ctx: LayerContext<'_, S>) {
        let mut fields = BTreeMap::new();
        event.record(&mut FieldVisitor { fields: &mut fields });
        let meta = event.metadata();
        self.events.lock().unwrap().push(RecordedEvent {
            level: *meta.level(),
            target: meta.target().to_string(),
            fields,
        });
    }
}

fn install_tracing() -> (Arc<Mutex<Vec<RecordedEvent>>>, tracing::dispatcher::DefaultGuard) {
    let recorded_events = Arc::new(Mutex::new(Vec::new()));
    let collector = tracing_subscriber::registry()
        .with(RecordingLayer {
            events: recorded_events.clone(),
        })
        .with(LevelFilter::TRACE);
    let dispatcher = Dispatch::new(collector);
    let guard = dispatcher::set_default(&dispatcher);
    (recorded_events, guard)
}

fn normalize(value: &str) -> String {
    value.trim_matches('"').to_string()
}

fn field(event: &RecordedEvent, key: &str) -> Option<String> {
    event.fields.get(key).map(|v| normalize(v))
}

fn metrics_observability_config(label: &str) -> ObservabilityConfig {
    ObservabilityConfig {
        metrics_enabled: true,
        metrics_export_endpoint: None,
        metrics_export_interval_ms: 1000,
        log_format: LogFormat::Compact,
        log_export_endpoint: None,
        log_level: "error".to_string(),
        service_name: format!("duroxide-observability-test-{label}"),
        service_version: Some("test".to_string()),
    }
}

struct FailingProvider {
    inner: Arc<SqliteProvider>,
    fail_next_ack_worker: AtomicBool,
}

impl FailingProvider {
    fn new(inner: Arc<SqliteProvider>) -> Self {
        Self {
            inner,
            fail_next_ack_worker: AtomicBool::new(false),
        }
    }

    fn fail_next_ack_worker(&self) {
        self.fail_next_ack_worker.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl Provider for FailingProvider {
    async fn fetch_orchestration_item(&self) -> Option<OrchestrationItem> {
        self.inner.fetch_orchestration_item().await
    }

    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        execution_id: u64,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
    ) -> Result<(), String> {
        self.inner
            .ack_orchestration_item(
                lock_token,
                execution_id,
                history_delta,
                worker_items,
                orchestrator_items,
                metadata,
            )
            .await
    }

    async fn abandon_orchestration_item(&self, lock_token: &str, delay_ms: Option<u64>) -> Result<(), String> {
        self.inner.abandon_orchestration_item(lock_token, delay_ms).await
    }

    async fn read(&self, instance: &str) -> Vec<Event> {
        self.inner.read(instance).await
    }

    async fn enqueue_worker_work(&self, item: WorkItem) -> Result<(), String> {
        self.inner.enqueue_worker_work(item).await
    }

    async fn dequeue_worker_peek_lock(&self) -> Option<(WorkItem, String)> {
        self.inner.dequeue_worker_peek_lock().await
    }

    async fn ack_worker(&self, token: &str, completion: WorkItem) -> Result<(), String> {
        if self.fail_next_ack_worker.swap(false, Ordering::SeqCst) {
            self.inner.ack_worker(token, completion.clone()).await?;
            Err("simulated infrastructure failure".to_string())
        } else {
            self.inner.ack_worker(token, completion).await
        }
    }

    async fn append_with_execution(
        &self,
        instance: &str,
        execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), String> {
        self.inner
            .append_with_execution(instance, execution_id, new_events)
            .await
    }

    async fn enqueue_orchestrator_work(&self, item: WorkItem, delay_ms: Option<u64>) -> Result<(), String> {
        self.inner.enqueue_orchestrator_work(item, delay_ms).await
    }

    fn as_management_capability(&self) -> Option<&dyn ManagementCapability> {
        self.inner.as_management_capability()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn activity_tracing_emits_all_levels() {
    let (recorded_events, _guard) = install_tracing();

    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let activities = ActivityRegistry::builder()
        .register("TraceActivity", |ctx: ActivityContext, _input: String| async move {
            ctx.trace_info("activity info");
            ctx.trace_warn("activity warn");
            ctx.trace_error("activity error");
            ctx.trace_debug("activity debug");
            Ok("ok".to_string())
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register("TraceOrch", |ctx: OrchestrationContext, _input: String| async move {
            ctx.schedule_activity("TraceActivity", "payload").into_activity().await
        })
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("trace-activity-instance", "TraceOrch", "")
        .await
        .unwrap();
    match client
        .wait_for_orchestration("trace-activity-instance", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "ok"),
        OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {other:?}"),
    }

    rt.shutdown(None).await;

    let events = recorded_events.lock().unwrap();
    let activity_events: Vec<&RecordedEvent> = events
        .iter()
        .filter(|event| event.target == "duroxide::activity")
        .collect();
    assert!(activity_events.len() >= 4, "expected activity traces, found none");

    let find_event = |level: Level, message: &str| -> &RecordedEvent {
        activity_events
            .iter()
            .copied()
            .find(|event| event.level == level && field(event, "message").as_deref() == Some(message))
            .unwrap_or_else(|| panic!("missing {level:?} event with message '{message}'"))
    };

    find_event(Level::INFO, "activity info");
    find_event(Level::WARN, "activity warn");
    find_event(Level::ERROR, "activity error");
    find_event(Level::DEBUG, "activity debug");

    for event in activity_events {
        assert_eq!(field(event, "instance_id").as_deref(), Some("trace-activity-instance"));
        assert_eq!(field(event, "execution_id").as_deref(), Some("1"));
        assert_eq!(field(event, "orchestration_name").as_deref(), Some("TraceOrch"));
        assert_eq!(field(event, "activity_name").as_deref(), Some("TraceActivity"));
        assert!(event.fields.contains_key("activity_id"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn orchestration_tracing_emits_all_levels() {
    let (recorded_events, _guard) = install_tracing();

    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let activities = ActivityRegistry::builder().build();

    let orchestrations = OrchestrationRegistry::builder()
        .register("TraceOrch", |ctx: OrchestrationContext, _input: String| async move {
            ctx.trace_info("orch info");
            ctx.trace_warn("orch warn");
            ctx.trace_error("orch error");
            ctx.trace_debug("orch debug");
            Ok("done".to_string())
        })
        .build();

    let rt = runtime::Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
    let client = Client::new(store.clone());
    client
        .start_orchestration("trace-orch-instance", "TraceOrch", "")
        .await
        .unwrap();
    match client
        .wait_for_orchestration("trace-orch-instance", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { output } => assert_eq!(output, "done"),
        OrchestrationStatus::Failed { details } => {
            panic!("orchestration failed: {}", details.display_message())
        }
        other => panic!("unexpected orchestration status: {other:?}"),
    }

    rt.shutdown(None).await;

    let events = recorded_events.lock().unwrap();
    let orchestration_events: Vec<&RecordedEvent> = events
        .iter()
        .filter(|event| event.target == "duroxide::orchestration")
        .collect();
    assert!(
        orchestration_events.len() >= 4,
        "expected orchestration traces, found none"
    );

    let find_event = |level: Level, message: &str| -> &RecordedEvent {
        orchestration_events
            .iter()
            .copied()
            .find(|event| event.level == level && field(event, "message").as_deref() == Some(message))
            .unwrap_or_else(|| panic!("missing {level:?} event with message '{message}'"))
    };

    find_event(Level::INFO, "orch info");
    find_event(Level::WARN, "orch warn");
    find_event(Level::ERROR, "orch error");
    find_event(Level::DEBUG, "orch debug");

    for event in orchestration_events {
        assert_eq!(field(event, "instance_id").as_deref(), Some("trace-orch-instance"));
        assert_eq!(field(event, "execution_id").as_deref(), Some("1"));
        assert_eq!(field(event, "orchestration_name").as_deref(), Some("TraceOrch"));
        assert!(event.fields.contains_key("orchestration_version"));
        assert!(!event.fields.contains_key("activity_name"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn metrics_capture_activity_and_orchestration_outcomes() {
    let sqlite = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let failing_provider = Arc::new(FailingProvider::new(sqlite));
    let provider_trait: Arc<dyn Provider> = failing_provider.clone();

    let activities = ActivityRegistry::builder()
        .register("AlwaysOk", |ctx: ActivityContext, _input: String| async move {
            ctx.trace_info("activity ok");
            Ok("ok".to_string())
        })
        .register("AlwaysFail", |ctx: ActivityContext, _input: String| async move {
            ctx.trace_error("activity failure");
            Err("boom".to_string())
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register("SuccessOrch", |ctx: OrchestrationContext, _input: String| async move {
            ctx.schedule_activity("AlwaysOk", "payload")
                .into_activity()
                .await
                .expect("activity should succeed");
            Ok("done".to_string())
        })
        .register(
            "AppFailureOrch",
            |ctx: OrchestrationContext, _input: String| async move {
                match ctx.schedule_activity("AlwaysFail", "payload").into_activity().await {
                    Ok(_) => Ok("unexpected".to_string()),
                    Err(err) => Err(err),
                }
            },
        )
        .register(
            "ConfigFailureOrch",
            |ctx: OrchestrationContext, _input: String| async move {
                match ctx
                    .schedule_activity("MissingActivity", "payload")
                    .into_activity()
                    .await
                {
                    Ok(_) => Ok("unexpected".to_string()),
                    Err(err) => Err(err),
                }
            },
        )
        .build();

    let options = RuntimeOptions {
        observability: metrics_observability_config("metrics-outcomes"),
        ..Default::default()
    };
    let rt =
        runtime::Runtime::start_with_options(provider_trait.clone(), Arc::new(activities), orchestrations, options)
            .await;

    let client = Client::new(provider_trait.clone());

    // Trigger infrastructure error (first ack fails but work has already been committed)
    failing_provider.fail_next_ack_worker();
    client
        .start_orchestration("metrics-success-infra", "SuccessOrch", "")
        .await
        .unwrap();
    match client
        .wait_for_orchestration("metrics-success-infra", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { .. } => {}
        other => panic!("unexpected status for infra success: {other:?}"),
    }

    // Clean success (records success metrics)
    client
        .start_orchestration("metrics-success", "SuccessOrch", "")
        .await
        .unwrap();
    match client
        .wait_for_orchestration("metrics-success", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { .. } => {}
        other => panic!("unexpected status for success: {other:?}"),
    }

    // Application failure via activity error
    client
        .start_orchestration("metrics-app-fail", "AppFailureOrch", "")
        .await
        .unwrap();
    match client
        .wait_for_orchestration("metrics-app-fail", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Failed { .. } => {}
        other => panic!("unexpected status for app failure: {other:?}"),
    }

    // Configuration failure via unregistered activity
    client
        .start_orchestration("metrics-config-fail", "ConfigFailureOrch", "")
        .await
        .unwrap();
    match client
        .wait_for_orchestration("metrics-config-fail", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Failed { .. } => {}
        other => panic!("unexpected status for config failure: {other:?}"),
    }

    let snapshot = rt
        .metrics_snapshot()
        .expect("metrics should be available when observability is enabled");

    rt.clone().shutdown(None).await;

    assert_eq!(snapshot.activity_success, 1, "expected one successful activity");
    assert_eq!(
        snapshot.activity_app_errors, 1,
        "expected one application activity failure"
    );
    assert_eq!(
        snapshot.activity_config_errors, 1,
        "expected one configuration activity failure"
    );
    assert_eq!(
        snapshot.activity_infra_errors, 1,
        "expected one infrastructure activity failure"
    );

    assert_eq!(snapshot.orch_completions, 2, "expected two successful orchestrations");
    assert_eq!(snapshot.orch_failures, 2, "expected two failed orchestrations");
    assert_eq!(
        snapshot.orch_application_errors, 1,
        "expected one application orchestration failure"
    );
    assert_eq!(
        snapshot.orch_configuration_errors, 1,
        "expected one configuration orchestration failure"
    );
    assert_eq!(
        snapshot.orch_infrastructure_errors, 0,
        "no orchestrations should have infra failures"
    );
}
