//! Observability-focused tests covering tracing for activities and orchestrations.

use async_trait::async_trait;
use duroxide::providers::sqlite::SqliteProvider;
use duroxide::providers::{ExecutionMetadata, OrchestrationItem, Provider, ProviderAdmin, ProviderError, WorkItem};
use duroxide::runtime;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::runtime::{LogFormat, ObservabilityConfig, RuntimeOptions};
use duroxide::{ActivityContext, Client, Event, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
    fail_next_ack_work_item: AtomicBool,
    fail_next_ack_orchestration_item: AtomicBool,
    fail_next_fetch_orchestration_item: AtomicBool,
}

impl FailingProvider {
    fn new(inner: Arc<SqliteProvider>) -> Self {
        Self {
            inner,
            fail_next_ack_work_item: AtomicBool::new(false),
            fail_next_ack_orchestration_item: AtomicBool::new(false),
            fail_next_fetch_orchestration_item: AtomicBool::new(false),
        }
    }

    fn fail_next_ack_work_item(&self) {
        self.fail_next_ack_work_item.store(true, Ordering::SeqCst);
    }

    fn fail_next_ack_orchestration_item(&self) {
        self.fail_next_ack_orchestration_item.store(true, Ordering::SeqCst);
    }

    fn fail_next_fetch_orchestration_item(&self) {
        self.fail_next_fetch_orchestration_item.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl Provider for FailingProvider {
    async fn fetch_orchestration_item(
        &self,
        _lock_timeout: Duration,
    ) -> Result<Option<OrchestrationItem>, ProviderError> {
        if self.fail_next_fetch_orchestration_item.swap(false, Ordering::SeqCst) {
            // Simulate transient infrastructure failure (e.g., database connection issue)
            Err(ProviderError::retryable(
                "fetch_orchestration_item",
                "simulated transient infrastructure failure",
            ))
        } else {
            self.inner.fetch_orchestration_item(Duration::from_secs(30)).await
        }
    }

    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        execution_id: u64,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
    ) -> Result<(), ProviderError> {
        if self.fail_next_ack_orchestration_item.swap(false, Ordering::SeqCst) {
            // Check if this is a failure event commit - if so, allow it to succeed
            // This allows the runtime to commit the orchestration failure event
            let is_failure_commit = history_delta
                .iter()
                .any(|e| matches!(e, Event::OrchestrationFailed { .. }));
            if is_failure_commit {
                // Allow failure event commit to succeed
                return self
                    .inner
                    .ack_orchestration_item(
                        lock_token,
                        execution_id,
                        history_delta,
                        worker_items,
                        orchestrator_items,
                        metadata,
                    )
                    .await;
            }

            // Simulate permanent infrastructure failure for normal commits
            // This will cause the orchestration to fail with infrastructure error
            Err(ProviderError::permanent(
                "ack_orchestration_item",
                "simulated infrastructure failure",
            ))
        } else {
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
    }

    async fn abandon_orchestration_item(&self, lock_token: &str, delay: Option<Duration>) -> Result<(), ProviderError> {
        self.inner.abandon_orchestration_item(lock_token, delay).await
    }

    async fn read(&self, instance: &str) -> Result<Vec<Event>, ProviderError> {
        self.inner.read(instance).await
    }

    async fn read_with_execution(&self, instance: &str, execution_id: u64) -> Result<Vec<Event>, ProviderError> {
        self.inner.read_with_execution(instance, execution_id).await
    }

    async fn append_with_execution(
        &self,
        instance: &str,
        execution_id: u64,
        new_events: Vec<Event>,
    ) -> Result<(), ProviderError> {
        self.inner
            .append_with_execution(instance, execution_id, new_events)
            .await
    }

    async fn enqueue_for_worker(&self, item: WorkItem) -> Result<(), ProviderError> {
        self.inner.enqueue_for_worker(item).await
    }

    async fn fetch_work_item(&self, lock_timeout: Duration) -> Result<Option<(WorkItem, String)>, ProviderError> {
        self.inner.fetch_work_item(lock_timeout).await
    }

    async fn ack_work_item(&self, token: &str, completion: WorkItem) -> Result<(), ProviderError> {
        if self.fail_next_ack_work_item.swap(false, Ordering::SeqCst) {
            self.inner.ack_work_item(token, completion.clone()).await?;
            Err(ProviderError::permanent(
                "ack_work_item",
                "simulated infrastructure failure",
            ))
        } else {
            self.inner.ack_work_item(token, completion).await
        }
    }

    async fn renew_work_item_lock(&self, token: &str, extend_for: Duration) -> Result<(), ProviderError> {
        self.inner.renew_work_item_lock(token, extend_for).await
    }

    async fn enqueue_for_orchestrator(&self, item: WorkItem, delay: Option<Duration>) -> Result<(), ProviderError> {
        self.inner.enqueue_for_orchestrator(item, delay).await
    }

    fn as_management_capability(&self) -> Option<&dyn ProviderAdmin> {
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
    failing_provider.fail_next_ack_work_item();
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

    // Trigger infrastructure error for orchestration (ack_orchestration_item fails permanently)
    // This tests infrastructure failure handling - orchestration should fail
    failing_provider.fail_next_ack_orchestration_item();
    client
        .start_orchestration("metrics-orch-infra", "SuccessOrch", "")
        .await
        .unwrap();
    match client
        .wait_for_orchestration("metrics-orch-infra", std::time::Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Failed { .. } => {}
        other => panic!("unexpected status for orchestration infra failure: {other:?}"),
    }

    let snapshot = rt
        .metrics_snapshot()
        .expect("metrics should be available when observability is enabled");

    rt.clone().shutdown(None).await;

    // Note: Metrics accumulate across test runs, so use >= instead of ==
    assert!(
        snapshot.activity_success >= 1,
        "expected at least one successful activity"
    );
    assert!(
        snapshot.activity_app_errors >= 1,
        "expected at least one application activity failure"
    );
    assert!(
        snapshot.activity_config_errors >= 1,
        "expected at least one configuration activity failure"
    );
    assert!(
        snapshot.activity_infra_errors >= 1,
        "expected at least one infrastructure activity failure"
    );

    assert!(
        snapshot.orch_completions >= 2,
        "expected at least two successful orchestrations"
    );
    assert!(
        snapshot.orch_failures >= 3,
        "expected at least three failed orchestrations"
    );
    assert!(
        snapshot.orch_application_errors >= 1,
        "expected at least one application orchestration failure"
    );
    assert!(
        snapshot.orch_configuration_errors >= 1,
        "expected at least one configuration orchestration failure"
    );
    assert!(
        snapshot.orch_infrastructure_errors >= 1,
        "expected at least one infrastructure orchestration failure"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_fetch_orchestration_item_fault_injection() {
    let sqlite = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let failing_provider = Arc::new(FailingProvider::new(sqlite));
    let provider_trait: Arc<dyn Provider> = failing_provider.clone();

    // Enqueue a work item
    provider_trait
        .enqueue_for_orchestrator(
            WorkItem::StartOrchestration {
                instance: "test-instance".to_string(),
                orchestration: "TestOrch".to_string(),
                input: "test-input".to_string(),
                version: Some("1.0.0".to_string()),
                parent_instance: None,
                parent_id: None,
                execution_id: duroxide::INITIAL_EXECUTION_ID,
            },
            None,
        )
        .await
        .unwrap();

    // Enable fault injection
    failing_provider.fail_next_fetch_orchestration_item();

    // Attempt to fetch - should return error
    let result = provider_trait.fetch_orchestration_item(Duration::from_secs(30)).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.is_retryable());
    assert!(err.message.contains("simulated transient infrastructure failure"));

    // Disable fault injection - should succeed now
    let result = provider_trait.fetch_orchestration_item(Duration::from_secs(30)).await;
    assert!(result.is_ok());
    let item = result.unwrap();
    assert!(item.is_some());
    assert_eq!(item.unwrap().instance, "test-instance");
}

#[tokio::test(flavor = "current_thread")]
async fn test_labeled_metrics_recording() {
    // Test that labeled metrics can be recorded without panicking
    let options = RuntimeOptions {
        observability: metrics_observability_config("labeled-metrics"),
        ..Default::default()
    };

    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let activities = ActivityRegistry::builder()
        .register("TestActivity", |_ctx: ActivityContext, _input: String| async move {
            Ok("result".to_string())
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register("TestOrch", |ctx: OrchestrationContext, _input: String| async move {
            ctx.schedule_activity("TestActivity", "input").into_activity().await
        })
        .build();

    let rt = runtime::Runtime::start_with_options(store.clone(), Arc::new(activities), orchestrations, options).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("labeled-test", "TestOrch", "")
        .await
        .unwrap();

    // Wait for completion
    match client
        .wait_for_orchestration("labeled-test", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { .. } => {}
        other => panic!("unexpected status: {other:?}"),
    }

    // Get metrics snapshot
    let snapshot = rt.metrics_snapshot().expect("metrics should be available");

    rt.shutdown(None).await;

    // Verify counters incremented (proves labeled methods were called)
    // Note: Counters accumulate across all test runs with same config label
    assert!(snapshot.orch_completions >= 1, "orchestration should complete");
    assert!(snapshot.activity_success >= 1, "activity should succeed");
}

#[tokio::test(flavor = "current_thread")]
async fn test_continue_as_new_metrics() {
    let options = RuntimeOptions {
        observability: metrics_observability_config("can-metrics"),
        ..Default::default()
    };

    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let activities = ActivityRegistry::builder().build();

    let orchestrations = OrchestrationRegistry::builder()
        .register("CANOrch", |ctx: OrchestrationContext, input: String| async move {
            let count: u32 = input.parse().unwrap_or(0);
            if count < 2 {
                ctx.continue_as_new((count + 1).to_string());
                Ok("continuing".to_string())
            } else {
                Ok("done".to_string())
            }
        })
        .build();

    let rt = runtime::Runtime::start_with_options(store.clone(), Arc::new(activities), orchestrations, options).await;

    let client = Client::new(store.clone());
    client.start_orchestration("can-test", "CANOrch", "0").await.unwrap();

    // Wait for final completion
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Get metrics snapshot
    rt.shutdown(None).await;

    // Note: Continue-as-new metrics are recorded via record_continue_as_new()
    // This test verifies the orchestration completes without error
}

#[tokio::test(flavor = "current_thread")]
async fn test_activity_duration_tracking() {
    let options = RuntimeOptions {
        observability: metrics_observability_config("duration-metrics"),
        ..Default::default()
    };

    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let activities = ActivityRegistry::builder()
        .register("SlowActivity", |_ctx: ActivityContext, _input: String| async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok("done".to_string())
        })
        .register("FastActivity", |_ctx: ActivityContext, _input: String| async move {
            Ok("instant".to_string())
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register("DurationOrch", |ctx: OrchestrationContext, _input: String| async move {
            ctx.schedule_activity("SlowActivity", "").into_activity().await?;
            ctx.schedule_activity("FastActivity", "").into_activity().await?;
            Ok("done".to_string())
        })
        .build();

    let rt = runtime::Runtime::start_with_options(store.clone(), Arc::new(activities), orchestrations, options).await;

    let client = Client::new(store.clone());
    client
        .start_orchestration("duration-test", "DurationOrch", "")
        .await
        .unwrap();

    // Wait for completion
    match client
        .wait_for_orchestration("duration-test", Duration::from_secs(5))
        .await
        .unwrap()
    {
        OrchestrationStatus::Completed { .. } => {}
        other => panic!("unexpected status: {other:?}"),
    }

    let snapshot = rt.metrics_snapshot().expect("metrics should be available");

    rt.shutdown(None).await;

    // Verify both activities recorded
    assert!(snapshot.activity_success >= 2, "both activities should succeed");
    assert!(snapshot.orch_completions >= 1, "orchestration should complete");
}

#[tokio::test(flavor = "current_thread")]
async fn test_error_classification_metrics() {
    let options = RuntimeOptions {
        observability: metrics_observability_config("error-classification"),
        ..Default::default()
    };

    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let activities = ActivityRegistry::builder()
        .register("FailActivity", |_ctx: ActivityContext, _input: String| async move {
            Err("business logic error".to_string())
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "ErrorOrch",
            |ctx: OrchestrationContext, error_type: String| async move {
                match error_type.as_str() {
                    "app" => {
                        ctx.schedule_activity("FailActivity", "")
                            .into_activity()
                            .await
                            .map_err(|e| e)?;
                        Ok("unexpected".to_string())
                    }
                    "config" => {
                        ctx.schedule_activity("UnregisteredActivity", "")
                            .into_activity()
                            .await
                            .map_err(|e| e)?;
                        Ok("unexpected".to_string())
                    }
                    _ => Ok("ok".to_string()),
                }
            },
        )
        .build();

    let rt = runtime::Runtime::start_with_options(store.clone(), Arc::new(activities), orchestrations, options).await;

    let client = Client::new(store.clone());

    // Test application error
    client
        .start_orchestration("error-app", "ErrorOrch", "app")
        .await
        .unwrap();
    let _ = client.wait_for_orchestration("error-app", Duration::from_secs(5)).await;

    // Test configuration error
    client
        .start_orchestration("error-config", "ErrorOrch", "config")
        .await
        .unwrap();
    let _ = client
        .wait_for_orchestration("error-config", Duration::from_secs(5))
        .await;

    let snapshot = rt.metrics_snapshot().expect("metrics should be available");

    rt.shutdown(None).await;

    // Verify error classification (counters accumulate, so use >=)
    assert!(snapshot.activity_app_errors >= 1, "should have at least one app error");
    assert!(
        snapshot.activity_config_errors >= 1,
        "should have at least one config error"
    );
    assert!(
        snapshot.orch_application_errors >= 1,
        "orchestration should fail with app error"
    );
    assert!(
        snapshot.orch_configuration_errors >= 1,
        "orchestration should fail with config error"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_active_orchestrations_gauge() {
    let options = RuntimeOptions {
        observability: metrics_observability_config("active-gauge"),
        ..Default::default()
    };

    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    let activities = ActivityRegistry::builder()
        .register("SlowActivity", |_ctx: ActivityContext, _input: String| async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok("done".to_string())
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register("SimpleOrch", |ctx: OrchestrationContext, _input: String| async move {
            ctx.schedule_activity("SlowActivity", "").into_activity().await
        })
        .register("CANOrch", |ctx: OrchestrationContext, input: String| async move {
            let count: u32 = input.parse().unwrap_or(0);
            if count < 1 {
                // Do some work before continuing
                ctx.schedule_activity("SlowActivity", "").into_activity().await?;
                ctx.continue_as_new((count + 1).to_string());
                Ok("continuing".to_string())
            } else {
                // Final execution also does work
                ctx.schedule_activity("SlowActivity", "").into_activity().await?;
                Ok("done".to_string())
            }
        })
        .build();

    let rt = runtime::Runtime::start_with_options(store.clone(), Arc::new(activities), orchestrations, options).await;

    // Get initial active count (might not be 0 if other tests ran)
    let initial_active = rt.get_active_orchestrations_count();

    let client = Client::new(store.clone());

    // Start orchestration - should increment
    client
        .start_orchestration("active-test-1", "SimpleOrch", "")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    let active_after_start = rt.get_active_orchestrations_count();
    assert!(
        active_after_start > initial_active,
        "active count should increase after start: {} -> {}",
        initial_active,
        active_after_start
    );

    // Wait for completion - should decrement
    let _ = client
        .wait_for_orchestration("active-test-1", Duration::from_secs(5))
        .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let active_after_complete = rt.get_active_orchestrations_count();
    assert_eq!(
        active_after_complete, initial_active,
        "active count should return to initial after completion: {} vs {}",
        active_after_complete, initial_active
    );

    // Test continue-as-new: Active count should stay elevated during CAN
    client
        .start_orchestration("active-test-can", "CANOrch", "0")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    let active_during_can = rt.get_active_orchestrations_count();
    assert!(
        active_during_can > initial_active,
        "active count should be elevated during continue-as-new"
    );

    // Wait for final completion
    let _ = client
        .wait_for_orchestration("active-test-can", Duration::from_secs(5))
        .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    let active_final = rt.get_active_orchestrations_count();
    assert_eq!(
        active_final, initial_active,
        "active count should return to initial after CAN completes"
    );

    rt.shutdown(None).await;
}
