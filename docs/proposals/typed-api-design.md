# Typed API Design Proposal (Activities, Orchestrations, External Events)

## Problem

Call sites are currently stringly-typed (names) with ad-hoc serialization calls. We want compile-time typing for inputs/outputs of activities, orchestrations, and external events.

## Goals
- Typed inputs/outputs at call sites with minimal boilerplate.
- Keep existing string APIs for interop and gradual migration.
- No runtime overhead beyond existing JSON encode/decode.

## Design A: Traits + small extensions (no macros)

- Traits
```rust
pub trait ActivityDef { type In: Serialize; type Out: DeserializeOwned; const NAME: &'static str; }
pub trait ExternalEventDef { type Payload: DeserializeOwned; const NAME: &'static str; }
pub trait OrchestrationDef { type In: DeserializeOwned + Send + 'static; type Out: Serialize + Send + 'static; const NAME: &'static str; fn run(ctx: OrchestrationContext, input: Self::In) -> impl Future<Output = Result<Self::Out, String>> + Send; }
```

- OrchestrationContext extensions
```rust
impl OrchestrationContext {
    pub fn call<A: ActivityDef>(&self, input: &A::In) -> impl Future<Output = Result<A::Out, String>> { /* schedule_activity_typed */ }
    pub fn wait<E: ExternalEventDef>(&self) -> impl Future<Output = E::Payload> { /* schedule_wait_typed */ }
    pub fn call_sub<O: OrchestrationDef>(&self, input: &O::In) -> impl Future<Output = Result<O::Out, String>> { /* schedule_sub_orchestration_typed */ }
}
```

- Registry helpers
```rust
ActivityRegistryBuilder::register_activity::<A: ActivityDef>(handler)
OrchestrationRegistryBuilder::register_orchestration::<O: OrchestrationDef>()
```

- Runtime helper
```rust
Runtime::start_typed::<O: OrchestrationDef>(instance, input)
```

- Usage example
```rust
struct SendEmail; impl ActivityDef for SendEmail { type In = EmailReq; type Out = EmailRes; const NAME: &'static str = "SendEmail"; }
struct PaymentApproved; impl ExternalEventDef for PaymentApproved { type Payload = String; const NAME: &'static str = "PaymentApproved"; }
struct Checkout; impl OrchestrationDef for Checkout { /* run(ctx, input) */ }

let res = ctx.call::<SendEmail>(&req).await?;
let approval: String = ctx.wait::<PaymentApproved>().await;
let h = rt.clone().start_typed::<Checkout>("inst-1", input).await?;
```

Pros: No macros, explicit names in one place, easy migration. Cons: Slight boilerplate to define zero-sized marker types.

## Design B: Attribute macros (ergonomics)

- #[activity(name="SendEmail")] async fn send_email(input: EmailReq) -> Result<EmailRes, String> { }
- #[orchestration(name="Checkout")] async fn checkout(ctx: OrchestrationContext, input: In) -> Result<Out, String> { }
- #[external_event(name="PaymentApproved")] struct PaymentApproved(String);

Macros generate the marker types, trait impls, and helpers like `ctx.send_email(&req)` and `rt.start_checkout(...)`.

Pros: Very ergonomic call sites. Cons: Requires a proc-macro crate and maintenance.

## Design C: Const-generic names (not recommended)

- Use `struct A<const NAME: &'static str, In, Out>` and call as `ctx.call::<A<"SendEmail", EmailReq, EmailRes>>(&req)`.
- Current limitations and ergonomics make this less attractive.

## Error handling & versioning
- Names act as ABI; keep `NAME` stable or version them (e.g., "SendEmail.v2").
- Typed wrappers rely on existing `*_typed` adapters; decode on success only.
- External senders may still raise by string name; recommend putting the name on the typed definition to avoid drift.

## Migration plan
- Land Design A first (traits + helpers) behind feature flags or as additive APIs.
- Convert a couple of samples and one e2e test to typed usage.
- Consider Design B later for ergonomics.

## Open questions
- Should events use `Result<T, String>` rather than panic on decode error? (symmetry with activities)
- Provide derive macros to generate marker types from plain types?
