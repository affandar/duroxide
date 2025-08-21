# API Consistency Checklist (Versioning + Start Semantics)

- Start APIs must exist for all flavors:
  - Runtime start: string I/O, typed I/O, and explicit-version variants.
  - Sub-orchestration: string I/O, typed I/O, and optional explicit-version variants.
  - Detached orchestration: string I/O, typed I/O, and optional explicit-version variants.
  - ContinueAsNew: string I/O, typed I/O, and explicit-version variant.
- Version selection precedence:
  - Explicit version (when provided) wins.
  - Otherwise, resolve via registry policy (Latest or Exact pinned).
  - The chosen version is pinned per-instance for deterministic replay.
- Events:
  - OrchestrationStarted currently carries no version; if behavior requires history introspection by version, add a version field.
- Typed variants mirror string APIs 1:1 (naming and availability).
- Adding a new start surface area requires adding all three: string, typed, and versioned.

DurableFuture vs terminal decisions:
- Any API that returns a DurableFuture must not record actions at call-time. The action should be recorded on first poll only (after correlating with history and pushing the corresponding Scheduled event).
- APIs that are terminal decisions (e.g., continue_as_new, detached schedule_orchestration) should record actions at call-time within the current turn.
