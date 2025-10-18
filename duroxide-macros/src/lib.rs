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
    let input_str = input.to_string();
    
    // Parse the input to extract ctx and function call
    // Expected format: schedule!(ctx, function_name(args))
    let parts: Vec<&str> = input_str.split(',').collect();
    if parts.len() != 2 {
        panic!("schedule! macro expects format: schedule!(ctx, function_name(args))");
    }
    
    let ctx_part = parts[0].trim();
    let call_part = parts[1].trim();
    
    // Extract function name and arguments
    let func_name = if let Some(paren_pos) = call_part.find('(') {
        &call_part[..paren_pos]
    } else {
        panic!("Function call must include parentheses");
    };
    
    let args_part = if let Some(start) = call_part.find('(') {
        if let Some(end) = call_part.rfind(')') {
            &call_part[start+1..end]
        } else {
            panic!("Missing closing parenthesis");
        }
    } else {
        panic!("Missing opening parenthesis");
    };
    
    quote! {
        {
            let func_name = #func_name;
            let args = (#args_part);
            let serialized_args = serde_json::to_string(&args).expect("Failed to serialize args");
            
            // Auto-detect based on registration
            if duroxide::__internal::is_orchestration(func_name) {
                let instance_id = format!("{}-{}", func_name, uuid::Uuid::new_v4());
                #ctx_part.schedule_sub_orchestration(func_name, &instance_id, serialized_args).into_sub_orchestration()
            } else {
                #ctx_part.schedule_activity(func_name, serialized_args).into_activity()
            }
        }
    }.into()
}

// Durable system calls
#[proc_macro]
pub fn durable_newguid(_input: TokenStream) -> TokenStream {
    quote! {
        {
            let ctx = duroxide::__internal::get_current_context();
            ctx.new_guid_macro()
        }
    }.into()
}

#[proc_macro]
pub fn durable_utcnow(_input: TokenStream) -> TokenStream {
    quote! {
        {
            let ctx = duroxide::__internal::get_current_context();
            ctx.utc_now_macro()
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