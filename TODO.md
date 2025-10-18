## Session Notes (2025-10-18)

### Macro System - COMPLETE! 🎉

Just completed full implementation of duroxide macro system (Phases 1-6):
- All 10 macros implemented and tested (100%)
- ~8,000 lines of comprehensive design documentation
- 577 lines of production-ready macro code
- 250+ tests passing, zero breaking changes
- 6 examples updated to use macros, 1 low-level reference created

**Key insight:** Macros provide ~70% code reduction with 100% type safety and zero runtime overhead.

### Current Focus Areas

**Deterministic Patterns:**
- Identified that `ctx.join()` and `ctx.select2()` must be used instead of `futures::try_join_all()` and `tokio::select!`
- Documented in `docs/DETERMINISTIC-PATTERNS.md`
- Examples updated to use correct patterns

**Future macro enhancements to consider:**
- `durable_future!()` macro or `.schedule()` method for cleaner join/select patterns
- Currently requires manual `ctx.schedule_activity()` for fan-out
- Could provide better ergonomics while maintaining determinism

**Next priorities:**
- Cancellation mechanism improvements (see line 23)
- Provider error classification (Retryable/NonRetryable)
- Inline orchestration functions (orch-side functions)

### Macro Feature Requests Noted
- Join/select with typed macros (better ergonomics for fan-out patterns)
- Versioning across crates
- Inline orch functions for lightweight deterministic transforms
- Enhanced tracing with structured fields

---

## Durable Task Rust Core – TODOs

- Drop crates/dependencies that aren't needed
- Add orchestrator functions
- Macros for syntactic sugar [DESIGNED - see docs/proposals/MACRO-FINAL-DESIGN.md]

### Reliability & Provider API

- Macros: 
  - macros for joins
  - macros for timers and external events
  - macros for newguid, 
  - detailed tests
  - version of e2e_samples
  - provider, and tracing config
- macros for activities, orchestrations
- orchestration functions!
- build node and python bindings as well
- convert execution, instance statuses to enums
- Introduce a provider error type with Retryable/NonRetryable classification; update runtime to use it for retries across all provider ops (not just ack_orchestration_item)
- Proper lock / visibility timeouts
- review duplicate orch instance ids
- perf pass over replay engine
- parallelize dispatcher loops
- lock TTL for timer and worker queues and update lease
- Reduce ornamental user code in orchestrations and acivities
- Continue the provider simplification
- Rename to provider
- fault inject: "TODO : fault injection :"
- code coverage
- CLI tooling to manipulate history/queue state for devops
- add cancellation and status from within the orchestration
- example versioned crates with orchestrations and loaders
- profiling replay and providers
- performance improvements for runtime
- "pub-sub" external events
- sharding and scale out to multiple runtimes
- strongly typed activity and orchestration calls?
- cancellations via JoinHandles?
- Add proper metrics.
- Build a website to visualize execution.
- Write an Azure Blob based provider.
- Batch the calls to logging, don't spin up an activity per
- Orchestration state? Monitoring? Visiblity? 
- Real world samples (provisioning resources in Azure e.g.)

## DONE

- Macros for syntactic sugar [COMPLETE - All 10 macros implemented! Phases 1-6 done!]
  - #[activity(typed)] - Type-safe activities with auto-registration
  - #[orchestration] - Orchestrations with versioning & client helpers  
  - durable!() - Universal invocation macro
  - durable_trace_*!() - Durable logging (info/warn/error/debug)
  - durable_newguid!() - Deterministic GUID generation
  - durable_utcnow!() - Deterministic timestamps
  - Runtime::builder() - Builder pattern with auto-discovery
  - #[duroxide::main] - Zero-ceremony main function
  - Client helpers auto-generated (::start, ::wait, ::run, ::status, ::cancel)
  - Cross-crate composition support via linkme
  - ~70% code reduction, 100% type-safe, zero runtime overhead
  - See: docs/proposals/MACRO-FINAL-DESIGN.md (5,999 lines)
  - Examples updated: hello_world.rs, fan_out_fan_in.rs, delays_and_timeouts.rs + 3 more
  - Reference example: low_level_api.rs (shows manual API)

## DONE

- sqlite instance table should not have a status, it should only be in the execution table
- updated to sqlx 0.8
- remove unused crate dependencies
- Update all provider and API docs, get ready to push crate
- implementing a provider requires the implementor to look deeply at the sqlite provider code
- clean up leaky abstractions (provider should not have to compute the orchestration state and output e.g)
- drop the polling frequence for the dispatchers
- Cleanup the docs before making public
- write a real world orchestrations with versioning etc
- fix up the tracing to not use activities [DONE - tracing is now host-side only]
- host level events (tracing, guid, time) [IN PROGRESS via RFC]
- SQLite provider with full transactional support and e2e test parity
  - Full ACID transactional semantics
  - Provider-backed timer queue with delayed visibility
  - Handles concurrent instance execution
  - All 25 core e2e tests passing
- Fixed trace activities to be fire-and-forget (no longer cause nondeterminism)
- Timer acknowledgment only after firing (reliability fix)
- Worker queue acknowledgment only after completion enqueue (reliability fix)
- Return instance history as well
- Update history + complete locks + enqueue in the same call
- typed parameters for activities and orchestrations
- proper timer implementation in the provider
- versioning strategy
- Support for orchestration chaining + eternal orchestrations
- Need to understand this oneshot channel to await on
- transactional processing - harden queue read semantics (see docs/reliability-queue-and-history.md)
- review how active_instances in the runtime work, why are we reenqueuing events in certain cases
- On next session: review tests covering ContinueAsNew and multi-execution IDs
	- Files: `tests/e2e_continue_as_new.rs` (both tests)
	- Also revisit runtime APIs to surface `list_executions` and `get_execution_history` consistently- ContinueAsNew support
- test hung because the initial orchestration takes longer to start!
- tests for orchestration state, 
- tests for provider <-> runtime resumption of multiple orchestrations. 
- dequeue multiple item batch
- do a pass through the code. 
- Add orchestration registry
- Add capability to the runtime to resume persisted orchestrations from the history provider
- Add signalling mechanism in the provider which runtime can poll to trigger replay
- resolve the test hang!!
- Document all methods
- Add proper logging
- Write a file system based provider.
- Crash recovery tests.
- Error handling for bad activity or orchestration names, or accessing wrong instance IDs
- Add a Gabbar test
- Logging typed handlers
- Max size of orchestration history
- Detailed documentation in the docs folder for how the system works
- Remove dead code, including the one with allow(dead_code)
- Write detailed architecture and user documentation 
- Formalize a provider model for the state, queues and timers.
- Write GUID and time deterministic helper methods.

## POSTPONED

- implement Unpin typed future wrappers for `_typed` adapters
- redo the orchestration registry change with gpt5 and compare
- mermaid diagrams for orchestrations???
- remove the into_activity() and similar methods
