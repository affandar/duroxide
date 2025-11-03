//! Provider validation tests for SQLite
//!
//! This test file validates the SQLite provider using the reusable
//! provider validation test suite from `duroxide::provider_validations`.
//!
//! **Note:** These tests require the `provider-test` feature to be enabled.
//! Run with: `cargo test --test sqlite_provider_validations --features provider-test`

#[cfg(feature = "provider-test")]
mod tests {
    use duroxide::provider_validations::{
        ProviderFactory,
        // Atomicity tests
        test_atomicity_failure_rollback, test_multi_operation_atomic_ack,
        test_lock_released_only_on_successful_ack, test_concurrent_ack_prevention,
        // Error handling tests
        test_invalid_lock_token_on_ack, test_duplicate_event_id_rejection,
        test_missing_instance_metadata, test_corrupted_serialization_data,
        test_lock_expiration_during_ack,
        // Instance locking tests
        test_exclusive_instance_lock, test_lock_token_uniqueness,
        test_invalid_lock_token_rejection, test_concurrent_instance_fetching,
        test_completions_arriving_during_lock_blocked, test_cross_instance_lock_isolation,
        test_message_tagging_during_lock, test_ack_only_affects_locked_messages,
        test_multi_threaded_lock_contention, test_multi_threaded_no_duplicate_processing,
        test_multi_threaded_lock_expiration_recovery,
        // Lock expiration tests
        test_lock_expires_after_timeout, test_abandon_releases_lock_immediately,
        test_lock_renewal_on_ack, test_concurrent_lock_attempts_respect_expiration,
        // Multi-execution tests
        test_execution_isolation, test_latest_execution_detection,
        test_execution_id_sequencing, test_continue_as_new_creates_new_execution,
        test_execution_history_persistence,
        // Queue semantics tests
        test_worker_queue_fifo_ordering, test_worker_peek_lock_semantics,
        test_worker_ack_atomicity, test_timer_delayed_visibility,
        test_lost_lock_token_handling,
        // Management tests
        test_list_instances, test_list_instances_by_status, test_list_executions,
        test_get_instance_info, test_get_execution_info, test_get_system_metrics,
        test_get_queue_depths,
    };
    use duroxide::providers::Provider;
    use duroxide::providers::sqlite::{SqliteOptions, SqliteProvider};
    use std::sync::Arc;
    use std::time::Duration;

    const TEST_LOCK_TIMEOUT_MS: u64 = 1000;

    struct SqliteTestFactory;

    #[async_trait::async_trait]
    impl ProviderFactory for SqliteTestFactory {
        async fn create_provider(&self) -> Arc<dyn Provider> {
            let options = SqliteOptions {
                lock_timeout: Duration::from_millis(TEST_LOCK_TIMEOUT_MS),
            };
            Arc::new(SqliteProvider::new_in_memory_with_options(Some(options)).await.unwrap())
        }

        fn lock_timeout_ms(&self) -> u64 {
            TEST_LOCK_TIMEOUT_MS
        }
    }

    // Atomicity tests
    #[tokio::test]
    async fn test_sqlite_atomicity_failure_rollback() {
        test_atomicity_failure_rollback(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_multi_operation_atomic_ack() {
        test_multi_operation_atomic_ack(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_lock_released_only_on_successful_ack() {
        test_lock_released_only_on_successful_ack(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_concurrent_ack_prevention() {
        test_concurrent_ack_prevention(&SqliteTestFactory).await;
    }

    // Error handling tests
    #[tokio::test]
    async fn test_sqlite_invalid_lock_token_on_ack() {
        test_invalid_lock_token_on_ack(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_duplicate_event_id_rejection() {
        test_duplicate_event_id_rejection(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_missing_instance_metadata() {
        test_missing_instance_metadata(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_corrupted_serialization_data() {
        test_corrupted_serialization_data(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_lock_expiration_during_ack() {
        test_lock_expiration_during_ack(&SqliteTestFactory).await;
    }

    // Instance locking tests
    #[tokio::test]
    async fn test_sqlite_exclusive_instance_lock() {
        test_exclusive_instance_lock(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_lock_token_uniqueness() {
        test_lock_token_uniqueness(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_invalid_lock_token_rejection() {
        test_invalid_lock_token_rejection(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_concurrent_instance_fetching() {
        test_concurrent_instance_fetching(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_completions_arriving_during_lock_blocked() {
        test_completions_arriving_during_lock_blocked(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_cross_instance_lock_isolation() {
        test_cross_instance_lock_isolation(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_message_tagging_during_lock() {
        test_message_tagging_during_lock(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_ack_only_affects_locked_messages() {
        test_ack_only_affects_locked_messages(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_multi_threaded_lock_contention() {
        test_multi_threaded_lock_contention(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_multi_threaded_no_duplicate_processing() {
        test_multi_threaded_no_duplicate_processing(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_multi_threaded_lock_expiration_recovery() {
        test_multi_threaded_lock_expiration_recovery(&SqliteTestFactory).await;
    }

    // Lock expiration tests
    #[tokio::test]
    async fn test_sqlite_lock_expires_after_timeout() {
        test_lock_expires_after_timeout(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_abandon_releases_lock_immediately() {
        test_abandon_releases_lock_immediately(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_lock_renewal_on_ack() {
        test_lock_renewal_on_ack(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_concurrent_lock_attempts_respect_expiration() {
        test_concurrent_lock_attempts_respect_expiration(&SqliteTestFactory).await;
    }

    // Multi-execution tests
    #[tokio::test]
    async fn test_sqlite_execution_isolation() {
        test_execution_isolation(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_latest_execution_detection() {
        test_latest_execution_detection(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_execution_id_sequencing() {
        test_execution_id_sequencing(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_continue_as_new_creates_new_execution() {
        test_continue_as_new_creates_new_execution(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_execution_history_persistence() {
        test_execution_history_persistence(&SqliteTestFactory).await;
    }

    // Queue semantics tests
    #[tokio::test]
    async fn test_sqlite_worker_queue_fifo_ordering() {
        test_worker_queue_fifo_ordering(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_worker_peek_lock_semantics() {
        test_worker_peek_lock_semantics(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_worker_ack_atomicity() {
        test_worker_ack_atomicity(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_timer_delayed_visibility() {
        test_timer_delayed_visibility(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_lost_lock_token_handling() {
        test_lost_lock_token_handling(&SqliteTestFactory).await;
    }

    // Management tests
    #[tokio::test]
    async fn test_sqlite_list_instances() {
        test_list_instances(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_list_instances_by_status() {
        test_list_instances_by_status(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_list_executions() {
        test_list_executions(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_get_instance_info() {
        test_get_instance_info(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_get_execution_info() {
        test_get_execution_info(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_get_system_metrics() {
        test_get_system_metrics(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_get_queue_depths() {
        test_get_queue_depths(&SqliteTestFactory).await;
    }
}
