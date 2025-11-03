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
        ProviderFactory, run_atomicity_tests, run_error_handling_tests, run_instance_locking_tests,
        run_lock_expiration_tests, run_management_tests, run_multi_execution_tests, run_queue_semantics_tests,
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

    #[tokio::test]
    async fn test_sqlite_atomicity() {
        let factory = SqliteTestFactory;
        run_atomicity_tests(&factory).await;
    }

    #[tokio::test]
    async fn test_sqlite_error_handling() {
        let factory = SqliteTestFactory;
        run_error_handling_tests(&factory).await;
    }

    #[tokio::test]
    async fn test_sqlite_instance_locking() {
        let factory = SqliteTestFactory;
        run_instance_locking_tests(&factory).await;
    }

    #[tokio::test]
    async fn test_sqlite_lock_expiration() {
        let factory = SqliteTestFactory;
        run_lock_expiration_tests(&factory).await;
    }

    #[tokio::test]
    async fn test_sqlite_multi_execution() {
        let factory = SqliteTestFactory;
        run_multi_execution_tests(&factory).await;
    }

    #[tokio::test]
    async fn test_sqlite_queue_semantics() {
        let factory = SqliteTestFactory;
        run_queue_semantics_tests(&factory).await;
    }

    #[tokio::test]
    async fn test_sqlite_management_capability() {
        let factory = SqliteTestFactory;
        run_management_tests(&factory).await;
    }
}
