# ContinueAsNew Semantics

ContinueAsNew (CAN) terminates the current execution and starts a new execution for the same instance with fresh input and empty history.

Key points
- Terminal for current execution: the current execution appends `OrchestrationContinuedAsNew { input }` and stops.
- New execution history: the runtime creates a new execution (incremented execution id) and stamps a fresh `OrchestrationStarted` with the provided input. The provider simply persists what the runtime sends.
- Event targeting: activity/timer/external events always target the currently active execution. After CAN, the next execution must resubscribe/re-arm as needed.
- Result propagation: the initial start handle resolves with an empty output when the first execution continues-as-new; the runtime then automatically starts the next execution.

Flow
1. Orchestrator calls `ctx.continue_as_new(new_input)`.
2. Runtime appends terminal `OrchestrationContinuedAsNew` to the current execution.
3. Runtime enqueues a `WorkItem::ContinueAsNew` and later processes it as a new execution with empty history, stamping `OrchestrationStarted { name, version, input: new_input, parent_instance?, parent_id? }`.
4. Provider receives `ack_orchestration_item(lock_token, execution_id = prev + 1, ...)` and atomically persists the new execution row, updates `instances.current_execution_id = MAX(..., execution_id)`, and appends events.

Practical tips
- Re-create subscriptions and timers in the new execution; carry forward any needed state via the new input.
- External events sent before the new execution subscribes will be dropped; raise them after the new execution has subscribed.
- Use deterministic correlation ids if you coordinate sub-orchestrations across executions.
