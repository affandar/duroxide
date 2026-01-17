//! Procedural macros for duroxide durable execution framework.
//!
//! This crate provides macros that make writing durable workflows feel like
//! writing normal async Rust code.
//!
//! # Example
//!
//! ```rust,ignore
//! use duroxide::prelude::*;
//!
//! #[durable_activity]
//! async fn greet(ctx: ActivityContext, name: String) -> Result<String, String> {
//!     Ok(format!("Hello, {}!", name))
//! }
//!
//! #[durable_workflow]
//! async fn hello_workflow(ctx: OrchestrationContext, name: String) -> Result<String, String> {
//!     let greeting = activity!(ctx, greet(name)).await?;
//!     Ok(greeting)
//! }
//! ```

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, FnArg, ItemFn, Pat, Type};

/// Marks an async function as a durable activity.
///
/// This macro generates:
/// 1. The activity handler that can be registered with `ActivityRegistry`
/// 2. A `call_{name}` function for ergonomic scheduling from orchestrations
/// 3. Activity metadata for registration
///
/// # Usage
///
/// ```rust,ignore
/// #[durable_activity]
/// async fn fetch_user(ctx: ActivityContext, user_id: String) -> Result<User, String> {
///     // Your activity logic here
///     Ok(User { id: user_id, name: "Example".to_string() })
/// }
///
/// // From an orchestration, you can call it like:
/// let user = activity!(ctx, fetch_user(user_id)).await?;
/// // Or using the generated helper:
/// let user = call_fetch_user(&ctx, user_id).await?;
/// ```
///
/// # Parameters
///
/// The function must have:
/// - First parameter: `ctx: ActivityContext` (the activity execution context)
/// - Additional parameters: Inputs to the activity (must implement `Serialize + DeserializeOwned`)
/// - Return type: `Result<T, String>` where `T: Serialize`
#[proc_macro_attribute]
pub fn durable_activity(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let asyncness = &input.sig.asyncness;
    let generics = &input.sig.generics;
    let where_clause = &input.sig.generics.where_clause;
    let block = &input.block;
    let attrs = &input.attrs;
    
    // Parse function parameters
    let params: Vec<_> = input.sig.inputs.iter().collect();
    
    if params.is_empty() {
        return syn::Error::new_spanned(
            &input.sig,
            "durable_activity requires at least one parameter (ctx: ActivityContext)"
        ).to_compile_error().into();
    }
    
    // Extract parameter info
    let ctx_param = &params[0];
    let input_params: Vec<_> = params.iter().skip(1).collect();
    
    // Get return type
    let return_type = match &input.sig.output {
        syn::ReturnType::Default => {
            return syn::Error::new_spanned(
                &input.sig,
                "durable_activity requires an explicit return type"
            ).to_compile_error().into();
        }
        syn::ReturnType::Type(_, ty) => ty,
    };
    
    // Build the input struct type for serialization if there are input parameters
    let (input_struct_def, serialize_call, deserialize_call, input_params_for_call) = 
        if input_params.is_empty() {
            (
                quote! {},
                quote! { String::new() },
                quote! { () },
                quote! {},
            )
        } else {
            // Extract parameter names and types
            let param_names: Vec<_> = input_params.iter().filter_map(|p| {
                if let FnArg::Typed(pat_type) = *p
                    && let Pat::Ident(ident) = &*pat_type.pat
                {
                    return Some(ident.ident.clone());
                }
                None
            }).collect();
            
            let param_types: Vec<_> = input_params.iter().filter_map(|p| {
                if let FnArg::Typed(pat_type) = *p {
                    return Some(&*pat_type.ty);
                }
                None
            }).collect();
            
            let struct_name = format_ident!("__{}Input", fn_name);
            
            (
                quote! {
                    #[derive(::serde::Serialize, ::serde::Deserialize)]
                    struct #struct_name {
                        #( #param_names: #param_types ),*
                    }
                },
                quote! {
                    ::serde_json::to_string(&#struct_name { #( #param_names: #param_names.clone() ),* })
                        .map_err(|e| format!("serialization error: {}", e))?
                },
                quote! {
                    let __input: #struct_name = ::serde_json::from_str(&__input_str)
                        .map_err(|e| format!("deserialization error: {}", e))?;
                    #( let #param_names = __input.#param_names; )*
                },
                quote! { #( #param_names ),* },
            )
        };
    
    // Generate the call_* helper function for orchestrations
    let call_fn_name = format_ident!("call_{}", fn_name);
    let call_fn_params: Vec<_> = input_params.iter().map(|p| quote! { #p }).collect();
    
    // Generate activity name constant
    let activity_name_const = format_ident!("{}_ACTIVITY_NAME", fn_name.to_string().to_uppercase());
    
    // Generate handler function name
    let handler_fn_name = format_ident!("{}_handler", fn_name);
    
    let expanded = quote! {
        #input_struct_def
        
        /// Activity name constant for registration
        #vis const #activity_name_const: &str = #fn_name_str;
        
        /// The original activity implementation
        #(#attrs)*
        #vis #asyncness fn #fn_name #generics (#ctx_param, #( #input_params ),*) #where_clause -> #return_type 
        #block
        
        /// Activity handler wrapper for registration.
        /// This function deserializes inputs, calls the activity, and serializes outputs.
        #vis fn #handler_fn_name() -> impl Fn(::duroxide::ActivityContext, String) -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = Result<String, String>> + Send>> + Send + Sync + 'static {
            |__ctx: ::duroxide::ActivityContext, __input_str: String| {
                Box::pin(async move {
                    #deserialize_call
                    let __result = #fn_name(__ctx, #input_params_for_call).await?;
                    ::serde_json::to_string(&__result)
                        .map_err(|e| format!("serialization error: {}", e))
                })
            }
        }
        
        /// Helper function to schedule this activity from an orchestration.
        /// Returns a DurableFuture that can be awaited.
        #vis fn #call_fn_name<__Input: ::serde::Serialize>(
            __ctx: &::duroxide::OrchestrationContext,
            #( #call_fn_params ),*
        ) -> ::duroxide::DurableFuture {
            let __serialized = if ::std::mem::size_of::<(__Input,)>() == 0 {
                String::new()
            } else {
                #serialize_call
            };
            __ctx.schedule_activity(#fn_name_str, __serialized)
        }
    };
    
    expanded.into()
}

/// Marks an async function as a durable workflow (orchestration).
///
/// This macro transforms an async function into a durable orchestration that
/// can survive process restarts and failures.
///
/// # Usage
///
/// ```rust,ignore
/// #[durable_workflow]
/// async fn order_processing(ctx: OrchestrationContext, order_id: String) -> Result<String, String> {
///     // Validate order
///     let order = activity!(ctx, validate_order(order_id.clone())).await?;
///     
///     // Process payment
///     let payment = activity!(ctx, process_payment(order.clone())).await?;
///     
///     // Ship order
///     let tracking = activity!(ctx, ship_order(order)).await?;
///     
///     Ok(tracking)
/// }
/// ```
///
/// # Parameters
///
/// The function must have:
/// - First parameter: `ctx: OrchestrationContext`
/// - Second parameter: Input to the orchestration (must implement `Serialize + DeserializeOwned`)
/// - Return type: `Result<T, String>` where `T: Serialize`
#[proc_macro_attribute]
pub fn durable_workflow(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let asyncness = &input.sig.asyncness;
    let generics = &input.sig.generics;
    let where_clause = &input.sig.generics.where_clause;
    let block = &input.block;
    let attrs = &input.attrs;
    
    // Parse function parameters
    let params: Vec<_> = input.sig.inputs.iter().collect();
    
    if params.len() < 2 {
        return syn::Error::new_spanned(
            &input.sig,
            "durable_workflow requires at least two parameters (ctx: OrchestrationContext, input: T)"
        ).to_compile_error().into();
    }
    
    let ctx_param = &params[0];
    let input_param = &params[1];
    
    // Get return type
    let return_type = match &input.sig.output {
        syn::ReturnType::Default => {
            return syn::Error::new_spanned(
                &input.sig,
                "durable_workflow requires an explicit return type"
            ).to_compile_error().into();
        }
        syn::ReturnType::Type(_, ty) => ty,
    };
    
    // Extract input parameter name and type
    let (input_name, input_type) = match input_param {
        FnArg::Typed(pat_type) => {
            let name = if let Pat::Ident(ident) = &*pat_type.pat {
                &ident.ident
            } else {
                return syn::Error::new_spanned(
                    pat_type,
                    "expected identifier pattern for input parameter"
                ).to_compile_error().into();
            };
            (name, &*pat_type.ty)
        }
        _ => {
            return syn::Error::new_spanned(
                input_param,
                "expected typed parameter"
            ).to_compile_error().into();
        }
    };
    
    // Generate orchestration name constant and handler function name
    let orch_name_const = format_ident!("{}_WORKFLOW_NAME", fn_name.to_string().to_uppercase());
    let handler_fn_name = format_ident!("{}_handler", fn_name);
    
    let expanded = quote! {
        /// Workflow name constant for registration
        #vis const #orch_name_const: &str = #fn_name_str;
        
        /// The original workflow implementation
        #(#attrs)*
        #vis #asyncness fn #fn_name #generics (#ctx_param, #input_name: #input_type) #where_clause -> #return_type 
        #block
        
        /// Workflow handler wrapper for registration.
        /// This function deserializes inputs, calls the workflow, and serializes outputs.
        #vis fn #handler_fn_name() -> impl Fn(::duroxide::OrchestrationContext, String) -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = Result<String, String>> + Send>> + Send + Sync + 'static {
            |__ctx: ::duroxide::OrchestrationContext, __input_str: String| {
                Box::pin(async move {
                    let __input: #input_type = ::serde_json::from_str(&__input_str)
                        .map_err(|e| format!("deserialization error: {}", e))?;
                    let __result = #fn_name(__ctx, __input).await?;
                    ::serde_json::to_string(&__result)
                        .map_err(|e| format!("serialization error: {}", e))
                })
            }
        }
    };
    
    expanded.into()
}

// Helper to extract the Ok type from Result<T, E> - reserved for future use
#[allow(dead_code)]
fn extract_result_ok_type(ty: &Type) -> Option<&Type> {
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
        && segment.ident == "Result"
        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(syn::GenericArgument::Type(ok_type)) = args.args.first()
    {
        return Some(ok_type);
    }
    None
}
