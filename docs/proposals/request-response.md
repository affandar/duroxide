# Proposal: Request-Response for Orchestrations (`ctx.on_request`)

**Status:** Draft  
**Created:** 2026-02-21  
**Related:** [External Events](../external-events.md), [POC Test](../../tests/request_response_poc.rs)

---

## Summary

Add a `ctx.on_request()` API that registers a handler for incoming requests
within a running orchestration. External callers send requests via the Client
API and receive typed responses — enabling orchestrations to serve as
long-lived stateful services that process queries against their internal state.

---

## Motivation

### The problem

Orchestrations accumulate rich internal state over their lifetime (order status,
connection health, processing progress, etc.) but there's no way for external
callers to **query** that state with request-response semantics. The existing
options are:

1. **`custom_status`** — write-only from the orchestration side. The orchestration
   sets a string, clients poll for it. No request parameters, no per-query logic,
   no way to ask "what's the status of item X specifically?"

2. **`schedule_wait` + `raise_event`** — the orchestration waits for an event,
   processes it, and... has no way to send a response back to the specific caller.
   The caller fires an event and has to poll `custom_status` or another event
   to get the answer. This requires the caller to invent correlation IDs and
   manage them manually.

3. **Activities** — stateless. Can't see the orchestration's internal state
   without reading from an external database, which defeats the purpose of
   keeping state in the orchestration.

### What we want

```rust
// Inside orchestration — register a handler that can see internal state
let order_status = "processing".to_string();
let items_done = 42u32;

ctx.on_request("GetStatus", |req: StatusRequest| {
    // Full access to orchestration state via closure captures
    Ok(StatusResponse {
        order_status: order_status.clone(),
        items_done,
        requested_item: req.item_id,
    })
});

// Continue orchestration work while handler stays active...
ctx.schedule_activity("DoWork", &input).await?;
```

```rust
// External caller — typed request with typed response
let response: StatusResponse = client
    .send_request::<StatusRequest, StatusResponse>(
        "order-123",        // instance ID
        "GetStatus",        // handler name
        &StatusRequest { item_id: 7 },
        Duration::from_secs(5),  // timeout
    )
    .await?;
```

---

## Design

### Architecture overview

```
  External Caller                    Orchestration
  ──────────────                    ──────────────
  client.send_request() ──┐
                           │  1. raise_event("__req:GetStatus", {correlation_id, payload})
                           ├──────────────────────────────►
                           │
                           │  2. orchestration wakes, on_request handler runs
                           │     handler produces response
                           │
                           │  3. set_custom_status or write response event
                           ◄──────────────────────────────┤
                           │
  receives response  ◄─────┘
```

### How it maps to existing primitives

`on_request` is syntactic sugar over existing duroxide primitives:

| Concept | Built from |
|---|---|
| Receiving a request | `schedule_wait` (or `dequeue_event`) |
| Sending a response | A new response channel (see options below) |
| Correlation | Runtime-managed correlation ID |
| Handler registration | Closure capture over orchestration locals |

### `ctx.on_request` API

```rust
impl OrchestrationContext {
    /// Register a request handler for the given name.
    ///
    /// The handler is an async closure that receives the `OrchestrationContext`
    /// and the request payload. It can `schedule_activity`, `schedule_timer`,
    /// etc. — exactly like the main orchestration body.
    ///
    /// When a request arrives, the runtime spawns the handler as a parallel
    /// future alongside the main orchestration future. The runtime joins all
    /// in-flight handlers before completing the orchestration.
    ///
    /// The handler takes raw strings (request payload → response payload).
    pub fn on_request<F, Fut>(
        &self,
        name: impl Into<String>,
        handler: F,
    )
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send;

    /// Typed variant: deserializes request, serializes response.
    pub fn on_request_typed<Req, Resp, F, Fut>(
        &self,
        name: impl Into<String>,
        handler: F,
    )
    where
        Req: DeserializeOwned,
        Resp: Serialize,
        F: Fn(OrchestrationContext, Req) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Resp, String>> + Send;
}
```

The handler receives the `OrchestrationContext` just like the main orchestration.
All `ctx.schedule_*` methods work inside handlers — they emit actions into the
same shared history.
```

### Client API

```rust
impl Client {
    /// Send a request to a running orchestration and wait for a response.
    /// Payload and response are raw strings.
    pub async fn send_request(
        &self,
        instance: &str,
        handler_name: &str,
        payload: &str,
        timeout: Duration,
    ) -> Result<String, ClientError>;

    /// Typed variant: serializes request, deserializes response.
    pub async fn send_request_typed<Req, Resp>(
        &self,
        instance: &str,
        handler_name: &str,
        request: &Req,
        timeout: Duration,
    ) -> Result<Resp, ClientError>
    where
        Req: Serialize,
        Resp: DeserializeOwned;
}
```

---

## Response channel: request/response hashmap

The response channel follows the same pattern as `custom_status` — a
provider-level hashmap that tracks requests and their response state.
The client polls this hashmap to get the response when available.

### Provider contract

```rust
/// New provider methods for request-response:
trait Provider {
    /// Write a pending request entry (no response yet).
    async fn write_request(
        &self,
        instance_id: &str,
        correlation_id: &str,
        handler_name: &str,
        payload: &str,
    ) -> Result<(), ProviderError>;

    /// Write the response for a previously-written request.
    async fn write_response(
        &self,
        instance_id: &str,
        correlation_id: &str,
        response: &str,
    ) -> Result<(), ProviderError>;

    /// Read the current state of a request (pending / completed with response).
    /// Returns None if correlation_id not found.
    async fn read_request_state(
        &self,
        instance_id: &str,
        correlation_id: &str,
    ) -> Result<Option<RequestState>, ProviderError>;

    /// Clean up completed request entries (garbage collection).
    async fn cleanup_requests(
        &self,
        instance_id: &str,
        older_than: Duration,
    ) -> Result<u64, ProviderError>;
}

/// State of a request in the hashmap.
pub enum RequestState {
    /// Request received, handler not yet responded.
    Pending,
    /// Handler produced a response.
    Completed { response: String },
    /// Handler returned an error.
    Failed { error: String },
}
```

### Storage (SQLite example)

```sql
CREATE TABLE IF NOT EXISTS request_responses (
    instance_id  TEXT NOT NULL,
    correlation_id TEXT NOT NULL,
    handler_name TEXT NOT NULL,
    request_payload TEXT NOT NULL,
    response_payload TEXT,
    error TEXT,
    status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'completed', 'failed'
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at DATETIME,
    PRIMARY KEY (instance_id, correlation_id)
);
```

### Flow

```
Client                          Provider                     Orchestration
──────                          ────────                     ──────────────
send_request(corr_id, payload)
  → write_request(corr_id, pending)
  → raise_event(req)  ──────────────────────────────────►  handler spawned
                                                           handler runs
                                                           (may schedule activities)
                                                           handler returns response
  ← poll read_request_state(corr_id) ◄── write_response() ◄──
  ← poll read_request_state(corr_id) ──► Completed!
  returns response
```

The client polls `read_request_state` with backoff until it sees `Completed`
or `Failed`, or gives up with a client-side timeout.

---

## Internal mechanics

### Request flow (detailed)

1. **Client calls `send_request`:**
   - Generates a correlation ID (`Uuid::new_v4`)
   - Wraps the payload: `{ "correlation_id": "...", "payload": <serialized Req> }`
   - Calls `raise_event(instance, "__rr:{handler_name}", wrapped_payload)`
   - Starts polling for response

2. **Orchestration receives request:**
   - `on_request` internally calls `schedule_wait("__rr:{handler_name}")` in a loop
   - When an event arrives, unwraps correlation ID and payload
   - Deserializes `Req`, calls handler
   - Serializes response, writes it to the response channel with the correlation ID

3. **Client receives response:**
   - Polls `read_response(instance, correlation_id)` with backoff
   - Deserializes `Resp`, returns to caller
   - On timeout, returns `ClientError::Timeout`

### Determinism and replay

Request handlers must be deterministic — they execute within the orchestration
context and are subject to replay. This means:

- Handlers **cannot** do I/O directly (same rule as orchestration code)
- Handlers **can** schedule activities if they need I/O
- Handler results are recorded in history as events, ensuring replay produces
  the same responses

The handler loop (`schedule_wait` in a loop) is deterministic because each
wait resolves to the same event during replay.

### Execution model: replay-driven fork with automatic join barrier

`on_request` just registers a handler function into the context. It doesn't
return a future, guard, or handle — it's a pure registration call. The real
work happens in the replay engine and orchestration dispatcher.

#### How it works

1. **Registration:** `ctx.on_request("GetStatus", handler_fn)` stores the handler
   in the context's handler map, keyed by name. No futures are created yet.

2. **Request arrival:** When the replay engine encounters a `RequestReceived` event
   in the history (or a new request event arrives), it looks up the registered
   handler by name, and **spawns the handler as a parallel future** alongside the
   main orchestration future. Each request invocation becomes its own concurrent
   task within the orchestration turn.

3. **Main orchestration completes:** When the main orchestration future returns
   (`Ok`, `Err`, or `continue_as_new`), the runtime does **not** immediately
   declare the orchestration complete. Instead, it inserts an automatic
   **`join_all` barrier** — it waits for all in-flight handler executions to
   finish and their responses to be written before completing.

4. **Orchestration declared complete:** Only after all handler futures have
   resolved does the runtime emit `OrchestrationCompleted` / `OrchestrationFailed`
   / process `continue_as_new`.

```
Timeline:

  main future ─────────────────────────────────► Ok("done")
                                                      │
  request 1 ─────► handler_fn() ─► response            │
       request 2 ──────► handler_fn() ─► response      │
            request 3 ────────► handler_fn() ─► resp    │
                                                      ▼
                                              join_all barrier
                                              (wait for request 3)
                                                      │
                                              OrchestrationCompleted
```

#### User code is completely unaware

```rust
let orchestration = |ctx: OrchestrationContext, input: String| async move {
    let shared = Arc::new(Mutex::new(MyState::new()));

    // Register handler — just a registration, nothing visible happens.
    // The handler is an async block — it can schedule activities, timers, etc.
    ctx.on_request("GetStatus", {
        let shared = shared.clone();
        move |ctx: OrchestrationContext, req: String| {
            let shared = shared.clone();
            async move {
                // Can schedule activities, timers, sub-orchestrations...
                let enriched = ctx.schedule_activity("Enrich", &req).await?;
                let s = shared.lock().unwrap();
                Ok(format!(r#"{{"status":"{}","enriched":"{enriched}"}}"#, s.status))
            }
        }
    });

    // Main work — requests are processed concurrently in the background
    for i in 0..10 {
        let r = ctx.schedule_activity("Process", &format!("{i}")).await?;
        shared.lock().unwrap().status = format!("item-{i}");
    }

    // Return value — but orchestration won't actually complete until
    // all in-flight handler invocations finish. This is transparent.
    Ok("done".into())
};
```

**Multiple handlers** just register independently:

```rust
ctx.on_request_typed("GetStatus", status_handler);
ctx.on_request_typed("GetItem", item_handler);
// Both process requests concurrently, managed by the runtime
```

#### What the dispatcher does (internal pseudocode)

```rust
// Orchestration dispatcher (user never sees this):
async fn run_orchestration(ctx: OrchestrationContext, orchestration_fn, input: String) {
    // Run the user's orchestration function
    let main_result = orchestration_fn(ctx.clone(), input).await;

    // Automatic join barrier: wait for all in-flight handler invocations
    // to complete before declaring the orchestration done
    let pending_handlers = ctx.take_pending_handler_futures();
    futures::future::join_all(pending_handlers).await;

    // NOW the orchestration is actually complete
    match main_result {
        Ok(output) => emit(OrchestrationCompleted { output }),
        Err(error) => emit(OrchestrationFailed { error }),
    }
}
```

#### Replay determinism

During replay, the engine sees `RequestReceived` events in the history and
re-spawns the handler futures in the same order. Since handler functions are
deterministic (same rule as orchestration code), they produce the same results.
The `ResponseSent` events in history are used to verify correctness during
replay (or short-circuit if the response is already recorded).

### Concurrency

Multiple requests can be in-flight simultaneously because each request spawns
its own handler future. The handler function itself is shared (via `Fn`), but
each invocation is independent. Concurrent callers don't block each other.

If the handler uses `Arc<Mutex<S>>` for shared state, contention is managed
by the user's locking strategy — same as any concurrent Rust code.

### Completion semantics

| Scenario | Behavior |
|---|---|
| Main returns `Ok`, no in-flight handlers | Complete immediately |
| Main returns `Ok`, 2 handlers in-flight | Wait for both handlers, then complete |
| Main returns `Err`, 1 handler in-flight | Wait for handler, then fail |
| `continue_as_new`, handlers in-flight | Wait for handlers, then continue-as-new |
| Handler returns `Err` | Response contains error, orchestration unaffected |
| Handler panics | Treated like activity panic — infrastructure error |
| New request after main returns | Accepted — handler spawned, barrier extended |
| Handler stuck (slow activity) | No timeout — same treatment as stuck main orchestration |

---

## Patterns from the POC

The [POC test](../../tests/request_response_poc.rs) validated three handler
patterns using existing primitives:

### Pattern 1: Inline (simplest)

```rust
let request = ctx.schedule_wait("GetStatus").await;
let response = process(request, &local_state);
ctx.set_custom_status(&response);  // or write_response
```

Full access to locals, zero overhead, most idiomatic.
**Limitation:** blocking — the orchestration pauses at the wait point.

### Pattern 2: Async fn with parameters

```rust
async fn handle(ctx: &OrchestrationContext, state: &str, req: &str) -> Result<String, String> {
    ctx.schedule_activity("Process", &format!("{state}:{req}")).await
}

let req = ctx.schedule_wait("Handler").await;
handle(&ctx, &status, &req).await?;
```

Reusable, testable in isolation. Verbose parameter passing.

### Pattern 3: Arc<Mutex<T>> closure

```rust
let shared = Arc::new(Mutex::new(MyState { ... }));
let handler = {
    let shared = shared.clone();
    move |req: String| {
        let s = shared.lock().unwrap();
        Ok(format!("status: {}", s.status))
    }
};
```

Both mainline and handler can read/write shared state.
Requires double-clone boilerplate.

### What `on_request` provides

`on_request` subsumes all three patterns — the handler is an async block
that receives the `OrchestrationContext` and can do everything the main
orchestration can:

```rust
// Register handler — pure registration, nothing visible happens.
// Handler is an async block — can schedule activities, timers, etc.
ctx.on_request("GetStatus", |ctx: OrchestrationContext, req: String| async move {
    let enriched = ctx.schedule_activity("Enrich", &req).await?;
    Ok(format!(r#"{{"enriched":"{enriched}"}}"#))
});

// Continue orchestration work — requests are handled transparently
ctx.schedule_activity("DoWork", &input).await?;

// Orchestration returns — runtime waits for any in-flight handlers
// before actually completing. Fully transparent.
Ok("done".into())
```

---

## Full example

```rust
#[derive(Serialize, Deserialize)]
struct StatusRequest { item_id: u64 }

#[derive(Serialize, Deserialize)]
struct StatusResponse { status: String, item_count: u32, item_status: Option<String> }

// --- Orchestration: long-running order processor ---
let orchestration = |ctx: OrchestrationContext, input: String| async move {
    let mut status = "initializing".to_string();
    let mut items: HashMap<u64, String> = HashMap::new();
    let shared = Arc::new(Mutex::new((status.clone(), items.clone())));

    // Register request handler — pure registration, fully transparent.
    // Handler is an async block — can schedule activities.
    ctx.on_request_typed("GetStatus", {
        let shared = shared.clone();
        move |ctx: OrchestrationContext, req: StatusRequest| {
            let shared = shared.clone();
            async move {
                let (status, items) = &*shared.lock().unwrap();
                Ok(StatusResponse {
                    status: status.clone(),
                    item_count: items.len() as u32,
                    item_status: items.get(&req.item_id).cloned(),
                })
            }
        }
    });

    // Main work — requests handled concurrently in the background
    for i in 0..10u64 {
        let result = ctx.schedule_activity("ProcessItem", &format!("{i}")).await?;
        {
            let (ref mut s, ref mut m) = *shared.lock().unwrap();
            *s = format!("processing-{i}");
            m.insert(i, result);
        }
    }
    {
        let (ref mut s, _) = *shared.lock().unwrap();
        *s = "completed".to_string();
    }

    // Return — runtime waits for in-flight handlers before completing
    Ok("done".into())
};

// --- External caller: query status mid-flight ---
client.start_orchestration_typed("order-1", "Processor", &input).await?;

// Wait a bit for processing to start, then query
tokio::time::sleep(Duration::from_secs(1)).await;

let status: StatusResponse = client
    .send_request_typed("order-1", "GetStatus", &StatusRequest { item_id: 3 }, Duration::from_secs(5))
    .await?;

println!("Status: {}, Item 3: {:?}", status.status, status.item_status);
```

---

## Scope of changes

| Component | Change |
|---|---|
| `OrchestrationContext` | Add `on_request`, `on_request_typed` |
| `Client` | Add `send_request`, `send_request_typed` |
| Provider trait | Add `write_response`, `read_response` (Option D) or use existing events (Option A) |
| `Action` enum | Add `RequestResponse` variant (for history recording) |
| `EventKind` enum | Add `RequestReceived`, `ResponseSent` variants |
| Orchestration dispatcher | Multi-future polling: main + N handler futures per turn |

---

## What doesn't change

| Component | Why |
|---|---|
| Activity system | Requests are handled in orchestration context, not as activities |
| `DurableFuture` | Handler loop uses existing `schedule_wait` / `dequeue_event` |
| Replay engine | New events are additive — existing orchestrations unaffected |
| Provider storage model | Response channel is a new table/column, not modifying existing |

---

## Node.js and Python SDK compatibility

Both `duroxide-node` and `duroxide-python` use the same architecture:
**generator-based orchestrations** that yield `ScheduledTask` descriptors.
The Rust runtime receives each descriptor, executes the real `DurableFuture`,
and feeds the result back to drive the next generator step.

### How the generator bridge works today

```
JS/Python orchestration:                Rust handler loop:
─────────────────────────               ──────────────────
function*(ctx, input) {                 createGenerator(payload)
  ↓                                       ↓
  const r = yield ctx.scheduleActivity()  → execute_task(Activity) → DurableFuture
  ↑ result fed back ──────────────────────── nextStep(result)
  ↓
  return "done"                           → Completed
}
```

Today there is **one generator per orchestration invocation**.

### Handlers are generators too

Request handlers need to be able to `yield ctx.scheduleActivity(...)` just
like the main orchestration. A handler is just another async block / generator
that shares the same `OrchestrationContext`. This means:

- Each handler invocation creates a **new generator** with its own generator ID
- The Rust handler bridge drives **multiple generators in parallel** per
  orchestration instance — one for the main flow, plus one per in-flight request
- All generators share the same `OrchestrationContext` (same history)
- The Rust side drives each generator independently via the existing
  `createGenerator` → `nextStep` → `disposeGenerator` protocol

### How it works

```
Timeline (one orchestration instance driving 3 generators):

  main generator ────step──step──step──step────────────► return "done"
                         ↑ request arrives
  handler gen #1 ────────step──step──► return response
                              ↑ another request
  handler gen #2 ──────────────step──step──step──► return response
                                                        |
                                                  join_all barrier
                                                        |
                                                  OrchestrationCompleted
```

Each generator is driven by the same `execute_task` → `nextStep` loop. The
Rust replay engine polls all active generators each turn.

#### Node.js SDK

```javascript
// User code — handler is a generator, just like the main orchestration:
function* orderProcessor(ctx, input) {
  let status = 'starting';
  const items = {};

  // Register handler — fire-and-forget, no yield.
  // The handler itself IS a generator — it can yield scheduled tasks.
  ctx.onRequest('GetStatus', function*(ctx, req) {
    // Can schedule activities, timers, sub-orchestrations...
    const enriched = yield ctx.scheduleActivity('Enrich', req);
    return { status, items: Object.keys(items).length, enriched };
  });

  // Main work — generator proceeds normally
  for (let i = 0; i < 10; i++) {
    const result = yield ctx.scheduleActivity('Process', i);
    status = `processing-${i}`;
    items[i] = result;
  }

  return 'done';
}
```

The handler generator has closure access to the main generator's local
variables (`status`, `items`). JavaScript closures capture by reference,
so the handler sees current state when it runs.

#### Python SDK

```python
# User code — handler is a generator:
def order_processor(ctx, input):
    state = {'status': 'starting', 'items': {}}

    def get_status(ctx, req):
        # Generator handler — can yield scheduled tasks
        enriched = yield ctx.schedule_activity('Enrich', req)
        return {'status': state['status'], 'items': len(state['items']), 'enriched': enriched}

    ctx.on_request('GetStatus', get_status)

    # Main work
    for i in range(10):
        result = yield ctx.schedule_activity('Process', i)
        state['status'] = f'processing-{i}'
        state['items'][i] = result

    return 'done'
```

#### Multi-generator bridge implementation

The Rust bridge needs to manage multiple generators per orchestration:

```rust
// Today: one generator per orchestration
static GENERATORS: LazyLock<Mutex<HashMap<u64, Generator>>> = ...;

// New: generators map stays the same — each handler invocation just gets
// its own generator ID. The bridge doesn't care whether a generator is
// the "main" orchestration or a request handler.

impl JsOrchestrationHandler {
    /// Spawn a handler generator when a request arrives.
    /// Returns a future that drives the generator to completion.
    fn spawn_handler_generator(
        &self,
        ctx: &OrchestrationContext,
        handler_name: &str,
        request_payload: &str,
    ) -> impl Future<Output = Result<String, String>> {
        // 1. Call createHandlerGenerator(instanceId, handlerName, payload)
        //    → JS/Python creates a new generator from the registered handler fn
        //    → Returns first GeneratorStepResult with a NEW generator ID
        // 2. Drive the generator with the same nextStep/execute_task loop
        // 3. When generator returns, the result is the response
        async move {
            let first_step = self.call_create_handler_blocking(
                ctx.instance_id(), handler_name, request_payload
            )?;

            match first_step {
                GeneratorStepResult::Completed { output } => return Ok(output),
                GeneratorStepResult::Error { message } => return Err(message),
                GeneratorStepResult::Yielded { generator_id, task } => {
                    // Same execute_task → nextStep loop as main orchestration
                    self.drive_generator(ctx, generator_id, task).await
                }
            }
        }
    }
}
```

The key insight: **the existing generator protocol is reused unchanged**.
A handler generator and a main generator are indistinguishable to the Rust
bridge — they both yield `ScheduledTask` descriptors and get results fed back.
The only new part is a `createHandlerGenerator` call (instead of
`createGenerator`) that tells JS/Python to create a generator from a
registered handler function rather than from the registry.

#### JS-side implementation

```javascript
// New: registered request handler generators
class OrchestrationContext {
  constructor(ctxInfo) {
    // ... existing fields ...
    this._requestHandlers = new Map();
  }

  onRequest(name, handlerGeneratorFn) {
    this._requestHandlers.set(name, handlerGeneratorFn);
    // Notify Rust that this handler name is registered
    orchestrationRegisterHandler(this.instanceId, name);
  }
}

// New: called from Rust when a request arrives
function createHandlerGenerator(payloadJson) {
  const { instanceId, handlerName, requestPayload, ctxInfo } = JSON.parse(payloadJson);

  // Look up the handler generator function
  const handlerFn = getRegisteredHandler(instanceId, handlerName);
  if (!handlerFn) {
    return JSON.stringify({ status: 'error', message: `No handler '${handlerName}'` });
  }

  // Create context for the handler (same instanceId = shared history)
  const ctx = new OrchestrationContext(ctxInfo);

  // Parse request and create the generator
  const req = JSON.parse(requestPayload);
  const gen = handlerFn(ctx, req);

  // Same protocol as createGenerator — assign ID, drive first step
  const id = nextGeneratorId++;
  generators.set(id, gen);
  return driveStep(id, gen, undefined);
}
```

**No changes to `ScheduledTask`, `driveStep`, `nextStep`, or `disposeGenerator`.**
The handler generator is driven by the exact same loop.

#### What needs to change in each SDK

| Change | Node.js | Python |
|---|---|---|
| `OrchestrationContext.onRequest()` | Store handler generator fn | Store handler generator fn |
| `createHandlerGenerator()` | New function, called from Rust | New function, called from Rust |
| Rust bridge: `call_create_handler_blocking` | New `ThreadsafeFunction` | New GIL call |
| Rust bridge: `spawn_handler_generator` | Reuses existing `execute_task`/`nextStep` | Same |
| `Client.sendRequest()` | Add to JS `Client` class | Add to Python `Client` class |
| `ScheduledTask` enum | **No change** | **No change** |
| `driveStep` / `nextStep` | **No change** | **No change** |
| `disposeGenerator` | **No change** | **No change** |
| Generator protocol | **No change** | **No change** |

### Summary: fully compatible

The design works with both SDKs because:

1. Handler registration is a non-yielding side-effect (like `setCustomStatus`)
2. Handler invocation creates a new generator using the **same protocol** as
   the main orchestration — `createGenerator` → `nextStep` → `disposeGenerator`
3. Handlers can `yield`/`await` any scheduled task (activities, timers, etc.)
4. The join barrier is Rust-internal — SDKs don't need to know about it
5. Closure capture gives handlers access to orchestration state in both languages
6. The existing `ScheduledTask` enum and generator driver are reused unchanged

---

## Resolved decisions

1. **Response channel:** Dedicated request/response hashmap in the provider,
   following the same pattern as `custom_status`. The provider stores a map of
   `(instance_id, correlation_id) → RequestState` that tracks pending requests
   and their responses. The client polls this hashmap. See "Response channel"
   section above for full provider contract.

2. **Replay model:** `RequestReceived` + `ResponseSent` event pairs are sufficient.
   The replay engine does not treat these events differently — a handler invocation
   is just a parallel branch that was created implicitly. The execution model is
   exactly the same as any other parallel fork in the history.

3. **No drain timeout:** A stuck handler is no different from a stuck main
   orchestration. No special timeout mechanism — the orchestration stays running
   until all branches complete, same as `join_all` on parallel activities.

4. **Requests after main returns:** Accepted and followed through. If a request
   arrives after the main future returns but before the join barrier completes,
   it spawns a new handler and the barrier extends to include it.

5. **History interleaving:** Not needed. Handlers are just parallel invocations
   in the history — they get translated into the same parallel branch representation
   as any other concurrent fork. No special fork ID or scoped sequence required.

6. **Python shared state:** Document that Python handlers should use mutable
   containers (`dict`, `list`, dataclass) rather than bare variable rebinding
   to share state with the main generator.
