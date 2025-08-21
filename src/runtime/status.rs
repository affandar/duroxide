use crate::{Event};

impl super::Runtime {
    /// Returns the current status of an orchestration instance by inspecting its history.
    pub async fn get_orchestration_status(&self, instance: &str) -> super::OrchestrationStatus {
        let hist = self.history_store.read(instance).await;
        if hist.is_empty() {
            return super::OrchestrationStatus::NotFound;
        }
        for e in hist.iter().rev() {
            match e {
                Event::OrchestrationFailed { error } => return super::OrchestrationStatus::Failed { error: error.clone() },
                Event::OrchestrationCompleted { output } => return super::OrchestrationStatus::Completed { output: output.clone() },
                _ => {}
            }
        }
        super::OrchestrationStatus::Running
    }

    /// Return status for a specific execution. Currently single-execution only; `execution_id` is ignored.
    pub async fn get_orchestration_status_with_execution(&self, instance: &str, _execution_id: u64) -> super::OrchestrationStatus {
        self.get_orchestration_status(instance).await
    }

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


