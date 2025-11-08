# Evaluation: ProviderError Enum for Provider Error Handling

## Current State

### Error Handling Today
- **Provider methods return**: `Result<(), String>` - just error messages
- **Runtime handling**: All provider errors are treated as transient and retried with exponential backoff
- **Error conversion**: Runtime wraps provider errors in `ErrorDetails::Infrastructure { retryable: true }`
- **No differentiation**: Cannot distinguish between retryable (database busy) vs non-retryable (corruption, invalid data) errors

### Current Error Types in SQLite Provider
Looking at `src/providers/sqlite.rs`, providers can encounter:
1. **Database errors**: Connection failures, transaction failures, constraint violations
2. **Lock errors**: Invalid/expired lock tokens, lock contention
3. **Serialization errors**: JSON serialization/deserialization failures
4. **Data corruption**: Missing instances, invalid data formats
5. **Concurrency errors**: Deadlocks, race conditions
6. **Validation errors**: Invalid work item types, duplicate events

## Proposed ProviderError Type

### Design (Simple Version)

```rust
/// Provider-specific error with retry classification
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    /// Operation that failed (e.g., "ack_orchestration_item", "fetch_orchestration_item")
    pub operation: String,
    /// Human-readable error message
    pub message: String,
    /// Whether this error should be retried
    pub retryable: bool,
}

impl ProviderError {
    /// Create a retryable (transient) error
    pub fn retryable(operation: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            message: message.into(),
            retryable: true,
        }
    }

    /// Create a non-retryable (permanent) error
    pub fn permanent(operation: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            message: message.into(),
            retryable: false,
        }
    }

    /// Check if error is retryable
    pub fn is_retryable(&self) -> bool {
        self.retryable
    }

    /// Convert to ErrorDetails::Infrastructure for runtime
    pub fn to_infrastructure_error(&self) -> crate::ErrorDetails {
        crate::ErrorDetails::Infrastructure {
            operation: self.operation.clone(),
            message: self.message.clone(),
            retryable: self.retryable,
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.operation, self.message)
    }
}

impl std::error::Error for ProviderError {}

// Backward compatibility: convert String errors to retryable ProviderError
impl From<String> for ProviderError {
    fn from(s: String) -> Self {
        Self {
            operation: "unknown".to_string(),
            message: s,
            retryable: true, // Default to retryable for backward compatibility
        }
    }
}
```

### Updated Provider Trait

```rust
#[async_trait::async_trait]
pub trait Provider: Any + Send + Sync {
    // Change return type from Result<(), String> to Result<(), ProviderError>
    
    async fn fetch_orchestration_item(&self) -> Result<Option<OrchestrationItem>, ProviderError>;
    async fn ack_orchestration_item(
        &self,
        lock_token: &str,
        execution_id: u64,
        history_delta: Vec<Event>,
        worker_items: Vec<WorkItem>,
        orchestrator_items: Vec<WorkItem>,
        metadata: ExecutionMetadata,
    ) -> Result<(), ProviderError>;
    
    async fn abandon_orchestration_item(
        &self,
        lock_token: &str,
        delay_ms: Option<u64>,
    ) -> Result<(), ProviderError>;
    
    // ... other methods
}
```

## Benefits

### 1. **Smarter Retry Logic**
**Current**: Runtime retries ALL provider errors
**With ProviderError**: Runtime can skip retries for permanent errors

```rust
// Runtime can now make intelligent decisions
match provider.ack_orchestration_item(...).await {
    Err(ProviderError::Transient { .. }) => {
        // Retry with backoff
        retry_with_backoff().await;
    }
    Err(ProviderError::Permanent { kind, .. }) => {
        // Fail immediately, don't retry
        fail_orchestration(kind.to_infrastructure_error()).await;
    }
    Err(ProviderError::InvalidState { .. }) => {
        // Idempotent - already processed, just log
        tracing::debug!("Idempotent error, ignoring");
    }
    Ok(()) => { /* success */ }
}
```

### 2. **Better Observability**
- Track error types in metrics (transient vs permanent vs invalid_state)
- Better structured logging with error classification
- Can alert on permanent errors (corruption) vs transient (database busy)

### 3. **Clearer Error Semantics**
- Providers can express error semantics explicitly
- Runtime doesn't need to parse error strings
- Better error messages with context

### 4. **Improved Testing**
- Test providers can return specific error types
- Validation tests can verify error handling
- Can test retry vs non-retry behavior

### 5. **Future Extensibility**
- Easy to add new error types
- Can add error-specific metadata
- Can support error recovery strategies

## Drawbacks

### 1. **Breaking Change**
- Changes Provider trait signature (all methods)
- Requires updating all providers (SQLite, future providers)
- Requires updating runtime error handling

### 2. **Complexity**
- More types to maintain
- Providers need to classify errors correctly
- Runtime needs to handle all error variants

### 3. **Migration Effort**
- Need to update SQLite provider error handling
- Need to update runtime retry logic
- Need to update tests

### 4. **Error Classification Challenges**
- Some errors are ambiguous (is a deadlock transient or permanent?)
- Providers might misclassify errors
- Need clear guidelines for classification

## Recommendation

### ✅ **YES, implement ProviderError enum** - but with phased approach

### Phase 1: Add enum without breaking changes
1. Create `ProviderError` enum alongside existing `Result<(), String>`
2. Add conversion helpers: `ProviderError::from_string()` for backward compatibility
3. Update SQLite provider to use enum internally, convert to String for trait
4. Add tests for error classification

### Phase 2: Update Provider trait (breaking change)
1. Change trait methods to return `Result<(), ProviderError>`
2. Update runtime to handle enum (smart retry logic)
3. Update all providers
4. Update documentation

### Phase 3: Enhancements
1. Add error-specific metadata
2. Add error recovery strategies
3. Improve observability integration

## Alternative: Minimal Approach

If full enum is too much, consider a simpler approach:

```rust
/// Simple provider error with retry classification
#[derive(Debug, Clone)]
pub struct ProviderError {
    pub message: String,
    pub retryable: bool,
    pub operation: String,
}

impl ProviderError {
    pub fn transient(operation: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            message: message.into(),
            retryable: true,
        }
    }
    
    pub fn permanent(operation: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            message: message.into(),
            retryable: false,
        }
    }
}
```

**Pros**: Simpler, less breaking change
**Cons**: Less expressive, can't distinguish error types for metrics

## Implementation Considerations

### Error Classification Guidelines

**Transient (retryable)**:
- Database busy/locked
- Connection timeouts
- Network failures
- Temporary resource exhaustion

**Permanent (non-retryable)**:
- Data corruption (missing instance, invalid format)
- Duplicate events (indicates bug)
- Invalid input (malformed work item)
- Configuration errors

**InvalidState (idempotent)**:
- Invalid/expired lock token
- Instance already terminal
- Work item already processed

### Runtime Integration

```rust
// In runtime/mod.rs
async fn ack_orchestration_with_changes(...) {
    match self.history_store.ack_orchestration_item(...).await {
        Ok(()) => { /* success */ }
        Err(ProviderError::Transient { .. }) => {
            // Retry with backoff
            self.execute_with_retry(...).await;
        }
        Err(ProviderError::Permanent { kind, .. }) => {
            // Fail orchestration immediately
            self.fail_orchestration_with_infrastructure_error(
                kind.to_infrastructure_error()
            ).await;
        }
        Err(ProviderError::InvalidState { .. }) => {
            // Idempotent - log and continue
            tracing::debug!("Idempotent error, ignoring");
        }
    }
}
```

## Conclusion

**Recommendation**: Implement `ProviderError` enum with phased approach

**Priority**: Medium-High
- Improves error handling and observability
- Enables smarter retry logic
- Better separation of concerns
- Worth the migration effort

**Timing**: Good time to do this now since we just refactored provider code
- Can include in next breaking change
- SQLite provider is already being updated
- Documentation is fresh

**Risk**: Low-Medium
- Breaking change but manageable
- Clear migration path
- Can test thoroughly before release

