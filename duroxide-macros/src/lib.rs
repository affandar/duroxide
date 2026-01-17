use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn, FnArg};

#[proc_macro_attribute]
pub fn activity(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let func_name = &input.sig.ident;
    let func_name_str = func_name.to_string();
    
    let args = &input.sig.inputs;
    
    // Check arguments. Expected: ctx, input
    let has_input = args.len() == 2;
    
    let input_type = if has_input {
        match &args[1] {
            FnArg::Typed(pat_type) => Some(&pat_type.ty),
            _ => None
        }
    } else {
        None
    };

    let factory_body = if let Some(in_ty) = input_type {
        // Typed input
        quote! {
            std::sync::Arc::new(duroxide::runtime::FnActivity(
                move |ctx: duroxide::ActivityContext, input_str: String| {
                    async move {
                        let input: #in_ty = duroxide::internal::serde_json::from_str(&input_str)
                            .map_err(|e| e.to_string())?;
                        let result = #func_name(ctx, input).await?;
                        duroxide::internal::serde_json::to_string(&result)
                            .map_err(|e| e.to_string())
                    }
                }
            ))
        }
    } else {
         // Assume raw String input or no input?
         // If signature is fn(ctx, String), it works directly.
         quote! {
             std::sync::Arc::new(duroxide::runtime::FnActivity(
                 move |ctx: duroxide::ActivityContext, input_str: String| {
                     async move {
                         // Assuming fn(ctx, String) -> Result<String, String>
                         #func_name(ctx, input_str).await
                     }
                 }
             ))
         }
    };

    let expanded = quote! {
        #input
        
        const _: () = {
            use duroxide::runtime::registry::ActivityRegistration;
            
            duroxide::internal::inventory::submit! {
                ActivityRegistration {
                    name: #func_name_str,
                    factory: || {
                        #factory_body
                    }
                }
            }
        };
    };
    
    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn orchestration(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let func_name = &input.sig.ident;
    let func_name_str = func_name.to_string();
    
    // Assume typed input
    let args = &input.sig.inputs;
    let input_type = if args.len() == 2 {
        match &args[1] {
             FnArg::Typed(pat_type) => Some(&pat_type.ty),
             _ => None
        }
    } else {
        None
    };
    
    let factory_body = if let Some(in_ty) = input_type {
         quote! {
            std::sync::Arc::new(duroxide::runtime::FnOrchestration(
                move |ctx: duroxide::OrchestrationContext, input_str: String| {
                    async move {
                         let input: #in_ty = duroxide::internal::serde_json::from_str(&input_str)
                            .map_err(|e| e.to_string())?;
                         let result = #func_name(ctx, input).await?;
                         duroxide::internal::serde_json::to_string(&result)
                            .map_err(|e| e.to_string())
                    }
                }
            ))
         }
    } else {
         quote! {
            std::sync::Arc::new(duroxide::runtime::FnOrchestration(
                move |ctx: duroxide::OrchestrationContext, input_str: String| {
                    async move {
                        #func_name(ctx, input_str).await
                    }
                }
            ))
         }
    };
    
    let expanded = quote! {
        #input
        
        const _: () = {
            use duroxide::runtime::registry::OrchestrationRegistration;
            
            duroxide::internal::inventory::submit! {
                OrchestrationRegistration {
                    name: #func_name_str,
                    factory: || {
                        #factory_body
                    }
                }
            }
        };
    };
    
    TokenStream::from(expanded)
}
