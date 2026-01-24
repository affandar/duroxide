# Activity Retry Policy [IMPLEMENTED]

**Status:** Implemented

## Summary

Add orchestration-level helper methods that wrap existing `schedule_activity` calls with configurable retry logic and optional per-attempt timeout. Retries are implemented entirely in orchestration code using existing primitives (loops + timers + select), keeping history deterministic and requiring no worker/provider changes.

## Motivation

- **Current state**: Activities fail immediately; users must manually implement retry loops and timeout logic in orchestration code.
- **Goal**: Provide ergonomic `schedule_activity_with_retry()` helpers that encapsulate retry best practices (backoff, max attempts, per-attempt timeout) while remaining fully deterministic.

## Scope

### Methods to Add

| Existing Method | Retry Variant |
|-----------------|---------------|
| `schedule_activity(name, input)` | `schedule_activity_with_retry(name, input, policy)` |
| `schedule_activity_typed<In, Out>(name, input)` | `schedule_activity_with_retry_typed<In, Out>(name, input, policy)` |

### Methods NOT Covered

- `schedule_timer` — timers don't fail in a retryable way
- `schedule_wait` / `schedule_wait_typed` — external events don't fail
- `schedule_sub_orchestration*` — sub-orchestrations have their own retry/timeout semantics; users should handle at the child orchestration level
- `schedule_orchestration*` (fire-and-forget) — no result to check, no retry semantics

## API Design

### RetryPolicy

```rust
/// Retry policy for activities.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of attempts (including initial). Default: 3
    pub max_attempts: u32,
    /// Backoff strategy between retries. Default: Exponential(100ms, 2.0, 30s)
    pub backoff: BackoffStrategy,
    /// Per-attempt timeout. Each activity attempt is raced against this timeout.
    /// If timeout fires, returns error immediately (no retry). Retries only
    /// occur for activity errors, not timeouts. None = no timeout.
    pub timeout: Option<Duration>,
}

/// Backoff strategy for computing delay between retry attempts.
#[derive(Debug, Clone)]
pub enum BackoffStrategy {
    /// No delay between retries.
    None,
    /// Fixed delay between all retries.
    Fixed { delay: Duration },
    /// Linear backoff: delay = base * attempt
    Linear { base: Duration, max: Duration },
    /// Exponential backoff: delay = base * multiplier^(attempt-1), capped at max
    Exponential { base: Duration, multiplier: f64, max: Duration },
}

impl Default for BackoffStrategy {
    fn default() -> Self {
        BackoffStrategy::Exponential {
            base: Duration::from_millis(100),
            multiplier: 2.0,
            max: Duration::from_secs(30),
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff: BackoffStrategy::default(),
            timeout: None,
        }
    }
}

impl RetryPolicy {
    pub fn new(max_attempts: u32) -> Self { ... }
    /// Set per-attempt timeout. Timeouts cause immediate exit (no retry).
    pub fn with_timeout(mut self, timeout: Duration) -> Self { ... }
    pub fn with_backoff(mut self, backoff: BackoffStrategy) -> Self { ... }
    
    /// Compute delay for given attempt (1-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration { ... }
}
```

### Helper Method Signatures

```rust
impl OrchestrationContext {
    /// Schedule activity with retry policy.
    /// 
    /// **Retry behavior:**
    /// - Retries on activity **errors** up to `policy.max_attempts`
    /// - **Timeouts are NOT retried** - if any attempt times out, returns error immediately
    /// 
    /// **Timeout behavior (if `policy.timeout` is set):**
    /// - Each activity attempt is raced against the timeout
    /// - If timeout fires before activity completes → returns timeout error (no retry)
    /// - If activity fails with error before timeout → retry according to policy
    pub async fn schedule_activity_with_retry(
        &self,
        name: impl Into<String>,
        input: impl Into<String>,
        policy: RetryPolicy,
    ) -> Result<String, String>;

    /// Typed variant.
    pub async fn schedule_activity_with_retry_typed<In, Out>(
        &self,
        name: impl Into<String>,
        input: &In,
        policy: RetryPolicy,
    ) -> Result<Out, String>
    where
        In: serde::Serialize,
        Out: serde::de::DeserializeOwned;
}
```

## Implementation

### Core Logic

```rust
pub async fn schedule_activity_with_retry(
    &self,
    name: impl Into<String>,
    input: impl Into<String>,
    policy: RetryPolicy,
) -> Result<String, String> {
    let name = name.into();
    let input = input.into();
    let mut last_error = String::new();

    for attempt in 1..=policy.max_attempts {
        // Each attempt: optionally race against per-attempt timeout
        let activity_result = if let Some(timeout) = policy.timeout {
            // Race activity vs per-attempt timeout
            let deadline = self.schedule_timer(timeout);
            let activity = self.schedule_activity(&name, &input);
            let (winner, output) = self.select2(activity, deadline).await;

            match winner {
                0 => match output {
                    DurableOutput::Activity(result) => result,
                    _ => unreachable!(),
                },
                1 => {
                    // Timeout fired - exit immediately, no retry for timeouts
                    return Err("timeout: activity timed out".to_string());
                }
                _ => unreachable!(),
            }
        } else {
            // No timeout - just await the activity
            self.schedule_activity(&name, &input).into_activity().await
        };

        match activity_result {
            Ok(result) => return Ok(result),
            Err(e) => {
                // Activity failed with error - apply retry policy
                last_error = e;
                if attempt < policy.max_attempts {
                    let delay = policy.delay_for_attempt(attempt);
                    if !delay.is_zero() {
                        self.schedule_timer(delay).into_timer().await;
                    }
                }
            }
        }
    }
    Err(last_error)
}
```

### Key Points

1. **Deterministic**: Each retry attempt creates a new `ActivityScheduled` event. If timeout is set, each attempt also creates a `TimerCreated`. Backoff delays use `schedule_timer()`. All recorded in history; replay follows the same path.

2. **No worker/provider changes**: Retries and timeouts are orchestration control flow.

3. **Per-attempt timeout semantics**: The timeout applies to each individual activity attempt. If the deadline timer fires before the activity completes, returns immediately with timeout error (NO retry). Only activity errors trigger retry logic.

4. **Input cloning**: String inputs are cloned per attempt. Typed variants serialize once and reuse the payload string.

5. **Error propagation**: Returns the last activity error after exhausting attempts, or "timeout: activity timed out" if timeout fires first.

6. **Only Application errors reach orchestration**: Infrastructure and configuration errors abort the turn at the runtime layer—they never surface to orchestration code. All errors the retry helper sees are `ErrorDetails::Application`. The `Application` variant has a `retryable: bool` field; future work could respect this flag.

## History Example

Activity with 3 max attempts, 10s per-attempt timeout, exponential backoff (100ms base):

```
[1] OrchestrationStarted { ... }
[2] TimerCreated { fire_at_ms: now+10000, ... }          // attempt 1 timeout
[3] ActivityScheduled { name: "Flaky", input: "x", ... }
[4] ActivityFailed { source_event_id: 3, ... }           // attempt 1 failed (before timeout)
[5] TimerCreated { fire_at_ms: now+100, ... }            // backoff 100ms
[6] TimerFired { source_event_id: 5, ... }
[7] TimerCreated { fire_at_ms: now+10000, ... }          // attempt 2 timeout
[8] ActivityScheduled { name: "Flaky", input: "x", ... }
[9] ActivityFailed { source_event_id: 8, ... }           // attempt 2 failed (before timeout)
[10] TimerCreated { fire_at_ms: now+200, ... }           // backoff 200ms
[11] TimerFired { source_event_id: 10, ... }
[12] TimerCreated { fire_at_ms: now+10000, ... }         // attempt 3 timeout
[13] ActivityScheduled { name: "Flaky", input: "x", ... }
[14] ActivityCompleted { source_event_id: 13, result: "ok" } // attempt 3 succeeded
[15] OrchestrationCompleted { ... }
```

Note: The per-attempt timeout timers ([2], [7], [12]) were never consumed because activities completed first. In a timeout scenario, the timeout timer would fire before the activity completes.

### Timeout Scenario (Activity Too Slow)

```
[1] OrchestrationStarted { ... }
[2] TimerCreated { fire_at_ms: now+1000, ... }           // per-attempt timeout: 1s
[3] ActivityScheduled { name: "SlowActivity", ... }
[4] TimerFired { source_event_id: 2, ... }               // timeout fires first!
[5] OrchestrationCompleted { output: "Err(timeout: activity timed out)" }
```

Note: No retry happens because timeouts exit immediately.

## Usage Examples

### Simple retry (no timeout)
```rust
let result = ctx.schedule_activity_with_retry(
    "CallExternalAPI",
    request_json,
    RetryPolicy::new(3),
).await?;
```

### Retry with per-attempt timeout and custom backoff
```rust
let result = ctx.schedule_activity_with_retry(
    "CallExternalAPI",
    request_json,
    RetryPolicy::new(5)
        .with_timeout(Duration::from_secs(30)) // 30s per attempt
        .with_backoff(BackoffStrategy::Exponential {
            base: Duration::from_millis(500),
            multiplier: 2.0,
            max: Duration::from_secs(10),
        }),
).await?;
```

### Timeout only (single attempt with deadline)
```rust
let result = ctx.schedule_activity_with_retry(
    "QuickTask",
    input,
    RetryPolicy::new(1).with_timeout(Duration::from_secs(5)),
).await?;
```

## Future Work

- **Orchestration-level metrics**: Once `ctx.record_counter()` is available, retry helpers can emit `activity_retry_total`, `activity_retry_exhausted_total`, `activity_timeout_total` metrics.
- **Retry predicate**: Allow users to specify which errors are retryable via `RetryPolicy::retry_if(|err| ...)`.
- **Jitter**: Add optional jitter to backoff to avoid thundering herd.
- **Total timeout**: If needed, add a separate `total_timeout` field for bounding all attempts combined.
- **Sub-orchestration retries**: If needed, add similar helpers for sub-orchestrations.

## Alternatives Considered

1. **Worker-level retries**: Would require changes to `ActivityScheduled` event, worker dispatch loop, and provider delayed visibility. More complex, less transparent in history.

2. **Macro-based retry**: Could provide `#[retry(policy)]` attribute macro. Deferred until macro system is finalized.

3. **Total timeout (instead of per-attempt)**: Could timeout the entire retry sequence. Per-attempt timeout is simpler and prevents a single slow activity from consuming all retry time.

## Testing Plan

### Unit Tests (`tests/retry_policy_tests.rs`)

#### RetryPolicy Construction
- `test_retry_policy_default` — default values: max_attempts=3, exponential backoff, no timeout
- `test_retry_policy_builder` — chained `.with_timeout()`, `.with_backoff()` methods
- `test_retry_policy_new` — `RetryPolicy::new(n)` sets max_attempts correctly

#### BackoffStrategy::delay_for_attempt()
- `test_backoff_none` — always returns Duration::ZERO
- `test_backoff_fixed` — returns same delay for all attempts
- `test_backoff_linear` — delay = base * attempt, capped at max
- `test_backoff_exponential` — delay = base * multiplier^(attempt-1), capped at max
- `test_backoff_exponential_overflow` — large attempt numbers don't panic, cap at max

### Integration Tests (`tests/retry_integration_tests.rs`)

#### Basic Retry Behavior
- `test_activity_succeeds_first_attempt` — no retry needed, returns immediately
- `test_activity_fails_then_succeeds` — fails twice, succeeds on third attempt
- `test_activity_exhausts_all_attempts` — fails max_attempts times, returns last error
- `test_activity_single_attempt_fails` — max_attempts=1, single failure returns error

#### Backoff Timing
- `test_retry_with_fixed_backoff` — verify timer events have correct delays
- `test_retry_with_exponential_backoff` — verify delay doubles (or multiplies) each attempt
- `test_retry_with_no_backoff` — BackoffStrategy::None creates no timer events between attempts
- `test_retry_backoff_respects_max` — delay never exceeds backoff max even after many attempts

#### Total Timeout
- `test_timeout_fires_before_success` — activity slow, timeout wins, returns timeout error
- `test_timeout_fires_during_backoff` — timeout fires while waiting for backoff timer
- `test_success_before_timeout` — activity completes, timeout timer unconsumed in history
- `test_timeout_with_single_attempt` — max_attempts=1 + timeout, useful for "try once with deadline"
- `test_no_timeout_retries_until_exhausted` — total_timeout=None, retries all attempts regardless of time

#### Error Handling
- `test_returns_last_error_message` — after exhausting attempts, error message is from final attempt
- `test_timeout_error_message` — timeout returns "timeout: retry deadline exceeded"
- `test_only_application_errors_retried` — verify invariant: only app errors reach orchestration (debug_assert coverage)

#### Typed Variants
- `test_typed_activity_retry_success` — `schedule_activity_with_retry_typed` deserializes result
- `test_typed_activity_retry_failure` — typed variant returns error string on exhaustion
- `test_typed_input_serialized_once` — input serialization happens once, not per attempt

### Replay Determinism Tests (`tests/retry_replay_tests.rs`)

- `test_replay_retry_success_on_third_attempt` — capture history, replay from scratch, verify identical events
- `test_replay_timeout_scenario` — timeout history replays identically
- `test_replay_partial_then_complete` — replay midway through retries, verify continuation
- `test_replay_with_different_policy_fails` — changing policy after history exists causes nondeterminism error

### History Verification Tests

- `test_history_contains_all_activity_scheduled_events` — N attempts = N ActivityScheduled events
- `test_history_contains_backoff_timers` — N-1 backoff timers for N attempts (no timer after last failure)
- `test_history_timeout_timer_at_start` — total_timeout creates TimerCreated as first event after OrchestrationStarted
- `test_history_no_timeout_timer_when_none` — total_timeout=None creates no deadline timer

### Edge Cases

- `test_max_attempts_zero` — should panic or clamp to 1 (define behavior)
- `test_max_attempts_one_no_backoff` — single attempt, no backoff timers created
- `test_empty_activity_name` — behavior with empty string name
- `test_large_max_attempts` — 100+ attempts doesn't cause issues
- `test_very_short_timeout` — 1ms timeout fires immediately
- `test_zero_duration_backoff` — backoff delay of 0 creates no timer (or instant timer)

### Concurrency / Real Runtime Tests

- `test_retry_with_real_failing_activity` — register activity that fails N times then succeeds
- `test_retry_timeout_with_slow_activity` — activity sleeps longer than timeout
- `test_multiple_orchestrations_retrying` — concurrent orchestrations each with retry logic don't interfere

### Cross-Execution Stale Event Tests

These tests verify that stale completion events (from race losers or previous executions) are correctly dropped by the runtime and don't corrupt subsequent executions.

- `test_timeout_fires_activity_completes_after_continue_as_new` — Scenario:
  1. Orchestration starts activity with retry + timeout
  2. Timeout fires first, orchestration returns error and calls `continue_as_new()`
  3. New execution starts (execution_id=2)
  4. Meanwhile, the activity from execution_id=1 finally completes
  5. Verify: ActivityCompleted event for execution_id=1 is dropped/ignored, doesn't appear in execution_id=2 history, new execution proceeds cleanly

- `test_activity_succeeds_timeout_fires_after_continue_as_new` — Scenario:
  1. Orchestration starts activity with retry + timeout
  2. Activity succeeds first, orchestration completes and calls `continue_as_new()`
  3. New execution starts (execution_id=2)
  4. The unconsumed timeout timer from execution_id=1 finally fires
  5. Verify: TimerFired event for execution_id=1 is dropped/ignored, doesn't corrupt execution_id=2 history

- `test_multiple_stale_events_across_executions` — Scenario:
  1. Orchestration runs retry loop with timeout, times out, continues-as-new
  2. Second execution also times out, continues-as-new
  3. Third execution succeeds
  4. Various stale ActivityCompleted/TimerFired events from executions 1 and 2 arrive late
  5. Verify: All stale events dropped, execution_id=3 history clean, final result correct

- `test_stale_event_same_execution_after_select_winner` — Scenario:
  1. `select2(activity, timer)` — activity wins
  2. Orchestration proceeds to next activity
  3. The losing timer fires and TimerFired event arrives
  4. Verify: Stale TimerFired event is correctly not consumed by subsequent operations, history shows it but orchestration ignores it

- `test_replay_with_stale_events_in_history` — Scenario:
  1. Run orchestration where timeout wins race, activity completes later (both events in history)
  2. Replay from that history
  3. Verify: Replay correctly ignores the stale ActivityCompleted, produces same outcome
