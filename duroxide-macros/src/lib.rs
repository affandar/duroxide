use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

// Activity macro
#[proc_macro_attribute]
pub fn activity(_args: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);
    
    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    
    let registration = quote! {
        #[cfg(feature = "macros")]
        duroxide::__internal::ACTIVITIES.get_or_init(|| std::sync::Mutex::new(Vec::new())).lock().unwrap().push(duroxide::__internal::ActivityDescriptor {
            name: #fn_name_str,
            invoke: |input: String| {
                Box::pin(async move {
                    #fn_name(input).await
                })
            },
        });
    };
    
    quote! {
        #input_fn
        
        #registration
    }.into()
}

// Orchestration macro
#[proc_macro_attribute]
pub fn orchestration(_args: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);
    
    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    
    let registration = quote! {
        #[cfg(feature = "macros")]
        duroxide::__internal::ORCHESTRATIONS.get_or_init(|| std::sync::Mutex::new(Vec::new())).lock().unwrap().push(duroxide::__internal::OrchestrationDescriptor {
            name: #fn_name_str,
            version: "1.0.0",
            invoke: |ctx: duroxide::OrchestrationContext, input: String| {
                Box::pin(async move {
                    #fn_name(ctx, input).await
                })
            },
        });
    };
    
    quote! {
        #input_fn
        
        #registration
    }.into()
}

// Schedule macro
#[proc_macro]
pub fn schedule(input: TokenStream) -> TokenStream {
    // For now, just pass through the input as a placeholder
    // This will be implemented properly once we have the basic structure working
    quote! {
        {
            // Placeholder implementation
            todo!("Schedule macro not yet implemented")
        }
    }.into()
}

// Durable system calls
#[proc_macro]
pub fn durable_newguid(_input: TokenStream) -> TokenStream {
    quote! {
        {
            let ctx = duroxide::__internal::get_current_context();
            ctx.new_guid_macro().await
        }
    }.into()
}

#[proc_macro]
pub fn durable_utcnow(_input: TokenStream) -> TokenStream {
    quote! {
        {
            let ctx = duroxide::__internal::get_current_context();
            ctx.utc_now_macro().await
        }
    }.into()
}

#[proc_macro]
pub fn durable_trace_info(input: TokenStream) -> TokenStream {
    let input_str = input.to_string();
    quote! {
        {
            let ctx = duroxide::__internal::get_current_context();
            ctx.trace_info(format!(#input_str));
        }
    }.into()
}

#[proc_macro]
pub fn durable_trace_warn(input: TokenStream) -> TokenStream {
    let input_str = input.to_string();
    quote! {
        {
            let ctx = duroxide::__internal::get_current_context();
            ctx.trace_warn(format!(#input_str));
        }
    }.into()
}

#[proc_macro]
pub fn durable_trace_error(input: TokenStream) -> TokenStream {
    let input_str = input.to_string();
    quote! {
        {
            let ctx = duroxide::__internal::get_current_context();
            ctx.trace_error(format!(#input_str));
        }
    }.into()
}

#[proc_macro]
pub fn durable_trace_debug(input: TokenStream) -> TokenStream {
    let input_str = input.to_string();
    quote! {
        {
            let ctx = duroxide::__internal::get_current_context();
            ctx.trace_debug(format!(#input_str));
        }
    }.into()
}