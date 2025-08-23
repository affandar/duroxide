## Durable Task Rust Core – TODOs

- code coverage
- Cleanup the docs before making public
- CLI tooling to manipulate history/queue state for devops
- add cancellation and status from within the orchestration
- write a real world orchestrations with versioning etc
- example versioned crates with orchestrations and loaders
- proper timer implementation in the provider
- profiling replay and providers
- performance improvements for runtime
- "pub-sub" external events
- sharding and scale out to multiple runtimes
- mermaid diagrams for orchestrations???
- strongly typed activity and orchestration calls?
- typed parameters for activities and orchestrations
- cancellations via JoinHandles?
- Add proper metrics.
- Build a website to visualize execution.
- Write an Azure Blob based provider.
- Batch the calls to logging, don't spin up an activity per
- Orchestration state? Monitoring? Visiblity? 
- Real world samples (provisioning resources in Azure e.g.)

## DONE

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

- implement Unpin typed future wrappers for `_typed` adapters (see docs/typed-futures-design.md)
- redo the orchestration registry change with gpt5 and compare
