use std::sync::Arc;

use crate::{Event, Action, LogLevel};
use crate::runtime::OrchestrationHandler;

/// Decisions emitted by the replay engine for the host to materialize via
/// provider queues and history persistence. These are pure and carry no
/// side-effects; the runtime applies them deterministically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    ScheduleActivity { id: u64, name: String, input: String },
    CreateTimer { id: u64, delay_ms: u64 },
    SubscribeExternal { id: u64, name: String },
    StartDetached { id: u64, name: String, version: Option<String>, instance: String, input: String },
    StartSubOrch { id: u64, name: String, version: Option<String>, child_suffix: String, input: String },
    ContinueAsNew { input: String, version: Option<String> },
}

pub trait ReplayEngine: Send + Sync {
    /// Replays one turn and returns updated history, pure decisions, logs,
    /// optional output, and claimed ids snapshot for diagnostics.
    fn replay(
        &self,
        history: Vec<Event>,
        turn_index: u64,
        handler: Arc<dyn OrchestrationHandler>,
        input: String,
    ) -> (Vec<Event>, Vec<Decision>, Vec<(LogLevel, String)>, Option<Result<String, String>>, crate::ClaimedIdsSnapshot);
}

pub struct DefaultReplayEngine;

impl DefaultReplayEngine {
    pub fn new() -> Self { Self }
}

impl ReplayEngine for DefaultReplayEngine {
    fn replay(
        &self,
        history: Vec<Event>,
        turn_index: u64,
        handler: Arc<dyn OrchestrationHandler>,
        input: String,
    ) -> (Vec<Event>, Vec<Decision>, Vec<(LogLevel, String)>, Option<Result<String, String>>, crate::ClaimedIdsSnapshot) {
        // Adapt the existing replay core; keep it pure and map Actions -> Decisions
        let orchestrator = |ctx: crate::OrchestrationContext| {
            let h = handler.clone();
            let inp = input.clone();
            async move { h.invoke(ctx, inp).await }
        };
        let (hist_after, actions, logs, out_opt, claims) = crate::run_turn_with_claims(history, turn_index, orchestrator);
        let decisions: Vec<Decision> = actions
            .into_iter()
            .map(|a| match a {
                Action::CallActivity { id, name, input } => Decision::ScheduleActivity { id, name, input },
                Action::CreateTimer { id, delay_ms } => Decision::CreateTimer { id, delay_ms },
                Action::WaitExternal { id, name } => Decision::SubscribeExternal { id, name },
                Action::StartOrchestrationDetached { id, name, version, instance, input } =>
                    Decision::StartDetached { id, name, version, instance, input },
                Action::StartSubOrchestration { id, name, version, instance, input } =>
                    Decision::StartSubOrch { id, name, version, child_suffix: instance, input },
                Action::ContinueAsNew { input, version } => Decision::ContinueAsNew { input, version },
            })
            .collect();
        (hist_after, decisions, logs, out_opt, claims)
    }
}


