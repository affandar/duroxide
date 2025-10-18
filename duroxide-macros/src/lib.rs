use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

/// Mark an async function as a durable activity.
///
/// Activities execute on workers and can perform I/O operations.
///
/// # Example
/// ```rust,ignore
/// #[activity(typed)]
/// async fn charge_payment(order: Order) -> Result<String, String> {
///     // I/O operations allowed
///     Ok("TXN-123".into())
/// }
/// ```
#[proc_macro_attribute]
pub fn activity(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    
    // For Phase 1: Just pass through the function unchanged
    // We'll add the real implementation in later phases
    let expanded = quote! {
        #input
    };
    
    TokenStream::from(expanded)
}

/// Mark an async function as a durable orchestration.
///
/// Orchestrations coordinate activities and sub-orchestrations.
///
/// # Example
/// ```rust,ignore
/// #[orchestration]
/// async fn process_order(ctx: OrchestrationContext, order: Order) -> Result<String, String> {
///     // Coordinate activities
///     Ok("Done".into())
/// }
/// ```
#[proc_macro_attribute]
pub fn orchestration(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    
    // For Phase 1: Just pass through the function unchanged
    let expanded = quote! {
        #input
    };
    
    TokenStream::from(expanded)
}

/// Make a function call durable.
///
/// Works for both activities and sub-orchestrations.
///
/// # Example
/// ```rust,ignore
/// let result = durable!(my_activity(input)).await?;
/// ```
#[proc_macro]
pub fn durable(_input: TokenStream) -> TokenStream {
    // Phase 1: Placeholder
    // Will implement in Phase 3
    TokenStream::new()
}

/// Durable info-level trace.
#[proc_macro]
pub fn durable_trace_info(_input: TokenStream) -> TokenStream {
    // Phase 1: Placeholder
    TokenStream::new()
}

/// Durable warning-level trace.
#[proc_macro]
pub fn durable_trace_warn(_input: TokenStream) -> TokenStream {
    // Phase 1: Placeholder
    TokenStream::new()
}

/// Durable error-level trace.
#[proc_macro]
pub fn durable_trace_error(_input: TokenStream) -> TokenStream {
    // Phase 1: Placeholder
    TokenStream::new()
}

/// Durable debug-level trace.
#[proc_macro]
pub fn durable_trace_debug(_input: TokenStream) -> TokenStream {
    // Phase 1: Placeholder
    TokenStream::new()
}

/// Generate a deterministic GUID.
#[proc_macro]
pub fn durable_newguid(_input: TokenStream) -> TokenStream {
    // Phase 1: Placeholder
    TokenStream::new()
}

/// Get deterministic UTC timestamp.
#[proc_macro]
pub fn durable_utcnow(_input: TokenStream) -> TokenStream {
    // Phase 1: Placeholder
    TokenStream::new()
}

