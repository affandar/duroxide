# ContinueAsNew Semantics

ContinueAsNew (CAN) terminates the current execution and starts a new execution for the same instance with fresh input and empty history.

Key points
- Terminal for current execution: the current execution appends `OrchestrationContinuedAsNew { input }` and stops.
- New execution history: the provider creates a new execution (incremented execution id) and appends a fresh `OrchestrationStarted` with the provided input.
- Event targeting: activity/timer/external events always target the currently active execution. After CAN, the next execution must resubscribe/re-arm as needed.
- Result propagation: the initial start handle resolves with an empty output when the first execution continues-as-new; the runtime then automatically starts the next execution.

Flow
1. Orchestrator calls `ctx.continue_as_new(new_input)`.
2. Runtime appends terminal `OrchestrationContinuedAsNew` to the current execution.
3. Provider resets to a new execution file and appends `OrchestrationStarted { input: new_input }`.
4. Runtime enqueues a new start for the instance and drives the new execution.

Practical tips
- Re-create subscriptions and timers in the new execution; carry forward any needed state via the new input.
- External events sent before the new execution subscribes will be dropped; raise them after the new execution has subscribed.
- Use deterministic correlation ids if you coordinate sub-orchestrations across executions.
