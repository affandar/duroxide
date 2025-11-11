# Update Documentation After Code Changes

## Objective
Ensure all documentation is accurate, complete, and helpful for both humans and LLMs after a session of code changes. Pay special attention to what changes are in the current workspace vs where were we at the time of last update of these docs.

## Documentation Hierarchy

### 1. User-Facing Guides (Priority: High, these MUST be scanned and updated)
Review and update these guides for accuracy and clarity:

- **`README.md`** - Project overview, quick examples, getting started
- **`QUICK_START.md`** - 5-minute introduction for new users
- **`docs/ORCHESTRATION-GUIDE.md`** - Comprehensive guide to writing orchestrations
- **`docs/provider-implementation-guide.md`** - Guide for implementing custom providers
- **`docs/cross-crate-registry-pattern.md`** - Guide for building reusable libraries

**Review criteria:**
- Read all of the docs, look through the code for accuracy and relevance
- Examples compile and use current API signatures
- Terminology is consistent with codebase
- Code patterns match what's in working examples
- Instructions are prescriptive and actionable
- Examples are succinct but complete

### 2. API Documentation (Priority: High, these MUST be scanned and updated)
Review doc comments in public-facing modules:

- **`src/lib.rs`** - Core types: `OrchestrationContext`, `ActivityContext`, `Event`, `Action`
- **`src/runtime/mod.rs`** - `Runtime`, `RuntimeOptions`, `ObservabilityConfig`
- **`src/client/mod.rs`** - `Client` and all control-plane methods
- **`src/providers/mod.rs`** - `Provider` trait and related types
- **`src/runtime/registry.rs`** - `ActivityRegistry`, `OrchestrationRegistry`

**Review criteria:**
- Read all of the docs, look through the code for accuracy and relevance
- Every public struct/enum has a doc comment
- Every public method has examples or usage notes
- Doc examples compile (`cargo test --doc`)
- Breaking changes are documented
- Related types are cross-referenced with `[`TypeName`]`

### 3. Examples (Priority: Medium)
Review and test all examples in `examples/`:

- **`hello_world.rs`** - Basic setup and execution
- **`fan_out_fan_in.rs`** - Parallel processing
- **`delays_and_timeouts.rs`** - Timer patterns
- **`timers_and_events.rs`** - Human-in-the-loop
- **`with_observability.rs`** - Observability features
- **`metrics_cli.rs`** - Metrics dashboard

**Review criteria:**
- Examples compile: `cargo build --examples`
- Examples demonstrate current best practices
- Key features added in recent sessions are illustrated
- Comments explain why, not just what
- Examples can be copy-pasted and adapted

### 4. Specialized Documentation (Priority: Low)
Review if major changes were made:

- **`docs/observability-guide.md`** - Observability features
- **`docs/provider-observability.md`** - Provider instrumentation
- **`docs/library-observability.md`** - Library developer guide
- **`docs/activity-context-design.md`** - ActivityContext design document
- **`sqlite-stress/README.md`** - Stress testing guide

## Execution Checklist

- [ ] Read through changed guides and fix outdated examples
- [ ] Update API signatures if they changed (e.g., ActivityContext parameter)
- [ ] Add examples for new features
- [ ] Test that doc examples compile: `cargo test --doc`
- [ ] Verify examples run: `cargo run --example hello_world`
- [ ] Check for broken cross-references
- [ ] Ensure terminology is consistent
- [ ] Remove references to removed features

## Common Issues to Watch For

1. **Outdated function signatures** - Activity registrations missing `ActivityContext`
2. **Old logging patterns** - Using `println!` instead of `ctx.trace_*()`
3. **Incorrect imports** - Missing `use duroxide::ActivityContext;`
4. **Broken examples** - Code that doesn't compile or uses deprecated APIs
5. **Inconsistent terminology** - Mixing "orchestrator" vs "orchestration", "turn" vs "execution"
6. **Missing feature flags** - Observability features without `#[cfg(feature = "observability")]` notes

## Quality Standards

### Good Documentation
- **Prescriptive**: "Do X, then Y" not "You could do X"
- **Complete**: Shows full context, not just fragments
- **Accurate**: Actually compiles and works
- **Helpful**: Explains why, not just how
- **Discoverable**: Uses proper headings, cross-references

### Good Examples
```rust
// ✅ GOOD: Complete, compiles, explains why
let activities = ActivityRegistry::builder()
    .register("ProvisionVM", |ctx: ActivityContext, config: String| async move {
        ctx.trace_info(format!("Provisioning VM: {}", config));
        // Activity can sleep/poll as part of provisioning
        let vm_id = create_vm(&config).await?;
        Ok(vm_id)
    })
    .build();
```

### Poor Examples
```rust
// ❌ BAD: Incomplete, wrong signature, no context
register("Task", |input| async { Ok(input) })
```

## Ask Before Making Large Changes

If documentation updates require:
- Creating new guide documents
- Removing entire sections
- Restructuring organization
- Adding new examples

Then summarize the proposed changes and ask for confirmation before proceeding.

## Final Validation

After completing documentation updates:
1. Run `cargo test --doc` - All doctests should pass
2. Run `cargo build --examples` - All examples should compile
3. Spot-check by running 2-3 examples: `cargo run --example hello_world`
4. Search for common outdated patterns: `rg "println!" examples/` (should mostly be output, not logging)
5. Verify cross-references work: Look for broken `[TypeName]` links in doc comments
