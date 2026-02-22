# Provider Improvements Roadmap

This document captures needed improvements to duroxide providers, including scalability, performance, and new provider implementations.

## Summary

| # | Improvement | Issue | Description |
|---|-------------|-------|-------------|
| 1 | [Distributed Provider](#1-distributed-provider) | [#18](https://github.com/microsoft/duroxide/issues/18) | Sharded provider layer aggregating multiple underlying providers for horizontal scale |
| 2 | [Postgres Provider Performance](#2-postgres-provider-performance) | [#19](https://github.com/microsoft/duroxide/issues/19) | Performance improvements for duroxide-pg, including Citus/Elastic cluster evaluation |
| 3 | [Zero-Disk Architecture Provider](#3-zero-disk-architecture-provider) | [#20](https://github.com/microsoft/duroxide/issues/20) | Provider based on cloud blob storage (Azure Storage) with no local disk dependency |

---

## 1. Distributed Provider

### Problem

The current provider model assumes a single backend (SQLite, Postgres, etc.). For large-scale deployments:
- Single database becomes a bottleneck
- Vertical scaling has limits
- No horizontal scaling path for orchestration throughput
- All orchestrations compete for the same resources

### Proposed Solution

Build a **distributed provider** that aggregates multiple underlying providers as shards. This layer sits on top of existing providers without requiring changes to the `OrchestrationProvider` trait.

**Architecture:**

```
                    ┌─────────────────────────────┐
                    │    Distributed Provider     │
                    │  (implements Provider trait)│
                    └─────────────┬───────────────┘
                                  │
           ┌──────────────────────┼──────────────────────┐
           │                      │                      │
           ▼                      ▼                      ▼
    ┌─────────────┐        ┌─────────────┐        ┌─────────────┐
    │  Shard 0    │        │  Shard 1    │        │  Shard 2    │
    │ (Provider)  │        │ (Provider)  │        │ (Provider)  │
    └─────────────┘        └─────────────┘        └─────────────┘
```

### Sharding Strategy

**Instance-based sharding:**
- Orchestrations are sharded by `hash(instance_id) % num_shards`
- All events, history, and state for an orchestration live on one shard
- All activities for an orchestration go to the same shard

**Cross-shard operations:**
- **Sub-orchestrations**: Sharded by their own instance_id → may land on different shard than parent
- **Detached orchestrations**: Same as sub-orchestrations
- **External events**: Routed to shard owning the target instance_id

### Transfer Queues (Eventual Consistency)

Cross-shard communication requires reliable message transfer:

```
┌─────────────┐    transfer queue    ┌─────────────┐
│  Shard 0    │ ──────────────────▶  │  Shard 1    │
│             │                      │             │
│ Parent orch │  SubOrchCompleted    │ Child orch  │
│ waiting...  │ ◀────────────────── │ completes   │
└─────────────┘    transfer queue    └─────────────┘
```

**Transfer queue semantics:**
- Each shard has outbound queues to every other shard
- Messages are persisted locally before send (at-least-once delivery)
- Receiver deduplicates by message ID
- Background workers drain transfer queues
- Supports: work item routing, completion notifications, external events

**Message types:**
```rust
enum TransferMessage {
    /// Route work item to correct shard
    WorkItem { target_shard: u32, item: WorkItem },
    /// Sub-orchestration/activity completed
    Completion { source_shard: u32, target_instance: String, event: Event },
    /// External event for instance on another shard
    ExternalEvent { target_instance: String, name: String, data: String },
}
```

### Consistency Model

- **Within shard**: Strong consistency (same as underlying provider)
- **Cross-shard**: Eventual consistency via transfer queues
- **Orchestration state**: Always consistent (single shard owns it)
- **Parent-child relationships**: Eventually consistent (acceptable for durable workflows)

### Configuration

```rust
pub struct DistributedProviderConfig {
    /// Underlying providers (one per shard)
    pub shards: Vec<Arc<dyn OrchestrationProvider>>,
    /// Sharding function (default: consistent hashing)
    pub shard_fn: Option<Box<dyn Fn(&str) -> u32>>,
    /// Transfer queue poll interval
    pub transfer_poll_interval: Duration,
    /// Max batch size for transfer queue drain
    pub transfer_batch_size: usize,
}
```

### Key Design Points

1. **No contract changes**: Distributed provider implements existing `OrchestrationProvider` trait
2. **Transparent to runtime**: Runtime doesn't know it's using sharded storage
3. **Pluggable shards**: Each shard can be any provider (SQLite, Postgres, etc.)
4. **Homogeneous recommended**: All shards should be same provider type for consistency
5. **Rebalancing**: Future work — support adding/removing shards with migration

### Implementation Considerations

- Transfer queues need persistent storage (can use shard's own provider)
- Deduplication requires tracking processed message IDs (with TTL cleanup)
- Health checks for individual shards
- Metrics per-shard for monitoring
- Consider consistent hashing for better rebalancing story

---

## 2. Postgres Provider Performance

### Current State

The Postgres provider is implemented in a separate repository:
- **Repository**: [microsoft/duroxide-pg](https://github.com/microsoft/duroxide-pg)

### Performance Improvement Areas

**2.1 Query Optimization:**
- Review and optimize hot-path queries
- Add missing indexes based on query patterns
- Use `EXPLAIN ANALYZE` to identify slow queries
- Consider partial indexes for status-based filtering

**2.2 Connection Pooling:**
- Tune pool size for workload
- Evaluate connection pool libraries (deadpool, bb8, sqlx built-in)
- Consider PgBouncer for external pooling

**2.3 Batch Operations:**
- Batch event inserts instead of individual INSERTs
- Use `COPY` for bulk history loading
- Batch queue operations where possible

**2.4 Lock Contention:**
- Review row-level locking strategy
- Consider advisory locks for orchestration-level locking
- Evaluate `SELECT FOR UPDATE SKIP LOCKED` patterns

### Citus / Azure Flexible Server Elastic Clusters

Evaluate using Citus (distributed Postgres) or Azure Database for PostgreSQL - Flexible Server with Elastic clusters:

**Potential Benefits:**
- Automatic sharding at database level
- Distributed query execution
- Reference tables for shared data
- Columnar storage for analytics

**Evaluation Questions:**
- Does duroxide query pattern fit Citus distribution model?
- Can `instance_id` be the distribution key?
- How does Citus handle cross-shard transactions?
- Cost comparison vs. application-level sharding (Distributed Provider above)

**Recommendation:** Evaluate Citus as an alternative to the Distributed Provider. If Citus handles sharding well at the database level, it may be simpler than application-level sharding.

---

## 3. Zero-Disk Architecture Provider

### Problem

Current providers require either:
- Local disk (SQLite)
- Managed database (Postgres)

For serverless/ephemeral compute environments:
- Local disk may not be available or persistent
- Managed databases add operational overhead and cost
- Want minimal infrastructure dependencies

### Proposed Solution

Build a provider that uses **cloud blob storage** as the primary persistence layer with no local disk dependency.

### Option A: SlateDB-based Provider

> 📄 See detailed proposal: [duroxide-pg/docs/SLATEDB_AZURE_PROPOSAL.md](https://github.com/microsoft/duroxide-pg/blob/main/docs/SLATEDB_AZURE_PROPOSAL.md)

[SlateDB](https://github.com/slatedb/slatedb) is an embedded key-value store built on object storage:
- LSM-tree architecture with object storage as SST backend
- No local disk required (uses object storage directly)
- Supports Azure Blob, S3, GCS
- Rust-native, designed for cloud-native workloads

**Architecture:**
```
┌─────────────────────────┐
│   Duroxide Runtime      │
└───────────┬─────────────┘
            │
┌───────────▼─────────────┐
│   SlateDB Provider      │
│   (OrchestrationProvider)│
└───────────┬─────────────┘
            │
┌───────────▼─────────────┐
│       SlateDB           │
│   (embedded KV store)   │
└───────────┬─────────────┘
            │
┌───────────▼─────────────┐
│    Azure Blob Storage   │
│    (or S3, GCS)         │
└─────────────────────────┘
```

**Benefits:**
- True zero-disk: all data in object storage
- Cost-effective: blob storage is cheap
- Serverless-friendly: no database to manage
- Durability: object storage provides 11 9's durability

**Considerations:**
- Latency: object storage has higher latency than local disk
- Compaction: SlateDB handles compaction in background
- Caching: may need in-memory caching layer for hot data

### Option B: RocksDB with Blob Storage Env

RocksDB supports custom `Env` implementations that can redirect I/O:

**Concept:**
- Implement RocksDB `Env` that reads/writes to blob storage
- RocksDB handles LSM-tree logic
- Blob storage provides persistence

**Challenges:**
- No strong production examples of this pattern
- RocksDB assumes low-latency I/O
- May require significant caching to be performant
- Compaction with high-latency I/O is problematic

**Recommendation:** SlateDB is purpose-built for this use case and is the preferred path. RocksDB-over-blob is a fallback if SlateDB doesn't meet requirements.

### Azure Storage Specifics

For Azure-focused deployments:
- Use Azure Blob Storage with hot tier for active data
- Consider Azure Data Lake Storage Gen2 for hierarchical namespace
- Leverage blob leases for distributed locking
- Use Azure CDN or caching layer if read latency is critical

### Implementation Phases

1. **Phase 1**: Prototype SlateDB provider with basic operations
2. **Phase 2**: Implement full `OrchestrationProvider` trait
3. **Phase 3**: Performance testing and optimization
4. **Phase 4**: Production hardening (retries, error handling, metrics)

---

## Open Questions

1. **Distributed Provider**: How to handle shard failures? Failover vs. wait for recovery?
2. **Distributed Provider**: Should we support heterogeneous shards (mix of SQLite and Postgres)?
3. **Citus**: Is the query pattern compatible with distributed tables?
4. **SlateDB**: What's the latency impact for hot-path operations?
5. **Zero-Disk**: How to handle provider initialization in serverless cold starts?

