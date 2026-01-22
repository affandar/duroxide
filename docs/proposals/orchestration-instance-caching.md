# Proposal: Orchestration Instance Caching (Decoupled Replay Lifetime)

## Problem Statement

Today, the orchestration dispatcher follows this lifecycle for every message receive:

```
1. Fetch orchestration item (instance lock + history + messages)
2. Rehydrate: Load full history from storage
3. Replay: Execute orchestration from beginning to rebuild in-memory state
4. Process: Feed new messages, execute one turn
5. Persist: Commit history delta + new work items
6. Dehydrate: Release lock, discard in-memory orchestration state
7. Repeat from step 1 for next message
```

**The Problem**: When messages trickle in for an orchestration (not fast enough to batch, not slow enough to justify full rehydration), we pay the full CPU + I/O cost of replay for every single message. For orchestrations with long histories, this becomes very expensive.

### Cost Breakdown Per Message Today

| Step | I/O | CPU | Memory |
|------|-----|-----|--------|
| Fetch item | DB read (history + queue) | Minimal | Load history |
| Replay | None | O(history_len) - re-execute all awaits | Full orchestration state |
| Process | None | O(1) - one new event | Delta events |
| Persist | DB write | Minimal | None |
| Dehydrate | None | None | Free all |

For an orchestration with 1000 events in history receiving 10 messages 100ms apart:
- **Today**: 10 × full replay = 10 × O(1000) = O(10,000) replay operations
- **With caching**: 1 × full replay + 9 × incremental = O(1000) + 9 × O(1) ≈ O(1000) replay operations

## Proposed Solutions

### Option 1: In-Memory Instance Cache with Extended Lock

**Concept**: Keep hydrated orchestration instances in memory with an extended lock, allowing subsequent messages to be processed without replay.

```
┌─────────────────────────────────────────────────────────────────┐
│                    Orchestration Worker                          │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────────────────────────────────────────────────────┐│
│  │              In-Memory Instance Cache                        ││
│  │  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐            ││
│  │  │ instance-A  │ │ instance-B  │ │ instance-C  │            ││
│  │  │ - history   │ │ - history   │ │ - history   │            ││
│  │  │ - state     │ │ - state     │ │ - state     │            ││
│  │  │ - lock      │ │ - lock      │ │ - lock      │            ││
│  │  │ - last_msg  │ │ - last_msg  │ │ - last_msg  │            ││
│  │  └─────────────┘ └─────────────┘ └─────────────┘            ││
│  └─────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
```

**Lifecycle**:
```
1. Fetch message for instance
2. Check cache: instance in cache?
   ├─ YES: Use cached state, skip replay
   └─ NO: Fetch history, replay, cache instance
3. Process message with cached state
4. Persist history delta
5. Update cache state
6. Start idle timer (configurable, e.g., 5-30 seconds)
7. If new message arrives before timer: goto step 3
8. If timer expires: evict from cache, release lock
```

**Configuration Options**:
```rust
pub struct InstanceCacheOptions {
    /// Enable instance caching
    pub enabled: bool,
    
    /// Max time to keep an instance in cache after last message
    /// Default: 10 seconds
    pub idle_timeout: Duration,
    
    /// Max instances to keep in cache per worker
    /// Uses LRU eviction when exceeded
    /// Default: 100
    pub max_cached_instances: usize,
    
    /// Max time to hold an instance lock (extended from normal lock timeout)
    /// Must be >= idle_timeout
    /// Default: 60 seconds
    pub extended_lock_timeout: Duration,
    
    /// Max history size (events) before forcing eviction
    /// Prevents memory bloat from very large histories
    /// Default: 10,000 events
    pub max_history_size: usize,
}
```

**Pros**:
- Significant CPU savings for trickling messages
- Transparent to orchestration code
- Configurable tradeoffs (memory vs CPU)
- Works with existing provider interface

**Cons**:
- Increased memory usage
- Longer lock hold times reduce parallelism across workers
- Complexity in cache eviction and lock management
- Risk of stale state if provider is modified externally

**Implementation Complexity**: Medium

---

### Option 2: Message Coalescing / Delayed Dequeue

**Concept**: Instead of processing messages immediately, wait briefly for more messages to arrive and batch them together.

```
Message arrives → Start coalesce timer (e.g., 100ms)
                         │
                         ▼
            ┌────────────────────────┐
            │  Coalesce Window       │
            │  - Collect messages    │
            │  - Max wait: 100ms     │
            │  - Max batch: 50       │
            └────────────────────────┘
                         │
                         ▼
              Process entire batch
              (single replay)
```

**Provider Changes**:
```rust
pub trait Provider {
    /// Fetch orchestration item with coalescing
    /// Waits up to `coalesce_timeout` for additional messages
    async fn fetch_orchestration_item_coalesced(
        &self,
        lock_timeout: Duration,
        poll_timeout: Duration,
        coalesce_timeout: Duration,
        max_batch_size: usize,
    ) -> Result<Option<(OrchestrationItem, String, u32)>, ProviderError>;
}
```

**Configuration**:
```rust
pub struct CoalesceOptions {
    /// Enable message coalescing
    pub enabled: bool,
    
    /// How long to wait for additional messages
    /// Default: 100ms
    pub coalesce_window: Duration,
    
    /// Maximum messages to batch
    /// Default: 50
    pub max_batch_size: usize,
    
    /// Only coalesce if history size exceeds this threshold
    /// Small orchestrations don't benefit from coalescing
    /// Default: 100 events
    pub min_history_for_coalesce: usize,
}
```

**Pros**:
- Simple conceptually
- No memory overhead between batches
- Works well for bursty workloads
- Minimal changes to existing architecture

**Cons**:
- Adds latency (coalesce window) to every message
- Doesn't help if messages are spaced > coalesce_window apart
- May hurt throughput for fast message streams (waiting when not needed)
- Tuning the window is workload-dependent

**Implementation Complexity**: Low

---

### Option 3: Incremental Replay with Checkpoints

**Concept**: Periodically snapshot the orchestration state so replay can start from the checkpoint instead of from the beginning.

```
History:  [E1][E2][E3][E4][E5][CP1][E6][E7][E8][E9][E10][CP2][E11][E12]
                            │                        │
                     Checkpoint 1              Checkpoint 2
                     (at event 5)              (at event 10)

To process E13:
  - Load checkpoint CP2 (state at E10)
  - Replay only E11, E12
  - Process E13
```

**Checkpoint Data**:
```rust
pub struct OrchestrationCheckpoint {
    /// Event ID this checkpoint was taken after
    pub after_event_id: u64,
    
    /// Serialized orchestration state
    /// This requires orchestrations to be serializable
    pub state: Vec<u8>,
    
    /// Pending actions at checkpoint time
    pub pending_actions: Vec<Action>,
    
    /// Completion map state
    pub completion_map: CompletionMapSnapshot,
    
    /// Checkpoint creation timestamp
    pub created_at: DateTime<Utc>,
}
```

**Provider Changes**:
```rust
pub trait Provider {
    /// Store a checkpoint for an instance
    async fn store_checkpoint(
        &self,
        instance: &str,
        execution_id: u64,
        checkpoint: OrchestrationCheckpoint,
    ) -> Result<(), ProviderError>;
    
    /// Load the latest checkpoint for an instance
    async fn load_checkpoint(
        &self,
        instance: &str,
        execution_id: u64,
    ) -> Result<Option<OrchestrationCheckpoint>, ProviderError>;
    
    /// Fetch orchestration item with checkpoint
    async fn fetch_orchestration_item_with_checkpoint(
        &self,
        lock_timeout: Duration,
        poll_timeout: Duration,
    ) -> Result<Option<(OrchestrationItem, Option<OrchestrationCheckpoint>, String, u32)>, ProviderError>;
}
```

**Checkpoint Strategy Options**:
```rust
pub enum CheckpointStrategy {
    /// Checkpoint every N events
    EveryNEvents(usize),
    
    /// Checkpoint when history size exceeds threshold
    OnHistorySize(usize),
    
    /// Checkpoint after specific event types (e.g., ActivityCompleted)
    OnEventType(Vec<EventKind>),
    
    /// Time-based checkpointing
    EveryDuration(Duration),
    
    /// Adaptive: checkpoint when replay cost exceeds threshold
    Adaptive { max_replay_events: usize },
}
```

**Pros**:
- Bounds replay cost regardless of history length
- Works well for very long-running orchestrations
- Checkpoints can be garbage collected with history compaction
- No lock extension needed

**Cons**:
- Requires serializable orchestration state (major constraint!)
- Storage overhead for checkpoints
- Checkpoint creation adds overhead
- Complexity in checkpoint validation/versioning
- **BREAKING**: Current design relies on replay from history, not serialized state

**Implementation Complexity**: High (requires fundamental changes to execution model)

---

### Option 4: Hybrid - Instance Cache with Fallback Coalescing

**Concept**: Combine Options 1 and 2 for best of both worlds.

```
┌──────────────────────────────────────────────────────────────────┐
│                      Message Arrival                              │
└──────────────────────────────────────────────────────────────────┘
                              │
                              ▼
              ┌───────────────────────────────┐
              │   Is instance in cache?       │
              └───────────────────────────────┘
                    │                │
                   YES               NO
                    │                │
                    ▼                ▼
           ┌─────────────┐   ┌─────────────────────┐
           │ Process     │   │ Start coalesce      │
           │ immediately │   │ timer (100ms)       │
           │ with cache  │   └─────────────────────┘
           └─────────────┘              │
                                        ▼
                              ┌─────────────────────┐
                              │ Fetch + Replay +    │
                              │ Process batch       │
                              │ + Cache instance    │
                              └─────────────────────┘
```

**Benefits**:
- Cached instances get immediate processing (no latency)
- Cold instances benefit from coalescing (batch replay)
- Automatic cache warming for active orchestrations
- Graceful degradation when cache is full

**Configuration**:
```rust
pub struct HybridCacheOptions {
    // Instance cache settings
    pub cache: InstanceCacheOptions,
    
    // Coalescing for cache misses
    pub coalesce_on_miss: CoalesceOptions,
    
    /// Whether to coalesce even for cached instances
    /// (wait briefly for more messages before processing)
    pub coalesce_on_hit: bool,
}
```

**Implementation Complexity**: Medium-High

---

### Option 5: Sticky Instance Routing

**Concept**: Route messages for the same instance to the same worker, maximizing cache hit rate.

```
                    ┌─────────────────────┐
                    │   Message Router    │
                    │  (consistent hash)  │
                    └─────────────────────┘
                              │
         ┌────────────────────┼────────────────────┐
         │                    │                    │
         ▼                    ▼                    ▼
   ┌───────────┐        ┌───────────┐        ┌───────────┐
   │ Worker 0  │        │ Worker 1  │        │ Worker 2  │
   │           │        │           │        │           │
   │ Cache:    │        │ Cache:    │        │ Cache:    │
   │ inst-A    │        │ inst-B    │        │ inst-C    │
   │ inst-D    │        │ inst-E    │        │ inst-F    │
   └───────────┘        └───────────┘        └───────────┘
```

**Routing Strategies**:
```rust
pub enum RoutingStrategy {
    /// Hash instance ID to worker
    ConsistentHash,
    
    /// Track which worker has instance cached, route to it
    CacheAware,
    
    /// Prefer last worker that processed instance
    Affinity { max_age: Duration },
}
```

**Pros**:
- Maximizes cache efficiency
- Works with Option 1 (Instance Cache)
- Natural load balancing with consistent hashing

**Cons**:
- Requires coordination between workers (or provider support)
- Hot instances can overload single worker
- Complexity in failover when worker dies
- May not work well with external queue systems

**Implementation Complexity**: Medium-High

---

## Recommendation

For duroxide, I recommend **Option 4 (Hybrid - Instance Cache with Fallback Coalescing)** with a phased implementation:

### Phase 1: Message Coalescing (Option 2)
- Low complexity, immediate benefit
- Add `coalesce_window` to `fetch_orchestration_item`
- Provider waits briefly for additional messages before returning
- No runtime changes needed

### Phase 2: In-Memory Instance Cache (Option 1)
- Add instance cache to orchestration dispatcher
- Extend lock while instance is cached
- LRU eviction with configurable limits
- Background lock renewal for cached instances

### Phase 3: Sticky Routing (Option 5)
- Only if multi-worker scenarios show cache thrashing
- Add cache-aware routing hints to provider

### Why Not Checkpoints (Option 3)?

Checkpointing requires serializable orchestration state, which is a fundamental change to the execution model. Rust closures and async state machines are not easily serializable. This would require:
- Custom serialization for all user orchestration state
- Versioning for checkpoint compatibility across code changes
- Careful handling of captured references and runtime state

This is a much larger undertaking and may not be feasible without significant API changes.

---

## API Sketch for Phase 1 + 2

### Runtime Options

```rust
pub struct RuntimeOptions {
    // ... existing options ...
    
    /// Instance caching configuration
    pub instance_cache: InstanceCacheConfig,
}

pub struct InstanceCacheConfig {
    /// Enable the instance cache
    /// Default: false (opt-in initially)
    pub enabled: bool,
    
    /// How long to keep instance in cache after last message
    /// Default: 10 seconds
    pub idle_timeout: Duration,
    
    /// Maximum instances to cache per worker
    /// Default: 100
    pub max_instances: usize,
    
    /// Maximum history size (events) before forcing eviction
    /// Default: 10_000
    pub max_history_size: usize,
    
    /// Coalesce window for cache misses
    /// How long to wait for additional messages when instance is not cached
    /// Default: 100ms
    pub coalesce_window: Duration,
    
    /// Maximum messages to coalesce
    /// Default: 50
    pub max_coalesce_batch: usize,
}
```

### New Provider Method (Optional Enhancement)

```rust
pub trait Provider {
    // ... existing methods ...
    
    /// Peek for additional messages for an instance (non-blocking)
    /// Used to collect more messages while coalescing
    async fn peek_orchestration_messages(
        &self,
        instance: &str,
        lock_token: &str,
        max_messages: usize,
    ) -> Result<Vec<WorkItem>, ProviderError>;
}
```

### Cache Structure

```rust
struct CachedInstance {
    /// Instance identifier
    instance: String,
    
    /// Hydrated history manager (maintains full state)
    history_manager: HistoryManager,
    
    /// Current lock token
    lock_token: String,
    
    /// When the lock expires
    lock_expires_at: Instant,
    
    /// Last activity timestamp (for idle eviction)
    last_activity: Instant,
    
    /// Execution ID for this cached state
    execution_id: u64,
}

struct InstanceCache {
    /// LRU cache of instances
    instances: LruCache<String, CachedInstance>,
    
    /// Configuration
    config: InstanceCacheConfig,
}
```

---

## Metrics to Add

```rust
// Cache metrics
orchestration_cache_hits_total
orchestration_cache_misses_total
orchestration_cache_evictions_total{reason="idle|lru|size|error"}
orchestration_cache_size_current
orchestration_replay_events_saved_total  // Events skipped due to cache

// Coalescing metrics
orchestration_coalesce_batches_total
orchestration_coalesce_messages_per_batch  // Histogram
orchestration_coalesce_wait_duration_seconds  // Histogram
```

---

## Open Questions

1. **Lock extension strategy**: Should we proactively extend locks on a timer, or extend on each message processed?

2. **Cache coherence**: What happens if an external event is sent to a cached instance from a different worker?

3. **Graceful degradation**: When cache is full, should we evict LRU or reject new cache entries?

4. **Provider changes**: Should coalescing be a provider responsibility or runtime responsibility?

5. **Memory limits**: Should we track total cache memory usage, not just instance count?

---

## Next Steps

1. Discuss and refine this proposal
2. Choose implementation phases
3. Design detailed API for selected options
4. Implement Phase 1 (Coalescing) as MVP
5. Measure impact before proceeding to Phase 2
