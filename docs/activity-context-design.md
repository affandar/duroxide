# ActivityContext Design

## Problem Statement

Activities currently have no way to emit correlated logs. When an activity needs to log information, there's no mechanism to include:
- `instance_id` - Which orchestration instance is running this activity
- `execution_id` - Which execution within the instance
- `orchestration_name` - Parent orchestration name
- `activity_name` - The activity's own name
- `activity_id` - The event correlation ID

This creates a gap in observability where activity-level logs cannot be correlated with orchestration logs.

## Proposed Solution: ActivityContext

### Design Overview

Add a new `ActivityContext` type that provides:
1. **Metadata accessors** - Read-only access to instance_id, execution_id, etc.
2. **Trace helpers** - `trace_info()`, `trace_warn()`, `trace_error()`, `trace_debug()`
3. **Future extensibility** - Placeholder for cancellation tokens, progress reporting, etc.

### ActivityContext Structure

```rust
/// Context provided to activities for logging and metadata access.
///
/// Unlike OrchestrationContext, ActivityContext:
/// - Cannot schedule work (activities are leaf nodes)
/// - Does not use deterministic replay (logs are just logs)
/// - Provides read-only access to orchestration metadata
///
/// # Example
///
/// ```rust
/// async fn process_payment(ctx: ActivityContext, amount: String) -> Result<String, String> {
///     ctx.trace_info("Payment processing started");
///     ctx.trace_info(format!("Amount: ${}", amount));
///     
///     // Process payment...
///     
///     ctx.trace_info("Payment successful");
///     Ok(transaction_id)
/// }
/// ```
#[derive(Clone, Debug)]
pub struct ActivityContext {
    instance_id: String,
    execution_id: u64,
    orchestration_name: String,
    orchestration_version: String,
    activity_name: String,
    // Internal: correlation ID used for logging, not exposed to users
    activity_id: u64,
}

impl ActivityContext {
    /// Create a new activity context (internal use by runtime)
    pub(crate) fn new(
        instance_id: String,
        execution_id: u64,
        orchestration_name: String,
        orchestration_version: String,
        activity_name: String,
        activity_id: u64,
    ) -> Self {
        Self {
            instance_id,
            execution_id,
            orchestration_name,
            orchestration_version,
            activity_name,
            activity_id,
        }
    }

    // Public metadata accessors (activity_id intentionally omitted - internal only)
    
    /// Get the orchestration instance identifier.
    pub fn instance_id(&self) -> &str { &self.instance_id }
    
    /// Get the execution ID within the instance (for ContinueAsNew tracking).
    pub fn execution_id(&self) -> u64 { self.execution_id }
    
    /// Get the parent orchestration name.
    pub fn orchestration_name(&self) -> &str { &self.orchestration_name }
    
    /// Get the parent orchestration version.
    pub fn orchestration_version(&self) -> &str { &self.orchestration_version }
    
    /// Get the activity name.
    pub fn activity_name(&self) -> &str { &self.activity_name }

    // Trace helpers
    pub fn trace_info(&self, message: impl Into<String>) {
        tracing::info!(
            target: "duroxide::activity",
            instance_id = %self.instance_id,
            execution_id = %self.execution_id,
            orchestration_name = %self.orchestration_name,
            orchestration_version = %self.orchestration_version,
            activity_name = %self.activity_name,
            activity_id = %self.activity_id,
            "{}", message.into()
        );
    }

    pub fn trace_warn(&self, message: impl Into<String>) {
        tracing::warn!(
            target: "duroxide::activity",
            instance_id = %self.instance_id,
            execution_id = %self.execution_id,
            orchestration_name = %self.orchestration_name,
            orchestration_version = %self.orchestration_version,
            activity_name = %self.activity_name,
            activity_id = %self.activity_id,
            "{}", message.into()
        );
    }

    pub fn trace_error(&self, message: impl Into<String>) {
        tracing::error!(
            target: "duroxide::activity",
            instance_id = %self.instance_id,
            execution_id = %self.execution_id,
            orchestration_name = %self.orchestration_name,
            orchestration_version = %self.orchestration_version,
            activity_name = %self.activity_name,
            activity_id = %self.activity_id,
            "{}", message.into()
        );
    }

    pub fn trace_debug(&self, message: impl Into<String>) {
        tracing::debug!(
            target: "duroxide::activity",
            instance_id = %self.instance_id,
            execution_id = %self.execution_id,
            orchestration_name = %self.orchestration_name,
            orchestration_version = %self.orchestration_version,
            activity_name = %self.activity_name,
            activity_id = %self.activity_id,
            "{}", message.into()
        );
    }
}
```

## Implementation Plan

### Phase 1: Core Types (src/lib.rs)

1. **Add ActivityContext struct** (as shown above)
2. **Export ActivityContext** in lib.rs public API
3. **Document with examples**

### Phase 2: Update ActivityHandler Trait (src/runtime/registry.rs)

**Current**:
```rust
#[async_trait]
pub trait ActivityHandler: Send + Sync {
    async fn invoke(&self, input: String) -> Result<String, String>;
}
```

**New**:
```rust
#[async_trait]
pub trait ActivityHandler: Send + Sync {
    async fn invoke(&self, ctx: ActivityContext, input: String) -> Result<String, String>;
}
```

### Phase 3: Update FnActivity Wrapper (src/runtime/registry.rs)

**Current**:
```rust
pub struct FnActivity<F, Fut>(pub F)
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static;

#[async_trait]
impl<F, Fut> ActivityHandler for FnActivity<F, Fut>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    async fn invoke(&self, input: String) -> Result<String, String> {
        (self.0)(input).await
    }
}
```

**New**:
```rust
pub struct FnActivity<F, Fut>(pub F)
where
    F: Fn(ActivityContext, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static;

#[async_trait]
impl<F, Fut> ActivityHandler for FnActivity<F, Fut>
where
    F: Fn(ActivityContext, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    async fn invoke(&self, ctx: ActivityContext, input: String) -> Result<String, String> {
        (self.0)(ctx, input).await
    }
}
```

### Phase 4: Update ActivityRegistry Methods (src/runtime/registry.rs)

**Current**:
```rust
pub fn register<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    self.map.insert(name.into(), Arc::new(FnActivity(f)));
    self
}
```

**New**:
```rust
pub fn register<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
where
    F: Fn(ActivityContext, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    self.map.insert(name.into(), Arc::new(FnActivity(f)));
    self
}
```

**Typed variant**:
```rust
pub fn register_typed<In, Out, F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
where
    In: serde::de::DeserializeOwned + Send + 'static,
    Out: serde::Serialize + Send + 'static,
    F: Fn(ActivityContext, In) -> Fut + Send + Sync + 'static,  // Added ActivityContext
    Fut: std::future::Future<Output = Result<Out, String>> + Send + 'static,
{
    let f_clone = std::sync::Arc::new(f);
    let wrapper = move |ctx: ActivityContext, input_s: String| {
        let f_inner = f_clone.clone();
        async move {
            let input: In = crate::_typed_codec::Json::decode(&input_s)?;
            let out: Out = (f_inner)(ctx, input).await?;
            crate::_typed_codec::Json::encode(&out)
        }
    };
    self.map.insert(name.into(), Arc::new(FnActivity(wrapper)));
    self
}
```

### Phase 5: Update Worker Dispatcher (src/runtime/mod.rs)

**Create ActivityContext in worker dispatcher**:

```rust
// When executing activity
WorkItem::ActivityExecute {
    instance,
    execution_id,
    id,
    name,
    input,
} => {
    // Get orchestration metadata from history
    let orch_metadata = rt.get_orchestration_descriptor(&instance).await;
    
    // Create activity context
    let activity_ctx = ActivityContext::new(
        instance.clone(),
        execution_id,
        orch_metadata.as_ref().map(|d| d.name.clone()).unwrap_or_else(|| "unknown".to_string()),
        orch_metadata.as_ref().map(|d| d.version.clone()).unwrap_or_else(|| "unknown".to_string()),
        name.clone(),
        id,
    );
    
    // Log activity start
    tracing::info!(
        target: "duroxide::runtime",
        instance_id = %instance,
        execution_id = %execution_id,
        activity_name = %name,
        activity_id = %id,
        worker_id = %worker_id,
        "Activity started"
    );
    let start_time = std::time::Instant::now();

    // Execute activity with context
    let ack_result = if let Some(handler) = activities.get(&name) {
        match handler.invoke(activity_ctx, input).await {
            Ok(result) => { /* ... */ }
            Err(error) => { /* ... */ }
        }
    } else {
        // Unregistered activity error
    };
}
```

### Phase 6: Update All Activity Registrations

This is the breaking change - every activity registration needs updating:

**Examples**:
```rust
// Before
.register("Greet", |ctx: ActivityContext, name: String| async move {
    Ok(format!("Hello, {}!", name))
})

// After
.register("Greet", |ctx: ActivityContext, name: String| async move {
    ctx.trace_info("Greeting activity started");
    Ok(format!("Hello, {}!", name))
})
```

**Typed activities**:
```rust
// Before
.register_typed("Add", |input: AddRequest| async move {
    Ok(input.a + input.b)
})

// After
.register_typed("Add", |ctx: ActivityContext, input: AddRequest| async move {
    ctx.trace_debug(format!("Adding {} + {}", input.a, input.b));
    Ok(input.a + input.b)
})
```

### Files Requiring Updates

**Core Library**:
- `src/lib.rs` - Add ActivityContext struct
- `src/runtime/registry.rs` - Update ActivityHandler trait and registration methods
- `src/runtime/mod.rs` - Create and pass ActivityContext in worker dispatcher

**Examples** (~5 files):
- `examples/hello_world.rs`
- `examples/fan_out_fan_in.rs`
- `examples/delays_and_timeouts.rs`
- `examples/timers_and_events.rs`
- `examples/sqlite_persistence.rs`
- `examples/with_observability.rs`
- `examples/metrics_cli.rs`

**Tests** (~25 test files):
- `tests/e2e_samples.rs` (largest - many activity registrations)
- `tests/determinism_tests.rs`
- `tests/futures_tests.rs`
- `tests/errors_tests.rs`
- `tests/reliability_tests.rs`
- `tests/worker_reliability_test.rs`
- ... and ~20 more test files

**Stress Tests**:
- `stress-tests/src/lib.rs`

**README**:
- `README.md` - Update hello world example

**Total Estimated Changes**: ~200-300 activity registrations across ~30 files

## Test Plan

### Phase 1: Unit Tests for ActivityContext

**Test File**: `src/lib.rs` (inline tests) or new `tests/activity_context_tests.rs`

#### Test 1: ActivityContext Creation and Accessors
```rust
#[test]
fn test_activity_context_creation() {
    let ctx = ActivityContext::new(
        "test-instance".to_string(),
        1,
        "TestOrch".to_string(),
        "1.0.0".to_string(),
        "TestActivity".to_string(),
        5,
    );
    
    assert_eq!(ctx.instance_id(), "test-instance");
    assert_eq!(ctx.execution_id(), 1);
    assert_eq!(ctx.orchestration_name(), "TestOrch");
    assert_eq!(ctx.orchestration_version(), "1.0.0");
    assert_eq!(ctx.activity_name(), "TestActivity");
    // activity_id is private - not accessible
}
```

#### Test 2: ActivityContext Trace Methods Emit Logs
```rust
#[tokio::test]
async fn test_activity_context_trace_methods() {
    // Use tracing-test crate or capture logs
    let ctx = ActivityContext::new(
        "test-instance".to_string(),
        1,
        "TestOrch".to_string(),
        "1.0.0".to_string(),
        "TestActivity".to_string(),
        5,
    );
    
    ctx.trace_info("Test info message");
    ctx.trace_warn("Test warn message");
    ctx.trace_error("Test error message");
    ctx.trace_debug("Test debug message");
    
    // Verify logs emitted with correct target and fields
    // (Would need tracing-subscriber test infrastructure)
}
```

#### Test 3: ActivityContext is Clone
```rust
#[test]
fn test_activity_context_clone() {
    let ctx1 = ActivityContext::new(
        "test".to_string(),
        1,
        "Orch".to_string(),
        "1.0.0".to_string(),
        "Activity".to_string(),
        5,
    );
    
    let ctx2 = ctx1.clone();
    assert_eq!(ctx1.instance_id(), ctx2.instance_id());
    assert_eq!(ctx1.execution_id(), ctx2.execution_id());
}
```

### Phase 2: ActivityHandler Trait Tests

**Test File**: `src/runtime/registry.rs` or `tests/registry_tests.rs`

#### Test 4: ActivityHandler with Context
```rust
#[tokio::test]
async fn test_activity_handler_with_context() {
    let handler = |ctx: ActivityContext, input: String| async move {
        assert_eq!(ctx.instance_id(), "test-instance");
        assert_eq!(ctx.activity_name(), "TestActivity");
        Ok(input.to_uppercase())
    };
    
    let activity = FnActivity(handler);
    
    let ctx = ActivityContext::new(
        "test-instance".to_string(),
        1,
        "TestOrch".to_string(),
        "1.0.0".to_string(),
        "TestActivity".to_string(),
        5,
    );
    
    let result = activity.invoke(ctx, "hello".to_string()).await.unwrap();
    assert_eq!(result, "HELLO");
}
```

### Phase 3: ActivityRegistry Tests

**Test File**: `tests/registry_tests.rs`

#### Test 5: Register Activity with Context
```rust
#[tokio::test]
async fn test_register_activity_with_context() {
    let registry = ActivityRegistry::builder()
        .register("TestActivity", |ctx: ActivityContext, input: String| async move {
            ctx.trace_info("Processing");
            Ok(input.to_uppercase())
        })
        .build();
    
    assert!(registry.has("TestActivity"));
    
    let handler = registry.get("TestActivity").unwrap();
    let ctx = ActivityContext::new(
        "test".to_string(),
        1,
        "Orch".to_string(),
        "1.0.0".to_string(),
        "TestActivity".to_string(),
        1,
    );
    
    let result = handler.invoke(ctx, "test".to_string()).await.unwrap();
    assert_eq!(result, "TEST");
}
```

#### Test 6: Register Typed Activity with Context
```rust
#[derive(Serialize, Deserialize)]
struct AddRequest { a: i32, b: i32 }

#[tokio::test]
async fn test_register_typed_activity_with_context() {
    let registry = ActivityRegistry::builder()
        .register_typed("Add", |ctx: ActivityContext, input: AddRequest| async move {
            ctx.trace_debug(format!("Adding {} + {}", input.a, input.b));
            Ok(input.a + input.b)
        })
        .build();
    
    let handler = registry.get("Add").unwrap();
    let ctx = ActivityContext::new(
        "test".to_string(),
        1,
        "Orch".to_string(),
        "1.0.0".to_string(),
        "Add".to_string(),
        1,
    );
    
    let input_json = r#"{"a": 5, "b": 3}"#;
    let result = handler.invoke(ctx, input_json.to_string()).await.unwrap();
    assert_eq!(result, "8");
}
```

### Phase 4: Worker Dispatcher Integration Tests

**Test File**: `tests/activity_execution_tests.rs` (new)

#### Test 7: Worker Creates Correct ActivityContext
```rust
#[tokio::test]
async fn test_worker_creates_activity_context() {
    // This is more of an integration test
    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    
    let activities = ActivityRegistry::builder()
        .register("ContextChecker", |ctx: ActivityContext, _input: String| async move {
            // Verify context has correct values
            assert_eq!(ctx.instance_id(), "test-instance");
            assert_eq!(ctx.execution_id(), 1);
            assert_eq!(ctx.activity_name(), "ContextChecker");
            Ok("ok".to_string())
        })
        .build();
    
    let orch = |ctx: OrchestrationContext, _input: String| async move {
        ctx.schedule_activity("ContextChecker", "data".to_string())
            .into_activity()
            .await
    };
    
    let orchestrations = OrchestrationRegistry::builder()
        .register("TestOrch", orch)
        .build();
    
    let rt = Runtime::start_with_store(store.clone(), Arc::new(activities), orchestrations).await;
    let client = Client::new(store);
    
    client.start_orchestration("test-instance", "TestOrch", "").await.unwrap();
    
    let status = client.wait_for_orchestration("test-instance", Duration::from_secs(5)).await.unwrap();
    assert!(matches!(status, OrchestrationStatus::Completed { .. }));
    
    rt.shutdown(None).await;
}
```

#### Test 8: Activity Logs Include All Correlation Fields
```rust
#[tokio::test]
async fn test_activity_logs_correlation() {
    // Use tracing-subscriber test to capture logs
    // Verify activity logs include:
    // - instance_id
    // - execution_id
    // - orchestration_name
    // - orchestration_version
    // - activity_name
    // - activity_id (internal, included automatically)
}
```

### Phase 5: End-to-End Tests

**Test File**: Update existing `tests/e2e_samples.rs`

#### Test 9: Verify All E2E Tests Still Pass
After updating all activity registrations, run:
```bash
cargo test --test e2e_samples -- --test-threads=1
```

Expected: All 25 tests pass with new ActivityContext parameter.

#### Test 10: Add Activity Tracing to First Few E2E Tests

Instead of creating separate tests, enhance the first few existing E2E tests to include activity tracing:

```rust
// In sample_dtf_legacy_gabbar_greetings test:
let activities = ActivityRegistry::builder()
    .register("Greetings", |ctx: ActivityContext, s: String| async move {
        ctx.trace_info("Greeting activity started");
        ctx.trace_debug(format!("Greeting: {}", s));
        let res = format!("Gabbar says: {s}");
        ctx.trace_info("Greeting activity complete");
        Ok(res)
    })
    .build();

// In sample_select2 test:
.register("Upper", |ctx: ActivityContext, s: String| async move {
    ctx.trace_info("Converting to uppercase");
    Ok(s.to_uppercase())
})

// In sample_fanout_join test:
.register("SlowTask", |ctx: ActivityContext, idx: String| async move {
    ctx.trace_info(format!("Processing task {}", idx));
    tokio::time::sleep(Duration::from_millis(50)).await;
    ctx.trace_info("Task complete");
    Ok(format!("result-{idx}"))
})
```

This validates:
- Activity traces appear in logs
- Correlation fields are correct
- No test failures from adding traces
- Real-world usage pattern

### Phase 6: Stress Test Validation

**Test File**: `stress-tests/src/lib.rs`

#### Test 11: Stress Test with ActivityContext
After updating stress test activities:
```bash
cd stress-tests && cargo run --release --bin parallel_orchestrations
```

**Validation**:
- ✅ No performance regression
- ✅ All orchestrations complete successfully
- ✅ Activity logs include correlation fields
- ✅ No errors or panics

### Phase 7: Example Validation

**Test Files**: All examples in `examples/`

#### Test 12: Each Example Runs Successfully
```bash
cargo run --example hello_world
cargo run --example fan_out_fan_in  
cargo run --example delays_and_timeouts
cargo run --example timers_and_events
cargo run --example sqlite_persistence
cargo run --example with_observability
cargo run --example metrics_cli
```

Expected: All examples run without errors and produce expected output.

#### Test 13: Verify Activity Traces in Examples
Run `with_observability` example and verify:
- Activity traces appear with target `duroxide::activity`
- All correlation fields present
- User trace calls from activities work correctly

### Phase 8: Backward Compatibility Validation

#### Test 14: No Breaking Changes to Orchestrations
```rust
#[tokio::test]
async fn test_orchestration_api_unchanged() {
    // Verify OrchestrationContext API is unchanged
    let orch = |ctx: OrchestrationContext, input: String| async move {
        ctx.trace_info("Test");
        ctx.schedule_activity("Test", input).into_activity().await
    };
    
    // Should compile and run without changes
}
```

#### Test 15: Provider API Unchanged
```rust
#[tokio::test]
async fn test_provider_api_unchanged() {
    // Verify Provider trait is unchanged
    // Verify WorkItem types unchanged
    // Only ActivityHandler trait changed
}
```

### Phase 9: Documentation Tests

#### Test 16: Doc Examples Compile
```bash
cargo test --doc
```

Expected: All doc examples compile (after updating activity examples in docs).

### Phase 10: Regression Test Suite

Run full test suite to ensure no regressions:

```bash
# All unit tests
cargo test --lib

# All integration tests
cargo test --tests

# All examples
for ex in examples/*.rs; do
    cargo run --example $(basename $ex .rs) --quiet
done

# Stress tests
cd stress-tests && cargo test
```

**Success Criteria**:
- ✅ Zero test failures
- ✅ All examples run successfully
- ✅ Stress tests complete without errors
- ✅ No performance regression (< 2% overhead)

## Test Execution Order

1. **Unit tests first** (ActivityContext, ActivityHandler, ActivityRegistry)
2. **Integration tests** (Worker dispatcher with context creation)
3. **Update all activities** in examples, tests, stress-tests
4. **E2E test suite** (validate end-to-end behavior)
5. **Stress tests** (validate performance)
6. **Examples** (validate user-facing behavior)
7. **Documentation tests** (validate doc examples)

## Automated Testing Checklist

Before considering this change complete:

- [ ] ActivityContext unit tests pass
- [ ] ActivityHandler trait tests pass
- [ ] ActivityRegistry registration tests pass
- [ ] Worker dispatcher integration tests pass
- [ ] All 31 lib unit tests pass
- [ ] All 25 e2e tests pass
- [ ] All provider correctness tests pass
- [ ] All examples run successfully
- [ ] Stress tests complete (30s run)
- [ ] No performance regression detected
- [ ] Doc tests compile
- [ ] No new linter warnings (except expected dead_code)

## Test Coverage Goals

- **ActivityContext**: 100% coverage (simple struct)
- **Trace methods**: All four methods tested
- **Metadata accessors**: All five accessors tested
- **Worker dispatcher**: Context creation path tested
- **End-to-end**: At least 3 tests with activity logging
- **Stress test**: Full 30s run with observability enabled

## Validation Metrics

Track these during testing:

1. **Test pass rate**: Should remain 100%
2. **Compilation time**: Should not increase significantly
3. **Runtime performance**: < 2% overhead from ActivityContext
4. **Log output**: Activity traces appear with all correlation fields
5. **Log volume**: Verify trace calls don't over-log

## Risk Mitigation

### High Risk Areas

1. **Breaking all activity registrations** - Systematic update required
   - Mitigation: Update in phases (examples → tests → stress)
   - Validation: Compile after each phase

2. **Worker dispatcher context creation** - Complex logic
   - Mitigation: Add comprehensive integration tests
   - Validation: Verify metadata is correct in tests

3. **Performance impact** - ActivityContext created per activity execution
   - Mitigation: Keep struct lightweight (no Arc/Mutex)
   - Validation: Stress test comparison

### Medium Risk Areas

1. **Typed activity wrapper** - Generic type changes
   - Mitigation: Test both simple and typed variants
   - Validation: Type-specific tests

2. **Registry composition** - Cross-crate patterns
   - Mitigation: Test merge() functionality
   - Validation: Registry composition tests

### Low Risk Areas

1. **Orchestration API** - Unchanged
2. **Provider API** - Unchanged
3. **Client API** - Unchanged
4. **History format** - Unchanged

## Rollback Plan

If tests fail or issues are discovered:

1. **Revert core changes** (ActivityHandler trait, FnActivity wrapper)
2. **Keep ActivityContext struct** (harmless if unused)
3. **Fix issues** and re-attempt
4. **Alternative**: Make ActivityContext optional initially

## Success Criteria

The change is successful when:

1. ✅ All existing tests pass (100% pass rate)
2. ✅ All examples run without errors
3. ✅ Activity logs show correlation fields
4. ✅ Stress test completes successfully
5. ✅ No performance regression (< 2%)
6. ✅ Documentation is updated
7. ✅ Users can log from activities with full context

## Benefits

### Observability
- ✅ Full correlation between orchestration and activity logs
- ✅ Trace entire workflow execution with `instance_id`
- ✅ Understand activity behavior in context
- ✅ Debug which orchestration triggered which activity

### Future Extensibility
ActivityContext can be extended to support:
- **Cancellation tokens**: `ctx.cancellation_token()` to check if orchestration cancelled
- **Progress reporting**: `ctx.report_progress(percent)` for long-running activities
- **Heartbeat**: `ctx.heartbeat()` to extend lock timeout for very long activities
- **Metadata**: `ctx.get_metadata("key")` for activity-scoped configuration

### Example: Cancellation Support (Future)

```rust
async fn long_running_task(ctx: ActivityContext, input: String) -> Result<String, String> {
    for i in 0..1000 {
        // Check for cancellation
        if ctx.is_cancelled() {
            ctx.trace_warn("Activity cancelled by orchestration");
            return Err("cancelled".to_string());
        }
        
        // Do work
        process_batch(i).await;
        
        // Report progress
        ctx.report_progress((i * 100) / 1000);
    }
    Ok("complete".to_string())
}
```

## Log Output Examples

### Activity with Logging

```rust
async fn validate_payment(ctx: ActivityContext, card: String) -> Result<String, String> {
    ctx.trace_info("Validating payment method");
    
    let valid = card.len() == 16;
    
    if !valid {
        ctx.trace_warn("Invalid card number");
        return Err("invalid_card".to_string());
    }
    
    ctx.trace_info("Payment method validated");
    Ok("valid".to_string())
}
```

**Log Output**:
```
2025-11-01T10:15:23.456Z INFO duroxide::runtime Activity started instance_id=order-123 execution_id=1 activity_name=ValidatePayment activity_id=5 worker_id=work-a1b2c
2025-11-01T10:15:23.457Z INFO duroxide::activity Validating payment method instance_id=order-123 execution_id=1 orchestration_name=ProcessOrder activity_name=ValidatePayment activity_id=5
2025-11-01T10:15:23.458Z INFO duroxide::activity Payment method validated instance_id=order-123 execution_id=1 orchestration_name=ProcessOrder activity_name=ValidatePayment activity_id=5
2025-11-01T10:15:23.459Z INFO duroxide::runtime Activity completed instance_id=order-123 execution_id=1 activity_name=ValidatePayment worker_id=work-a1b2c outcome="success" duration_ms=3
```

### Query: All Logs for an Instance

```
# Grep
grep "instance_id=order-123" logs.txt

# Elasticsearch
instance_id:"order-123"

# Result: Both orchestration AND activity logs
2025-11-01T10:15:23.450Z INFO duroxide::runtime Orchestration started instance_id=order-123...
2025-11-01T10:15:23.451Z INFO duroxide::orchestration Processing order instance_id=order-123...
2025-11-01T10:15:23.456Z INFO duroxide::runtime Activity started instance_id=order-123 activity_name=ValidatePayment...
2025-11-01T10:15:23.457Z INFO duroxide::activity Validating payment method instance_id=order-123...
2025-11-01T10:15:23.458Z INFO duroxide::activity Payment method validated instance_id=order-123...
```

Perfect correlation!

## Breaking Change Impact

### What Breaks
- ✅ All existing activity registrations (need to add `ctx` parameter)
- ✅ All activity function signatures

### What Doesn't Break
- ✅ Orchestration registrations (unchanged)
- ✅ Provider implementations (unchanged)
- ✅ Client API (unchanged)
- ✅ History format (unchanged)
- ✅ Runtime behavior (unchanged)

### Migration Effort

**Small Projects** (< 10 activities): 5-10 minutes
- Add `ctx: ActivityContext` to each activity
- Optionally add trace calls

**Medium Projects** (10-50 activities): 30-60 minutes
- Systematic update of all registrations
- Good opportunity to add meaningful logs

**Large Projects** (> 50 activities): 2-4 hours
- May want to write a script to assist
- Or do incrementally (old activities can coexist during transition)

### Migration Script Idea

Could provide a regex-based migration helper:

```bash
# Find all .register( patterns
# Add ctx parameter before input
# Flag for manual review
```

## Alternative: Backward Compatible Approach

If we want to support both old and new style temporarily:

```rust
pub trait ActivityHandler: Send + Sync {
    async fn invoke(&self, ctx: ActivityContext, input: String) -> Result<String, String>;
}

// Add a "legacy" registration that wraps old-style activities
pub fn register_legacy<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    let wrapper = move |_ctx: ActivityContext, input: String| {
        f(input)
    };
    self.map.insert(name.into(), Arc::new(FnActivity(wrapper)));
    self
}
```

This allows gradual migration but adds complexity. **Recommendation**: Clean break - update all at once.

## Testing Strategy

1. **Update core tests first** - Ensure trait changes work
2. **Update examples** - Demonstrate new pattern
3. **Update e2e tests systematically** - Largest effort
4. **Update stress tests** - Validate performance
5. **Run full test suite** - Ensure nothing breaks

## Documentation Updates

- Update `docs/ORCHESTRATION-GUIDE.md` - Activity examples
- Update `docs/observability-guide.md` - Activity logging examples
- Update `README.md` - Hello world example
- Update `docs/library-observability.md` - Best practices

## Timeline Estimate

- Phase 1 (Core types): 15 minutes
- Phase 2-3 (Trait updates): 15 minutes
- Phase 4 (Worker dispatcher): 15 minutes
- Phase 5 (Update all activities): 60-90 minutes (largest effort)
- Testing & validation: 30 minutes

**Total**: ~2.5-3 hours for complete implementation

## Next Steps

1. Implement ActivityContext in src/lib.rs
2. Update ActivityHandler trait
3. Update worker dispatcher to create and pass context
4. Systematically update all activity registrations
5. Run tests to ensure everything works
6. Update documentation

## Benefits Summary

- 🎯 **Closes observability gap** - Activities can now log with full context
- 🔗 **Perfect correlation** - All logs for an instance_id include orchestration AND activity traces
- 🚀 **Future-proof** - Context can hold cancellation, progress, heartbeat in future
- 📊 **Better debugging** - Understand what activities are doing
- 🏗️ **Foundation for advanced features** - Cancellation tokens, progress reporting, etc.

