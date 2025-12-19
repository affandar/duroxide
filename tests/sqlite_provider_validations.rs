//! Provider validation tests for SQLite
//!
//! This test file validates the SQLite provider using the reusable
//! provider validation test suite from `duroxide::provider_validations`.
//!
//! These tests automatically enable the `provider-test` feature when running
//! tests within the duroxide repository.

#[cfg(feature = "provider-test")]
mod tests {
    use duroxide::provider_validations::{
        ProviderFactory,
        // Long polling tests
        long_polling::{
            test_fetch_respects_timeout_upper_bound, test_short_poll_returns_immediately,
            test_short_poll_work_item_returns_immediately,
        },
        // Poison message tests
        poison_message::{
            abandon_orchestration_item_ignore_attempt_decrements, abandon_work_item_ignore_attempt_decrements,
            attempt_count_is_per_message, ignore_attempt_never_goes_negative, max_attempt_count_across_message_batch,
            orchestration_attempt_count_increments_on_refetch, orchestration_attempt_count_starts_at_one,
            worker_attempt_count_increments_on_lock_expiry, worker_attempt_count_starts_at_one,
        },
        test_abandon_releases_lock_immediately,
        test_abandon_work_item_releases_lock,
        test_abandon_work_item_with_delay,
        test_ack_only_affects_locked_messages,
        // Atomicity tests
        test_atomicity_failure_rollback,
        test_completions_arriving_during_lock_blocked,
        test_concurrent_ack_prevention,
        test_concurrent_instance_fetching,
        test_concurrent_lock_attempts_respect_expiration,
        test_continue_as_new_creates_new_execution,
        test_corrupted_serialization_data,
        test_cross_instance_lock_isolation,
        test_duplicate_event_id_rejection,
        // Instance locking tests
        test_exclusive_instance_lock,
        test_execution_history_persistence,
        test_execution_id_sequencing,
        // Multi-execution tests
        test_execution_isolation,
        test_get_execution_info,
        test_get_instance_info,
        test_get_queue_depths,
        test_get_system_metrics,
        // Instance creation tests
        test_instance_creation_via_metadata,
        // Error handling tests
        test_invalid_lock_token_on_ack,
        test_invalid_lock_token_rejection,
        test_latest_execution_detection,
        test_list_executions,
        // Management tests
        test_list_instances,
        test_list_instances_by_status,
        test_lock_expiration_during_ack,
        // Lock expiration tests
        test_lock_expires_after_timeout,
        test_lock_released_only_on_successful_ack,
        test_lock_renewal_on_ack,
        test_lock_token_uniqueness,
        test_lost_lock_token_handling,
        test_message_tagging_during_lock,
        test_missing_instance_metadata,
        test_multi_operation_atomic_ack,
        test_multi_threaded_lock_contention,
        test_multi_threaded_lock_expiration_recovery,
        test_multi_threaded_no_duplicate_processing,
        test_no_instance_creation_on_enqueue,
        test_null_version_handling,
        test_sub_orchestration_instance_creation,
        test_timer_delayed_visibility,
        test_worker_ack_atomicity,
        test_worker_delayed_visibility_skips_future_items,
        test_worker_item_immediate_visibility,
        test_worker_peek_lock_semantics,
        // Queue semantics tests
        test_worker_queue_fifo_ordering,
    };
    use duroxide::providers::Provider;
    use duroxide::providers::sqlite::SqliteProvider;
    use std::sync::Arc;
    use std::time::Duration;

    const TEST_LOCK_TIMEOUT: Duration = Duration::from_millis(1000);

    struct SqliteTestFactory;

    #[async_trait::async_trait]
    impl ProviderFactory for SqliteTestFactory {
        async fn create_provider(&self) -> Arc<dyn Provider> {
            // Lock timeout is now configured via RuntimeOptions, not SqliteOptions
            Arc::new(SqliteProvider::new_in_memory().await.unwrap())
        }

        fn lock_timeout(&self) -> Duration {
            TEST_LOCK_TIMEOUT
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

    #[tokio::test]
    async fn test_sqlite_worker_item_immediate_visibility() {
        test_worker_item_immediate_visibility(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_worker_delayed_visibility_skips_future_items() {
        test_worker_delayed_visibility_skips_future_items(&SqliteTestFactory).await;
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

    // Instance creation tests
    #[tokio::test]
    async fn test_sqlite_instance_creation_via_metadata() {
        test_instance_creation_via_metadata(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_no_instance_creation_on_enqueue() {
        test_no_instance_creation_on_enqueue(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_null_version_handling() {
        test_null_version_handling(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_sub_orchestration_instance_creation() {
        test_sub_orchestration_instance_creation(&SqliteTestFactory).await;
    }

    // Long polling tests (SQLite uses short polling)
    #[tokio::test]
    async fn test_sqlite_short_poll_returns_immediately() {
        let provider = SqliteTestFactory.create_provider().await;
        test_short_poll_returns_immediately(&*provider).await;
    }

    #[tokio::test]
    async fn test_sqlite_short_poll_work_item_returns_immediately() {
        let provider = SqliteTestFactory.create_provider().await;
        test_short_poll_work_item_returns_immediately(&*provider).await;
    }

    #[tokio::test]
    async fn test_sqlite_fetch_respects_timeout_upper_bound() {
        let provider = SqliteTestFactory.create_provider().await;
        test_fetch_respects_timeout_upper_bound(&*provider).await;
    }

    // Poison message tests
    #[tokio::test]
    async fn test_sqlite_orchestration_attempt_count_starts_at_one() {
        orchestration_attempt_count_starts_at_one(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_orchestration_attempt_count_increments_on_refetch() {
        orchestration_attempt_count_increments_on_refetch(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_worker_attempt_count_starts_at_one() {
        worker_attempt_count_starts_at_one(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_worker_attempt_count_increments_on_lock_expiry() {
        worker_attempt_count_increments_on_lock_expiry(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_attempt_count_is_per_message() {
        attempt_count_is_per_message(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_abandon_work_item_ignore_attempt_decrements() {
        abandon_work_item_ignore_attempt_decrements(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_abandon_orchestration_item_ignore_attempt_decrements() {
        abandon_orchestration_item_ignore_attempt_decrements(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_ignore_attempt_never_goes_negative() {
        ignore_attempt_never_goes_negative(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_max_attempt_count_across_message_batch() {
        max_attempt_count_across_message_batch(&SqliteTestFactory).await;
    }

    // abandon_work_item tests
    #[tokio::test]
    async fn test_sqlite_abandon_work_item_releases_lock() {
        test_abandon_work_item_releases_lock(&SqliteTestFactory).await;
    }

    #[tokio::test]
    async fn test_sqlite_abandon_work_item_with_delay() {
        test_abandon_work_item_with_delay(&SqliteTestFactory).await;
    }
}
