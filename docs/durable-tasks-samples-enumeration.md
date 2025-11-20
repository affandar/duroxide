# Durable Tasks Framework Samples Enumeration

## Repository
**Source**: https://github.com/Azure/durabletask

## Sample Orchestrations

### 1. **Average Calculator** (`AverageCalculator`)
**Location**: `test/DurableTask.Samples.Tests/AverageCalculatorTests.cs`
**Description**: Simple orchestration that computes average by calling sum activity
**Features**:
- Single activity invocation
- Array input handling
- Simple aggregation (sum / count)
- Input validation

**Key Concepts**:
- Basic activity invocation
- Input validation
- Result computation
- Error handling

**Portability to Duroxide**: ✅ High - Simple pattern, direct mapping
**Status**: ✅ Already ported (see `tests/scenarios/average_calculator.rs`)

---

### 2. **Greeting** (`GreetingOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Basic hello world pattern with activity chaining
**Features**:
- Sequential activity execution
- String concatenation
- Simple workflow pattern

**Key Concepts**:
- Sequential workflows
- Activity chaining
- String operations
- Basic orchestration

**Portability to Duroxide**: ✅ High - Direct mapping

---

### 3. **Cron** (`CronOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Scheduled/recurring workflow execution
**Features**:
- Cron-style scheduling
- Recurring execution
- Continue-as-new pattern
- Schedule expressions

**Key Concepts**:
- Scheduled workflows
- Continue-as-new
- Recurring patterns
- Timer-based execution

**Portability to Duroxide**: ⚠️ Medium - Requires external scheduler, continue-as-new supported

---

### 4. **Human Interaction** (`HumanInteractionOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Human-in-the-loop workflow with external events
**Features**:
- Wait for external events
- Approval workflows
- Timeout handling
- State management across events

**Key Concepts**:
- External events
- Timeouts
- Human-in-the-loop patterns
- Event waiting

**Portability to Duroxide**: ✅ High - `schedule_wait().into_event()` supports external events

---

### 5. **Fan Out/Fan In** (`FanOutFanInOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Parallel activity execution with result aggregation
**Features**:
- Execute multiple activities in parallel
- Aggregate results
- Error handling per activity
- Result collection

**Key Concepts**:
- Fan-out/fan-in
- Parallel execution
- Result aggregation
- Partial failure handling

**Portability to Duroxide**: ✅ High - `ctx.join()` and `ctx.select()` support parallel execution

---

### 6. **Sub Orchestration** (`SubOrchestrationOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Parent-child orchestration relationships
**Features**:
- Parent orchestration spawns child orchestrations
- Child orchestration execution
- Result aggregation from children
- Error handling in child orchestrations

**Key Concepts**:
- Sub-orchestrations
- Parent-child relationships
- Result aggregation
- Error propagation

**Portability to Duroxide**: ✅ High - `schedule_sub_orchestration()` supports this

---

### 7. **Error Handling** (`ErrorHandlingOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Comprehensive error handling patterns
**Features**:
- Activity failure handling
- Retry logic
- Error propagation
- Compensation patterns

**Key Concepts**:
- Error handling
- Retries
- Exception handling
- Failure recovery

**Portability to Duroxide**: ✅ High - Error handling well-supported

---

### 8. **Timer** (`TimerOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Timer and delay patterns
**Features**:
- Workflow delays
- Timer creation
- Timer cancellation
- Timeout handling

**Key Concepts**:
- Timers
- Delays
- Timeouts
- Cancellation

**Portability to Duroxide**: ✅ High - `schedule_timer()` fully supported

---

### 9. **Orchestration Versioning** (`VersioningOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Version management and upgrade patterns
**Features**:
- Version handling
- Upgrade scenarios
- Backward compatibility
- Version-based routing

**Key Concepts**:
- Versioning
- Upgrades
- Compatibility
- Version routing

**Portability to Duroxide**: ✅ High - Registry versioning supports this

---

### 10. **Continue As New** (`ContinueAsNewOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Long-running workflows using continue-as-new
**Features**:
- Continue-as-new pattern
- State preservation
- Long-running workflows
- Iteration management

**Key Concepts**:
- Continue-as-new
- State preservation
- Long-running workflows
- Iteration patterns

**Portability to Duroxide**: ✅ High - `continue_as_new()` fully supported

---

### 11. **Signal** (`SignalOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: External event signaling to workflows
**Features**:
- Send signals to running workflows
- Handle signals in workflow
- Signal-based state updates
- Dynamic workflow control

**Key Concepts**:
- Signals
- External events
- Dynamic control
- State updates

**Portability to Duroxide**: ✅ High - `schedule_wait().into_event()` supports external events

---

### 12. **Retry** (`RetryOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Activity retry patterns and policies
**Features**:
- Retry policies
- Exponential backoff
- Maximum attempts
- Error handling

**Key Concepts**:
- Retry policies
- Backoff strategies
- Error handling
- Resilience patterns

**Portability to Duroxide**: ⚠️ Medium - Duroxide has fixed retry logic, may need workarounds

---

### 13. **Orchestration Cancellation** (`CancellationOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Workflow cancellation patterns
**Features**:
- Cancel running workflows
- Handle cancellation tokens
- Cleanup on cancellation
- Cancellation propagation

**Key Concepts**:
- Cancellation
- Cleanup
- Token handling
- Cancellation propagation

**Portability to Duroxide**: ⚠️ Medium - Duroxide supports cancellation but patterns may differ

---

### 14. **Orchestration Status** (`StatusOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Workflow status querying and monitoring
**Features**:
- Query workflow status
- Status inspection
- Monitoring support
- State queries

**Key Concepts**:
- Status queries
- State inspection
- Monitoring
- Status tracking

**Portability to Duroxide**: ✅ High - Client API supports status queries

---

### 15. **Orchestration History** (`HistoryOrchestration`)
**Location**: `test/DurableTask.Samples.Tests/`
**Description**: Access workflow execution history
**Features**:
- Read execution history
- History inspection
- Event replay
- Debugging support

**Key Concepts**:
- History access
- Event replay
- Debugging
- State inspection

**Portability to Duroxide**: ✅ High - `read_execution_history()` supports this

---

## DTF → Duroxide Mapping Reference

### API Mappings

| Durable Tasks | Duroxide | Notes |
|--------------|----------|-------|
| `context.CallActivityAsync<T>()` | `ctx.schedule_activity().into_activity().await` | Direct mapping |
| `context.CreateTimer()` | `ctx.schedule_timer().into_timer().await` | Direct mapping |
| `context.ContinueAsNew()` | `ctx.continue_as_new()` | Direct mapping |
| `context.WaitForExternalEvent<T>()` | `ctx.schedule_wait().into_event().await` | Direct mapping |
| `context.CallSubOrchestratorAsync<T>()` | `ctx.schedule_sub_orchestration().into_sub_orchestration().await` | Direct mapping |
| `context.GetInput<T>()` | Parse from `input: String` parameter | JSON serialization required |
| `TaskOrchestrationContext` | `OrchestrationContext` | Similar API |
| `TaskActivity` | `ActivityHandler` trait | Similar pattern |
| `TaskOrchestration` | `OrchestrationHandler` trait | Similar pattern |

### Key Differences

1. **Type System**:
   - DTF: Strongly-typed inputs/outputs
   - Duroxide: JSON strings (use `schedule_activity_typed` for typed APIs)

2. **Registration**:
   - DTF: Separate registration of orchestrations and activities
   - Duroxide: `OrchestrationRegistry` and `ActivityRegistry`

3. **Error Handling**:
   - DTF: Exceptions
   - Duroxide: `Result<String, String>`

4. **Logging**:
   - DTF: `ILogger` or `TraceSource`
   - Duroxide: `ctx.trace_*()` methods (replay-safe)

---

## Summary by Portability

### ✅ High Portability (Direct Mapping)
- Average Calculator ✅ (Already ported)
- Greeting
- Human Interaction
- Fan Out/Fan In
- Sub Orchestration
- Error Handling
- Timer
- Versioning
- Continue As New
- Signal
- Status
- History

### ⚠️ Medium Portability (Workarounds Needed)
- Cron (External scheduler needed)
- Retry (Fixed retry policies)
- Cancellation (Pattern differences)

### ❌ Low Portability (Not Supported or Significant Changes)
- None identified - DTF patterns map well to Duroxide

---

## Recommended Samples for Scenario Tests

Based on complexity and portability:

1. **Average Calculator** ✅ - Already ported, simple pattern
2. **Greeting** - Simplest pattern, good for basic examples
3. **Fan Out/Fan In** - Common pattern, well-supported
4. **Human Interaction** - External events, important pattern
5. **Sub Orchestration** - Parent-child pattern
6. **Timer** - Basic but fundamental pattern
7. **Continue As New** - Long-running pattern
8. **Error Handling** - Comprehensive error patterns
9. **Versioning** - Version management patterns

---

## Porting Guidelines

### Common Patterns

1. **Activity Invocation**:
   ```csharp
   // DTF
   var result = await context.CallActivityAsync<string>("ActivityName", input);
   ```
   ```rust
   // Duroxide
   let result = ctx
       .schedule_activity("ActivityName", serde_json::to_string(&input)?)
       .into_activity()
       .await?;
   ```

2. **Typed Activities**:
   ```csharp
   // DTF
   var result = await context.CallActivityAsync<MyType>("ActivityName", input);
   ```
   ```rust
   // Duroxide
   let result = ctx
       .schedule_activity_typed::<InputType, OutputType>("ActivityName", &input)
       .into_activity_typed::<OutputType>()
       .await?;
   ```

3. **Timers**:
   ```csharp
   // DTF
   await context.CreateTimer(context.CurrentUtcDateTime.AddSeconds(5), CancellationToken.None);
   ```
   ```rust
   // Duroxide
   ctx.schedule_timer(5000).into_timer().await;
   ```

4. **External Events**:
   ```csharp
   // DTF
   var approval = await context.WaitForExternalEvent<string>("Approval");
   ```
   ```rust
   // Duroxide
   let approval = ctx.schedule_wait("Approval").into_event().await;
   ```

5. **Sub-Orchestrations**:
   ```csharp
   // DTF
   var result = await context.CallSubOrchestratorAsync<string>("ChildOrchestration", input);
   ```
   ```rust
   // Duroxide
   let result = ctx
       .schedule_sub_orchestration("ChildOrchestration", input_json)
       .into_sub_orchestration()
       .await?;
   ```

6. **Continue As New**:
   ```csharp
   // DTF
   context.ContinueAsNew(newInput);
   ```
   ```rust
   // Duroxide
   ctx.continue_as_new(new_input_json);
   ```

---

## Notes

- DTF samples use C# async/await patterns that translate well to Rust async/await
- Activity registration differs (DTF registers separately, Duroxide uses Registry)
- Error handling patterns are similar (C# exception → Rust Result)
- State management is similar (both use deterministic replay)
- External events require `schedule_wait()` in Duroxide vs `WaitForExternalEvent` in DTF
- Most DTF patterns have direct equivalents in Duroxide

---

## References

- **Repository**: https://github.com/Azure/durabletask
- **Tests**: `test/DurableTask.Samples.Tests/`
- **Documentation**: https://github.com/Azure/durabletask/tree/main/docs
- **NuGet Package**: https://www.nuget.org/packages/Microsoft.Azure.DurableTask.Core

