# Duroxide Ecosystem Architecture

This document describes the Duroxide ecosystem—a family of Rust crates for building durable, fault-tolerant workflows.

---

## What is Duroxide?

**Duroxide** is a durable execution framework for Rust. It lets you write async workflows that survive crashes, restarts, and failures. The framework handles replay, state persistence, and recovery automatically.

The ecosystem consists of:

| Project | Role | Description |
|---------|------|-------------|
| **duroxide** | Core Framework | Replay engine, orchestration runtime, SQLite provider |
| **duroxide-pg** | Storage Provider | PostgreSQL provider using stored procedures |
| **toygres** | Sample Application | PostgreSQL fleet management built on duroxide |

---

## Architecture Layers

```
┌───────────────────────────────────────────────────────────┐
│                      APPLICATIONS                          │
│                                                            │
│                        toygres                             │
│                    (Fleet Manager)                         │
│                                                            │
├───────────────────────────────────────────────────────────┤
│                       PROVIDERS                            │
│                                                            │
│                      duroxide-pg                           │
│                     (PostgreSQL)                           │
│                                                            │
├───────────────────────────────────────────────────────────┤
│                     CORE FRAMEWORK                         │
│                                                            │
│                        duroxide                            │
│   (Runtime, Replay Engine, SQLite Provider, Client API)   │
│                                                            │
└───────────────────────────────────────────────────────────┘
```

**Core Framework (duroxide):** The foundation. Provides the runtime, replay engine, and a bundled SQLite provider. All other projects depend on this.

**Providers (duroxide-pg):** Storage backends that implement the Provider trait. Swap providers without changing application code.

**Applications (toygres):** Sample applications built on duroxide. Demonstrate real-world usage patterns.

---

## Duroxide Core Components

The core `duroxide` crate contains several key modules:

```
duroxide/
├── Runtime
│   ├── Orchestration Dispatcher  (processes workflow turns)
│   ├── Worker Dispatcher         (executes activities)
│   ├── Replay Engine             (deterministic state recovery)
│   └── Observability             (tracing, metrics)
│
├── Providers
│   ├── Provider Trait            (storage abstraction)
│   ├── SQLite Provider           (bundled, file or in-memory)
│   └── Provider Validation       (test harness for custom providers)
│
├── Client
│   └── Orchestration Client API  (start, wait, cancel, query)
│
└── Futures
    └── Durable Futures           (join, select, deterministic resolution)
```

**Deep-dive documentation:**

| Topic | Document |
|-------|----------|
| Replay algorithm | [Durable Futures Internals](durable-futures-internals.md) |
| Implementation details | [Durable Futures Internals — Implementation](durable-futures-internals.md#implementation-details-for-maintainers) |
| Turn-based execution | [Execution Model](execution-model.md) |
| Sub-orchestrations | [Sub-orchestrations](sub-orchestrations.md) |
| Continue-as-new | [ContinueAsNew Semantics](continue-as-new.md) |
| External events | [External Event Semantics](external-events.md) |
| Provider implementation | [Provider Implementation Guide](provider-implementation-guide.md) |
| Observability | [Observability Guide](observability-guide.md) |

---

## Data Flow

How data flows through a duroxide application:

```
┌───────────────────────────────────────────────────────────────────────────────┐
│                                  Your App                                     │
│                                                                               │
│  • Registers orchestrations and activities                                    │
│  • Uses Client API to start/wait/cancel                                       │
└───────────────────┬───────────────────────────────────┬───────────────────────┘
                    │                                   │
                    │ start/wait/cancel                 │ register functions
                    ▼                                   ▼
           ┌─────────────────┐                 ┌─────────────────┐
           │     Client      │                 │     Runtime     │
           └────────┬────────┘                 └────────┬────────┘
                    │                                   │
                    │                    ┌──────────────┴──────────────┐
                    │                    │                             │
                    │                    ▼                             ▼
                    │           ┌─────────────────┐           ┌─────────────────┐
                    │           │  Orchestration  │           │    Worker       │
                    │           │  Dispatcher     │           │    Dispatcher   │
                    │           │                 │           │                 │
                    │           │ • Fetch turn    │           │ • Fetch work    │
                    │           │ • Replay        │           │ • Execute       │
                    │           │ • Commit        │           │ • Report result │
                    │           └────────┬────────┘           └────────┬────────┘
                    │                    │                             │
                    │                    │ fetch/ack                   │ fetch/ack
                    │                    │                             │
                    ▼                    ▼                             ▼
           ┌────────────────────────────────────────────────────────────────────┐
           │                          Provider                                  │
           │                        (SQLite/PG)                                 │
           ├────────────────────┬───────────────────────┬───────────────────────┤
           │  Orchestrator      │    Worker Queue       │    Event History      │
           │  Queue             │                       │                       │
           │                    │    • ActivityExec     │    [Event 1]          │
           │  • Start           │                       │    [Event 2]          │
           │  • Completed       │                       │    [Event 3]          │
           │  • TimerFired      │                       │    ...                │
           │  • ExternalEvent   │                       │                       │
           └────────────────────┴───────────────────────┴───────────────────────┘
```

1. **Client** enqueues work (StartOrchestration) via Provider
2. **Orchestration Dispatcher** fetches, replays, executes, commits
3. **Worker Dispatcher** fetches activities, executes, reports completion
4. **Provider** stores all state in Event History and manages queues

---

## Dependency Graph

```
                    Applications
                         │
                         ▼
                   ┌───────────┐
                   │  toygres  │
                   └─────┬─────┘
                         │
                    Providers
                         │
                         ▼
                ┌─────────────────┐
                │   duroxide-pg   │
                └────────┬────────┘
                         │
                         ▼
                ┌─────────────────┐
                │    duroxide     │
                │ (core framework)│
                └─────────────────┘
```

All projects ultimately depend on the core `duroxide` crate.

---

## Project Details

### duroxide (Core Framework)

The foundation of the ecosystem. Provides:

- **Replay Engine:** Deterministic state recovery from event history. See [Durable Futures Internals](durable-futures-internals.md) for details.
- **Runtime:** Two dispatchers (orchestration + worker) with lock renewal
- **SQLite Provider:** Bundled provider for development and embedded use
- **Provider Trait:** Abstraction for custom storage backends
- **Client API:** Start, wait, cancel, query orchestrations
- **Durable Futures:** `join()`, `select()`, `select2()` with history-ordered resolution

### duroxide-pg (PostgreSQL Provider)

PostgreSQL implementation:

- Implements Provider trait for PostgreSQL
- Uses stored procedures for atomic operations
- Includes `pg-stress` benchmarking tool
- Repository: [github.com/affandar/duroxide-pg](https://github.com/affandar/duroxide-pg)

### toygres (PostgreSQL Fleet Manager)

Sample application demonstrating duroxide:

- REST API and CLI for managing PostgreSQL instances
- Kubernetes deployment orchestrations
- Long-running "instance actor" pattern with health checks
- System pruning for history cleanup
- Repository: [github.com/affandar/toygres](https://github.com/affandar/toygres)

---

## Choosing a Provider

| Use Case | Recommended Provider |
|----------|---------------------|
| Development, testing | SQLite (bundled) |
| Embedded applications | SQLite (file-based) |
| Production with PostgreSQL | duroxide-pg |
| Custom storage | Implement Provider trait |

---

## Getting Started

1. **Learn the core:** Start with `duroxide` and the [Orchestration Guide](ORCHESTRATION-GUIDE.md)
2. **Understand internals:** Read [Durable Futures Internals](durable-futures-internals.md) for how replay works
3. **Build a provider:** See [Provider Implementation Guide](provider-implementation-guide.md)
4. **Study real usage:** Look at `toygres` for production patterns
