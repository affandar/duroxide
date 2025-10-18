use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

// Activity macro
#[proc_macro_attribute]
pub fn activity(args: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);
    
    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    
    // Parse args to see if a custom name is provided
    let full_name = if args.is_empty() {
        // Default: use function name only (proc macros can't easily get calling module path)
        // Users should provide explicit names if they want namespacing
        fn_name_str.clone()
    } else {
        // Custom name provided
        let args_str = args.to_string();
        args_str.trim().trim_matches('"').to_string()
    };
    
    let registration = quote! {
        #[cfg(feature = "macros")]
        duroxide::__internal::ACTIVITIES.get_or_init(|| std::sync::Mutex::new(Vec::new())).lock().unwrap().push(duroxide::__internal::ActivityDescriptor {
            name: #full_name,
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
pub fn orchestration(args: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);
    
    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    
    // Parse args to see if a custom name is provided
    let full_name = if args.is_empty() {
        // Default: use function name only (proc macros can't easily get calling module path)
        // Users should provide explicit names if they want namespacing
        fn_name_str.clone()
    } else {
        // Custom name provided
        let args_str = args.to_string();
        args_str.trim().trim_matches('"').to_string()
    };
    
    let registration = quote! {
        #[cfg(feature = "macros")]
        duroxide::__internal::ORCHESTRATIONS.get_or_init(|| std::sync::Mutex::new(Vec::new())).lock().unwrap().push(duroxide::__internal::OrchestrationDescriptor {
            name: #full_name,
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
    
    let ctx_part_str = parts[0].trim();
    let ctx_part = syn::parse_str::<syn::Ident>(ctx_part_str).expect("Invalid context identifier");
    let call_part = parts[1].trim();
    
    // Parse the function call as a Rust expression
    let call_expr = syn::parse_str::<syn::ExprCall>(call_part).expect("Invalid function call syntax");
    let func_name = match &*call_expr.func {
        syn::Expr::Path(path) => path.path.get_ident().map(|i| i.to_string()).unwrap_or_else(|| "unknown".to_string()),
        _ => panic!("Function call must use a simple identifier"),
    };
    
    // Handle single argument case
    let args_expr = if call_expr.args.len() == 1 {
        &call_expr.args[0]
    } else {
        panic!("schedule! macro currently only supports single-argument function calls");
    };
    
    quote! {
        {
            let func_name = #func_name;
            
            // Auto-detect based on registration
            if duroxide::__internal::is_orchestration(&func_name) {
                let instance_id = format!("{}-{}", func_name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
                #ctx_part.schedule_sub_orchestration(func_name.clone(), #args_expr)
            } else {
                #ctx_part.schedule_activity(func_name.clone(), (#args_expr).to_string())
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