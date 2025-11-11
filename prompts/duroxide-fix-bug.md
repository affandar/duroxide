# Fix Bug in Duroxide

## Objective
Diagnose and fix a reported bug while maintaining correctness and test coverage.

## Phase 1: Understand the Bug

### 1. Reproduce the Issue
- Get exact steps to reproduce
- Identify affected versions/configurations
- Note the expected vs actual behavior
- Capture error messages/stack traces

### 2. Locate the Root Cause
- Search for relevant code: `rg "error_message"` or related function names
- Check recent changes that might have introduced it: `git log --oneline -20`
- Review the component where failure occurs (runtime, provider, client, etc.)
- Add debug logging if needed to understand flow

### 3. Classify the Bug
**Severity:**
- **Critical**: Data loss, crashes, security issues
- **High**: Core functionality broken, workaround difficult
- **Medium**: Feature broken, workaround exists
- **Low**: Edge case, cosmetic issue

**Category:**
- **Logic error**: Wrong algorithm/condition
- **Concurrency**: Race condition, deadlock
- **Determinism**: Replay produces different results
- **Provider**: Storage/queuing issue
- **API misuse**: Incorrect usage pattern

## Phase 2: Plan the Fix

### Before Writing Code

**Check:**
1. Is there an existing test that should have caught this? (If yes, why didn't it?)
2. What's the minimal change that fixes the root cause?
3. Are there other places with the same pattern that might have the same bug?
4. Will this fix break existing behavior?

**Plan:**
1. Write a failing test that reproduces the bug
2. Implement the fix
3. Verify the test passes
4. Check for regressions
5. Update documentation if behavior changed

## Phase 3: Implement the Fix

### 1. Write a Failing Test First
```rust
#[tokio::test]
async fn test_bug_reproduction() {
    // Setup that triggers the bug
    let store = Arc::new(SqliteProvider::new_in_memory().await.unwrap());
    // ... orchestration/activity setup ...
    
    // Action that should work but currently fails
    let result = client.some_operation().await;
    
    // This assertion should fail before fix, pass after
    assert!(result.is_ok(), "Bug: operation should succeed");
}
```

### 2. Implement the Minimal Fix
- Change only what's necessary to fix the bug
- Don't refactor unrelated code
- Don't add new features "while you're at it"
- Maintain the existing API contract

### 3. Verify Fix
```bash
# Test should now pass
cargo test test_bug_reproduction

# No regressions
cargo test --all

# No new warnings
cargo clippy --all-targets
```

## Phase 4: Prevent Recurrence

### 1. Add Regression Test
If no existing test caught this, add one:
```rust
#[tokio::test]
async fn regression_test_issue_123() {
    // Specific test for the bug scenario
    // Include comment explaining what bug this prevents
}
```

### 2. Check Related Code
Search for similar patterns that might have the same issue:
```bash
rg "similar_pattern" src/
```

### 3. Update Documentation
If bug revealed unclear documentation:
- Update relevant guide with clarification
- Add example showing correct usage
- Add to troubleshooting section if common mistake

## Bug Type Patterns

### Logic Bug
```rust
// ❌ Bug: Off-by-one error
if count > limit { /* error */ }

// ✅ Fix: Correct boundary
if count >= limit { /* error */ }
```

### Concurrency Bug
```rust
// ❌ Bug: Reading without lock
let value = self.shared_state.value;  // Race condition!

// ✅ Fix: Proper locking
let value = self.shared_state.lock().unwrap().value;
```

### Determinism Bug
```rust
// ❌ Bug: Non-deterministic ordering
for item in hashmap.values() { /* process */ }  // Order undefined!

// ✅ Fix: Deterministic iteration
let mut items: Vec<_> = hashmap.values().collect();
items.sort_by_key(|item| item.id);
for item in items { /* process */ }
```

### Provider Bug
```rust
// ❌ Bug: Missing transaction
self.append_history(events).await?;  // Committed
self.enqueue_work(items).await?;     // Might fail, history already saved!

// ✅ Fix: Atomic transaction
let tx = self.begin_transaction().await?;
self.append_history_tx(&tx, events).await?;
self.enqueue_work_tx(&tx, items).await?;
tx.commit().await?;  // All or nothing
```

## Debugging Techniques

### Add Targeted Logging
```rust
tracing::debug!(
    target: "duroxide::debug",
    instance = %instance_id,
    execution_id = %execution_id,
    "Debug point: variable={:?}",
    variable
);
```

Run with: `RUST_LOG=duroxide::debug=trace cargo test failing_test`

### Use Replay Testing
For orchestration bugs:
```rust
// Capture history that triggers bug
let history = client.read_execution_history(instance, exec_id).await?;

// Replay directly
let result = run_turn(history, |ctx| orchestration_logic(ctx));

// Verify behavior
```

### Isolate with Unit Tests
Break down complex scenario into minimal reproducible case:
```rust
#[test]
fn test_isolated_logic() {
    // Test just the broken function/logic
    // No runtime, no async, minimal dependencies
    let result = broken_function(input);
    assert_eq!(result, expected);
}
```

## Documentation Updates

### If Behavior Changed
Update relevant guides:
```markdown
## Breaking Changes

### Version X.Y.Z
- **Fixed**: Description of bug and how behavior now works correctly
- **Migration**: If users relied on buggy behavior, how to adapt
```

### If API Changed (Rare for Bug Fixes)
- Update all examples
- Update API reference docs
- Add deprecation warnings if needed
- Create migration guide

## Commit Message Format

```
Fix: Brief description of bug

Detailed explanation of:
- What was broken
- Why it was broken  
- How the fix addresses it

Fixes #issue-number (if applicable)

Testing:
- Added test_bug_reproduction test
- Verified no regressions with full test suite
```

## Prevention Strategies

After fixing a bug, consider:

1. **Add to test matrix**: Expand test coverage for this scenario
2. **Add assertions**: Runtime assertions that would catch this earlier
3. **Improve error messages**: Help users diagnose similar issues
4. **Update guide**: Add troubleshooting section if it's a common mistake
5. **Refactor if pattern is error-prone**: Consider safer API design

## When in Doubt

- **Consult tests**: Look at existing tests for the affected component
- **Read the design docs**: Check if behavior is intentional
- **Ask the user**: If fix requires API changes or might break compatibility
- **Start small**: Minimal fix first, refactor separately if needed

