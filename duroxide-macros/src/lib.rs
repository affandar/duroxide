use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse::Parser,
    parse_macro_input,
    punctuated::Punctuated,
    spanned::Spanned,
    visit_mut::VisitMut,
    Expr, ExprCall, ExprPath, FnArg, Ident, Item, ItemFn, Pat, PatIdent, PathArguments, ReturnType, Stmt, Type,
    TypePath,
};

// ============================================================================
// Attribute arg parsing: #[duroxide::orchestration(name = "...")]
// ============================================================================

fn parse_name_arg(attr: TokenStream) -> Result<Option<String>, syn::Error> {
    if attr.is_empty() {
        return Ok(None);
    }
    // Parse a simple `name = "..."` list.
    let parser = Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated;
    let metas = parser.parse(attr)?;
    for meta in metas {
        if let syn::Meta::NameValue(nv) = meta
            && nv.path.is_ident("name")
        {
            if let syn::Expr::Lit(ref expr_lit) = nv.value
                && let syn::Lit::Str(s) = &expr_lit.lit
            {
                return Ok(Some(s.value()));
            }
            return Err(syn::Error::new(nv.span(), "expected name = \"...\""));
        }
    }
    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "unsupported attribute args; expected: name = \"...\"",
    ))
}

// ============================================================================
// Type helpers
// ============================================================================

fn is_path_ident(ty: &Type, ident: &str) -> bool {
    match ty {
        Type::Path(TypePath { path, .. }) => path.segments.last().is_some_and(|s| s.ident == ident),
        _ => false,
    }
}

fn extract_result_ok_type(ret: &ReturnType) -> Result<Type, syn::Error> {
    let ty = match ret {
        ReturnType::Default => {
            return Err(syn::Error::new(
                ret.span(),
                "inline activity must return Result<Ok, String>",
            ));
        }
        ReturnType::Type(_arrow, ty) => ty.as_ref().clone(),
    };

    let Type::Path(TypePath { path, .. }) = &ty else {
        return Err(syn::Error::new(ty.span(), "expected Result<Ok, String> return type"));
    };
    let Some(seg) = path.segments.last() else {
        return Err(syn::Error::new(ty.span(), "expected Result<Ok, String> return type"));
    };
    if seg.ident != "Result" {
        return Err(syn::Error::new(ty.span(), "expected Result<Ok, String> return type"));
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return Err(syn::Error::new(ty.span(), "expected Result<Ok, String> return type"));
    };
    if args.args.len() != 2 {
        return Err(syn::Error::new(ty.span(), "expected Result<Ok, String> return type"));
    }
    let ok = args.args.first().unwrap();
    let syn::GenericArgument::Type(ok_ty) = ok else {
        return Err(syn::Error::new(ok.span(), "expected Ok type"));
    };
    Ok(ok_ty.clone())
}

fn extract_ctx_ident_from_orchestration(fn_item: &ItemFn) -> Result<Ident, syn::Error> {
    let Some(first) = fn_item.sig.inputs.first() else {
        return Err(syn::Error::new(
            fn_item.sig.span(),
            "orchestration must have first argument: ctx: OrchestrationContext",
        ));
    };
    let FnArg::Typed(pat_ty) = first else {
        return Err(syn::Error::new(first.span(), "expected typed argument for ctx"));
    };
    if !is_path_ident(&pat_ty.ty, "OrchestrationContext") {
        return Err(syn::Error::new(
            pat_ty.ty.span(),
            "first argument must be OrchestrationContext",
        ));
    }
    let Pat::Ident(PatIdent { ident, .. }) = pat_ty.pat.as_ref() else {
        return Err(syn::Error::new(pat_ty.pat.span(), "ctx must be an identifier pattern"));
    };
    Ok(ident.clone())
}

// ============================================================================
// Inline activity extraction & call rewriting
// ============================================================================

#[derive(Clone)]
struct InlineActivity {
    orig_ident: Ident,
    input_ty: Type,
    ok_ty: Type,
    wants_act_ctx: bool,
    ctx_pat: Option<Pat>,
    input_pat: Pat,
    body: syn::Block,
}

fn take_inline_activities(fn_item: &mut ItemFn) -> Result<Vec<InlineActivity>, syn::Error> {
    let mut found = Vec::new();
    let mut new_stmts: Vec<Stmt> = Vec::with_capacity(fn_item.block.stmts.len());

    for stmt in std::mem::take(&mut fn_item.block.stmts) {
        match stmt {
            Stmt::Item(Item::Fn(inner_fn)) => {
                let is_inline = inner_fn.attrs.iter().any(|a| {
                    a.path()
                        .segments
                        .last()
                        .is_some_and(|seg| seg.ident == "inline_activity")
                });
                if !is_inline {
                    new_stmts.push(Stmt::Item(Item::Fn(inner_fn)));
                    continue;
                }

                if inner_fn.sig.asyncness.is_none() {
                    return Err(syn::Error::new(
                        inner_fn.sig.span(),
                        "inline activity must be an async fn",
                    ));
                }

                // Allow:
                // - async fn foo() -> Result<Ok, String>
                // - async fn foo(arg: In) -> Result<Ok, String>
                // - async fn foo(ctx: ActivityContext) -> Result<Ok, String>
                // - async fn foo(ctx: ActivityContext, arg: In) -> Result<Ok, String>
                let mut inputs = inner_fn.sig.inputs.iter();
                let first = inputs.next();
                let second = inputs.next();
                let third = inputs.next();
                if third.is_some() {
                    return Err(syn::Error::new(
                        inner_fn.sig.span(),
                        "inline activity supports at most 2 parameters (optional ActivityContext + optional single input)",
                    ));
                }

                let (wants_act_ctx, ctx_pat, input_pat, input_ty) = match (first, second) {
                    (None, None) => (false, None, syn::parse_quote! { _ }, syn::parse_quote! { () }),
                    (Some(FnArg::Typed(pat_ty)), None) => {
                        if is_path_ident(&pat_ty.ty, "ActivityContext") {
                            (
                                true,
                                Some(pat_ty.pat.as_ref().clone()),
                                syn::parse_quote! { _ },
                                syn::parse_quote! { () },
                            )
                        } else {
                            (false, None, pat_ty.pat.as_ref().clone(), (*pat_ty.ty).clone())
                        }
                    }
                    (Some(FnArg::Typed(p1)), Some(FnArg::Typed(p2))) => {
                        if !is_path_ident(&p1.ty, "ActivityContext") {
                            return Err(syn::Error::new(
                                p1.ty.span(),
                                "first parameter must be ActivityContext if two parameters are used",
                            ));
                        }
                        (
                            true,
                            Some(p1.pat.as_ref().clone()),
                            p2.pat.as_ref().clone(),
                            (*p2.ty).clone(),
                        )
                    }
                    (Some(FnArg::Receiver(r)), _) => {
                        return Err(syn::Error::new(r.span(), "inline activity cannot take self"));
                    }
                    _ => {
                        return Err(syn::Error::new(
                            inner_fn.sig.span(),
                            "unsupported inline activity signature",
                        ));
                    }
                };

                let ok_ty = extract_result_ok_type(&inner_fn.sig.output)?;

                found.push(InlineActivity {
                    orig_ident: inner_fn.sig.ident.clone(),
                    input_ty,
                    ok_ty,
                    wants_act_ctx,
                    ctx_pat,
                    input_pat,
                    body: *inner_fn.block.clone(),
                });
                // Do not keep this inner function in the orchestration body.
            }
            other => new_stmts.push(other),
        }
    }

    fn_item.block.stmts = new_stmts;
    Ok(found)
}

fn rewrite_inline_activity_calls(
    block: &mut syn::Block,
    ctx_ident: &Ident,
    orch_ident: &Ident,
    inline_acts: &[InlineActivity],
) {
    struct Rewriter<'a> {
        ctx_ident: &'a Ident,
        orch_ident: &'a Ident,
        inline_acts: &'a [InlineActivity],
    }

    impl syn::visit_mut::VisitMut for Rewriter<'_> {
        fn visit_expr_mut(&mut self, node: &mut Expr) {
            syn::visit_mut::visit_expr_mut(self, node);

            let Expr::Call(ExprCall { func, args, .. }) = node else {
                return;
            };
            let Expr::Path(ExprPath { path, .. }) = func.as_ref() else {
                return;
            };
            if path.segments.len() != 1 {
                return;
            }
            let ident = &path.segments[0].ident;
            let Some(act) = self.inline_acts.iter().find(|a| a.orig_ident == *ident) else {
                return;
            };

            let act_name = format!("{}::{}", self.orch_ident, act.orig_ident);
            let ctx = self.ctx_ident;
            let in_ty = &act.input_ty;
            let out_ty = &act.ok_ty;

            let replacement = if args.is_empty() {
                quote! {
                    #ctx
                        .schedule_activity_typed::<#in_ty, #out_ty>(#act_name, &())
                        .into_activity_typed::<#out_ty>()
                }
            } else if args.len() == 1 {
                let arg0 = args.first().unwrap();
                let tmp = format_ident!("__duroxide_inline_arg_{}", act.orig_ident);
                quote! {
                    {
                        let #tmp = #arg0;
                        #ctx
                            .schedule_activity_typed::<#in_ty, #out_ty>(#act_name, &#tmp)
                            .into_activity_typed::<#out_ty>()
                    }
                }
            } else {
                // Keep as-is; typechecking will error, but we don't want to break random code.
                return;
            };

            *node = syn::parse2(replacement).expect("replacement expr must parse");
        }
    }

    let mut rw = Rewriter {
        ctx_ident,
        orch_ident,
        inline_acts,
    };
    rw.visit_block_mut(block);
}

fn expand_inline_activity_wrappers(orch_ident: &Ident, inline_acts: &[InlineActivity]) -> proc_macro2::TokenStream {
    let mut out = proc_macro2::TokenStream::new();
    for act in inline_acts {
        let orig = &act.orig_ident;
        let wrapper_ident = format_ident!("__duroxide_inline_activity_{}_{}", orch_ident, orig);
        let in_ty = &act.input_ty;
        let ok_ty = &act.ok_ty;
        let body = &act.body;
        let act_name = format!("{}::{}", orch_ident, orig);
        let input_pat = &act.input_pat;

        let wrapper = if act.wants_act_ctx {
            let ctx_pat = act
                .ctx_pat
                .as_ref()
                .cloned()
                .unwrap_or_else(|| syn::parse_quote! { ctx });
            quote! {
                #[::duroxide::activity(name = #act_name)]
                #[allow(non_snake_case)]
                async fn #wrapper_ident(
                    #ctx_pat: ::duroxide::ActivityContext,
                    #input_pat: #in_ty
                ) -> ::std::result::Result<#ok_ty, ::std::string::String> {
                    #body
                }
            }
        } else {
            quote! {
                #[::duroxide::activity(name = #act_name)]
                #[allow(non_snake_case)]
                async fn #wrapper_ident(
                    _ctx: ::duroxide::ActivityContext,
                    #input_pat: #in_ty
                ) -> ::std::result::Result<#ok_ty, ::std::string::String> {
                    #body
                }
            }
        };

        out.extend(wrapper);
    }
    out
}

// ============================================================================
// Public macros
// ============================================================================

#[proc_macro_attribute]
pub fn inline_activity(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // Marker attribute. The orchestration macro consumes these inner fns.
    item
}

#[proc_macro_attribute]
pub fn activity(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut fn_item = parse_macro_input!(item as ItemFn);
    let name = match parse_name_arg(attr) {
        Ok(Some(n)) => n,
        Ok(None) => fn_item.sig.ident.to_string(),
        Err(e) => return e.to_compile_error().into(),
    };

    if fn_item.sig.asyncness.is_none() {
        return syn::Error::new(fn_item.sig.span(), "activity must be an async fn")
            .to_compile_error()
            .into();
    }

    // Transform the user's function into:
    // - an internal activity handler (same signature/body, renamed)
    // - a scheduling stub (same original name) callable from orchestrations
    //
    // This supports your "activities are outside the orchestration fn" assumption,
    // while keeping ergonomics close to normal async Rust (`greet(&ctx, input).await?`).
    let orig_ident = fn_item.sig.ident.clone();
    let impl_ident = format_ident!("__duroxide_activity_impl_{}", orig_ident);
    fn_item.sig.ident = impl_ident.clone();

    let (stub_input_pat, stub_input_ty) = match fn_item.sig.inputs.len() {
        0 => (syn::parse_quote! { _input }, syn::parse_quote! { () }),
        1 => {
            let FnArg::Typed(p) = fn_item.sig.inputs.first().unwrap() else {
                return syn::Error::new(fn_item.sig.inputs.span(), "activity cannot take self")
                    .to_compile_error()
                    .into();
            };
            (p.pat.as_ref().clone(), (*p.ty).clone())
        }
        2 => {
            // Expect (ActivityContext, In)
            let first = fn_item.sig.inputs.first().unwrap();
            let second = fn_item.sig.inputs.iter().nth(1).unwrap();
            let FnArg::Typed(first) = first else {
                return syn::Error::new(first.span(), "activity cannot take self")
                    .to_compile_error()
                    .into();
            };
            if !is_path_ident(&first.ty, "ActivityContext") {
                return syn::Error::new(
                    first.ty.span(),
                    "if two parameters are used, the first must be ActivityContext",
                )
                .to_compile_error()
                .into();
            }
            let FnArg::Typed(second) = second else {
                return syn::Error::new(second.span(), "activity cannot take self")
                    .to_compile_error()
                    .into();
            };
            (second.pat.as_ref().clone(), (*second.ty).clone())
        }
        _ => {
            return syn::Error::new(
                fn_item.sig.inputs.span(),
                "activity supports 0 args, 1 arg, or (ActivityContext, 1 arg)",
            )
            .to_compile_error()
            .into();
        }
    };

    let out_ty = match extract_result_ok_type(&fn_item.sig.output) {
        Ok(t) => t,
        Err(e) => return e.to_compile_error().into(),
    };

    let register_fn = format_ident!("__duroxide_register_activity_{}", orig_ident);
    let tmp_ident = format_ident!("__duroxide_activity_arg_{}", orig_ident);
    let durable_fn_ident = format_ident!("{}_future", orig_ident);

    let expanded = quote! {
        #[allow(non_snake_case)]
        #fn_item

        #[allow(non_snake_case)]
        pub fn #orig_ident(
            ctx: &::duroxide::OrchestrationContext,
            #stub_input_pat: #stub_input_ty
        ) -> impl ::std::future::Future<Output = ::std::result::Result<#out_ty, ::std::string::String>> + '_ {
            async move {
                let #tmp_ident = #stub_input_pat;
                ctx.schedule_activity_typed::<#stub_input_ty, #out_ty>(#name, &#tmp_ident)
                    .into_activity_typed::<#out_ty>()
                    .await
            }
        }

        /// Durable (composable) variant for `join`/`select`.
        ///
        /// Use `.into_activity()` / `.into_activity_typed()` to await.
        #[allow(non_snake_case)]
        pub fn #durable_fn_ident(
            ctx: &::duroxide::OrchestrationContext,
            #stub_input_pat: #stub_input_ty
        ) -> ::duroxide::DurableFuture {
            let #tmp_ident = #stub_input_pat;
            ctx.schedule_activity_typed::<#stub_input_ty, #out_ty>(#name, &#tmp_ident)
        }

        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #register_fn(
            builder: ::duroxide::runtime::registry::ActivityRegistryBuilder
        ) -> ::duroxide::runtime::registry::ActivityRegistryBuilder {
            builder.register_typed(#name, #impl_ident)
        }

        ::duroxide::inventory::submit! {
            ::duroxide::auto_registry::ActivityAutoReg {
                register: #register_fn
            }
        }
    };

    expanded.into()
}

#[proc_macro_attribute]
pub fn orchestration(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut fn_item = parse_macro_input!(item as ItemFn);
    let name = match parse_name_arg(attr) {
        Ok(Some(n)) => n,
        Ok(None) => fn_item.sig.ident.to_string(),
        Err(e) => return e.to_compile_error().into(),
    };

    if fn_item.sig.asyncness.is_none() {
        return syn::Error::new(fn_item.sig.span(), "orchestration must be an async fn")
            .to_compile_error()
            .into();
    }

    let ctx_ident = match extract_ctx_ident_from_orchestration(&fn_item) {
        Ok(id) => id,
        Err(e) => return e.to_compile_error().into(),
    };

    let orch_ident = fn_item.sig.ident.clone();

    let inline_acts = match take_inline_activities(&mut fn_item) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };

    rewrite_inline_activity_calls(&mut fn_item.block, &ctx_ident, &orch_ident, &inline_acts);
    let inline_wrappers = expand_inline_activity_wrappers(&orch_ident, &inline_acts);

    let register_fn = format_ident!("__duroxide_register_orchestration_{}", orch_ident);
    let orch_fn_ident = &fn_item.sig.ident;

    let expanded = quote! {
        #inline_wrappers

        #[allow(non_snake_case)]
        #fn_item

        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #register_fn(
            builder: ::duroxide::runtime::registry::OrchestrationRegistryBuilder
        ) -> ::duroxide::runtime::registry::OrchestrationRegistryBuilder {
            builder.register_typed(#name, #orch_fn_ident)
        }

        ::duroxide::inventory::submit! {
            ::duroxide::auto_registry::OrchestrationAutoReg {
                register: #register_fn
            }
        }
    };

    expanded.into()
}

