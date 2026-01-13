# Activity Tags

**Status:** Proposal  
**Created:** 2026-01-12  
**Author:** AI Assistant  

## Summary

Add support for activity tags that allow routing activities to specific workers. This enables specialized worker pools for activities with different resource requirements (e.g., GPU workers, high-memory workers, region-specific workers), as well as dynamic worker affinity for session-based workloads.

## Motivation

Currently, all activity workers share a single queue and can process any registered activity. This doesn't support scenarios where:

- Some activities require specialized hardware (GPUs, high memory)
- Activities need to run in specific regions/zones for data locality
- Different SLA tiers require dedicated worker pools
- Resource isolation between teams or tenants
- Session affinity (route related activities to same worker for cache locality)

## Design Decisions

1. **Tags specified at schedule time only** - The orchestration decides the tag when calling `.with_tag()`.

2. **No default tags at registration** - Activities are registered without tags; routing is purely a scheduling concern.

3. **Workers subscribe to tags via RuntimeOptions** - Workers declare which tags they can process.

4. **No tag = default** - Activities scheduled without `.with_tag()` go to the default queue (None).

5. **Exact matches only** - No wildcard or pattern matching.

6. **Full observability** - Tag appears in metrics, traces, and logs.

7. **Backward compatible** - Existing code works unchanged (no `.with_tag()` = default).

## Terminology

- **Tag**: A routing label specified at schedule time (e.g., "gpu", "high-memory", "worker-001")
- **Default**: Activities without a tag (represented as `None`)

---

## System Architecture

### High-Level Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              SHARED PROVIDER                                 │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                         worker_queue                                 │   │
│  │  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌─────────────┐ │   │
│  │  │ tag: NULL    │ │ tag: "gpu"   │ │ tag: "gpu"   │ │ tag: "w-01" │ │   │
│  │  │ SendEmail    │ │ GPUInference │ │ VideoEncode  │ │ ProcessData │ │   │
│  │  └──────────────┘ └──────────────┘ └──────────────┘ └─────────────┘ │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
         │                      │                              │
         │ WHERE tag IS NULL    │ WHERE tag = 'gpu'           │ WHERE tag = 'w-01'
         ▼                      ▼                              ▼
┌─────────────────┐    ┌─────────────────┐            ┌─────────────────┐
│ DEFAULT WORKER  │    │   GPU WORKER    │            │  WORKER w-01    │
│                 │    │                 │            │  (with cache)   │
│ TagFilter::     │    │ TagFilter::     │            │ TagFilter::     │
│  default_only() │    │  tags(["gpu"])  │            │  tags(["w-01"]) │
└─────────────────┘    └─────────────────┘            └─────────────────┘
```

### Tag Flow: Schedule Time → Worker Queue → Worker

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         ORCHESTRATION                                    │
│                                                                          │
│  // No tag - goes to default queue                                       │
│  ctx.schedule_activity("SendEmail", input)                               │
│      .into_activity().await;                          // tag = None      │
│                                                                          │
│  // With tag - goes to "gpu" queue                                       │
│  ctx.schedule_activity("GPUInference", input)                            │
│      .with_tag("gpu")                                 // tag = "gpu"     │
│      .into_activity().await;                                             │
│                                                                          │
│  // Dynamic tag for worker affinity                                      │
│  ctx.schedule_activity("ProcessData", input)                             │
│      .with_tag(&session.worker_id)                    // tag = "w-01"    │
│      .into_activity().await;                                             │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────┐
                    │     WorkItem::ActivityExecute │
                    │     {                         │
                    │       name: "GPUInference",   │
                    │       tag: Some("gpu"),       │
                    │       ...                     │
                    │     }                         │
                    └───────────────────────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────┐
                    │         worker_queue          │
                    │   INSERT ... tag = 'gpu'      │
                    └───────────────────────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────┐
                    │        GPU WORKER             │
                    │  SELECT ... WHERE tag = 'gpu' │
                    └───────────────────────────────┘
```

### Worker Tag Filtering

```
                         ┌──────────────────┐
                         │   worker_queue   │
                         └──────────────────┘
                                    │
            ┌───────────────────────┼───────────────────────┐
            │                       │                       │
            ▼                       ▼                       ▼
   ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
   │  DefaultOnly    │    │  Tags(["gpu"])  │    │ DefaultAnd(     │
   │                 │    │                 │    │   ["gpu"])      │
   │ WHERE tag IS    │    │ WHERE tag IN    │    │ WHERE tag IS    │
   │       NULL      │    │   ('gpu')       │    │   NULL OR tag   │
   │                 │    │                 │    │   IN ('gpu')    │
   └─────────────────┘    └─────────────────┘    └─────────────────┘
            │                       │                       │
            ▼                       ▼                       ▼
   ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
   │ Gets: SendEmail │    │ Gets: GPUTask   │    │ Gets: Both      │
   │ Skips: GPUTask  │    │ Skips: SendEmail│    │                 │
   └─────────────────┘    └─────────────────┘    └─────────────────┘
```

### LLM Worker Example

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         ORCHESTRATION                                    │
│                                                                          │
│  // Route to LLM worker with loaded model                                │
│  let answer = ctx.schedule_activity("AskQuestion", question)             │
│      .with_tag("llm-worker")                                             │
│      .into_activity().await?;                                            │
│                                                                          │
│  let embeddings = ctx.schedule_activity("GenerateEmbeddings", text)      │
│      .with_tag("llm-worker")                                             │
│      .into_activity().await?;                                            │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                           LLM WORKER                                      │
│  ┌────────────────────────────────────────────────────────────────────┐  │
│  │ Local State (loaded at startup):                                    │  │
│  │   - LLM model in GPU memory                                         │  │
│  │   - Embeddings cache on disk                                        │  │
│  │   - Tokenizer                                                       │  │
│  │                                                                     │  │
│  │ TagFilter::tags(["llm-worker"])                                     │  │
│  └────────────────────────────────────────────────────────────────────┘  │
│                                                                          │
│  // Worker setup:                                                        │
│  let model = load_llm_model("/models/llama-7b").await;                   │
│  let cache = EmbeddingsCache::open("/cache/embeddings").await;           │
│                                                                          │
│  let activities = ActivityRegistry::builder()                            │
│      .register("AskQuestion", |ctx, q| ask(&model, q))                   │
│      .register("GenerateEmbeddings", |ctx, t| embed(&model, &cache, t))  │
│      .build();                                                           │
│                                                                          │
│  let options = RuntimeOptions::default()                                 │
│      .with_worker_tags(TagFilter::tags(["llm-worker"]));                 │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## API Design

### Activity Registration (Unchanged)

```rust
// No tags at registration - all activities registered the same way
ActivityRegistry::builder()
    .register("SendEmail", handler)
    .register("GPUInference", handler)
    .register("ProcessData", handler)
    .register_typed::<Input, Output, _, _>("TypedActivity", typed_handler)
    .build();
```

### Worker Configuration

```rust
// Only default (no tag) - backward compatible default
RuntimeOptions::default()

// Explicit default only
RuntimeOptions::default()
    .with_worker_tags(TagFilter::default_only())

// Only specific tags (NOT default)
RuntimeOptions::default()
    .with_worker_tags(TagFilter::tags(["gpu", "high-memory"]))

// Default + specific tags
RuntimeOptions::default()
    .with_worker_tags(TagFilter::default_and(["gpu"]))

// No activities - orchestrator-only mode
RuntimeOptions::default()
    .with_worker_tags(TagFilter::none())
```

### Scheduling with Tags

```rust
// No tag - goes to default queue
ctx.schedule_activity("SendEmail", input)
    .into_activity().await;

// With tag - goes to "gpu" queue
ctx.schedule_activity("GPUInference", input)
    .with_tag("gpu")
    .into_activity().await;

// Dynamic tag for worker affinity
ctx.schedule_activity("ProcessData", input)
    .with_tag(&session.worker_id)
    .into_activity().await;
```

### TagFilter Type

```rust
/// Maximum number of tags a worker can subscribe to.
pub const MAX_WORKER_TAGS: usize = 5;

/// Filter for which activity tags a worker will process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagFilter {
    /// Process only activities with no tag (default).
    DefaultOnly,
    
    /// Process only activities with the specified tags (NOT default).
    /// Limited to MAX_WORKER_TAGS (5) tags.
    Tags(HashSet<String>),
    
    /// Process activities with no tag AND the specified tags.
    /// Limited to MAX_WORKER_TAGS (5) tags.
    DefaultAnd(HashSet<String>),
    
    /// Don't process any activities (orchestrator-only mode).
    None,
}

impl Default for TagFilter {
    fn default() -> Self {
        TagFilter::DefaultOnly  // Backward compatible
    }
}

impl TagFilter {
    /// Create a filter for specific tags only.
    /// 
    /// # Panics
    /// Panics if more than MAX_WORKER_TAGS (5) tags are provided.
    pub fn tags<I, S>(tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let set: HashSet<String> = tags.into_iter().map(Into::into).collect();
        assert!(
            set.len() <= MAX_WORKER_TAGS,
            "Worker can subscribe to at most {} tags, got {}",
            MAX_WORKER_TAGS,
            set.len()
        );
        TagFilter::Tags(set)
    }
    
    /// Create a filter for default plus specific tags.
    /// 
    /// # Panics
    /// Panics if more than MAX_WORKER_TAGS (5) tags are provided.
    pub fn default_and<I, S>(tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let set: HashSet<String> = tags.into_iter().map(Into::into).collect();
        assert!(
            set.len() <= MAX_WORKER_TAGS,
            "Worker can subscribe to at most {} tags, got {}",
            MAX_WORKER_TAGS,
            set.len()
        );
        TagFilter::DefaultAnd(set)
    }
    
    pub fn default_only() -> Self { TagFilter::DefaultOnly }
    pub fn none() -> Self { TagFilter::None }
    
    pub fn matches(&self, tag: Option<&str>) -> bool {
        match (self, tag) {
            (TagFilter::None, _) => false,
            (TagFilter::DefaultOnly, None) => true,
            (TagFilter::DefaultOnly, Some(_)) => false,
            (TagFilter::Tags(set), None) => false,
            (TagFilter::Tags(set), Some(t)) => set.contains(t),
            (TagFilter::DefaultAnd(set), None) => true,
            (TagFilter::DefaultAnd(set), Some(t)) => set.contains(t),
        }
    }
}
```

---

## Data Model Changes

### EventKind::ActivityScheduled

```rust
#[serde(rename = "ActivityScheduled")]
ActivityScheduled { 
    name: String, 
    input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    tag: Option<String>,  // NEW - from .with_tag() at schedule time
},
```

### WorkItem::ActivityExecute

```rust
ActivityExecute {
    instance: String,
    execution_id: u64,
    id: u64,
    name: String,
    input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    tag: Option<String>,  // NEW - for queue routing
},
```

### DurableFuture Extension

```rust
pub struct DurableFuture {
    // ... existing fields ...
    tag: Option<String>,  // NEW - set by .with_tag()
}

impl DurableFuture {
    /// Set the tag for routing this activity to specific workers.
    /// 
    /// Activities without a tag go to workers with `TagFilter::DefaultOnly`
    /// or `TagFilter::DefaultAnd(...)`.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }
}
```

Note: No changes to `ActivityRegistry` - tags are not stored at registration.

---

## Provider Changes

### Schema Migration

```sql
-- Migration: 20240106000000_add_worker_tag.sql

-- Add tag column to worker_queue (nullable = default tag)
ALTER TABLE worker_queue ADD COLUMN tag TEXT;

-- Index for efficient tag filtering
CREATE INDEX IF NOT EXISTS idx_worker_tag ON worker_queue(tag, visible_at, lock_token);
```

### Provider Trait Changes

```rust
/// Fetch a work item matching the tag filter.
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    poll_timeout: Duration,
    tag_filter: &TagFilter,  // NEW parameter
) -> Result<Option<(WorkItem, String, u32)>, ProviderError>;
```

### SQLite Implementation

```rust
async fn fetch_work_item(
    &self,
    lock_timeout: Duration,
    _poll_timeout: Duration,
    tag_filter: &TagFilter,
) -> Result<Option<(WorkItem, String, u32)>, ProviderError> {
    let tag_condition = match tag_filter {
        TagFilter::None => return Ok(None),
        TagFilter::DefaultOnly => "tag IS NULL".to_string(),
        TagFilter::Tags(tags) => {
            let placeholders: Vec<_> = tags.iter()
                .map(|t| format!("'{}'", t.replace('\'', "''")))
                .collect();
            format!("tag IN ({})", placeholders.join(", "))
        }
        TagFilter::DefaultAnd(tags) => {
            let placeholders: Vec<_> = tags.iter()
                .map(|t| format!("'{}'", t.replace('\'', "''")))
                .collect();
            format!("(tag IS NULL OR tag IN ({}))", placeholders.join(", "))
        }
    };
    
    // ... rest of query with tag_condition in WHERE clause
}
```

---

## Runtime Changes

### RuntimeOptions

```rust
pub struct RuntimeOptions {
    // ... existing fields ...
    
    /// Tag filter for worker dispatcher.
    /// Default: `TagFilter::DefaultOnly`
    pub worker_tags: TagFilter,
}

impl RuntimeOptions {
    pub fn with_worker_tags(mut self, filter: TagFilter) -> Self {
        self.worker_tags = filter;
        self
    }
}
```

### Worker Dispatcher

```rust
pub(in crate::runtime) fn start_work_dispatcher(
    self: Arc<Self>,
    activities: Arc<registry::ActivityRegistry>,
) -> JoinHandle<()> {
    let tag_filter = self.options.worker_tags.clone();
    // ... pass tag_filter to fetch_work_item
}
```

### Tag Resolution in Replay Engine

```rust
// When scheduling an activity - tag comes directly from DurableFuture
let tag = durable_future.tag.clone();  // None if .with_tag() wasn't called

let event = Event::new(
    event_id,
    EventKind::ActivityScheduled { 
        name: activity_name.clone(), 
        input: input.clone(),
        tag: tag.clone(),
    },
);

let work_item = WorkItem::ActivityExecute {
    instance: instance.clone(),
    execution_id,
    id: event_id,
    name: activity_name,
    input,
    tag,
};
```

---

## Observability

### Metrics

```rust
duroxide_activity_started_total{activity_name="GPUInference", tag="gpu"}
duroxide_activity_completed_total{activity_name="SendEmail", tag="default"}
duroxide_activity_duration_seconds{activity_name="Process", tag="worker-001"}
```

### Tracing

```rust
tracing::debug!(
    target: "duroxide::runtime",
    instance_id = %ctx.instance,
    activity_name = %ctx.activity_name,
    tag = %ctx.tag.as_deref().unwrap_or("default"),
    "Activity started"
);
```

### ActivityContext

```rust
impl ActivityContext {
    /// Returns the tag this activity was routed with.
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }
}
```

---

## Backward Compatibility

| Scenario | Behavior |
|----------|----------|
| Existing activity registration | No change needed |
| Existing RuntimeOptions | `worker_tags` = `TagFilter::DefaultOnly` |
| Existing orchestration code | No `.with_tag()` = None (default queue) |
| Existing provider | Must update `fetch_work_item` signature |

**Migration sequence:**
1. Deploy schema migration (add `tag` column, defaults to NULL)
2. Deploy new code - all existing activities have NULL tag, workers use DefaultOnly
3. Start adding `.with_tag()` calls and deploy specialized workers

---

## Test Plan

### Unit Tests

#### TagFilter Tests (`src/providers/tag_filter_tests.rs`)

| Test | Description |
|------|-------------|
| `test_default_only_matches_none` | `DefaultOnly.matches(None)` → true |
| `test_default_only_rejects_tag` | `DefaultOnly.matches(Some("gpu"))` → false |
| `test_tags_matches_member` | `Tags(["gpu"]).matches(Some("gpu"))` → true |
| `test_tags_rejects_non_member` | `Tags(["gpu"]).matches(Some("cpu"))` → false |
| `test_tags_rejects_none` | `Tags(["gpu"]).matches(None)` → false |
| `test_default_and_matches_none` | `DefaultAnd(["gpu"]).matches(None)` → true |
| `test_default_and_matches_member` | `DefaultAnd(["gpu"]).matches(Some("gpu"))` → true |
| `test_default_and_rejects_non_member` | `DefaultAnd(["gpu"]).matches(Some("cpu"))` → false |
| `test_none_matches_nothing` | `None.matches(_)` → false for all inputs |
| `test_default_is_default_only` | `TagFilter::default() == DefaultOnly` |
| `test_tags_panics_over_limit` | `Tags` with 6+ tags panics |
| `test_default_and_panics_over_limit` | `DefaultAnd` with 6+ tags panics |
| `test_tags_accepts_max` | `Tags` with exactly 5 tags succeeds |

#### DurableFuture Tag Tests

| Test | Description |
|------|-------------|
| `test_with_tag_sets_tag` | `.with_tag("x")` stores tag |
| `test_no_with_tag_is_none` | Default has no tag (None) |
| `test_with_tag_chaining` | Can chain `.with_tag()` with `.into_activity()` |

#### Event Serialization Tests

| Test | Description |
|------|-------------|
| `test_activity_scheduled_with_tag_serializes` | Tag field present in JSON |
| `test_activity_scheduled_none_tag_omitted` | Tag field omitted when None |
| `test_activity_scheduled_deserialize_missing_tag` | Backwards compat - missing field → None |

### Integration Tests

#### Basic Routing (`tests/activity_tag_tests.rs`)

| Test | Description |
|------|-------------|
| `test_default_worker_processes_untagged_activity` | Worker with DefaultOnly picks up activity with no tag |
| `test_default_worker_ignores_tagged_activity` | Worker with DefaultOnly does NOT pick up activity with tag |
| `test_tagged_worker_processes_matching_activity` | Worker with Tags(["gpu"]) picks up activity with tag="gpu" |
| `test_tagged_worker_ignores_non_matching_activity` | Worker with Tags(["gpu"]) ignores activity with tag="cpu" |
| `test_tagged_worker_ignores_untagged_activity` | Worker with Tags(["gpu"]) ignores activity with no tag |
| `test_mixed_worker_processes_both` | Worker with DefaultAnd(["gpu"]) picks up both |
| `test_orchestrator_only_processes_no_activities` | Worker with None processes zero activities |

#### Schedule-Time Tags (`tests/activity_tag_schedule_tests.rs`)

| Test | Description |
|------|-------------|
| `test_with_tag_routes_to_tagged_worker` | `.with_tag("gpu")` routes to gpu worker |
| `test_no_tag_routes_to_default_worker` | No `.with_tag()` routes to default worker |
| `test_tag_in_event_history` | Tag appears in ActivityScheduled event |
| `test_tag_preserved_on_replay` | After restart, replay uses stored tag |
| `test_dynamic_tag_from_variable` | Tag can be dynamic string from activity result |

#### Specialized Worker Pattern (`tests/activity_tag_specialized_tests.rs`)

| Test | Description |
|------|-------------|
| `test_llm_worker_receives_tagged_activities` | Worker with "llm-worker" tag receives all LLM activities |
| `test_multiple_specialized_workers` | GPU worker and LLM worker each get their activities |

#### Multi-Worker Scenarios (`tests/activity_tag_multi_worker_tests.rs`)

| Test | Description |
|------|-------------|
| `test_two_workers_different_tags_no_overlap` | Each worker only gets its tagged activities |
| `test_two_workers_overlapping_tags` | Shared tag activities go to either worker |
| `test_orchestrator_schedules_for_other_workers` | Runtime with None filter can schedule activities picked up by other workers |

### Provider Validation Tests (`src/provider_validation/tag_routing.rs`)

| Test | Description |
|------|-------------|
| `test_enqueue_with_tag_stores_tag` | WorkItem with tag → stored with tag column |
| `test_enqueue_null_tag_stores_null` | WorkItem with None → NULL in tag column |
| `test_fetch_default_only_filter` | Only NULL tag items returned |
| `test_fetch_named_filter` | Only matching tag items returned |
| `test_fetch_mixed_filter` | NULL and matching tag items returned |
| `test_fetch_none_filter_returns_nothing` | Empty result for None filter |
| `test_tag_filter_with_lock_semantics` | Tag filtering works with peek-lock |

### Stress Tests

| Test | Description |
|------|-------------|
| `test_high_throughput_with_tags` | 1000 activities across 3 tags, verify correct routing |
| `test_tag_routing_under_contention` | Multiple workers competing for same tag |

### E2E Sample (`examples/llm_worker.rs`)

Demonstrates the LLM worker pattern with activity tags:

```rust
//! Example: LLM Worker with Activity Tags
//!
//! Demonstrates routing activities to a specialized worker
//! that has an LLM model and embeddings cache loaded.
//!
//! Run with: cargo run --example llm_worker

use duroxide::prelude::*;
use std::sync::Arc;

// Simulated LLM model (in real use, this would be a loaded model)
struct LlmModel {
    name: String,
}

impl LlmModel {
    fn new(name: &str) -> Self {
        println!("Loading LLM model: {}", name);
        Self { name: name.to_string() }
    }
    
    fn ask(&self, question: &str) -> String {
        format!("[{}] Answer to: {}", self.name, question)
    }
    
    fn embed(&self, text: &str) -> Vec<f32> {
        // Simulated embeddings
        vec![0.1, 0.2, 0.3, text.len() as f32]
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = SqliteProvider::new_in_memory().await?;
    let store = Arc::new(store);
    
    // === LLM WORKER SETUP ===
    // Load model once at startup (expensive operation)
    let model = Arc::new(LlmModel::new("llama-7b"));
    
    let model_ask = model.clone();
    let model_embed = model.clone();
    
    let llm_activities = ActivityRegistry::builder()
        .register("AskQuestion", move |_ctx: ActivityContext, question: String| {
            let model = model_ask.clone();
            async move { Ok(model.ask(&question)) }
        })
        .register("GenerateEmbeddings", move |_ctx: ActivityContext, text: String| {
            let model = model_embed.clone();
            async move { 
                let embeddings = model.embed(&text);
                Ok(serde_json::to_string(&embeddings).unwrap())
            }
        })
        .build();
    
    // LLM worker only processes "llm" tagged activities
    let llm_options = RuntimeOptions::default()
        .with_worker_tags(TagFilter::tags(["llm"]));
    
    let _llm_runtime = Runtime::start_with_options(
        store.clone(),
        Arc::new(llm_activities),
        OrchestrationRegistry::builder().build(),
        llm_options,
    ).await;
    
    // === ORCHESTRATOR SETUP ===
    let orchestrations = OrchestrationRegistry::builder()
        .register("ProcessDocument", |ctx: OrchestrationContext, doc: String| async move {
            // Route to LLM worker
            let summary = ctx.schedule_activity("AskQuestion", format!("Summarize: {}", doc))
                .with_tag("llm")
                .into_activity()
                .await?;
            
            let embeddings = ctx.schedule_activity("GenerateEmbeddings", doc)
                .with_tag("llm")
                .into_activity()
                .await?;
            
            Ok(format!("Summary: {}\nEmbeddings: {}", summary, embeddings))
        })
        .build();
    
    // Orchestrator doesn't process any activities (TagFilter::none())
    let orch_activities = ActivityRegistry::builder().build();
    let orch_options = RuntimeOptions::default()
        .with_worker_tags(TagFilter::none());
    
    let orch_runtime = Runtime::start_with_options(
        store.clone(),
        Arc::new(orch_activities),
        orchestrations,
        orch_options,
    ).await;
    
    // === RUN ===
    let client = Client::new(store);
    client.start_orchestration("doc-1", "ProcessDocument", "Hello world document").await?;
    
    // Wait for completion
    let result = client.wait_for_completion("doc-1", None).await?;
    println!("Result: {:?}", result);
    
    orch_runtime.shutdown(None).await;
    Ok(())
}
```

---

## Implementation Order

1. Add `TagFilter` type with `MAX_WORKER_TAGS` limit to `src/providers/mod.rs`
2. Add `tag` field to `EventKind::ActivityScheduled` and `WorkItem::ActivityExecute`
3. Add `tag` to `DurableFuture` and implement `.with_tag()`
4. Add `worker_tags` field to `RuntimeOptions`
5. Update `Provider` trait - add `tag_filter` param to `fetch_work_item()`
6. Add SQLite migration and implementation
7. Update worker dispatcher to pass filter from options
8. Update replay engine to store tag from DurableFuture
9. Add observability (metrics, tracing, logs)
10. Write tests
11. Add `examples/llm_worker.rs` e2e sample
12. Update documentation

---

## References

- [Temporal Task Queues](https://docs.temporal.io/workers#task-queue)
- [Azure Durable Functions Task Hubs](https://docs.microsoft.com/en-us/azure/azure-functions/durable/durable-functions-task-hubs)
