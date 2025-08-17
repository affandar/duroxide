## Durable Task Rust Core – TODOs

- do a pass through the code. 
- tests for provider <-> runtime resumption of multiple orchestrations. 
- tests for orchestration state, 
- tests for e
- redo the orchestration registry change with gpt5 and compare
- test hung because the initial orchestration takes longer to start!
- Add proper metrics.
- Build a website to visualize execution.
- Write an Azure Blob based provider.
- Batch the calls to logging, don't spin up an activity per
- Orchestration state? Monitoring? Visiblity? 
- Support for orchestration chaining + eternal orchestrations
- Real world samples (provisioning resources in Azure e.g.)

## DONE

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

