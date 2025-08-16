## Durable Task Rust Core – TODOs

- Add proper metrics.
- Build a website to visualize execution.
- Write an Azure Blob based provider.
- Batch the calls to logging, don't spin up an activity per
- Orchestration state? Monitoring? Visiblity? 
- Support for orchestration chaining + eternal orchestrations
- Real world samples (provisioning resources in Azure e.g.)
- Build orchestration registry and runtime ability to load and resume in-progress orchestrations from the provider.

## DONE

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

