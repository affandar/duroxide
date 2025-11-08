# ProviderError Type - Simple Design

## Overview

Simple `ProviderError` struct with `is_retryable` flag and error message.

## Design

```rust
pub struct ProviderError {
    pub operation: String,  // e.g., "ack_orchestration_item"
    pub message: String,    // Human-readable error message
    pub retryable: bool,     // Whether to retry this error
}
```

## Usage Examples

### In Provider Implementation

```rust
use duroxide::providers::ProviderError;

// Transient error - retryable
async fn ack_orchestration_item(...) -> Result<(), ProviderError> {
    match database.execute(...).await {
        Err(e) if e.is_busy() => {
            Err(ProviderError::retryable("ack_orchestration_item", "Database is busy"))
        }
        Err(e) => Err(ProviderError::permanent("ack_orchestration_item", &e.to_string())),
        Ok(_) => Ok(()),
    }
}

// Permanent error - not retryable
async fn ack_orchestration_item(...) -> Result<(), ProviderError> {
    if duplicate_event_detected {
        return Err(ProviderError::permanent(
            "ack_orchestration_item",
            "Duplicate event detected"
        ));
    }
    Ok(())
}

// Invalid lock token - idempotent (non-retryable)
async fn abandon_orchestration_item(...) -> Result<(), ProviderError> {
    if lock_token_not_found {
        return Err(ProviderError::permanent(
            "abandon_orchestration_item",
            "Invalid lock token"
        ));
    }
    Ok(())
}
```

### In Runtime

```rust
// Runtime can make smart retry decisions
match provider.ack_orchestration_item(...).await {
    Ok(()) => { /* success */ }
    Err(e) if e.is_retryable() => {
        // Retry with backoff
        retry_with_backoff().await;
    }
    Err(e) => {
        // Don't retry - fail immediately
        let infra_error = e.to_infrastructure_error();
        fail_orchestration(infra_error).await;
    }
}
```

## Benefits

1. **Simple**: Just 3 fields - operation, message, retryable
2. **Backward compatible**: `From<String>` conversion for existing code
3. **Clear semantics**: Explicit retry classification
4. **Easy migration**: Can be adopted incrementally

## Migration Path

### Phase 1: Add type (non-breaking)
- Create `ProviderError` type
- Add `From<String>` for backward compatibility
- Providers can start using it internally

### Phase 2: Update trait (breaking change)
- Change Provider trait methods to return `Result<(), ProviderError>`
- Update runtime to use `is_retryable()` for retry decisions
- Update all providers

## Error Classification Guidelines

**Retryable (retryable = true)**:
- Database busy/locked (`SQLITE_BUSY`)
- Connection timeouts
- Network failures
- Temporary resource exhaustion

**Non-retryable (retryable = false)**:
- Data corruption (missing instance, invalid format)
- Duplicate events (PRIMARY KEY constraint violation)
- Invalid input (malformed work item)
- Configuration errors
- Invalid lock tokens (idempotent - already processed)

