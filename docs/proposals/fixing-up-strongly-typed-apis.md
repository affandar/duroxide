# Proposal: Fixing Up Strongly-Typed APIs

**Status:** Draft  
**Created:** 2026-02-21

---

## Summary

Add a user-defined error type `E` to every `_typed` API surface in duroxide:
activity registration, activity scheduling, orchestration registration,
orchestration scheduling, and the Client APIs (`wait_for_orchestration_typed`,
`get_orchestration_status`). This uses the same JSON codec trick that already
works for `In`/`Out` — the runtime stays stringly-typed on the wire, and `E`
is encoded/decoded at the boundaries.

---

## Motivation

Today, all `_typed` variants handle `In` and `Out` via serde, but errors are
always `String`. Users must stringify errors at activity boundaries and
string-match to recover structure in orchestrations — fragile, un-idiomatic,
and impossible to refactor safely.

```rust
// Today: must stringify errors
.register_typed("ChargeCard", |_ctx, order: Order| async move {
    Err(format!("card_declined:{}", reason))  // ← lose structure
})

// Today: must string-match to recover
match result {
    Err(msg) if msg.starts_with("card_declined:") => { ... }  // ← fragile
}
```

The same gap exists at the Client level:

```rust
// Today: wait_for_orchestration_typed returns Result<Out, String>
let result: Result<Receipt, String> =
    client.wait_for_orchestration_typed("order-1", timeout).await?;

// Today: get_orchestration_status returns ErrorDetails (runtime type)
match client.get_orchestration_status("order-1").await? {
    OrchestrationStatus::Failed { details, .. } => {
        let msg: String = details.display_message();  // ← lose structure
    }
}
```

---

## Design

### Core idea

Apply the same JSON codec trick to errors. The `String` on the wire contains
serialized JSON of the user's error type. The runtime never sees the difference —
it's still `String` at every internal layer.

### Error layering (unchanged)

Duroxide has a two-tier error model for activities:

| Tier | Examples | Handled by | Reaches user code? |
|---|---|---|---|
| **Infrastructure** | Provider failures, lock expiration, poison messages, panics | Worker dispatcher (abandon/retry) | Never |
| **Application** | Activity returns `Err(...)` | Stored as `ErrorDetails::Application { message }`, delivered via `CompletionResult::ActivityErr(String)` | Always |

This proposal **only affects the application tier**. Infrastructure errors
continue to be absorbed by the runtime. The `ErrorDetails` envelope,
`CompletionResult`, `DurableFuture` polling — all unchanged.

```
User returns:     Err(PaymentError::CardDeclined { reason: "expired" })
                         ↓  Json::encode (in register_typed wrapper)
Wire:             Err("{\"CardDeclined\":{\"reason\":\"expired\"}}")
                         ↓
Worker wraps:     ErrorDetails::Application {
                      kind: ActivityFailed,         ← runtime sets this
                      message: "{\"CardDeclined\":{\"reason\":\"expired\"}}",
                      retryable: false,             ← runtime sets this
                  }
                         ↓  (stored in event history — unchanged)
Replay unwraps:   details.display_message() → "{\"CardDeclined\":{...}}"
                         ↓
CompletionResult: ActivityErr("{\"CardDeclined\":{...}}")
                         ↓
DurableFuture:    Err("{\"CardDeclined\":{...}}")
                         ↓  Json::decode (in schedule_activity_typed wrapper)
User sees:        Err(PaymentError::CardDeclined { reason: "expired" })
```

The same flow applies for orchestration errors through
`OrchestrationFailed.details.message` and for Client APIs through
`OrchestrationStatus::Failed { details }`.

---

## API changes

### Part 1: Activity registration and scheduling

#### Registration

```rust
// Before:
pub fn register_typed<In, Out, F, Fut>(self, name: &str, f: F) -> Self
where
    Fut: Future<Output = Result<Out, String>> + ...;

// After:
pub fn register_typed<In, Out, E, F, Fut>(self, name: &str, f: F) -> Self
where
    E: Serialize + Debug + Send + 'static,                // NEW
    Fut: Future<Output = Result<Out, E>> + ...;           // E replaces String
```

Wrapper change:

```rust
// Before:
let out: Out = (f)(ctx, input).await?;
Json::encode(&out)

// After:
match (f)(ctx, input).await {
    Ok(out) => Json::encode(&out),
    Err(e) => Err(Json::encode(&e).unwrap_or_else(|_| format!("{e:?}"))),
}
```

#### Scheduling

```rust
// Before:
pub fn schedule_activity_typed<In, Out>(...) -> impl Future<Output = Result<Out, String>>

// After:
pub fn schedule_activity_typed<In, Out, E>(...) -> impl Future<Output = Result<Out, E>>
where E: DeserializeOwned;
```

Wrapper change:

```rust
// Before:
let s = fut.await?;
Json::decode::<Out>(&s)

// After:
match fut.await {
    Ok(s) => Ok(Json::decode::<Out>(&s)?),
    Err(s) => Err(Json::decode::<E>(&s)?),
}
```

### Part 2: Orchestration registration and scheduling

#### Registration

```rust
// Before:
pub fn register_typed<In, Out, F, Fut>(self, name: &str, f: F) -> Self
where
    Fut: Future<Output = Result<Out, String>> + ...;

pub fn register_versioned_typed<In, Out, F, Fut>(self, name: &str, version: &str, f: F) -> Self
where
    Fut: Future<Output = Result<Out, String>> + ...;

// After — both gain E:
pub fn register_typed<In, Out, E, F, Fut>(self, name: &str, f: F) -> Self
where
    E: Serialize + Debug + Send + 'static,
    Fut: Future<Output = Result<Out, E>> + ...;

pub fn register_versioned_typed<In, Out, E, F, Fut>(self, name: &str, version: &str, f: F) -> Self
where
    E: Serialize + Debug + Send + 'static,
    Fut: Future<Output = Result<Out, E>> + ...;
```

The wrapper encodes `Err(E)` to `Err(String)` identically to activities.
The runtime stores it in `OrchestrationFailed { details: ErrorDetails::Application { message } }`.

#### Sub-orchestration scheduling

```rust
// Before:
pub fn schedule_orchestration_typed<In>(...) -> impl Future<Output = Result<String, String>>
pub fn schedule_orchestration_versioned_typed<In>(...) -> impl Future<Output = Result<String, String>>

// After — gain Out and E:
pub fn schedule_orchestration_typed<In, Out, E>(...) -> impl Future<Output = Result<Out, E>>
where Out: DeserializeOwned, E: DeserializeOwned;

pub fn schedule_orchestration_versioned_typed<In, Out, E>(...) -> impl Future<Output = Result<Out, E>>
where Out: DeserializeOwned, E: DeserializeOwned;
```

### Part 3: Client APIs

#### `wait_for_orchestration_typed`

```rust
// Before:
pub async fn wait_for_orchestration_typed<Out>(...) -> Result<Result<Out, String>, ClientError>

// After:
pub async fn wait_for_orchestration_typed<Out, E>(...) -> Result<Result<Out, E>, ClientError>
where Out: DeserializeOwned, E: DeserializeOwned;
```

Implementation change:

```rust
// Before:
OrchestrationStatus::Failed { details, .. } => Ok(Err(details.display_message())),

// After:
OrchestrationStatus::Failed { details, .. } => {
    let msg = details.display_message();
    match Json::decode::<E>(&msg) {
        Ok(e) => Ok(Err(e)),
        Err(decode_err) => Err(ClientError::InvalidInput {
            message: format!("error decode failed: {decode_err}"),
        }),
    }
}
```

#### `get_orchestration_status_typed` (new method)

New typed variant that decodes both output and error:

```rust
pub async fn get_orchestration_status_typed<Out, E>(
    &self,
    instance: &str,
) -> Result<OrchestrationStatusTyped<Out, E>, ClientError>
where
    Out: DeserializeOwned,
    E: DeserializeOwned;
```

With a new typed status enum:

```rust
pub enum OrchestrationStatusTyped<Out, E> {
    NotFound,
    Running {
        custom_status: Option<String>,
        custom_status_version: u64,
    },
    Completed {
        output: Out,
        custom_status: Option<String>,
        custom_status_version: u64,
    },
    Failed {
        error: E,
        custom_status: Option<String>,
        custom_status_version: u64,
    },
}
```

This is a new method (not modifying the existing `get_orchestration_status`),
since the return type is a different enum. The untyped method remains unchanged.

#### `start_orchestration_typed` (unchanged)

Already takes `In: Serialize` — no error type needed since starting an
orchestration doesn't return a result.

---

## Full scope of changes

| Method | Before | After |
|---|---|---|
| **Activity Registration** | | |
| `ActivityRegistryBuilder::register_typed` | `<In, Out>` | `<In, Out, E>` |
| **Activity Scheduling** | | |
| `OrchestrationContext::schedule_activity_typed` | `<In, Out>` | `<In, Out, E>` |
| `OrchestrationContext::schedule_activity_on_session_typed` | `<In, Out>` | `<In, Out, E>` |
| `OrchestrationContext::schedule_activity_with_retry_typed` | `<In, Out>` | `<In, Out, E>` |
| `OrchestrationContext::schedule_activity_with_retry_on_session_typed` | `<In, Out>` | `<In, Out, E>` |
| **Orchestration Registration** | | |
| `OrchestrationRegistryBuilder::register_typed` | `<In, Out>` | `<In, Out, E>` |
| `OrchestrationRegistryBuilder::register_versioned_typed` | `<In, Out>` | `<In, Out, E>` |
| **Sub-Orchestration Scheduling** | | |
| `OrchestrationContext::schedule_orchestration_typed` | `<In>` | `<In, Out, E>` |
| `OrchestrationContext::schedule_orchestration_versioned_typed` | `<In>` | `<In, Out, E>` |
| **Client APIs** | | |
| `Client::wait_for_orchestration_typed` | `<Out>` | `<Out, E>` |
| `Client::get_orchestration_status_typed` | — (new) | `<Out, E>` |
| `Client::start_orchestration_typed` | `<In>` | `<In>` (unchanged) |

---

## What changes and what doesn't

| Component | Changes? | Details |
|---|---|---|
| `_typed` method signatures | **Yes** | Add `E` type parameter |
| `_typed` wrappers | **Yes** | Encode/decode errors via `Json` |
| `OrchestrationStatusTyped<Out, E>` | **Yes** | New typed enum for `get_orchestration_status_typed` |
| `_typed_codec::Json` | No | Already handles arbitrary serde types |
| Wire format / `EventKind` | No | Still `String` in all event payloads |
| `ErrorDetails` | No | Still wraps `message: String` |
| `CompletionResult` | No | Still `ActivityErr(String)` / `SubOrchErr(String)` |
| `DurableFuture` | No | Still yields `Result<String, String>` |
| Provider trait | No | Pure storage |
| Replay engine | No | Calls `display_message()` → `String` as before |
| Rolling upgrade safety | No risk | Old runtimes see JSON strings — harmless |

---

## Backward compatibility

### Wire compatibility

**Fully wire compatible.** The event history format doesn't change. A typed
error stored by a new runtime is just a JSON string in the `message` field —
readable by any runtime version.

### Source compatibility — breaking change (intentional)

Adding `E` to existing methods breaks turbofish call sites:

```rust
// Before (2 type params):
ctx.schedule_activity_typed::<AddReq, AddRes>("Add", &req).await?

// After (3 type params — must add String):
ctx.schedule_activity_typed::<AddReq, AddRes, String>("Add", &req).await?
```

This is an intentional breaking change. The alternative — adding `_typed_err`
variants — would further explode the already-large method surface. The migration
is mechanical: add `, String` to every turbofish site. Non-turbofish call sites
(using type annotation or inference) are unaffected:

```rust
// These still work unchanged:
let receipt: Receipt = ctx.schedule_activity_typed("Charge", &order).await?;
// Rust infers <_, Receipt, String> from the Result<Receipt, String> context
```

### Migration

```bash
# Find all turbofish call sites:
grep -rn 'schedule_activity_typed::<\|register_typed::<\|schedule_orchestration_typed::<\|wait_for_orchestration_typed::<'

# Add `, String` as the last type param at each site. Behavior is identical.
```

---

## Example: end-to-end usage

```rust
#[derive(Serialize, Deserialize, Debug)]
struct Order { id: u64, amount: f64 }

#[derive(Serialize, Deserialize, Debug)]
struct Receipt { charge_id: String }

#[derive(Serialize, Deserialize, Debug)]
enum PaymentError {
    CardDeclined { reason: String },
    InsufficientFunds { balance: f64, required: f64 },
    NetworkTimeout,
}

// --- Activity registration ---
let activities = ActivityRegistry::builder()
    .register_typed("ChargeCard", |_ctx, order: Order| async move {
        match charge_card(&order).await {
            Ok(receipt) => Ok(receipt),
            Err(e) => Err(e),  // PaymentError — serialized to JSON automatically
        }
    })
    .build();

// --- Orchestration ---
let orchestration = |ctx: OrchestrationContext, input: String| async move {
    let order: Order = serde_json::from_str(&input).unwrap();

    let result: Result<Receipt, PaymentError> =
        ctx.schedule_activity_typed("ChargeCard", &order).await;

    match result {
        Ok(receipt) => Ok(format!("Charged: {}", receipt.charge_id)),

        Err(PaymentError::CardDeclined { reason }) => {
            ctx.schedule_activity("NotifyUser", &format!("Card declined: {reason}"))
                .await?;
            Err("payment_declined".into())
        }

        Err(PaymentError::InsufficientFunds { balance, required }) => {
            Err(format!("Need ${required}, have ${balance}"))
        }

        Err(PaymentError::NetworkTimeout) => {
            ctx.schedule_timer(Duration::from_secs(10)).await;
            let retry: Result<Receipt, PaymentError> =
                ctx.schedule_activity_typed("ChargeCard", &order).await;
            retry.map(|r| format!("Charged on retry: {}", r.charge_id))
                 .map_err(|e| format!("{e:?}"))
        }
    }
};

// --- Client-side typed wait ---
client.start_orchestration_typed("order-1", "Checkout", &order).await?;

let result: Result<Receipt, PaymentError> =
    client.wait_for_orchestration_typed("order-1", timeout).await?;

match result {
    Ok(receipt) => println!("Charged: {}", receipt.charge_id),
    Err(PaymentError::CardDeclined { reason }) => println!("Declined: {reason}"),
    Err(e) => println!("Failed: {e:?}"),
}

// --- Client-side typed status ---
match client.get_orchestration_status_typed::<Receipt, PaymentError>("order-1").await? {
    OrchestrationStatusTyped::Completed { output, .. } => {
        println!("Charged: {}", output.charge_id);
    }
    OrchestrationStatusTyped::Failed { error, .. } => {
        match error {
            PaymentError::CardDeclined { reason } => println!("Declined: {reason}"),
            PaymentError::NetworkTimeout => println!("Timed out"),
            _ => println!("Other error"),
        }
    }
    OrchestrationStatusTyped::Running { custom_status, .. } => {
        println!("Still running: {:?}", custom_status);
    }
    OrchestrationStatusTyped::NotFound => println!("Not found"),
}
```

---

## Test plan

### Positive tests — activities

#### 1. `typed_error_enum_round_trips`

Activity returns a typed enum error, orchestration pattern-matches on it.

```rust
#[derive(Serialize, Deserialize, Debug, PartialEq)]
enum PaymentError {
    CardDeclined { reason: String },
    InsufficientFunds { balance: f64, required: f64 },
}

// Activity returns Err(PaymentError::CardDeclined { reason: "expired" })
// Orchestration receives Err(PaymentError::CardDeclined { reason: "expired" })
// Assert: match arm fires, reason == "expired"
```

#### 2. `typed_error_string_backward_compat`

Existing code using `E = String` behaves identically. Activity registered with
`register_typed::<In, Out, String>`, orchestration calls
`schedule_activity_typed::<In, Out, String>`. Error strings round-trip unchanged.

```rust
// Activity returns Err("plain error message".to_string())
// Orchestration receives Err("plain error message".to_string())
// Assert: identical to today's behavior
```

#### 3. `typed_error_ok_path_unaffected`

Adding `E` doesn't change the `Ok` path. Activity returns `Ok(Receipt)`,
orchestration receives `Ok(Receipt)` with correct deserialization.

#### 4. `typed_error_struct_variant`

Error type is a struct (not an enum):

```rust
#[derive(Serialize, Deserialize, Debug)]
struct ValidationError {
    field: String,
    message: String,
    code: u32,
}
// Round-trips correctly through the codec
```

#### 5. `typed_error_with_serde_untagged_fallback`

Error enum uses `#[serde(untagged)]` variant to catch unstructured errors:

```rust
#[derive(Serialize, Deserialize, Debug)]
enum MyError {
    Structured { code: u32 },
    #[serde(untagged)]
    Unknown(String),
}
// Activity registered with plain `register` (returns Err(String))
// Orchestration calls schedule_activity_typed with MyError
// Raw string error decodes into MyError::Unknown("the error message")
```

### Positive tests — orchestrations and client

#### 6. `typed_orchestration_error_round_trips`

Orchestration registered with `register_typed::<In, Out, E>` returns `Err(E)`.
The error is encoded, stored in `OrchestrationFailed.details.message`, and decoded
by `Client::wait_for_orchestration_typed::<Out, E>`.

```rust
// Orchestration returns Err(CheckoutError::OutOfStock { item_id: 42 })
// Client: wait_for_orchestration_typed::<Receipt, CheckoutError>(...)
// Client receives: Err(CheckoutError::OutOfStock { item_id: 42 })
```

#### 7. `typed_orchestration_status_completed`

`get_orchestration_status_typed::<Receipt, E>` returns
`OrchestrationStatusTyped::Completed { output: Receipt { ... } }` with decoded output.

#### 8. `typed_orchestration_status_failed`

`get_orchestration_status_typed::<Out, CheckoutError>` returns
`OrchestrationStatusTyped::Failed { error: CheckoutError::OutOfStock { ... } }` with
decoded error.

#### 9. `typed_sub_orchestration_error_round_trips`

Parent orchestration calls `schedule_orchestration_typed::<In, Out, E>` on a child
that returns `Err(E)`. The parent receives the decoded `Err(E)` via
`SubOrchestrationFailed.details.display_message()` → `Json::decode`.

#### 10. `typed_wait_string_backward_compat`

`wait_for_orchestration_typed::<Out, String>` behaves identically to today's API.
The `display_message()` string round-trips as-is via `Json::decode::<String>`.

### Negative tests — codec failure cases

These tests cover all three type parameters (`In`, `Out`, `E`) across activities,
orchestrations, and client APIs.

#### 11. `typed_error_decode_mismatch_fails`

Activity returns error type `A`, orchestration expects error type `B`
(incompatible schemas).

```rust
#[derive(Serialize, Deserialize)]
enum ErrorA { Foo { x: u32 } }

#[derive(Serialize, Deserialize)]
enum ErrorB { Bar { y: String } }

// Activity registered with ErrorA, orchestration expects ErrorB
// Wire: "{\"Foo\":{\"x\":42}}"
// Json::decode::<ErrorB>(...) → fails
// Assert: Err with serde decode error, not panic
```

#### 12. `typed_error_non_json_string_from_untyped_register`

Activity registered with plain `register` returns a bare string.
Orchestration tries to decode as a typed error.

```rust
// Activity returns Err("connection refused")
// Orchestration expects PaymentError
// Json::decode::<PaymentError>("connection refused") → fails
// Assert: decode failure, not panic
```

#### 13. `typed_error_non_json_string_with_untagged_fallback`

Same as test 12, but the error type has `#[serde(untagged)] Raw(String)`.
The bare string successfully decodes into the fallback variant.

#### 14. `typed_input_decode_mismatch_fails`

Activity registered with `register_typed::<OrderV2, Out, E>` but orchestration
sends `OrderV1` (missing a required field).

```rust
#[derive(Serialize, Deserialize)]
struct OrderV1 { id: u64 }

#[derive(Serialize, Deserialize)]
struct OrderV2 { id: u64, region: String }

// Orchestration sends OrderV1 { id: 42 }
// Activity wrapper: Json::decode::<OrderV2>(...) → fails (missing `region`)
// Activity body never executes — error becomes ActivityFailed
// Assert: Err("missing field `region`...")
```

#### 15. `typed_output_decode_mismatch_fails`

Activity returns `ReceiptV1`, orchestration expects `ReceiptV2` with a required field.

```rust
#[derive(Serialize, Deserialize)]
struct ReceiptV1 { charge_id: String }

#[derive(Serialize, Deserialize)]
struct ReceiptV2 { charge_id: String, fee: f64 }

// Activity returns Ok(ReceiptV1 { charge_id: "ch_1" })
// Orchestration: Json::decode::<ReceiptV2>(...) → fails (missing `fee`)
// Assert: Err("missing field `fee`...")
//
// Reverse (extra fields): ReceiptV2 → ReceiptV1 succeeds (serde ignores unknowns)
```

#### 16. `typed_error_schema_evolution_missing_field`

Error type gains a required field. Old events in history decode with the old schema
but fail with the new schema.

```rust
// Old: enum E { Fail { msg: String } }
// New: enum E { Fail { msg: String, code: u32 } }
// History: "{\"Fail\":{\"msg\":\"oops\"}}"
// Json::decode with new schema → fails (missing `code`)
// Assert: decode failure. Demonstrates why new fields should be Option<T>.
```

#### 17. `typed_error_encode_failure_uses_debug_fallback`

Error type that fails `Json::encode`. The wrapper falls back to `format!("{e:?}")`.

```rust
// Encode fallback: Err(Json::encode(&e).unwrap_or_else(|_| format!("{e:?}")))
// Assert: no panic, error is the Debug string
```

#### 18. `typed_client_wait_error_decode_mismatch`

Orchestration produces `Err(ErrorA)`, but client calls
`wait_for_orchestration_typed::<Out, ErrorB>`. Decode fails.

```rust
// Orchestration returns Err(ErrorA::Foo { x: 1 })
// Client: wait_for_orchestration_typed::<Out, ErrorB>(...)
// Json::decode::<ErrorB>(display_message) → fails
// Assert: Err(ClientError::InvalidInput { message: "error decode failed: ..." })
```

#### 19. `typed_client_status_output_decode_mismatch`

Orchestration returns `Ok("raw string")` via untyped `register`.
Client calls `get_orchestration_status_typed::<Receipt, E>`. Output decode fails.

```rust
// Orchestration returns Ok("just a string")
// Client: get_orchestration_status_typed::<Receipt, E>(...)
// Json::decode::<Receipt>("just a string") → fails
// Assert: Err(ClientError::InvalidInput { message: "output decode failed: ..." })
```

### Schema evolution via versioned orchestrations

#### 20. `typed_schema_migration_via_versioning`

Demonstrates the recommended pattern for evolving `In`, `Out`, or `E` schemas:
register a new orchestration version with the new types, let existing instances
drain on the old version.

```rust
// V1 types
struct OrderV1 { id: u64, amount: f64 }
struct ReceiptV1 { charge_id: String }
enum PayErrorV1 { Declined { reason: String } }

// V2 types (new fields, new error variant)
struct OrderV2 { id: u64, amount: f64, currency: String }
struct ReceiptV2 { charge_id: String, fee: f64 }
enum PayErrorV2 {
    Declined { reason: String },
    RateLimited { retry_after_secs: u64 },
}

let orchestrations = OrchestrationRegistry::builder()
    // V1 — in-flight instances stay here
    .register_versioned_typed("Checkout", "1.0.0", |ctx, order: OrderV1| async move {
        let r: Result<ReceiptV1, PayErrorV1> =
            ctx.schedule_activity_typed("ChargeV1", &order).await;
        match r {
            Ok(r) => Ok(format!("v1: {}", r.charge_id)),
            Err(PayErrorV1::Declined { reason }) => Err(format!("declined: {reason}")),
        }
    })
    // V2 — new instances get this
    .register_versioned_typed("Checkout", "2.0.0", |ctx, order: OrderV2| async move {
        let r: Result<ReceiptV2, PayErrorV2> =
            ctx.schedule_activity_typed("ChargeV2", &order).await;
        match r {
            Ok(r) => Ok(format!("v2: {} (fee: {})", r.charge_id, r.fee)),
            Err(PayErrorV2::Declined { reason }) => Err(format!("declined: {reason}")),
            Err(PayErrorV2::RateLimited { retry_after_secs }) => {
                ctx.schedule_timer(Duration::from_secs(retry_after_secs)).await;
                ctx.continue_as_new_typed(&order).await
            }
        }
    })
    .build();

// Assert: new "Checkout" picks v2 (latest), existing v1 drain safely.
// V1 replays with V1 types, V2 uses V2 types. No decode failures.
// Once V1 instances drain, remove V1 registration.
```

**Key takeaway:** Schema evolution for typed errors follows the same versioning
pattern as any other type change. The pinned-version mechanism ensures in-flight
instances replay with the types they started with.

---

## Decisions

1. **Debug fallback (not Display):** When `Json::encode` fails for an error, the
   wrapper uses `format!("{e:?}")` as fallback. `Debug` is auto-derivable and
   universally available; `Display` would require an extra manual impl. The
   fallback only fires when serde serialization itself fails, which is extremely
   rare since the type already has `Serialize`.

2. **Modify existing methods (not new `_typed_err` variants):** Adding `E` to
   existing `_typed` methods is an intentional breaking change. The alternative
   of adding `_typed_err` variants would double the method surface, which is
   already large. The migration is mechanical (add `, String` to turbofish sites).

3. **`get_orchestration_status_typed` is a new method:** Unlike the others, this
   returns a different enum (`OrchestrationStatusTyped<Out, E>`) rather than
   modifying the existing `OrchestrationStatus`. The untyped method stays.
