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
    format!("warn,duroxide::orchestrator={level},duroxide::activity={level}")
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

        // Orchestration execution metrics
        pub orch_completions: Counter<u64>,
        pub orch_failures: Counter<u64>,
        pub orch_infrastructure_errors: Counter<u64>,
        pub orch_configuration_errors: Counter<u64>,
        pub orch_history_size_events: Histogram<u64>,
        pub orch_history_size_bytes: Histogram<u64>,
        pub orch_turns: Histogram<u64>,

        // Provider metrics
        pub provider_fetch_duration: Histogram<u64>,
        pub provider_ack_orch_duration: Histogram<u64>,
        pub provider_ack_work_item_duration: Histogram<u64>,
        pub provider_dequeue_worker_duration: Histogram<u64>,
        pub provider_enqueue_orch_duration: Histogram<u64>,
        pub provider_ack_retries: Counter<u64>,
        pub provider_infra_errors: Counter<u64>,

        // Activity metrics
        pub activity_executions: Counter<u64>,
        pub activity_duration: Histogram<u64>,
        pub activity_app_errors: Counter<u64>,
        pub activity_infra_errors: Counter<u64>,
        pub activity_config_errors: Counter<u64>,

        // Client metrics
        pub client_orch_starts: Counter<u64>,
        pub client_events_raised: Counter<u64>,
        pub client_cancellations: Counter<u64>,
        pub client_wait_duration: Histogram<u64>,

        // Test-observable counters
        orch_completions_total: AtomicU64,
        orch_failures_total: AtomicU64,
        orch_application_errors_total: AtomicU64,
        orch_infrastructure_errors_total: AtomicU64,
        orch_configuration_errors_total: AtomicU64,
        activity_success_total: AtomicU64,
        activity_app_errors_total: AtomicU64,
        activity_infra_errors_total: AtomicU64,
        activity_config_errors_total: AtomicU64,
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

            // Create all metrics instruments
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

            let orch_completions = meter
                .u64_counter("duroxide.orchestration.completions")
                .with_description("Successful orchestration completions")
                .build();

            let orch_failures = meter
                .u64_counter("duroxide.orchestration.failures")
                .with_description("Orchestration failures by error type")
                .build();

            let orch_infrastructure_errors = meter
                .u64_counter("duroxide.orchestration.infrastructure_errors")
                .with_description("Infrastructure-level orchestration errors")
                .build();

            let orch_configuration_errors = meter
                .u64_counter("duroxide.orchestration.configuration_errors")
                .with_description("Configuration-level orchestration errors")
                .build();

            let orch_history_size_events = meter
                .u64_histogram("duroxide.orchestration.history_size_events")
                .with_description("Event count at orchestration completion")
                .build();

            let orch_history_size_bytes = meter
                .u64_histogram("duroxide.orchestration.history_size_bytes")
                .with_description("Serialized history size at completion")
                .build();

            let orch_turns = meter
                .u64_histogram("duroxide.orchestration.turns")
                .with_description("Number of turns to completion")
                .build();

            let provider_fetch_duration = meter
                .u64_histogram("duroxide.provider.fetch_orchestration_item_duration_ms")
                .with_description("Provider fetch operation duration")
                .build();

            let provider_ack_orch_duration = meter
                .u64_histogram("duroxide.provider.ack_orchestration_item_duration_ms")
                .with_description("Provider orchestration ack duration")
                .build();

            let provider_ack_work_item_duration = meter
                .u64_histogram("duroxide.provider.ack_work_item_duration_ms")
                .with_description("Provider worker ack duration")
                .build();

            let provider_dequeue_worker_duration = meter
                .u64_histogram("duroxide.provider.dequeue_worker_duration_ms")
                .with_description("Provider worker dequeue duration")
                .build();

            let provider_enqueue_orch_duration = meter
                .u64_histogram("duroxide.provider.enqueue_orchestrator_duration_ms")
                .with_description("Provider orchestrator enqueue duration")
                .build();

            let provider_ack_retries = meter
                .u64_counter("duroxide.provider.ack_orchestration_retries")
                .with_description("Ack retry attempts")
                .build();

            let provider_infra_errors = meter
                .u64_counter("duroxide.provider.infrastructure_errors")
                .with_description("Provider infrastructure errors")
                .build();

            let activity_executions = meter
                .u64_counter("duroxide.activity.executions")
                .with_description("Activity execution outcomes")
                .build();

            let activity_duration = meter
                .u64_histogram("duroxide.activity.duration_ms")
                .with_description("Activity execution duration")
                .build();

            let activity_app_errors = meter
                .u64_counter("duroxide.activity.app_errors")
                .with_description("Application-level activity errors")
                .build();

            let activity_infra_errors = meter
                .u64_counter("duroxide.activity.infrastructure_errors")
                .with_description("Infrastructure-level activity errors")
                .build();

            let activity_config_errors = meter
                .u64_counter("duroxide.activity.configuration_errors")
                .with_description("Configuration-level activity errors")
                .build();

            let client_orch_starts = meter
                .u64_counter("duroxide.client.orchestration_starts")
                .with_description("Orchestration starts via client")
                .build();

            let client_events_raised = meter
                .u64_counter("duroxide.client.external_events_raised")
                .with_description("External events raised via client")
                .build();

            let client_cancellations = meter
                .u64_counter("duroxide.client.cancellations")
                .with_description("Orchestration cancellations via client")
                .build();

            let client_wait_duration = meter
                .u64_histogram("duroxide.client.wait_duration_ms")
                .with_description("Client wait operation duration")
                .build();

            Ok(Self {
                meter_provider,
                orch_dispatcher_items_fetched,
                orch_dispatcher_processing_duration,
                worker_dispatcher_items_fetched,
                worker_dispatcher_execution_duration,
                orch_completions,
                orch_failures,
                orch_infrastructure_errors,
                orch_configuration_errors,
                orch_history_size_events,
                orch_history_size_bytes,
                orch_turns,
                provider_fetch_duration,
                provider_ack_orch_duration,
                provider_ack_work_item_duration,
                provider_dequeue_worker_duration,
                provider_enqueue_orch_duration,
                provider_ack_retries,
                provider_infra_errors,
                activity_executions,
                activity_duration,
                activity_app_errors,
                activity_infra_errors,
                activity_config_errors,
                client_orch_starts,
                client_events_raised,
                client_cancellations,
                client_wait_duration,
                orch_completions_total: AtomicU64::new(0),
                orch_failures_total: AtomicU64::new(0),
                orch_application_errors_total: AtomicU64::new(0),
                orch_infrastructure_errors_total: AtomicU64::new(0),
                orch_configuration_errors_total: AtomicU64::new(0),
                activity_success_total: AtomicU64::new(0),
                activity_app_errors_total: AtomicU64::new(0),
                activity_infra_errors_total: AtomicU64::new(0),
                activity_config_errors_total: AtomicU64::new(0),
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

        #[inline]
        pub fn record_orchestration_completion(&self) {
            self.orch_completions.add(1, &[]);
            self.orch_completions_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_application_error(&self) {
            self.orch_failures.add(1, &[]);
            self.orch_failures_total.fetch_add(1, Ordering::Relaxed);
            self.orch_application_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_infrastructure_error(&self) {
            self.orch_failures.add(1, &[]);
            self.orch_infrastructure_errors.add(1, &[]);
            self.orch_failures_total.fetch_add(1, Ordering::Relaxed);
            self.orch_infrastructure_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_configuration_error(&self) {
            self.orch_failures.add(1, &[]);
            self.orch_configuration_errors.add(1, &[]);
            self.orch_failures_total.fetch_add(1, Ordering::Relaxed);
            self.orch_configuration_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_success(&self) {
            self.activity_executions.add(1, &[]);
            self.activity_success_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_app_error(&self) {
            self.activity_app_errors.add(1, &[]);
            self.activity_app_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_infra_error(&self) {
            self.activity_infra_errors.add(1, &[]);
            self.activity_infra_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_config_error(&self) {
            self.activity_config_errors.add(1, &[]);
            self.activity_config_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        pub fn snapshot(&self) -> MetricsSnapshot {
            MetricsSnapshot {
                orch_completions: self.orch_completions_total.load(Ordering::Relaxed),
                orch_failures: self.orch_failures_total.load(Ordering::Relaxed),
                orch_application_errors: self.orch_application_errors_total.load(Ordering::Relaxed),
                orch_infrastructure_errors: self.orch_infrastructure_errors_total.load(Ordering::Relaxed),
                orch_configuration_errors: self.orch_configuration_errors_total.load(Ordering::Relaxed),
                activity_success: self.activity_success_total.load(Ordering::Relaxed),
                activity_app_errors: self.activity_app_errors_total.load(Ordering::Relaxed),
                activity_infra_errors: self.activity_infra_errors_total.load(Ordering::Relaxed),
                activity_config_errors: self.activity_config_errors_total.load(Ordering::Relaxed),
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
        orch_completions_total: AtomicU64,
        orch_failures_total: AtomicU64,
        orch_application_errors_total: AtomicU64,
        orch_infrastructure_errors_total: AtomicU64,
        orch_configuration_errors_total: AtomicU64,
        activity_success_total: AtomicU64,
        activity_app_errors_total: AtomicU64,
        activity_infra_errors_total: AtomicU64,
        activity_config_errors_total: AtomicU64,
    }

    impl MetricsProvider {
        pub fn new(_config: &ObservabilityConfig) -> Result<Self, String> {
            Ok(Self {
                orch_completions_total: AtomicU64::new(0),
                orch_failures_total: AtomicU64::new(0),
                orch_application_errors_total: AtomicU64::new(0),
                orch_infrastructure_errors_total: AtomicU64::new(0),
                orch_configuration_errors_total: AtomicU64::new(0),
                activity_success_total: AtomicU64::new(0),
                activity_app_errors_total: AtomicU64::new(0),
                activity_infra_errors_total: AtomicU64::new(0),
                activity_config_errors_total: AtomicU64::new(0),
            })
        }

        pub async fn shutdown(self) -> Result<(), String> {
            Ok(())
        }

        #[inline]
        pub fn record_orchestration_completion(&self) {
            self.orch_completions_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_application_error(&self) {
            self.orch_failures_total.fetch_add(1, Ordering::Relaxed);
            self.orch_application_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_infrastructure_error(&self) {
            self.orch_failures_total.fetch_add(1, Ordering::Relaxed);
            self.orch_infrastructure_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_orchestration_configuration_error(&self) {
            self.orch_failures_total.fetch_add(1, Ordering::Relaxed);
            self.orch_configuration_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_success(&self) {
            self.activity_success_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_app_error(&self) {
            self.activity_app_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_infra_error(&self) {
            self.activity_infra_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn record_activity_config_error(&self) {
            self.activity_config_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        pub fn snapshot(&self) -> MetricsSnapshot {
            MetricsSnapshot {
                orch_completions: self.orch_completions_total.load(Ordering::Relaxed),
                orch_failures: self.orch_failures_total.load(Ordering::Relaxed),
                orch_application_errors: self.orch_application_errors_total.load(Ordering::Relaxed),
                orch_infrastructure_errors: self.orch_infrastructure_errors_total.load(Ordering::Relaxed),
                orch_configuration_errors: self.orch_configuration_errors_total.load(Ordering::Relaxed),
                activity_success: self.activity_success_total.load(Ordering::Relaxed),
                activity_app_errors: self.activity_app_errors_total.load(Ordering::Relaxed),
                activity_infra_errors: self.activity_infra_errors_total.load(Ordering::Relaxed),
                activity_config_errors: self.activity_config_errors_total.load(Ordering::Relaxed),
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
        if let Err(err) = init_logging(config) {
            eprintln!("duroxide observability logging init failed: {err}");
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

    #[inline]
    pub fn record_orchestration_completion(&self) {
        if let Some(provider) = &self.metrics_provider {
            provider.record_orchestration_completion();
        }
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
