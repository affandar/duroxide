/// Provider-specific error with retry classification
///
/// Providers return this error type to indicate whether an error should be retried.
/// The runtime uses `is_retryable()` to decide whether to retry the operation.
///
/// # Error Classification
///
/// **Retryable (is_retryable = true)**:
/// - Database busy/locked
/// - Connection timeouts
/// - Network failures
/// - Temporary resource exhaustion
///
/// **Non-retryable (is_retryable = false)**:
/// - Data corruption (missing instance, invalid format)
/// - Duplicate events (indicates bug)
/// - Invalid input (malformed work item)
/// - Configuration errors
/// - Invalid lock tokens (idempotent - already processed)
///
/// # Example Usage
///
/// ```rust
/// // Transient error - retryable
/// return Err(ProviderError::retryable("ack_orchestration_item", "Database is busy"));
///
/// // Permanent error - not retryable
/// return Err(ProviderError::permanent("ack_orchestration_item", "Duplicate event detected"));
/// ```
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
    ///
    /// Use for errors that might succeed on retry:
    /// - Database busy/locked
    /// - Connection timeouts
    /// - Network failures
    pub fn retryable(operation: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            message: message.into(),
            retryable: true,
        }
    }

    /// Create a non-retryable (permanent) error
    ///
    /// Use for errors that won't succeed on retry:
    /// - Data corruption
    /// - Duplicate events
    /// - Invalid input
    /// - Configuration errors
    /// - Invalid lock tokens (idempotent)
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

/// Conversion from String for backward compatibility
impl From<String> for ProviderError {
    /// Convert String error to retryable ProviderError
    /// 
    /// This allows existing code that returns `Err(String)` to work.
    /// By default, String errors are treated as retryable (conservative approach).
    fn from(s: String) -> Self {
        Self {
            operation: "unknown".to_string(),
            message: s,
            retryable: true, // Default to retryable for backward compatibility
        }
    }
}

impl From<&str> for ProviderError {
    fn from(s: &str) -> Self {
        s.to_string().into()
    }
}

