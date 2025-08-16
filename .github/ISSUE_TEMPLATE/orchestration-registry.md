---
name: Orchestration Registry & Runtime Resume
about: Track building the orchestration registry and adding runtime ability to load/resume in-progress orchestrations from the provider
title: "[Orchestration Registry] <concise title>"
labels: [enhancement, area:runtime, design-needed]
assignees: []
---

## Summary

Build an orchestration registry and enable the runtime to load and resume in-progress orchestrations from the configured provider.

## Motivation

- Allow long-running orchestrations to survive process restarts
- Automatically resume instances on startup without manual re-submission
- Align with durable workflow semantics

## Scope

- Orchestrator registration/discovery
- Startup scan or provider-driven enumeration of in-progress instances
- Safe resumption semantics (no duplicate execution, idempotent replay)
- Observability and logging for resume flow

## Out of Scope (for this issue)

- Cross-process leader election/leases (may be optional or follow-up)
- Distributed sharding across multiple runtimes
- UI/visualization

## Design references

- docs/orchestration-registry.md

## Acceptance Criteria

- [ ] Design doc reviewed and agreed upon (docs/orchestration-registry.md)
- [ ] New registry type/API for orchestrator functions implemented
- [ ] Runtime integrates registry and can resume from provider on startup
- [ ] Provider APIs exist to enumerate/list in-progress instances, or equivalent
- [ ] E2E tests: instances started before shutdown are resumed to completion on next start
- [ ] Docs and README updated where relevant

## Detailed Tasks

- [ ] Define orchestrator function type and registry builder
- [ ] Extend provider or add adapter to list in-progress instances with metadata
- [ ] Implement runtime startup hook to enumerate and resume instances
- [ ] Handle unknown orchestrator names gracefully with clear errors
- [ ] Add tracing for resume lifecycle (discover -> attach -> replay -> complete)
- [ ] Add tests under tests/e2e_recovery.rs or new suite for resume-on-start

## Risks / Mitigations

- Duplicate execution: use provider-level uniqueness/locks or optimistic checks
- Unknown orchestrators: fail with actionable diagnostics
- Large backlogs: resume in bounded batches; backoff/retry

## Additional Notes

Link related PRs, discussions, and experiments here.