# Create Scenario Test from Orchestration

## Objective
Generate a regression test file in `tests/scenarios/` that models a real-world orchestration pattern. The test should validate complex orchestration behavior, edge cases, and long-running patterns discovered in production usage.

## Input
Point to an orchestration implementation (Duroxide, Temporal, or Durable Tasks) that you want to convert into a scenario test.

## Output
Create a new test file in `tests/scenarios/` following the established pattern.

## Test File Structure

### File Location
- **Path**: `tests/scenarios/<descriptive_name>.rs`
- **Module**: Declared in `tests/scenarios.rs` using `#[path = "scenarios/<name>.rs"]`

### Source Attribution and Porting Notes

**CRITICAL**: Every scenario test must include:

1. **Source URL/Comment**: Include the exact source location and original code
2. **Porting Status**: Document what was ported completely, what required workarounds, and what couldn't be ported

Add this at the top of each test function:

```rust
/// Regression test derived from real-world usage pattern: [Pattern Name]
/// 
/// **Source**: [URL or file path to original implementation]
/// 
/// **Original Code**:
/// ```[language]
/// [paste exact source code here]
/// ```
/// 
/// **Porting Notes**:
/// - ✅ **Ported completely**: [list features that map 1:1]
/// - ⚠️ **Workarounds used**: [list features that required workarounds and why]
/// - ❌ **Not portable**: [list features that couldn't be ported and why]
/// 
/// This test validates: [list what it validates]
```

### File Template

```rust
use duroxide::runtime;
use duroxide::runtime::registry::ActivityRegistry;
use duroxide::{Event, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus};
use std::sync::Arc;
use std::time::Duration;

#[path = "../common/mod.rs"]
mod common;

/// Regression test derived from real-world usage pattern: [Pattern Name]
/// 
/// **Source**: [URL or file path to original implementation]
/// 
/// **Original Code**:
/// ```[language]
/// [paste exact source code here]
/// ```
/// 
/// **Porting Notes**:
/// - ✅ **Ported completely**: [list features that map 1:1]
/// - ⚠️ **Workarounds used**: [list features that required workarounds and why]
/// - ❌ **Not portable**: [list features that couldn't be ported and why]
/// 
/// This test validates: [list what it validates]
#[tokio::test]
async fn test_descriptive_name() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    // 1. Define activity types (if needed)
    #[derive(serde::Serialize, serde::Deserialize, Clone)]
    struct ActivityInput {
        // ... fields
    }

    #[derive(serde::Serialize, serde::Deserialize, Clone)]
    struct ActivityOutput {
        // ... fields
    }

    // 2. Define orchestration input/output types
    #[derive(serde::Serialize, serde::Deserialize, Clone)]
    struct OrchestrationInput {
        // ... fields
    }

    // 3. Register mock activities
    // Use register() for simple string-based activities
    let activity1 = |_ctx: duroxide::ActivityContext, input: String| async move {
        Ok(format!("processed-{}", input))
    };

    // Use register_typed() for typed activities with struct inputs/outputs
    let activity2 = |_ctx: duroxide::ActivityContext, input: String| async move {
        let parsed: ActivityInput = serde_json::from_str(&input)
            .map_err(|e| format!("Parse error: {}", e))?;
        
        // Mock implementation
        let output = ActivityOutput {
            // ... populate output
        };
        
        serde_json::to_string(&output).map_err(|e| format!("Serialize error: {}", e))
    };

    // 4. Define orchestration (model the pattern from source)
    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        let mut input_data: OrchestrationInput = serde_json::from_str(&input)
            .map_err(|e| format!("Failed to parse input: {}", e))?;

        ctx.trace_info(format!("Orchestration iteration {}: ...", input_data.iteration));

        // Model the orchestration logic here
        // - Schedule activities
        // - Handle timers
        // - Process results
        // - Continue-as-new if needed

        Ok("completed".to_string())
    };

    // 5. Build registries
    let orchestrations = OrchestrationRegistry::builder()
        .register("OrchestrationName", orchestration)
        .build();

    let activities = ActivityRegistry::builder()
        .register("Activity1", activity1)  // Simple string-based activity
        .register_typed("Activity2", activity2)  // Typed activity with structs
        // ... register more activities
        .build();

    // 6. Start runtime
    let rt = runtime::Runtime::start_with_store(
        store.clone(),
        Arc::new(activities),
        orchestrations,
    )
    .await;

    let client = duroxide::Client::new(store.clone());

    // 7. Start orchestration instance(s)
    let input = OrchestrationInput {
        // ... populate input
    };
    let input_json = serde_json::to_string(&input).unwrap();

    client
        .start_orchestration("test-instance-1", "OrchestrationName", &input_json)
        .await
        .unwrap();

    // 8. Wait for completion
    let status = client
        .wait_for_orchestration("test-instance-1", Duration::from_secs(30))
        .await
        .unwrap();

    // 9. Assertions
    match status {
        OrchestrationStatus::Completed { output } => {
            assert_eq!(output, "expected output");
            tracing::info!("✓ Test passed");
        }
        OrchestrationStatus::Failed { details } => {
            eprintln!("\n❌ Orchestration failed: {}\n", details.display_message());
            eprintln!("=== DUMPING EXECUTION HISTORIES ===\n");
            
            // Dump all execution histories on failure
            let mut exec_id = 1;
            loop {
                match client.read_execution_history("test-instance-1", exec_id).await {
                    Ok(hist) if !hist.is_empty() => {
                        eprintln!("--- Execution {} ---", exec_id);
                        eprintln!("Events: {}", hist.len());
                        for (idx, event) in hist.iter().enumerate() {
                            let event_json = serde_json::to_string_pretty(event)
                                .unwrap_or_else(|_| format!("{:?}", event));
                            eprintln!("  Event {}: {}", idx + 1, event_json);
                        }
                        eprintln!();
                        exec_id += 1;
                    }
                    _ => break,
                }
                
                if exec_id > 100 {
                    eprintln!("(stopping dump at execution 100)");
                    break;
                }
            }
            
            eprintln!("=== END OF HISTORY DUMP ===\n");
            panic!("Orchestration failed: {}", details.display_message());
        }
        _ => panic!("Unexpected status: {:?}", status),
    }

    // 10. Verify execution history (if needed)
    for exec_id in 1..=expected_execution_count {
        let hist = client
            .read_execution_history("test-instance-1", exec_id)
            .await
            .unwrap();

        // Verify expected events
        assert!(
            hist.iter().any(|e| matches!(e, Event::OrchestrationStarted { .. })),
            "Execution {} missing OrchestrationStarted",
            exec_id
        );

        // Verify version consistency
        if let Some(Event::OrchestrationStarted { version, .. }) =
            hist.iter().find(|e| matches!(e, Event::OrchestrationStarted { .. }))
        {
            assert!(
                version.starts_with("1."),
                "Execution {} has unexpected version: {}",
                exec_id,
                version
            );
        }

        // Verify terminal event
        let last = hist.last().unwrap();
        if exec_id < expected_execution_count {
            assert!(
                matches!(last, Event::OrchestrationContinuedAsNew { .. }),
                "Execution {} should end with ContinuedAsNew",
                exec_id
            );
        } else {
            assert!(
                matches!(last, Event::OrchestrationCompleted { .. }),
                "Execution {} should end with Completed",
                exec_id
            );
        }
    }

    rt.shutdown(None).await;
}
```

## Conversion Guidelines

### From Duroxide Orchestrations

**Direct mapping:**
- `ctx.schedule_activity()` → `ctx.schedule_activity().into_activity().await`
- `ctx.schedule_timer()` → `ctx.schedule_timer().into_timer().await`
- `ctx.continue_as_new()` → `ctx.continue_as_new()`
- `ctx.trace_*()` → `ctx.trace_*()` (already replay-safe)

**Test setup:**
- Extract activity implementations as mock functions
- Use typed activities if source uses typed APIs
- Preserve input/output types

### From Temporal Workflows

**Mapping Temporal → Duroxide:**
- `workflow.ExecuteActivity()` → `ctx.schedule_activity().into_activity().await`
- `workflow.Sleep()` → `ctx.schedule_timer().into_timer().await`
- `workflow.ContinueAsNew()` → `ctx.continue_as_new()`
- `workflow.WaitCondition()` → `ctx.schedule_wait().into_event().await`
- `workflow.ExecuteChildWorkflow()` → `ctx.schedule_sub_orchestration().into_sub_orchestration().await`
- `workflow.GetLogger()` → `ctx.trace_*()` methods

**Key differences:**
- Temporal uses `workflow.Context`, Duroxide uses `OrchestrationContext`
- Temporal activities are registered separately, Duroxide uses `ActivityRegistry`
- Temporal uses `workflow.Now()`, Duroxide uses deterministic system calls

### From Durable Tasks (.NET)

**Mapping DTF → Duroxide:**
- `context.CallActivityAsync()` → `ctx.schedule_activity().into_activity().await`
- `context.CreateTimer()` → `ctx.schedule_timer().into_timer().await`
- `context.ContinueAsNew()` → `ctx.continue_as_new()`
- `context.WaitForExternalEvent()` → `ctx.schedule_wait().into_event().await`
- `context.CallSubOrchestratorAsync()` → `ctx.schedule_sub_orchestration().into_sub_orchestration().await`
- `context.GetInput<T>()` → Parse from `input: String` parameter

**Key differences:**
- DTF uses strongly-typed inputs, Duroxide uses JSON strings
- DTF activities return typed results, Duroxide returns `Result<String, String>`
- DTF uses `ILogger`, Duroxide uses `ctx.trace_*()` methods

## Test Design Principles

### 1. Keep Tests Fast
- **Timers**: Use 50ms instead of 30s for unit tests
- **Iterations**: Limit to 5-10 iterations instead of 50+
- **Timeout**: Use 30s max wait time

### 2. Model Real Patterns
- **Don't simplify**: Keep the complexity that makes it a real scenario
- **Preserve logic**: Maintain the business logic flow
- **Mock activities**: Use simple mocks that return deterministic results

### 3. Comprehensive Assertions
- **Status verification**: Check `Completed` vs `Failed`
- **Execution history**: Verify expected number of executions
- **Event validation**: Check for expected events (Started, Completed, ContinuedAsNew)
- **Version consistency**: Verify version resolution across executions
- **Failure dumps**: Include full history dump on failure for debugging

### 4. Handle Edge Cases
- **Continue-as-new chains**: Test long chains (5+ executions)
- **Concurrent instances**: Test multiple instances running in parallel
- **Activity failures**: Test error handling paths
- **Timer expiration**: Test timer behavior

## Example: Converting a Temporal Workflow

**Source (Temporal Go):**
```go
func MyWorkflow(ctx workflow.Context, input MyInput) (MyOutput, error) {
    logger := workflow.GetLogger(ctx)
    logger.Info("Starting workflow")
    
    result1, err := workflow.ExecuteActivity(ctx, Activity1, input.Value).Get(ctx, nil)
    if err != nil {
        return MyOutput{}, err
    }
    
    workflow.Sleep(ctx, 5*time.Second)
    
    result2, err := workflow.ExecuteActivity(ctx, Activity2, result1).Get(ctx, nil)
    if err != nil {
        return MyOutput{}, err
    }
    
    return MyOutput{Result: result2}, nil
}
```

**Converted Duroxide Test:**
```rust
/// Regression test derived from real-world usage pattern: Sequential Activity Chain
/// 
/// **Source**: https://github.com/temporalio/samples-go/blob/main/workflow/example.go
/// 
/// **Original Code**:
/// ```go
/// func MyWorkflow(ctx workflow.Context, input MyInput) (MyOutput, error) {
///     logger := workflow.GetLogger(ctx)
///     logger.Info("Starting workflow")
///     
///     result1, err := workflow.ExecuteActivity(ctx, Activity1, input.Value).Get(ctx, nil)
///     if err != nil {
///         return MyOutput{}, err
///     }
///     
///     workflow.Sleep(ctx, 5*time.Second)
///     
///     result2, err := workflow.ExecuteActivity(ctx, Activity2, result1).Get(ctx, nil)
///     if err != nil {
///         return MyOutput{}, err
///     }
///     
///     return MyOutput{Result: result2}, nil
/// }
/// ```
/// 
/// **Porting Notes**:
/// - ✅ **Ported completely**: 
///   - Activity execution (`ExecuteActivity` → `schedule_activity().into_activity().await`)
///   - Sequential flow (activities execute in order)
///   - Error handling (Go error → Rust Result)
///   - Logging (`GetLogger().Info()` → `ctx.trace_info()`)
/// - ⚠️ **Workarounds used**: 
///   - Timer duration reduced from 5s to 50ms for unit test speed
///   - Strongly-typed inputs converted to JSON strings (Duroxide uses JSON serialization)
///   - Activity registration moved to `ActivityRegistry` (Temporal registers separately)
/// - ❌ **Not portable**: 
///   - None (all features supported)
/// 
/// This test validates: Sequential activity execution with timer delays
#[tokio::test]
async fn test_my_workflow_pattern() {
    let (store, _td) = common::create_sqlite_store_disk().await;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct MyInput {
        value: String,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct MyOutput {
        result: String,
    }

    // Simple string-based activity
    let activity1 = |_ctx: ActivityContext, input: String| async move {
        let parsed: MyInput = serde_json::from_str(&input)?;
        Ok(format!("processed-{}", parsed.value))
    };

    // Simple string-based activity (no parsing needed)
    let activity2 = |_ctx: ActivityContext, input: String| async move {
        Ok(format!("final-{}", input))
    };

    let orchestration = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("Starting workflow");
        
        let input_data: MyInput = serde_json::from_str(&input)?;
        
        // For typed activities, use schedule_activity_typed
        let result1 = ctx
            .schedule_activity_typed::<MyInput, String>(
                "Activity1",
                &input_data
            )
            .into_activity_typed::<String>()
            .await?;
        
        // Or for simple string-based activities:
        // let result1 = ctx
        //     .schedule_activity("Activity1", serde_json::to_string(&input_data)?)
        //     .into_activity()
        //     .await?;
        
        ctx.schedule_timer(50).into_timer().await; // 50ms instead of 5s
        
        let result2 = ctx
            .schedule_activity("Activity2", result1)
            .into_activity()
            .await?;
        
        let output = MyOutput { result: result2 };
        Ok(serde_json::to_string(&output)?)
    };

    // ... rest of test setup
}
```

## Validation Checklist

Before finalizing the test:

- [ ] **Source attribution**: Original code included in comments with URL/file path
- [ ] **Porting notes**: Documented what was ported completely, workarounds used, and what couldn't be ported
- [ ] Test compiles: `cargo test --test scenarios test_name`
- [ ] Test runs successfully: `cargo test --test scenarios test_name -- --nocapture`
- [ ] All assertions pass
- [ ] Execution history verification works
- [ ] Failure dumps are helpful (test by forcing a failure)
- [ ] Test completes in < 5 seconds
- [ ] Test follows naming convention: `test_<descriptive_name>`
- [ ] Test has doc comment explaining the pattern it models
- [ ] Test is added to `tests/scenarios.rs` module declaration

## Common Patterns to Model

1. **Instance Actor Pattern**: Long-running orchestration that manages a single resource
   - Health checks
   - Periodic updates
   - Continue-as-new loops
   - Multiple activities per iteration

2. **Saga Pattern**: Multi-step transaction with compensation
   - Sequential activities
   - Error handling
   - Rollback logic

3. **Fan-out/Fan-in**: Parallel processing with aggregation
   - Multiple parallel activities
   - Result aggregation
   - Error handling

4. **Human-in-the-loop**: External event waiting
   - Wait for external events
   - Timeout handling
   - Event processing

5. **Long-running Chains**: Continue-as-new patterns
   - State preservation
   - Version consistency
   - Multiple executions

## Porting Documentation Guidelines

### What to Document

1. **✅ Ported Completely**: Features that map 1:1 with no changes
   - Example: "Activity execution (`ExecuteActivity` → `schedule_activity().into_activity().await`)"
   - Example: "Error handling (Go error → Rust Result)"

2. **⚠️ Workarounds Used**: Features that work but require different approaches
   - Example: "Timer duration reduced from 5s to 50ms for unit test speed"
   - Example: "Strongly-typed inputs converted to JSON strings (Duroxide uses JSON serialization)"
   - Example: "Activity registration moved to `ActivityRegistry` (Temporal registers separately)"
   - Always explain WHY the workaround was needed

3. **❌ Not Portable**: Features that couldn't be ported
   - Example: "Custom retry policies (Duroxide uses fixed retry logic)"
   - Example: "Activity heartbeats (not supported in Duroxide)"
   - Always explain WHY it couldn't be ported

### Format for Porting Notes

```rust
/// **Porting Notes**:
/// - ✅ **Ported completely**: 
///   - Feature 1 (mapping explanation)
///   - Feature 2 (mapping explanation)
/// - ⚠️ **Workarounds used**: 
///   - Feature 3 (workaround explanation and why)
///   - Feature 4 (workaround explanation and why)
/// - ❌ **Not portable**: 
///   - Feature 5 (reason why not portable)
///   - Feature 6 (reason why not portable)
```

## Ask Before Creating

If the orchestration:
- Requires external services (databases, APIs)
- Has complex dependencies
- Uses features not yet supported in Duroxide
- Would require significant refactoring
- Has many "Not portable" items (>3)

Then summarize the conversion challenges and ask for guidance before proceeding.

