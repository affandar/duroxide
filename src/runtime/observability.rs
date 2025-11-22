//! Observability infrastructure for metrics and structured logging.
//!
//! This module provides OpenTelemetry-based metrics and structured logging
//! for production observability. It can be enabled via the `observability` feature flag.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Log format options for structured logging
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// Structured JSON output for log aggregators
    Json,
    /// Human-readable format for development (with all fields)
    Pretty,
    /// Compact format: timestamp level module [instance_id] message
    #[default]
    Compact,
}

/// Observability configuration for metrics and logging.
///
/// Controls structured logging and OpenTelemetry metrics collection.
/// Requires the `observability` feature flag for full functionality.
///
/// # Example
///
/// ```rust,no_run
/// # use duroxide::runtime::{ObservabilityConfig, LogFormat};
/// // Simplest: Compact logs to stdout
/// let config = ObservabilityConfig {
///     log_format: LogFormat::Compact,
///     log_level: "info".to_string(),
///     ..Default::default()
/// };
///
/// // Production: Metrics + JSON logs to OTLP collector
/// let config = ObservabilityConfig {
///     metrics_enabled: true,
///     metrics_export_endpoint: Some("http://localhost:4317".to_string()),
///     log_format: LogFormat::Json,
///     service_name: "my-app".to_string(),
///     ..Default::default()
/// };
/// ```
///
/// # Correlation Fields
///
/// All logs automatically include:
/// - `instance_id` - Orchestration instance identifier
/// - `execution_id` - Execution number (for ContinueAsNew)
/// - `orchestration_name` - Name of the orchestration
/// - `orchestration_version` - Semantic version
/// - `activity_name` - Activity name (in activity context)
/// - `worker_id` - Dispatcher worker ID
///
/// # See Also
///
/// - [Observability Guide](../../docs/observability-guide.md) - Full documentation
#[derive(Debug, Clone)]
pub struct ObservabilityConfig {
    // Metrics configuration
    /// Enable OpenTelemetry metrics collection
    pub metrics_enabled: bool,
    /// OTLP/gRPC endpoint for metrics export (e.g., "http://localhost:4317")
    pub metrics_export_endpoint: Option<String>,
    /// Metrics export interval in milliseconds
    pub metrics_export_interval_ms: u64,

    // Structured logging configuration
    /// Log output format
    pub log_format: LogFormat,
    /// OTLP/gRPC endpoint for trace/log export
    pub log_export_endpoint: Option<String>,
    /// Log level filter (e.g., "info", "debug")
    pub log_level: String,

    // Common configuration
    /// Service name for OpenTelemetry
    pub service_name: String,
    /// Optional service version
    pub service_version: Option<String>,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            metrics_enabled: false,
            metrics_export_endpoint: None,
            metrics_export_interval_ms: 60000,
            log_format: LogFormat::Pretty,
            log_export_endpoint: None,
            log_level: "info".to_string(),
            service_name: "duroxide".to_string(),
            service_version: None,
        }
    }
}

fn default_filter_expression(level: &str) -> String {
    format!("warn,duroxide::orchestration={level},duroxide::activity={level}")
}

/// Snapshot of key observability metrics counters for tests and diagnostics.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub orch_completions: u64,
    pub orch_failures: u64,
    pub orch_application_errors: u64,
    pub orch_infrastructure_errors: u64,
    pub orch_configuration_errors: u64,
    pub activity_success: u64,
    pub activity_app_errors: u64,
    pub activity_infra_errors: u64,
    pub activity_config_errors: u64,
}

#[cfg(feature = "observability")]
mod otel_impl {
    use super::*;
    use opentelemetry::KeyValue;
    use opentelemetry::metrics::{Counter, Histogram, MeterProvider as _};
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::metrics::{ManualReader, PeriodicReader, SdkMeterProvider};
    use std::time::Duration;
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    /// OpenTelemetry metrics provider with all instrumentation
    pub struct MetricsProvider {
        meter_provider: SdkMeterProvider,

        // Dispatcher metrics
        pub orch_dispatcher_items_fetched: Counter<u64>,
        pub orch_dispatcher_processing_duration: Histogram<u64>,
        pub worker_dispatcher_items_fetched: Counter<u64>,
        pub worker_dispatcher_execution_duration: Histogram<u64>,

        // Orchestration execution metrics (with labels)
        pub orch_starts_total: Counter<u64>,
        pub orch_completions_total: Counter<u64>,
        pub orch_failures_total: Counter<u64>,
        pub orch_duration_seconds: Histogram<f64>,
        pub orch_history_size_events: Histogram<u64>,
        pub orch_turns: Histogram<u64>,

        // Continue-as-new metrics
        pub orch_continue_as_new_total: Counter<u64>,

        // Sub-orchestration metrics
        pub suborchestration_calls_total: Counter<u64>,
        pub suborchestration_duration_seconds: Histogram<f64>,

        // Provider metrics
        pub provider_operation_duration_seconds: Histogram<f64>,
        pub provider_errors_total: Counter<u64>,

        // Activity metrics (with labels)
        pub activity_executions_total: Counter<u64>,
        pub activity_duration_seconds: Histogram<f64>,
        pub activity_errors_total: Counter<u64>,

        // Client metrics
        pub client_orch_starts_total: Counter<u64>,
        pub client_events_raised_total: Counter<u64>,
        pub client_cancellations_total: Counter<u64>,
        pub client_wait_duration_seconds: Histogram<f64>,

        // Test-observable counters
        orch_completions_atomic: AtomicU64,
        orch_failures_atomic: AtomicU64,
        orch_application_errors_atomic: AtomicU64,
        orch_infrastructure_errors_atomic: AtomicU64,
        orch_configuration_errors_atomic: AtomicU64,
        activity_success_atomic: AtomicU64,
        activity_app_errors_atomic: AtomicU64,
        activity_infra_errors_atomic: AtomicU64,
        activity_config_errors_atomic: AtomicU64,

        // Queue depth tracking (updated by background task)
        orch_queue_depth_atomic: AtomicU64,
        worker_queue_depth_atomic: AtomicU64,

        // Active orchestrations tracking (for gauge metrics)
        active_orchestrations_atomic: std::sync::atomic::AtomicI64,
    }

    impl MetricsProvider {
        /// Initialize OpenTelemetry metrics with OTLP export
        pub fn new(config: &ObservabilityConfig) -> Result<Self, String> {
            let resource = Resource::new(vec![
                KeyValue::new("service.name", config.service_name.clone()),
                KeyValue::new(
                    "service.version",
                    config.service_version.clone().unwrap_or_else(|| "unknown".to_string()),
                ),
            ]);

            let meter_provider = if let Some(ref endpoint) = config.metrics_export_endpoint {
                let exporter = opentelemetry_otlp::MetricExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .build()
                    .map_err(|e| format!("Failed to create metrics exporter: {}", e))?;

                let reader = PeriodicReader::builder(exporter, opentelemetry_sdk::runtime::Tokio)
                    .with_interval(Duration::from_millis(config.metrics_export_interval_ms))
                    .build();

                SdkMeterProvider::builder()
                    .with_reader(reader)
                    .with_resource(resource)
                    .build()
            } else {
                let reader = ManualReader::builder().build();
                SdkMeterProvider::builder()
                    .with_reader(reader)
                    .with_resource(resource)
                    .build()
            };

            let meter = meter_provider.meter("duroxide");

            // Create all metrics instruments with Prometheus-compliant names and buckets

            // Dispatcher metrics (internal, keep millisecond precision)
            let orch_dispatcher_items_fetched = meter
                .u64_counter("duroxide.orchestration.dispatcher.items_fetched")
                .with_description("Items fetched by orchestration dispatcher")
                .build();

            let orch_dispatcher_processing_duration = meter
                .u64_histogram("duroxide.orchestration.dispatcher.processing_duration_ms")
                .with_description("Time to process an orchestration item")
                .build();

            let worker_dispatcher_items_fetched = meter
                .u64_counter("duroxide.worker.dispatcher.items_fetched")
                .with_description("Activities fetched by worker dispatcher")
                .build();

            let worker_dispatcher_execution_duration = meter
                .u64_histogram("duroxide.worker.dispatcher.execution_duration_ms")
                .with_description("Activity execution duration")
                .build();

            // Orchestration lifecycle metrics (Prometheus-compliant with labels)
            let orch_starts_total = meter
                .u64_counter("duroxide_orchestration_starts_total")
                .with_description("Total orchestrations started")
                .build();

            let orch_completions_total = meter
                .u64_counter("duroxide_orchestration_completions_total")
                .with_description("Orchestrations that completed (successfully or failed)")
                .build();

            let orch_failures_total = meter
                .u64_counter("duroxide_orchestration_failures_total")
                .with_description("Orchestration failures with detailed error classification")
                .build();

            let orch_duration_seconds = meter
                .f64_histogram("duroxide_orchestration_duration_seconds")
                .with_description("End-to-end orchestration execution time")
                .with_boundaries(vec![
                    0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0,
                ])
                .build();

            let orch_history_size_events = meter
                .u64_histogram("duroxide_orchestration_history_size")
                .with_description("History event count at orchestration completion")
                .with_boundaries(vec![10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0])
                .build();

            let orch_turns = meter
                .u64_histogram("duroxide_orchestration_turns")
                .with_description("Number of turns to orchestration completion")
                .with_boundaries(vec![1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0])
                .build();

            let orch_continue_as_new_total = meter
                .u64_counter("duroxide_orchestration_continue_as_new_total")
                .with_description("Continue-as-new operations performed")
                .build();

            // Sub-orchestration metrics
            let suborchestration_calls_total = meter
                .u64_counter("duroxide_suborchestration_calls_total")
                .with_description("Sub-orchestration invocations")
                .build();

            let suborchestration_duration_seconds = meter
                .f64_histogram("duroxide_suborchestration_duration_seconds")
                .with_description("Sub-orchestration execution time")
                .with_boundaries(vec![0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0])
                .build();

            // Provider metrics (Prometheus-compliant)
            let provider_operation_duration_seconds = meter
                .f64_histogram("duroxide_provider_operation_duration_seconds")
                .with_description("Database operation latency")
                .with_boundaries(vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0])
                .build();

            let provider_errors_total = meter
                .u64_counter("duroxide_provider_errors_total")
                .with_description("Provider/storage layer errors")
                .build();

            // Activity metrics (Prometheus-compliant with labels)
            let activity_executions_total = meter
                .u64_counter("duroxide_activity_executions_total")
                .with_description("Activity execution attempts")
                .build();

            let activity_duration_seconds = meter
                .f64_histogram("duroxide_activity_duration_seconds")
                .with_description("Activity execution time (wall clock)")
                .with_boundaries(vec![
                    0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
                ])
                .build();

            let activity_errors_total = meter
                .u64_counter("duroxide_activity_errors_total")
                .with_description("Detailed activity error tracking")
                .build();

            // Client metrics (Prometheus-compliant)
            let client_orch_starts_total = meter
                .u64_counter("duroxide_client_orchestration_starts_total")
                .with_description("Orchestration starts via client")
                .build();

            let client_events_raised_total = meter
                .u64_counter("duroxide_client_external_events_raised_total")
                .with_description("External events raised via client")
                .build();

            let client_cancellations_total = meter
                .u64_counter("duroxide_client_cancellations_total")
                .with_description("Orchestration cancellations via client")
                .build();

            let client_wait_duration_seconds = meter
                .f64_histogram("duroxide_client_wait_duration_seconds")
                .with_description("Client wait operation duration")
                .with_boundaries(vec![0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0])
                .build();

            Ok(Self {
                meter_provider,
                orch_dispatcher_items_fetched,
                orch_dispatcher_processing_duration,
                worker_dispatcher_items_fetched,
                worker_dispatcher_execution_duration,
                orch_starts_total,
                orch_completions_total,
                orch_failures_total,
                orch_duration_seconds,
                orch_history_size_events,
                orch_turns,
                orch_continue_as_new_total,
                suborchestration_calls_total,
                suborchestration_duration_seconds,
                provider_operation_duration_seconds,
                provider_errors_total,
                activity_executions_total,
                activity_duration_seconds,
                activity_errors_total,
                client_orch_starts_total,
                client_events_raised_total,
                client_cancellations_total,
                client_wait_duration_seconds,
                orch_completions_atomic: AtomicU64::new(0),
                orch_failures_atomic: AtomicU64::new(0),
                orch_application_errors_atomic: AtomicU64::new(0),
                orch_infrastructure_errors_atomic: AtomicU64::new(0),
                orch_configuration_errors_atomic: AtomicU64::new(0),
                activity_success_atomic: AtomicU64::new(0),
                activity_app_errors_atomic: AtomicU64::new(0),
                activity_infra_errors_atomic: AtomicU64::new(0),
                activity_config_errors_atomic: AtomicU64::new(0),
                orch_queue_depth_atomic: AtomicU64::new(0),
                worker_queue_depth_atomic: AtomicU64::new(0),
            })
        }

        /// Get the OpenTelemetry meter provider
        pub fn meter_provider(&self) -> &SdkMeterProvider {
            &self.meter_provider
        }

        /// Shutdown the metrics provider gracefully
        pub async fn shutdown(self) -> Result<(), String> {
            self.meter_provider
                .shutdown()
                .map_err(|e| format!("Failed to shutdown metrics provider: {}", e))
        }

        // Orchestration lifecycle methods with labels
        #[inline]
        pub fn record_orchestration_start(&self, orchestration_name: &str, version: &str, initiated_by: &str) {
            self.orch_starts_total.add(
                1,
                &[
                    KeyValue::new("orchestration_name", orchestration_name.to_string()),
                    KeyValue::new("version", version.to_string()),
                    KeyValue::new("initiated_by", initiated_by.to_string()),
                ],
            );
        }

        #[inline]
        pub fn record_orchestration_completion(
            &self,
            orchestration_name: &str,
            version: &str,
            status: &str,
            duration_seconds: f64,
            turn_count: u64,
            history_events: u64,
        ) {
            let turn_bucket = match turn_count {
                1..=5 => "1-5",
                6..=10 => "6-10",
                11..=50 => "11-50",
                _ => "50+",
            };

            self.orch_completions_total.add(
                1,
                &[
                    KeyValue::new("orchestration_name", orchestration_name.to_string()),
                    KeyValue::new("version", version.to_string()),
                    KeyValue::new("status", status.to_string()),
                    KeyValue::new("final_turn_count", turn_bucket.to_string()),
                ],
            );

            self.orch_duration_seconds.record(
                duration_seconds,
                &[
                    KeyValue::new("orchestration_name", orchestration_name.to_string()),
                    KeyValue::new("version", version.to_string()),
                    KeyValue::new("status", status.to_string()),
                ],
            );

            self.orch_turns.record(
                turn_count,
                &[KeyValue::new("orchestration_name", orchestration_name.to_string())],
            );

            self.orch_history_size_events.record(
                history_events,
                &[KeyValue::new("orchestration_name", orchestration_name.to_string())],
            );

            self.orch_completions_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_failure(
            &self,
            orchestration_name: &str,
            version: &str,
            error_type: &str,
            error_category: &str,
        ) {
            self.orch_failures_total.add(
                1,
                &[
                    KeyValue::new("orchestration_name", orchestration_name.to_string()),
                    KeyValue::new("version", version.to_string()),
                    KeyValue::new("error_type", error_type.to_string()),
                    KeyValue::new("error_category", error_category.to_string()),
                ],
            );

            self.orch_failures_atomic.fetch_add(1, Ordering::Relaxed);

            match error_type {
                "app_error" => self.orch_application_errors_atomic.fetch_add(1, Ordering::Relaxed),
                "infrastructure_error" => self.orch_infrastructure_errors_atomic.fetch_add(1, Ordering::Relaxed),
                "config_error" => self.orch_configuration_errors_atomic.fetch_add(1, Ordering::Relaxed),
                _ => 0,
            };
        }

        #[inline]
        pub fn record_orchestration_application_error(&self) {
            self.orch_failures_atomic.fetch_add(1, Ordering::Relaxed);
            self.orch_application_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_infrastructure_error(&self) {
            self.orch_failures_atomic.fetch_add(1, Ordering::Relaxed);
            self.orch_infrastructure_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_configuration_error(&self) {
            self.orch_failures_atomic.fetch_add(1, Ordering::Relaxed);
            self.orch_configuration_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_continue_as_new(&self, orchestration_name: &str, execution_id: u64) {
            self.orch_continue_as_new_total.add(
                1,
                &[
                    KeyValue::new("orchestration_name", orchestration_name.to_string()),
                    KeyValue::new("execution_id", execution_id.to_string()),
                ],
            );
        }

        // Activity execution methods with labels
        #[inline]
        pub fn record_activity_execution(
            &self,
            activity_name: &str,
            outcome: &str,
            duration_seconds: f64,
            retry_attempt: u32,
        ) {
            let retry_label = match retry_attempt {
                0 => "0",
                1 => "1",
                2 => "2",
                _ => "3+",
            };

            self.activity_executions_total.add(
                1,
                &[
                    KeyValue::new("activity_name", activity_name.to_string()),
                    KeyValue::new("outcome", outcome.to_string()),
                    KeyValue::new("retry_attempt", retry_label.to_string()),
                ],
            );

            self.activity_duration_seconds.record(
                duration_seconds,
                &[
                    KeyValue::new("activity_name", activity_name.to_string()),
                    KeyValue::new("outcome", outcome.to_string()),
                ],
            );

            match outcome {
                "success" => self.activity_success_atomic.fetch_add(1, Ordering::Relaxed),
                "app_error" => self.activity_app_errors_atomic.fetch_add(1, Ordering::Relaxed),
                "infra_error" | "config_error" => self.activity_infra_errors_atomic.fetch_add(1, Ordering::Relaxed),
                _ => 0,
            };
        }

        #[inline]
        pub fn record_activity_error(&self, activity_name: &str, error_type: &str, retryable: bool) {
            self.activity_errors_total.add(
                1,
                &[
                    KeyValue::new("activity_name", activity_name.to_string()),
                    KeyValue::new("error_type", error_type.to_string()),
                    KeyValue::new("retryable", retryable.to_string()),
                ],
            );
        }

        #[inline]
        pub fn record_activity_success(&self) {
            self.activity_success_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_app_error(&self) {
            self.activity_app_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_infra_error(&self) {
            self.activity_infra_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_config_error(&self) {
            self.activity_config_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        // Sub-orchestration methods
        #[inline]
        pub fn record_suborchestration_call(
            &self,
            parent_orchestration: &str,
            child_orchestration: &str,
            outcome: &str,
        ) {
            self.suborchestration_calls_total.add(
                1,
                &[
                    KeyValue::new("parent_orchestration", parent_orchestration.to_string()),
                    KeyValue::new("child_orchestration", child_orchestration.to_string()),
                    KeyValue::new("outcome", outcome.to_string()),
                ],
            );
        }

        #[inline]
        pub fn record_suborchestration_duration(
            &self,
            parent_orchestration: &str,
            child_orchestration: &str,
            duration_seconds: f64,
            outcome: &str,
        ) {
            self.suborchestration_duration_seconds.record(
                duration_seconds,
                &[
                    KeyValue::new("parent_orchestration", parent_orchestration.to_string()),
                    KeyValue::new("child_orchestration", child_orchestration.to_string()),
                    KeyValue::new("outcome", outcome.to_string()),
                ],
            );
        }

        // Provider operation methods
        #[inline]
        pub fn record_provider_operation(&self, operation: &str, duration_seconds: f64, status: &str) {
            self.provider_operation_duration_seconds.record(
                duration_seconds,
                &[
                    KeyValue::new("operation", operation.to_string()),
                    KeyValue::new("status", status.to_string()),
                ],
            );
        }

        #[inline]
        pub fn record_provider_error(&self, operation: &str, error_type: &str) {
            self.provider_errors_total.add(
                1,
                &[
                    KeyValue::new("operation", operation.to_string()),
                    KeyValue::new("error_type", error_type.to_string()),
                ],
            );
        }

        // Queue depth management
        #[inline]
        pub fn update_queue_depths(&self, orch_depth: u64, worker_depth: u64) {
            self.orch_queue_depth_atomic.store(orch_depth, Ordering::Relaxed);
            self.worker_queue_depth_atomic.store(worker_depth, Ordering::Relaxed);
        }

        #[inline]
        pub fn get_queue_depths(&self) -> (u64, u64) {
            (
                self.orch_queue_depth_atomic.load(Ordering::Relaxed),
                self.worker_queue_depth_atomic.load(Ordering::Relaxed),
            )
        }

        // Active orchestrations tracking
        #[inline]
        pub fn increment_active_orchestrations(&self) {
            self.active_orchestrations_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn decrement_active_orchestrations(&self) {
            self.active_orchestrations_atomic.fetch_sub(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn set_active_orchestrations(&self, count: i64) {
            self.active_orchestrations_atomic.store(count, Ordering::Relaxed);
        }

        #[inline]
        pub fn get_active_orchestrations(&self) -> i64 {
            self.active_orchestrations_atomic.load(Ordering::Relaxed)
        }

        pub fn snapshot(&self) -> MetricsSnapshot {
            MetricsSnapshot {
                orch_completions: self.orch_completions_atomic.load(Ordering::Relaxed),
                orch_failures: self.orch_failures_atomic.load(Ordering::Relaxed),
                orch_application_errors: self.orch_application_errors_atomic.load(Ordering::Relaxed),
                orch_infrastructure_errors: self.orch_infrastructure_errors_atomic.load(Ordering::Relaxed),
                orch_configuration_errors: self.orch_configuration_errors_atomic.load(Ordering::Relaxed),
                activity_success: self.activity_success_atomic.load(Ordering::Relaxed),
                activity_app_errors: self.activity_app_errors_atomic.load(Ordering::Relaxed),
                activity_infra_errors: self.activity_infra_errors_atomic.load(Ordering::Relaxed),
                activity_config_errors: self.activity_config_errors_atomic.load(Ordering::Relaxed),
            }
        }
    }

    pub fn init_logging(config: &ObservabilityConfig) -> Result<(), String> {
        let env_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(default_filter_expression(&config.log_level)));

        match config.log_format {
            LogFormat::Json => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(tracing_subscriber::fmt::layer().json())
                    .try_init()
                    .map_err(|e| format!("Failed to initialize JSON logging: {}", e))?;
            }
            LogFormat::Pretty => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(tracing_subscriber::fmt::layer())
                    .try_init()
                    .map_err(|e| format!("Failed to initialize pretty logging: {}", e))?;
            }
            LogFormat::Compact => {
                // Custom compact format: timestamp level module [instance_id] message
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(tracing_subscriber::fmt::layer().compact())
                    .try_init()
                    .map_err(|e| format!("Failed to initialize compact logging: {}", e))?;
            }
        }

        Ok(())
    }
}

#[cfg(not(feature = "observability"))]
mod stub_impl {
    use super::*;

    /// Stub metrics provider when observability feature is disabled
    pub struct MetricsProvider {
        orch_completions_atomic: AtomicU64,
        orch_failures_atomic: AtomicU64,
        orch_application_errors_atomic: AtomicU64,
        orch_infrastructure_errors_atomic: AtomicU64,
        orch_configuration_errors_atomic: AtomicU64,
        activity_success_atomic: AtomicU64,
        activity_app_errors_atomic: AtomicU64,
        activity_infra_errors_atomic: AtomicU64,
        activity_config_errors_atomic: AtomicU64,
        orch_queue_depth_atomic: AtomicU64,
        worker_queue_depth_atomic: AtomicU64,
        active_orchestrations_atomic: std::sync::atomic::AtomicI64,
    }

    impl MetricsProvider {
        pub fn new(_config: &ObservabilityConfig) -> Result<Self, String> {
            Ok(Self {
                orch_completions_atomic: AtomicU64::new(0),
                orch_failures_atomic: AtomicU64::new(0),
                orch_application_errors_atomic: AtomicU64::new(0),
                orch_infrastructure_errors_atomic: AtomicU64::new(0),
                orch_configuration_errors_atomic: AtomicU64::new(0),
                activity_success_atomic: AtomicU64::new(0),
                activity_app_errors_atomic: AtomicU64::new(0),
                activity_infra_errors_atomic: AtomicU64::new(0),
                activity_config_errors_atomic: AtomicU64::new(0),
                orch_queue_depth_atomic: AtomicU64::new(0),
                worker_queue_depth_atomic: AtomicU64::new(0),
                active_orchestrations_atomic: std::sync::atomic::AtomicI64::new(0),
            })
        }

        pub async fn shutdown(self) -> Result<(), String> {
            Ok(())
        }

        // Stub implementations with same signatures as real provider
        #[inline]
        pub fn record_orchestration_start(&self, _: &str, _: &str, _: &str) {}

        #[inline]
        pub fn record_orchestration_completion(&self, _: &str, _: &str, _: &str, _: f64, _: u64, _: u64) {
            self.orch_completions_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_failure(&self, _: &str, _: &str, error_type: &str, _: &str) {
            self.orch_failures_atomic.fetch_add(1, Ordering::Relaxed);
            match error_type {
                "app_error" => self.orch_application_errors_atomic.fetch_add(1, Ordering::Relaxed),
                "infrastructure_error" => self.orch_infrastructure_errors_atomic.fetch_add(1, Ordering::Relaxed),
                "config_error" => self.orch_configuration_errors_atomic.fetch_add(1, Ordering::Relaxed),
                _ => 0,
            };
        }

        #[inline]
        pub fn record_orchestration_application_error(&self) {
            self.orch_failures_atomic.fetch_add(1, Ordering::Relaxed);
            self.orch_application_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_infrastructure_error(&self) {
            self.orch_failures_atomic.fetch_add(1, Ordering::Relaxed);
            self.orch_infrastructure_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_configuration_error(&self) {
            self.orch_failures_atomic.fetch_add(1, Ordering::Relaxed);
            self.orch_configuration_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_continue_as_new(&self, _: &str, _: u64) {}

        #[inline]
        pub fn record_activity_execution(&self, _: &str, outcome: &str, _: f64, _: u32) {
            match outcome {
                "success" => self.activity_success_atomic.fetch_add(1, Ordering::Relaxed),
                "app_error" => self.activity_app_errors_atomic.fetch_add(1, Ordering::Relaxed),
                _ => self.activity_infra_errors_atomic.fetch_add(1, Ordering::Relaxed),
            };
        }

        #[inline]
        pub fn record_activity_error(&self, _: &str, _: &str, _: bool) {}

        #[inline]
        pub fn record_activity_success(&self) {
            self.activity_success_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_app_error(&self) {
            self.activity_app_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_infra_error(&self) {
            self.activity_infra_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_config_error(&self) {
            self.activity_config_errors_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_suborchestration_call(&self, _: &str, _: &str, _: &str) {}

        #[inline]
        pub fn record_suborchestration_duration(&self, _: &str, _: &str, _: f64, _: &str) {}

        #[inline]
        pub fn record_provider_operation(&self, _: &str, _: f64, _: &str) {}

        #[inline]
        pub fn record_provider_error(&self, _: &str, _: &str) {}

        #[inline]
        pub fn update_queue_depths(&self, orch_depth: u64, worker_depth: u64) {
            self.orch_queue_depth_atomic.store(orch_depth, Ordering::Relaxed);
            self.worker_queue_depth_atomic.store(worker_depth, Ordering::Relaxed);
        }

        #[inline]
        pub fn get_queue_depths(&self) -> (u64, u64) {
            (
                self.orch_queue_depth_atomic.load(Ordering::Relaxed),
                self.worker_queue_depth_atomic.load(Ordering::Relaxed),
            )
        }

        // Active orchestrations tracking (stub - same signature as real)
        #[inline]
        pub fn increment_active_orchestrations(&self) {
            self.active_orchestrations_atomic.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn decrement_active_orchestrations(&self) {
            self.active_orchestrations_atomic.fetch_sub(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn set_active_orchestrations(&self, count: i64) {
            self.active_orchestrations_atomic.store(count, Ordering::Relaxed);
        }

        #[inline]
        pub fn get_active_orchestrations(&self) -> i64 {
            self.active_orchestrations_atomic.load(Ordering::Relaxed)
        }

        pub fn snapshot(&self) -> MetricsSnapshot {
            MetricsSnapshot {
                orch_completions: self.orch_completions_atomic.load(Ordering::Relaxed),
                orch_failures: self.orch_failures_atomic.load(Ordering::Relaxed),
                orch_application_errors: self.orch_application_errors_atomic.load(Ordering::Relaxed),
                orch_infrastructure_errors: self.orch_infrastructure_errors_atomic.load(Ordering::Relaxed),
                orch_configuration_errors: self.orch_configuration_errors_atomic.load(Ordering::Relaxed),
                activity_success: self.activity_success_atomic.load(Ordering::Relaxed),
                activity_app_errors: self.activity_app_errors_atomic.load(Ordering::Relaxed),
                activity_infra_errors: self.activity_infra_errors_atomic.load(Ordering::Relaxed),
                activity_config_errors: self.activity_config_errors_atomic.load(Ordering::Relaxed),
            }
        }
    }

    /// Stub logging initialization when observability feature is disabled
    pub fn init_logging(config: &ObservabilityConfig) -> Result<(), String> {
        // Fall back to basic tracing subscriber
        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter_expression(&config.log_level)));

        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .try_init()
            .map_err(|e| format!("Failed to initialize logging: {}", e))?;

        Ok(())
    }
}

#[cfg(feature = "observability")]
pub use otel_impl::*;

#[cfg(not(feature = "observability"))]
pub use stub_impl::*;

/// Observability handle that manages metrics and logging lifecycle
pub struct ObservabilityHandle {
    #[allow(dead_code)]
    metrics_provider: Option<Arc<MetricsProvider>>,
}

impl ObservabilityHandle {
    /// Initialize observability with the given configuration
    pub fn init(config: &ObservabilityConfig) -> Result<Self, String> {
        // Initialize logging first, but tolerate failures (e.g., global subscriber already set)
        if let Err(_err) = init_logging(config) {
            eprintln!("duroxide: logging already initialized (this is normal if running multiple runtimes or tests)");
        }

        // Initialize metrics if enabled
        let metrics_provider = if config.metrics_enabled {
            Some(Arc::new(MetricsProvider::new(config)?))
        } else {
            None
        };

        Ok(Self { metrics_provider })
    }

    /// Get the metrics provider if available
    pub fn metrics_provider(&self) -> Option<&Arc<MetricsProvider>> {
        self.metrics_provider.as_ref()
    }

    /// Shutdown observability gracefully
    pub async fn shutdown(self) -> Result<(), String> {
        if let Some(provider) = self.metrics_provider {
            // Take ownership out of Arc if we're the last reference
            if let Ok(provider) = Arc::try_unwrap(provider) {
                provider.shutdown().await?;
            }
        }
        Ok(())
    }

    // Legacy methods for backward compatibility (atomic counters only, no labels)
    #[inline]
    pub fn record_orchestration_completion(&self) {
        // Just update atomic counter for backward compatibility
        // New code should use Runtime methods that call label-aware versions
    }

    #[inline]
    pub fn record_orchestration_application_error(&self) {
        if let Some(provider) = &self.metrics_provider {
            provider.record_orchestration_application_error();
        }
    }

    #[inline]
    pub fn record_orchestration_infrastructure_error(&self) {
        if let Some(provider) = &self.metrics_provider {
            provider.record_orchestration_infrastructure_error();
        }
    }

    #[inline]
    pub fn record_orchestration_configuration_error(&self) {
        if let Some(provider) = &self.metrics_provider {
            provider.record_orchestration_configuration_error();
        }
    }

    #[inline]
    pub fn record_activity_success(&self) {
        if let Some(provider) = &self.metrics_provider {
            provider.record_activity_success();
        }
    }

    #[inline]
    pub fn record_activity_app_error(&self) {
        if let Some(provider) = &self.metrics_provider {
            provider.record_activity_app_error();
        }
    }

    #[inline]
    pub fn record_activity_infra_error(&self) {
        if let Some(provider) = &self.metrics_provider {
            provider.record_activity_infra_error();
        }
    }

    #[inline]
    pub fn record_activity_config_error(&self) {
        if let Some(provider) = &self.metrics_provider {
            provider.record_activity_config_error();
        }
    }

    pub fn metrics_snapshot(&self) -> Option<MetricsSnapshot> {
        self.metrics_provider.as_ref().map(|p| p.snapshot())
    }
}
