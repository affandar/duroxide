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
pub fn activity(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let attrs = parse_attributes(attr);
    
    let fn_name = &input.sig.ident;
    let activity_name = attrs.name.unwrap_or_else(|| fn_name.to_string());
    
    let vis = &input.vis;
    let fn_attrs = &input.attrs;
    let sig = &input.sig;
    let block = &input.block;
    
    if attrs.typed {
        // Phase 2: Generate typed wrapper
        let input_type = extract_first_param_type(&input.sig);
        let output_type = extract_result_ok_type(&input.sig);
        
        // Keep the original function as-is for the implementation
        // Generate a unique descriptor name
        let descriptor_name = syn::Ident::new(
            &format!("__DUROXIDE_ACTIVITY_DESCRIPTOR_{}", fn_name.to_string().to_uppercase()),
            fn_name.span()
        );
        
        // Phase 2: Generate struct with .call() method
        // Hidden implementation function
        let impl_fn_name = syn::Ident::new(&format!("__duroxide_impl_{}", fn_name), fn_name.span());
        
        // Reconstruct signature with new function name
        let sig_inputs = &input.sig.inputs;
        let sig_output = &input.sig.output;
        let sig_asyncness = &input.sig.asyncness;
        let sig_generics = &input.sig.generics;
        
        let expanded = quote! {
            // Implementation function (hidden but searchable)
            #[doc = concat!("Implementation for activity: ", #activity_name)]
            #[doc = ""]
            #[doc = "This is the actual activity implementation."]
            #[doc = concat!("Use `durable!(", stringify!(#fn_name), "(args))` to call it from orchestrations.")]
            #[allow(non_snake_case)]
            #(#fn_attrs)*
            #vis #sig_asyncness fn #impl_fn_name #sig_generics(#sig_inputs) #sig_output {
                #block
            }
            
            // Public struct with .call() method
            #[doc = concat!("Activity caller for: ", #activity_name)]
            #[doc = ""]
            #[doc = concat!("Implementation: [`", stringify!(#impl_fn_name), "`]")]
            #[doc = ""]
            #[doc = "Use with `durable!()` macro in orchestrations."]
            #[allow(non_camel_case_types)]
            #vis struct #fn_name;
            
            impl #fn_name {
                #[doc = "Call this activity (use with `durable!()` macro)."]
                pub fn call(
                    &self,
                    __ctx: &::duroxide::OrchestrationContext,
                    __input: #input_type,
                ) -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = Result<#output_type, String>> + Send + 'static>> {
                    // Clone context for 'static lifetime
                    let __ctx_clone = __ctx.clone();
                    
                    Box::pin(async move {
                        // Serialize input
                        let __input_json = ::serde_json::to_string(&__input)
                            .map_err(|e| format!("Failed to serialize input: {}", e))?;
                        
                        // Schedule activity
                        let __result_json = __ctx_clone
                            .schedule_activity(#activity_name, __input_json)
                            .into_activity()
                            .await?;
                        
                        // Deserialize output
                        let __result: #output_type = ::serde_json::from_str(&__result_json)
                            .map_err(|e| format!("Failed to deserialize output: {}", e))?;
                        
                        Ok(__result)
                    })
                }
            }
            
            // Submit to distributed slice (unique name per activity)
            #[::linkme::distributed_slice(::duroxide::__internal::ACTIVITIES)]
            #[linkme(crate = ::linkme)]
            static #descriptor_name: ::duroxide::__internal::ActivityDescriptor = 
                ::duroxide::__internal::ActivityDescriptor {
                    name: #activity_name,
                    invoke: |input_json: String| {
                        Box::pin(async move {
                            let input: #input_type = ::serde_json::from_str(&input_json)
                                .map_err(|e| format!("Deserialization error: {}", e))?;
                            // Call the hidden implementation function
                            let result = #impl_fn_name(input).await?;
                            ::serde_json::to_string(&result)
                                .map_err(|e| format!("Serialization error: {}", e))
                        })
                    },
                };
        };
        
        TokenStream::from(expanded)
    } else {
        // Non-typed: just pass through for now
        let expanded = quote! {
            #(#fn_attrs)*
            #vis #sig #block
        };
        
        TokenStream::from(expanded)
    }
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
pub fn orchestration(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let attrs = parse_attributes(attr);
    
    let fn_name = &input.sig.ident;
    let orch_name = attrs.name.unwrap_or_else(|| fn_name.to_string());
    let version = attrs.version.unwrap_or_else(|| "1.0.0".to_string());
    
    let vis = &input.vis;
    let fn_attrs = &input.attrs;
    let sig = &input.sig;
    let block = &input.block;
    
    // Extract types for client helpers
    let input_type = extract_second_param_type(&input.sig);
    let output_type = extract_result_ok_type(&input.sig);
    
    // Generate unique descriptor name
    let descriptor_name = syn::Ident::new(
        &format!("__DUROXIDE_ORCH_DESCRIPTOR_{}", fn_name.to_string().to_uppercase()),
        fn_name.span()
    );
    
    // Create a unique function name for the implementation
    let impl_fn_name = syn::Ident::new(&format!("__duroxide_orch_impl_{}", fn_name), fn_name.span());
    
    let sig_inputs = &input.sig.inputs;
    let sig_output = &input.sig.output;
    let sig_asyncness = &input.sig.asyncness;
    let sig_generics = &input.sig.generics;
    
    let expanded = quote! {
        // Hidden implementation function
        #[doc(hidden)]
        #[allow(non_snake_case)]
        #(#fn_attrs)*
        #vis #sig_asyncness fn #impl_fn_name #sig_generics(#sig_inputs) #sig_output #block
        
        // Generate client helper module (Phase 5)
        #[doc = concat!("Client helpers for orchestration: ", #orch_name)]
        #[allow(non_snake_case)]
        #vis mod #fn_name {
            use super::*;
            
            /// Start this orchestration with type-safe input
            pub async fn start(
                client: &::duroxide::Client,
                instance_id: impl AsRef<str>,
                input: #input_type,
            ) -> Result<(), String> {
                let input_json = ::serde_json::to_string(&input)
                    .map_err(|e| format!("Failed to serialize input: {}", e))?;
                client.start_orchestration(instance_id.as_ref(), #orch_name, input_json).await
            }
            
            /// Wait for orchestration to complete with typed output
            pub async fn wait(
                client: &::duroxide::Client,
                instance_id: impl AsRef<str>,
                timeout: ::std::time::Duration,
            ) -> Result<::duroxide::OrchestrationStatus<#output_type>, String> {
                let status = client.wait_for_orchestration(instance_id.as_ref(), timeout)
                    .await
                    .map_err(|e| format!("Wait error: {:?}", e))?;
                
                match status {
                    ::duroxide::OrchestrationStatus::Completed { output } => {
                        let typed: #output_type = ::serde_json::from_str(&output)
                            .map_err(|e| format!("Failed to deserialize output: {}", e))?;
                        Ok(::duroxide::OrchestrationStatus::Completed { output: typed })
                    }
                    ::duroxide::OrchestrationStatus::Failed { error } => {
                        Ok(::duroxide::OrchestrationStatus::Failed { error })
                    }
                    ::duroxide::OrchestrationStatus::Running => {
                        Ok(::duroxide::OrchestrationStatus::Running)
                    }
                    ::duroxide::OrchestrationStatus::NotFound => {
                        Ok(::duroxide::OrchestrationStatus::NotFound)
                    }
                }
            }
            
            /// Get current status
            pub async fn status(
                client: &::duroxide::Client,
                instance_id: impl AsRef<str>,
            ) -> Result<::duroxide::OrchestrationStatus<#output_type>, String> {
                // Use wait with zero timeout
                wait(client, instance_id, ::std::time::Duration::from_millis(0)).await
            }
            
            /// Cancel this orchestration
            pub async fn cancel(
                client: &::duroxide::Client,
                instance_id: impl AsRef<str>,
                reason: impl Into<String>,
            ) -> Result<(), String> {
                client.cancel_instance(instance_id.as_ref(), reason).await
            }
            
            /// Start and wait in one call (convenience)
            pub async fn run(
                client: &::duroxide::Client,
                instance_id: impl AsRef<str>,
                input: #input_type,
                timeout: ::std::time::Duration,
            ) -> Result<#output_type, String> {
                start(client, instance_id.as_ref(), input).await?;
                match wait(client, instance_id, timeout).await? {
                    ::duroxide::OrchestrationStatus::Completed { output } => Ok(output),
                    ::duroxide::OrchestrationStatus::Failed { error } => Err(error),
                    _ => Err("Unexpected status".to_string()),
                }
            }
        }
        
        // Submit to distributed slice
        #[::linkme::distributed_slice(::duroxide::__internal::ORCHESTRATIONS)]
        #[linkme(crate = ::linkme)]
        static #descriptor_name: ::duroxide::__internal::OrchestrationDescriptor = 
            ::duroxide::__internal::OrchestrationDescriptor {
                name: #orch_name,
                version: #version,
                invoke: |ctx: ::duroxide::OrchestrationContext, input_json: String| {
                    Box::pin(async move {
                        // Deserialize input
                        let input: #input_type = ::serde_json::from_str(&input_json)
                            .map_err(|e| format!("Failed to deserialize input: {}", e))?;
                        
                        // Call implementation function
                        let result = #impl_fn_name(ctx, input).await?;
                        
                        // Serialize output
                        ::serde_json::to_string(&result)
                            .map_err(|e| format!("Failed to serialize output: {}", e))
                    })
                },
            };
    };
    
    TokenStream::from(expanded)
}

/// Make a function call durable.
///
/// Works for both activities and sub-orchestrations.
/// Automatically captures `ctx` from surrounding scope.
///
/// # Example
/// ```rust,ignore
/// let result = durable!(my_activity(input)).await?;
/// ```
#[proc_macro]
pub fn durable(input: TokenStream) -> TokenStream {
    let call = parse_macro_input!(input as syn::ExprCall);
    
    let func = &call.func;
    let args = &call.args;
    
    // Transform: fn(args) → fn.call(&ctx, args)
    let expanded = quote! {
        #func.call(&ctx, #args)
    };
    
    TokenStream::from(expanded)
}

/// Durable info-level trace.
///
/// Automatically captures `ctx` from scope.
///
/// # Example
/// ```rust,ignore
/// durable_trace_info!("Processing: {}", value);
/// ```
#[proc_macro]
pub fn durable_trace_info(input: TokenStream) -> TokenStream {
    durable_trace_impl(input, "INFO")
}

/// Durable warning-level trace.
#[proc_macro]
pub fn durable_trace_warn(input: TokenStream) -> TokenStream {
    durable_trace_impl(input, "WARN")
}

/// Durable error-level trace.
#[proc_macro]
pub fn durable_trace_error(input: TokenStream) -> TokenStream {
    durable_trace_impl(input, "ERROR")
}

/// Durable debug-level trace.
#[proc_macro]
pub fn durable_trace_debug(input: TokenStream) -> TokenStream {
    durable_trace_impl(input, "DEBUG")
}

fn durable_trace_impl(input: TokenStream, level: &str) -> TokenStream {
    // Convert to TokenStream2 for quote
    let tokens = proc_macro2::TokenStream::from(input);
    
    // Transform: durable_trace_info!("msg", args) → ctx.trace("INFO", format!("msg", args))
    let expanded = quote! {
        ctx.trace(#level, format!(#tokens))
    };
    
    TokenStream::from(expanded)
}

/// Generate a deterministic GUID.
///
/// Automatically captures `ctx` from scope.
///
/// # Example
/// ```rust,ignore
/// let id = durable_newguid!().await?;
/// ```
#[proc_macro]
pub fn durable_newguid(_input: TokenStream) -> TokenStream {
    let expanded = quote! {
        ctx.new_guid()
    };
    
    TokenStream::from(expanded)
}

/// Get deterministic UTC timestamp in milliseconds.
///
/// Automatically captures `ctx` from scope.
///
/// # Example
/// ```rust,ignore
/// let timestamp = durable_utcnow!().await?;
/// ```
#[proc_macro]
pub fn durable_utcnow(_input: TokenStream) -> TokenStream {
    let expanded = quote! {
        ctx.utcnow_ms()
    };
    
    TokenStream::from(expanded)
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

struct MacroAttributes {
    name: Option<String>,
    version: Option<String>,
    typed: bool,
}

/// Parse macro attributes (simple manual parsing for Phase 2)
fn parse_attributes(attr: TokenStream) -> MacroAttributes {
    let mut name = None;
    let mut version = None;
    let mut typed = false;
    
    let attr_str = attr.to_string();
    
    // Check for 'typed'
    if attr_str.contains("typed") {
        typed = true;
    }
    
    // Extract name = "..."
    if let Some(start) = attr_str.find("name") {
        if let Some(eq_pos) = attr_str[start..].find('=') {
            let after_eq = &attr_str[start + eq_pos + 1..];
            if let Some(quote_start) = after_eq.find('"') {
                if let Some(quote_end) = after_eq[quote_start + 1..].find('"') {
                    name = Some(after_eq[quote_start + 1..quote_start + 1 + quote_end].to_string());
                }
            }
        }
    }
    
    // Extract version = "..."
    if let Some(start) = attr_str.find("version") {
        if let Some(eq_pos) = attr_str[start..].find('=') {
            let after_eq = &attr_str[start + eq_pos + 1..];
            if let Some(quote_start) = after_eq.find('"') {
                if let Some(quote_end) = after_eq[quote_start + 1..].find('"') {
                    version = Some(after_eq[quote_start + 1..quote_start + 1 + quote_end].to_string());
                }
            }
        }
    }
    
    MacroAttributes { name, version, typed }
}

/// Extract the first parameter's type from a function signature
fn extract_first_param_type(sig: &syn::Signature) -> syn::Type {
    if let Some(syn::FnArg::Typed(pat_type)) = sig.inputs.first() {
        (*pat_type.ty).clone()
    } else {
        syn::parse_quote! { String }
    }
}

/// Extract the second parameter's type (for orchestrations, after ctx)
fn extract_second_param_type(sig: &syn::Signature) -> syn::Type {
    if let Some(syn::FnArg::Typed(pat_type)) = sig.inputs.iter().nth(1) {
        (*pat_type.ty).clone()
    } else {
        syn::parse_quote! { String }
    }
}

/// Auto-setup main function with duroxide runtime.
///
/// # Example
/// ```rust,ignore
/// #[duroxide::main]
/// async fn main() {
///     process_order::start(&client, "order-1", order).await?;
/// }
/// ```
#[proc_macro_attribute]
pub fn main(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let attrs = parse_attributes(attr);
    
    let fn_block = &input.block;
    
    // Determine provider setup based on attributes
    let provider_setup = if let Some(db_path) = attrs.name {
        // name attribute is repurposed as db_path for main macro
        quote! {
            let __db_path = ::std::path::PathBuf::from(#db_path);
            if let Some(__parent) = __db_path.parent() {
                ::std::fs::create_dir_all(__parent)
                    .expect("Failed to create database directory");
            }
            if !__db_path.exists() {
                ::std::fs::File::create(&__db_path)
                    .expect("Failed to create database file");
            }
            let __db_url = format!("sqlite:{}", __db_path.to_str().unwrap());
            let __store = ::std::sync::Arc::new(
                ::duroxide::providers::sqlite::SqliteProvider::new(&__db_url)
                    .await
                    .expect("Failed to create SQLite provider")
            );
        }
    } else {
        // Default: temp directory
        quote! {
            let __temp_dir = ::tempfile::tempdir()
                .expect("Failed to create temp directory");
            let __db_path = __temp_dir.path().join("duroxide.db");
            ::std::fs::File::create(&__db_path)
                .expect("Failed to create database file");
            let __db_url = format!("sqlite:{}", __db_path.to_str().unwrap());
            let __store = ::std::sync::Arc::new(
                ::duroxide::providers::sqlite::SqliteProvider::new(&__db_url)
                    .await
                    .expect("Failed to create SQLite provider")
            );
            let _temp_guard = __temp_dir;  // Keep alive
        }
    };
    
    let expanded = quote! {
        fn main() -> Result<(), Box<dyn ::std::error::Error>> {
            // Create tokio runtime
            let __tokio_rt = ::tokio::runtime::Runtime::new()?;
            
            __tokio_rt.block_on(async {
                // Initialize tracing
                let _ = ::tracing_subscriber::fmt()
                    .with_env_filter(
                        ::tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| "info".into())
                    )
                    .try_init();
                
                // Set up provider
                #provider_setup
                
                // Start duroxide runtime with auto-discovery
                let __duroxide_rt = ::duroxide::runtime::Runtime::builder()
                    .store(__store.clone())
                    .discover_activities()
                    .discover_orchestrations()
                    .start()
                    .await;
                
                // Create client
                let client = ::duroxide::client::Client::new(__store.clone());
                
                // Execute user's main function
                let __result: Result<(), Box<dyn ::std::error::Error>> = async {
                    #fn_block
                    Ok(())
                }.await;
                
                // Shutdown runtime
                __duroxide_rt.shutdown().await;
                
                __result
            })
        }
    };
    
    TokenStream::from(expanded)
}

/// Extract T from Result<T, String>
fn extract_result_ok_type(sig: &syn::Signature) -> syn::Type {
    if let syn::ReturnType::Type(_, ty) = &sig.output {
        if let syn::Type::Path(type_path) = &**ty {
            if let Some(segment) = type_path.path.segments.last() {
                if segment.ident == "Result" {
                    if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(syn::GenericArgument::Type(ok_type)) = args.args.first() {
                            return ok_type.clone();
                        }
                    }
                }
            }
        }
    }
    syn::parse_quote! { String }
}

