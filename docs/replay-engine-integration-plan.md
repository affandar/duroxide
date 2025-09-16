# Replay Engine Integration Plan

## Overview

This document outlines the plan to replace the current orchestration execution logic in `run_single_execution_atomic` with the new replay engine from `replay.rs`. The goal is to unify the execution path while keeping the replay engine pure and portable.

## Current State

### Existing Architecture
- **Location**: `src/runtime/execution.rs::run_single_execution_atomic`
- **Current Flow**:
  1. Creates `OrchestrationTurn` instance
  2. Calls `prep_completions()` with incoming messages
  3. Calls `execute_orchestration()` which internally uses `completion_aware_futures::run_turn_with_completion_map`
  4. Processes the `TurnResult` to generate work items

### New Replay Engine
- **Location**: `src/runtime/replay.rs`
- **Key Components**:
  - `replay_orchestration()` - Main entry point
  - `ReplayOrchestrationContext` - Wrapper that intercepts scheduling
  - `ReplayHistoryEvent` - Events with monotonic IDs
  - Built-in determinism checking and schedule order verification

## Design Principles

1. **Keep replay engine pure** - No dependencies on runtime-specific types (`OrchestratorMsg`, `WorkItem`, etc.)
2. **All conversions in integration layer** - Keep conversion logic in `execution.rs`
3. **Maintain portability** - Replay engine should be extractable to other workflow systems
4. **Preserve existing behavior** - Same outputs, error messages, and side effects
5. **Fail on conversion errors** - Don't ignore failed conversions; these reveal corner cases that need handling

## Implementation Plan

### Phase 1: Make Replay Engine Accessible

**File**: `src/runtime/replay.rs`

1. Change visibility to `pub(crate)` for:
   ```rust
   pub(crate) struct ReplayOutput<O> { ... }
   pub(crate) fn replay_orchestration<F, Fut, O>(...) -> Result<ReplayOutput<O>, String>
   ```

2. **File**: `src/runtime/replay_context_wrapper.rs`
   
   Add conversion method:
   ```rust
   impl ReplayOrchestrationContext {
       pub(crate) fn into_orchestration_context(self) -> OrchestrationContext {
           self.inner
       }
   }
   ```

### Phase 2: Create Conversion Layer

**File**: `src/runtime/execution.rs`

Add the following conversion functions:

#### 2.1 Message to Event Conversion
```rust
/// Convert OrchestratorMsg to Event
/// Returns error for any unhandled cases to catch corner cases
fn orchestrator_msg_to_event(msg: &OrchestratorMsg) -> Result<Event, String> {
    match msg {
        OrchestratorMsg::ActivityCompleted { id, result, .. } => 
            Ok(Event::ActivityCompleted { id: *id, result: result.clone() }),
        
        OrchestratorMsg::ActivityFailed { id, error, .. } => 
            Ok(Event::ActivityFailed { id: *id, error: error.clone() }),
        
        OrchestratorMsg::TimerFired { id, fire_at_ms, .. } => 
            Ok(Event::TimerFired { id: *id, fire_at_ms: *fire_at_ms }),
        
        OrchestratorMsg::SubOrchCompleted { id, result, .. } => 
            Ok(Event::SubOrchestrationCompleted { id: *id, result: result.clone() }),
        
        OrchestratorMsg::SubOrchFailed { id, error, .. } => 
            Ok(Event::SubOrchestrationFailed { id: *id, error: error.clone() }),
        
        OrchestratorMsg::ExternalByName { .. } => {
            // External events need special handling - will be processed separately
            Err("External events require special handling".to_string())
        }
        
        OrchestratorMsg::CancelRequested { reason, .. } => 
            Ok(Event::OrchestrationCancelRequested { reason: reason.clone() }),
    }
}
```

#### 2.2 External Event ID Resolution
```rust
/// Find the correlation ID for an external event by name
/// Case-sensitive lookup
fn find_external_event_id(name: &str, history: &[Event]) -> Result<u64, String> {
    history.iter().rev().find_map(|e| match e {
        Event::ExternalSubscribed { id, name: n } if n == name => Some(*id),
        _ => None,
    })
    .ok_or_else(|| format!("No subscription found for external event '{}'", name))
}
```

#### 2.3 Handle Trace Activities
```rust
use tracing::{info, warn, error};

/// Check if an activity is a trace activity
fn is_trace_activity(name: &str) -> bool {
    name == crate::SYSTEM_TRACE_ACTIVITY
}

/// Convert trace activity to standard tracing
fn handle_trace_activity(input: &str) -> Result<(), String> {
    // Parse trace format: "LEVEL:message"
    let parts: Vec<&str> = input.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(format!("Invalid trace format: {}", input));
    }
    
    let (level, message) = (parts[0], parts[1]);
    match level {
        "INFO" => info!("system trace {}", message),
        "WARN" => warn!("system trace {}", message),
        "ERROR" => error!("system trace {}", message),
        "DEBUG" => tracing::debug!("system trace {}", message),
        _ => return Err(format!("Unknown trace level: {}", level)),
    }
    
    Ok(())
}

/// Filter out trace activities from messages
fn filter_trace_activities(messages: Vec<OrchestratorMsg>) -> (Vec<OrchestratorMsg>, Vec<String>) {
    let mut regular_messages = Vec::new();
    let mut trace_errors = Vec::new();
    
    for msg in messages {
        match &msg {
            OrchestratorMsg::ActivityCompleted { id, result, .. } => {
                // Check if this is a trace activity completion
                let is_trace = is_trace_activity_completion(*id, &baseline_history);
                if is_trace {
                    // Convert to standard tracing
                    if let Err(e) = handle_trace_activity(result) {
                        trace_errors.push(e);
                    }
                } else {
                    regular_messages.push(msg);
                }
            }
            _ => regular_messages.push(msg),
        }
    }
    
    (regular_messages, trace_errors)
}
```

#### 2.4 Build Replay History
```rust
use crate::runtime::event_ids::ReplayHistoryEvent;

/// Convert runtime history and messages to replay format
/// Fails on any conversion errors to catch corner cases
fn build_replay_history(
    baseline_history: &[Event], 
    completion_messages: &[OrchestratorMsg]
) -> Result<(Vec<ReplayHistoryEvent>, Vec<ReplayHistoryEvent>), String> {
    // Convert baseline history with monotonic IDs
    let baseline_replay: Vec<ReplayHistoryEvent> = baseline_history
        .iter()
        .enumerate()
        .map(|(idx, event)| {
            let event_id = (idx + 1) as u64;
            let scheduled_event_id = find_scheduled_event_id(event, baseline_history);
            ReplayHistoryEvent {
                event: event.clone(),
                event_id,
                scheduled_event_id,
            }
        })
        .collect();
    
    // Convert completion messages to delta events
    let mut delta_events = Vec::new();
    let mut event_id = (baseline_history.len() + 1) as u64;
    
    for msg in completion_messages {
        let (event, scheduled_id) = match msg {
            OrchestratorMsg::ExternalByName { name, data, .. } => {
                // Special handling for external events with proper error propagation
                let id = find_external_event_id(name, baseline_history)?;
                (
                    Event::ExternalEvent { 
                        id, 
                        name: name.clone(), 
                        data: data.clone() 
                    },
                    Some(id)
                )
            }
            _ => {
                let event = orchestrator_msg_to_event(msg)?;
                let scheduled_id = extract_scheduled_id_from_msg(msg);
                (event, scheduled_id)
            }
        };
        
        delta_events.push(ReplayHistoryEvent {
            event,
            event_id,
            scheduled_event_id: scheduled_id,
        });
        event_id += 1;
    }
    
    Ok((baseline_replay, delta_events))
}

/// Extract the scheduled event ID from a completion message
fn extract_scheduled_id_from_msg(msg: &OrchestratorMsg) -> Option<u64> {
    match msg {
        OrchestratorMsg::ActivityCompleted { id, .. } |
        OrchestratorMsg::ActivityFailed { id, .. } |
        OrchestratorMsg::TimerFired { id, .. } |
        OrchestratorMsg::SubOrchCompleted { id, .. } |
        OrchestratorMsg::SubOrchFailed { id, .. } => Some(*id),
        OrchestratorMsg::CancelRequested { .. } => None,
        OrchestratorMsg::ExternalByName { .. } => None, // Handled separately
    }
}

/// Find the scheduled_event_id for completion events in history
fn find_scheduled_event_id(event: &Event, history: &[Event]) -> Option<u64> {
    match event {
        Event::ActivityCompleted { id, .. } |
        Event::ActivityFailed { id, .. } |
        Event::TimerFired { id, .. } |
        Event::SubOrchestrationCompleted { id, .. } |
        Event::SubOrchestrationFailed { id, .. } |
        Event::ExternalEvent { id, .. } => Some(*id),
        _ => None,
    }
}
```

#### 2.5 Result Conversion
```rust
/// Convert replay output to TurnResult
fn convert_replay_to_turn_result(
    output: ReplayOutput<Result<String, String>>,
    pending_actions: &[Action]
) -> TurnResult {
    // Check for non-determinism error first
    if let Some(err) = output.non_determinism_error {
        return TurnResult::Failed(err);
    }
    
    // Check for continue-as-new in decisions
    for action in pending_actions {
        if let Action::ContinueAsNew { input, version } = action {
            return TurnResult::ContinueAsNew {
                input: input.clone(),
                version: version.clone(),
            };
        }
    }
    
    // Check orchestration output
    if let Some(result) = output.output {
        match result {
            Ok(output) => TurnResult::Completed(output),
            Err(error) => TurnResult::Failed(error),
        }
    } else {
        TurnResult::Continue
    }
}

/// Extract history delta from replay output
fn extract_history_delta(
    baseline_len: usize,
    final_history: &[Event]
) -> Vec<Event> {
    if final_history.len() > baseline_len {
        final_history[baseline_len..].to_vec()
    } else {
        Vec::new()
    }
}
```

### Phase 3: Replace `run_single_execution_atomic`

Replace the entire OrchestrationTurn logic with replay engine (keep old code as reference until complete):

```rust
pub async fn run_single_execution_atomic(
    self: Arc<Self>,
    instance: &str,
    orchestration_name: &str,
    initial_history: Vec<Event>,
    execution_id: u64,
    completion_messages: Vec<OrchestratorMsg>,
) -> (Vec<Event>, Vec<WorkItem>, Vec<WorkItem>, Vec<WorkItem>, Result<String, String>) {
    // ... existing setup code ...
    
    // Extract current execution history
    let current_execution_history = Self::extract_current_execution_history(&initial_history);
    
    // Filter messages for current execution
    let filtered_messages: Vec<OrchestratorMsg> = completion_messages
        .into_iter()
        .filter(|msg| is_for_current_execution(msg, execution_id))
        .collect();
    
    // Filter out trace activities and convert to standard tracing
    let (regular_messages, trace_errors) = filter_trace_activities(filtered_messages);
    
    // Log any trace conversion errors
    for err in trace_errors {
        tracing::warn!("Trace activity conversion error: {}", err);
    }
    
    // Convert to replay format - fail on any conversion errors
    let (baseline_replay, delta_replay) = match build_replay_history(
        &current_execution_history,
        &regular_messages
    ) {
        Ok(result) => result,
        Err(e) => {
            // Conversion error indicates a corner case we need to handle
            let error_msg = format!("History conversion error: {}", e);
            let error_event = Event::OrchestrationFailed { error: error_msg.clone() };
            return (vec![error_event], vec![], vec![], vec![], Err(error_msg));
        }
    };
    
    // Create orchestration function adapter
    let handler_clone = handler.clone();
    let input_clone = input.clone();
    let orch_fn = |ctx: ReplayOrchestrationContext| {
        let h = handler_clone.clone();
        let inp = input_clone.clone();
        async move { 
            // Convert ReplayOrchestrationContext to OrchestrationContext
            let regular_ctx = ctx.into_orchestration_context();
            h.invoke(regular_ctx, inp).await 
        }
    };
    
    // Execute via replay engine
    let replay_result = match replay_orchestration(orch_fn, baseline_replay, delta_replay) {
        Ok(result) => result,
        Err(e) => {
            let error_event = Event::OrchestrationFailed { error: e.clone() };
            return (vec![error_event], vec![], vec![], vec![], Err(e));
        }
    };
    
    // Extract history delta
    let history_delta = extract_history_delta(
        current_execution_history.len(),
        &replay_result.final_history // Need to add this to ReplayOutput
    );
    
    // Convert to turn result
    let turn_result = convert_replay_to_turn_result(replay_result, &replay_result.decisions);
    
    // Process turn result (existing code remains the same)
    match turn_result {
        // ... existing match arms ...
    }
}
```

### Phase 4: Testing Strategy

1. **Unit Tests for Conversions**
   ```rust
   #[test]
   fn test_conversion_errors_are_caught() {
       // Test that external events without proper handling fail
       let msg = OrchestratorMsg::ExternalByName { 
           name: "test".to_string(), 
           data: "data".to_string(),
           // ... 
       };
       
       let result = orchestrator_msg_to_event(&msg);
       assert!(result.is_err());
       assert!(result.unwrap_err().contains("External events require special handling"));
   }
   
   #[test]
   fn test_missing_external_subscription() {
       let history = vec![];
       let result = find_external_event_id("missing", &history);
       assert!(result.is_err());
       assert_eq!(result.unwrap_err(), "No subscription found for external event 'missing'");
   }
   ```

2. **Integration Tests**
   - Test trace activity conversion to standard tracing
   - Verify all error paths are handled
   - Test edge cases that might fail conversion

3. **Cleanup Strategy**
   - Once all tests pass, remove `OrchestrationTurn` completely
   - Remove `completion_aware_futures::run_turn_with_completion_map` if unused
   - Keep replay engine as the single execution path

## Error Handling

### Conversion Errors
All conversion functions return `Result<T, String>` to catch corner cases:
- Missing external event subscriptions
- Invalid trace activity formats
- Unexpected message types
- Execution ID mismatches

### Nondeterminism Errors
The replay engine produces specific error messages that must match test expectations:
- "Non-determinism detected: ActivityScheduled event found in history, but orchestration did not call schedule_activity()"
- "Non-determinism detected: ActivityScheduled mismatch. queued=(name1,input1) vs history=(name2,input2)"

### Trace Activity Handling
During this refactor:
- Trace activities are converted to standard `tracing` calls
- Invalid trace formats produce errors
- Later: convert back to replay-aware activities

## Migration Steps

1. **Implement conversion functions** with strict error handling
2. **Add unit tests** for all conversion edge cases
3. **Replace `run_single_execution_atomic`** implementation
4. **Run all existing tests** to verify behavior
5. **Remove old code** once tests pass
6. **Update documentation**

## Notes for Implementers

1. **No Feature Flags**: Directly replace the old implementation, keeping it only as reference
2. **Fail Fast**: Any conversion error should fail the execution to catch corner cases
3. **Trace Activities**: Convert to standard tracing for now
4. **Case Sensitivity**: External event lookups are case-sensitive
5. **Test Everything**: Every error path needs a test case

## Questions Resolved

1. ~~Should we add `final_history` to `ReplayOutput` or reconstruct it?~~ → Add it to ReplayOutput
2. ~~How to handle trace activities?~~ → Convert to standard tracing during refactor
3. ~~Should external event ID lookup be case-sensitive?~~ → Yes, case-sensitive
4. ~~Feature flags for gradual migration?~~ → No, direct replacement

---

This plan provides a clear path to integrate the replay engine with strict error handling and no compromise on corner cases.