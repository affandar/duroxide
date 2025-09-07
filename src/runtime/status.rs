use crate::Event;

impl super::DuroxideRuntime {
    // removed: get_orchestration_status (use DuroxideClient)

    // removed: get_orchestration_status_with_execution (use DuroxideClient)

    /// List all execution ids for an instance. Currently returns a single execution [1].
    pub async fn list_executions(&self, instance: &str) -> Vec<u64> {
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() { Vec::new() } else { vec![1] }
    }

    /// Return execution history for a specific execution id. Currently returns the single history.
    pub async fn get_execution_history(&self, instance: &str, _execution_id: u64) -> Vec<Event> {
        self.history_store.read(instance).await
    }
}
