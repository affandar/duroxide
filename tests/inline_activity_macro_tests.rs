use duroxide::{ActivityContext, OrchestrationContext};

#[duroxide::activity(name = "InlineMacroHello::greet")]
async fn greet(_ctx: ActivityContext, name: String) -> Result<String, String> {
    Ok(format!("Hello, {name}!"))
}

#[duroxide::orchestration]
async fn InlineMacroHello(ctx: OrchestrationContext, name: String) -> Result<String, String> {
    let greeting = greet(&ctx, name).await?;
    ctx.trace_info(format!("greeting = {greeting}"));
    Ok(greeting)
}

#[duroxide::activity]
async fn InlineMacroStandaloneActivity(_ctx: ActivityContext, input: String) -> Result<String, String> {
    Ok(format!("standalone:{input}"))
}

#[test]
fn auto_registries_include_macro_items() {
    let activities = duroxide::auto_registry::auto_activities();
    let orchestrations = duroxide::auto_registry::auto_orchestrations();

    assert!(orchestrations.has("InlineMacroHello"));
    assert!(activities.has("InlineMacroHello::greet"));
    assert!(activities.has("InlineMacroStandaloneActivity"));
}

