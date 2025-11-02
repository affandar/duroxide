# Refactor Code in Duroxide

## Objective
Improve code quality, maintainability, or performance without changing external behavior.

## When to Refactor

### Good Reasons
- Code duplication across multiple files
- Complex function that's hard to understand
- Poor naming that obscures intent
- Inefficient algorithm with measurable impact
- Unclear separation of concerns
- Hard-to-test code structure

### Bad Reasons
- "Just because" or "I prefer this style"
- During bug fix (fix bug first, refactor separately)
- During feature addition (add feature first, refactor separately)
- Without tests to verify behavior unchanged

## Planning Phase

### 1. Define the Refactoring Goal
Be specific about what you're improving:
- **Readability**: "Extract complex conditional into named function"
- **Maintainability**: "Remove duplication between worker and orch dispatchers"
- **Performance**: "Replace O(n²) loop with HashMap lookup"
- **Testability**: "Extract business logic from runtime coupling"

### 2. Ensure Test Coverage
**Before refactoring**, verify comprehensive tests exist:
```bash
# Run tests for affected component
cargo test --test e2e_samples
cargo test --test unit_tests

# Check coverage of specific function
rg "function_to_refactor" tests/
```

**If tests are insufficient:**
- Add tests first
- Verify they pass with current code
- Then refactor
- Verify tests still pass

### 3. Plan the Changes
- What code will move/change?
- What new abstractions will be introduced?
- Will any public APIs change?
- What's the migration path if breaking?

## Refactoring Patterns

### 1. Extract Function
**When**: Logic block appears multiple times or is complex

```rust
// ❌ Before: Duplication
fn handle_activity_success(...) {
    let duration_ms = start_time.elapsed().as_millis() as u64;
    tracing::debug!(target: "duroxide::runtime", instance_id = %instance, ...);
    rt.record_activity_success();
    // ... more logic ...
}

fn handle_activity_failure(...) {
    let duration_ms = start_time.elapsed().as_millis() as u64;
    tracing::warn!(target: "duroxide::runtime", instance_id = %instance, ...);
    rt.record_activity_app_error();
    // ... more logic ...
}

// ✅ After: Extracted common pattern
fn log_activity_outcome(instance: &str, execution_id: u64, name: &str, outcome: ActivityOutcome, start_time: Instant) {
    let duration_ms = start_time.elapsed().as_millis() as u64;
    match outcome {
        ActivityOutcome::Success => tracing::debug!(...),
        ActivityOutcome::AppError => tracing::warn!(...),
    }
}
```

### 2. Extract Type
**When**: Passing many related parameters around

```rust
// ❌ Before: Parameter clutter
fn process_item(
    instance: String,
    execution_id: u64,
    orch_name: String,
    orch_version: String,
    worker_id: String,
) { }

// ✅ After: Cohesive type
struct ExecutionContext {
    instance: String,
    execution_id: u64,
    orch_name: String,
    orch_version: String,
    worker_id: String,
}

fn process_item(ctx: &ExecutionContext) { }
```

### 3. Simplify Conditional
**When**: Nested or complex conditionals

```rust
// ❌ Before: Hard to read
if let Some(handler) = activities.get(&name) {
    match handler.invoke(ctx, input).await {
        Ok(result) => {
            if result.len() > 0 {
                // ...
            } else {
                // ...
            }
        }
        Err(e) => {
            // ...
        }
    }
} else {
    // ...
}

// ✅ After: Early returns, named functions
let handler = match activities.get(&name) {
    Some(h) => h,
    None => return handle_missing_activity(name, instance),
};

let result = match handler.invoke(ctx, input).await {
    Ok(r) => r,
    Err(e) => return handle_activity_error(e, instance),
};

if result.is_empty() {
    return handle_empty_result(instance);
}

process_activity_result(result, instance)
```

### 4. Replace Magic Values with Constants
```rust
// ❌ Before: Magic numbers
if attempts > 5 {
    // ...
}
tokio::time::sleep(Duration::from_millis(10 * (1 << attempts))).await;

// ✅ After: Named constants
const MAX_RETRY_ATTEMPTS: u32 = 5;
const BASE_BACKOFF_MS: u64 = 10;

if attempts > MAX_RETRY_ATTEMPTS {
    // ...
}
tokio::time::sleep(Duration::from_millis(BASE_BACKOFF_MS * (1 << attempts))).await;
```

### 5. Reduce Nesting
```rust
// ❌ Before: Deep nesting
fn process() {
    if condition1 {
        if condition2 {
            if condition3 {
                // actual work
            }
        }
    }
}

// ✅ After: Guard clauses
fn process() {
    if !condition1 {
        return;
    }
    if !condition2 {
        return;
    }
    if !condition3 {
        return;
    }
    
    // actual work
}
```

## Execution Steps

### 1. Create Snapshot
```bash
# Create a branch for the refactoring
git checkout -b refactor/description

# Run tests to establish baseline
cargo test --all > baseline-tests.txt
```

### 2. Make Incremental Changes
- **Small steps**: One logical change at a time
- **Test after each step**: `cargo test --all`
- **Commit frequently**: Easy to rollback if something breaks

### 3. Verify Behavior Unchanged
```bash
# All tests pass
cargo test --all

# No new warnings
cargo clippy --all-targets

# Format
cargo fmt --all

# Examples still work
cargo run --example hello_world

# Performance hasn't regressed (if relevant)
./run-stress-tests.sh 30
```

## Breaking vs Non-Breaking Refactoring

### Non-Breaking (Safe)
- Internal helper functions
- Private struct fields
- Implementation details
- Code organization within a module
- Performance optimizations with same API

### Breaking (Requires Care)
- Public function signatures
- Public struct fields
- Trait definitions
- Module re-exports
- Enum variants (if publicly exhaustive)

**For breaking refactors:**
1. Document the change
2. Update all call sites
3. Add migration notes
4. Consider deprecation path
5. Ask user before proceeding

## Code Quality Checks

After refactoring, verify:

### Maintainability
- [ ] Functions are focused (single responsibility)
- [ ] Names clearly indicate purpose
- [ ] Comments explain why, not what
- [ ] No deep nesting (max 3-4 levels)
- [ ] No long functions (prefer < 50 lines)

### Readability
- [ ] Code reads like prose
- [ ] Related code is grouped together
- [ ] Consistent naming conventions
- [ ] Appropriate use of types vs primitives
- [ ] Error messages are helpful

### Performance
- [ ] No accidental clones in hot paths
- [ ] Appropriate use of references
- [ ] No unnecessary allocations
- [ ] Efficient data structures
- [ ] Run stress test if critical path changed

## Common Refactoring Pitfalls

### 1. Changing Behavior by Accident
```rust
// Subtle bug introduced during refactoring:
// Before: processes even on error
let _ = operation().await;
process();

// After: doesn't process on error
operation().await?;  // ❌ Exits early on error now!
process();
```

### 2. Breaking Determinism
```rust
// Before: Deterministic iteration
for event in history.iter() { }

// After: Non-deterministic if using HashMap
for event in event_map.values() { }  // ❌ Order not guaranteed!
```

### 3. Introducing Race Conditions
```rust
// Before: Atomic
let value = self.shared.lock().unwrap().value;

// After: Non-atomic
let guard = self.shared.lock().unwrap();
drop(guard);  // ❌ Released lock!
let value = self.shared.lock().unwrap().value;  // Race window!
```

## Documentation Updates

After refactoring:
- Update doc comments if function purpose changed
- Update examples if API changed
- Update architecture docs if structure changed
- Add comments explaining non-obvious optimizations

## When to Stop and Ask

Stop and consult the user if:
- Refactoring requires changing public API
- Need to remove >100 lines of code
- Performance optimization requires complexity tradeoff
- Multiple approaches exist and choice isn't clear
- Tests don't cover the code you're refactoring

## Example Refactoring Commit

```
Refactor: Extract activity outcome logging into helper

The worker dispatcher had three similar logging blocks for activity
outcomes (success, app_error, config_error). This extracts the common
pattern into a dedicated helper function.

Changes:
- Add log_activity_outcome() helper
- Add ActivityOutcome enum for type safety
- Reduce duplication from 60 lines to 20
- No behavior changes

Testing: All existing tests pass
```

