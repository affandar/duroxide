# Add New Feature to Duroxide

## Objective
Add a new feature to duroxide following established patterns, with tests, documentation, and examples.

## Planning Phase

### 1. Understand the Request
- What is the feature trying to accomplish?
- Who is the target user (library users, provider implementors, orchestration authors)?
- What's the scope (core runtime, provider trait, client API, observability)?
- Are there any existing similar features to model after?

### 2. Design Considerations
Before implementing, consider:

**API Design**
- Does this fit naturally into existing types (`OrchestrationContext`, `ActivityContext`, `Client`)?
- Is it ergonomic for the common case?
- Does it maintain determinism guarantees (for orchestration-facing APIs)?
- Is it backwards compatible, or is this a breaking change?

**Implementation Scope**
- Core runtime changes needed?
- Provider trait changes needed?
- New events or work items needed?
- Observability/metrics needed?

**Testing Strategy**
- Unit tests for business logic
- Integration tests for end-to-end scenarios
- Edge case coverage (errors, timeouts, etc.)
- Determinism/replay tests if applicable

### 3. Create a Design Document (For Large Features)
If the feature is complex, create a design doc in `docs/`:

```markdown
# Feature Name Design

## Overview
Brief description and motivation

## API Design
Public interfaces and usage examples

## Implementation Plan
Key changes needed by component

## Testing Plan
What needs to be tested and how

## Migration Guide
If breaking changes, how users should adapt
```

## Implementation Phase

### 1. Core Implementation
Follow this order to minimize build errors:

1. **Data model** - Add new `Event` variants, `WorkItem` types, or config structs
2. **Provider trait** - Add new methods if needed (with default impls when possible)
3. **Runtime integration** - Wire up dispatchers, execution logic
4. **Public API** - Add methods to `OrchestrationContext`, `ActivityContext`, or `Client`
5. **Observability** - Add metrics and logging for the new feature

### 2. Update Provider Implementations
If you added provider trait methods:
- Update `SqliteProvider` implementation
- Update provider correctness tests if semantics changed
- Add migration notes to provider-implementation-guide.md

### 3. Add Tests

**Unit Tests** (`tests/unit_tests.rs` or new file)
```rust
#[tokio::test]
async fn test_new_feature_basic() {
    // Test the happy path
}

#[tokio::test]
async fn test_new_feature_error_handling() {
    // Test error cases
}
```

**E2E Tests** (`tests/e2e_samples.rs` or relevant file)
```rust
#[tokio::test]
async fn test_new_feature_full_scenario() {
    // Test realistic usage with full runtime
}
```

**Determinism Tests** (if applicable)
```rust
#[tokio::test]
async fn test_new_feature_deterministic_replay() {
    // Verify feature works correctly across replay
}
```

### 4. Add Example
Create `examples/new_feature.rs`:
- Show typical usage
- Include error handling
- Add comments explaining key concepts
- Keep it runnable and self-contained

### 5. Update Documentation

**README.md**
- Add bullet point to feature list if significant
- Update relevant code examples

**ORCHESTRATION-GUIDE.md** (if user-facing)
- Add API reference entry
- Add common pattern example
- Add to troubleshooting if applicable

**Provider Guide** (if provider-facing)
- Document new trait methods
- Add implementation examples
- Update checklist

### 6. Add Observability (If Applicable)
- Add metrics for key operations
- Add structured logging with correlation fields
- Document in observability-guide.md
- Add metrics test coverage

## Quality Checklist

Before considering the feature complete:

- [ ] Feature works in happy path
- [ ] Error cases are handled gracefully
- [ ] Tests cover main scenarios and edge cases
- [ ] All tests pass: `cargo test --all`
- [ ] Doctests pass: `cargo test --doc`
- [ ] Example compiles and runs: `cargo run --example new_feature`
- [ ] Documentation updated in all relevant guides
- [ ] API docs have examples
- [ ] No new compiler warnings
- [ ] No new clippy warnings
- [ ] Code is formatted: `cargo fmt --all`
- [ ] Breaking changes are documented
- [ ] Migration path is clear (if breaking)

## Feature Patterns

### Non-Breaking: Add New Method
```rust
impl OrchestrationContext {
    /// New helper method
    pub fn new_feature(&self, param: String) -> Result<String, String> {
        // Implementation
    }
}
```

**Checklist:**
- Add to API reference in docs
- Add usage example in guide
- Add test coverage

### Breaking: Change Existing Signature
```rust
// Old
fn schedule_activity(&self, name: impl Into<String>) -> DurableFuture

// New - adds parameter
fn schedule_activity(&self, name: impl Into<String>, options: ActivityOptions) -> DurableFuture
```

**Checklist:**
- Update ALL call sites (tests, examples, docs)
- Create migration guide
- Document as breaking change in commit message
- Consider deprecation path if possible

### New Event Type
```rust
pub enum Event {
    // Existing variants...
    
    /// New event type
    NewFeatureEvent {
        event_id: u64,
        data: String,
    },
}
```

**Checklist:**
- Add to event dispatch logic in replay_engine
- Update provider if storage semantics change
- Add to relevant match statements
- Test serialization/deserialization
- Document in provider guide

## Examples of Past Features

Study these for patterns:
- **ActivityContext** - New required parameter across all activities
- **Observability** - New config struct, metrics, logging, feature flag
- **ContinueAsNew** - New event type, client API, runtime logic
- **External Events** - New scheduling API, event matching logic

## Anti-Patterns

**Don't:**
- Add features without tests
- Create breaking changes without migration path
- Skip documentation updates
- Implement in one giant commit (break into logical steps)
- Merge without running full test suite
- Add `TODO` comments and leave them
- Copy-paste code without understanding it

