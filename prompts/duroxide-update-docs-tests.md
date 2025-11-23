# Update Documentation and Tests After Code Changes

## Objective
Ensure all documentation is accurate, complete, and helpful after code changes. Also propose additional tests to cover the changes and verify metrics are properly documented and wired to OpenTelemetry.

## Step 1: Scan Changes and Propose Tests

**First, analyze what changed:**
1. Run `git diff --cached` to see staged changes
2. Run `git diff` to see unstaged changes
3. Identify new features, bug fixes, or behavior changes

**For each significant change, propose tests:**
- New features → Integration tests
- Bug fixes → Regression tests
- Metrics changes → Observability tests with OTel audit
- API changes → Example updates and doc tests
- Provider changes → Provider validation tests

**Ask the user:**
- Present a list of changes found
- Propose specific tests for each change
- Ask which tests to implement before proceeding

## Step 2: Metrics Changes (If Applicable)

If ANY changes touch observability/metrics code, perform a **complete metrics audit**:

### 2.1 Update `docs/metrics-specification.md`

**Check if metrics were:**
- Added → Add to spec with full documentation
- Modified → Update labels, buckets, or descriptions
- Removed → Remove from spec
- Renamed → Update all references

**For each metric, verify:**
1. **Counter/Histogram/Gauge is defined** in `src/runtime/observability.rs`
2. **Recording method exists** and has correct signature
3. **Method is called** from runtime code (grep for usage)
4. **Labels are correct** in the recording call
5. **Buckets are correct** for histograms

### 2.2 Perform OTel Export Audit

For EVERY metric in the specification, audit the code to verify OTel export:

**Audit Process:**
```bash
# 1. Find where metric instrument is created
grep "metric_name" src/runtime/observability.rs

# 2. Find where recording method is defined
grep "pub fn record_" src/runtime/observability.rs

# 3. Find where method is called from runtime
grep "record_metric_name" src/runtime/

# 4. Verify labels in the call site
# Check that all labels from spec are passed correctly
```

**Update OTel Export column in summary table:**
- **✅ Code Review** - Metric is properly wired with all labels and buckets
- **⚠️ Defined** - Method exists but not called (will be zero)
- **❌ Not Called** - Method never invoked from runtime
- **❌ Not Wired** - Instrument created but never used

**Example Audit:**
```
Metric: duroxide_activity_executions_total
Labels: activity_name, outcome, retry_attempt

✅ Instrument defined: observability.rs:330
✅ Method exists: observability.rs:617 record_activity_execution(activity_name, outcome, ...)
✅ Called from runtime: worker.rs:122, worker.rs:156
✅ All labels present: ✓ activity_name ✓ outcome ✓ retry_attempt
✅ Buckets N/A (counter)

Status: ✅ Code Review
```

### 2.3 Add Tests for New Metrics

For any new metrics:
1. Add test to `tests/observability_tests.rs`
2. Exercise the code path that records the metric
3. Verify atomic counter increments (via `metrics_snapshot()`)
4. Document test in spec's "Test Coverage" section

## Step 3: Documentation Hierarchy

### 3.1 User-Facing Guides (Priority: High, MUST scan)
Review and update these guides for accuracy:

- **`README.md`** - Project overview, quick examples, getting started
- **`QUICK_START.md`** - 5-minute introduction for new users
- **`docs/ORCHESTRATION-GUIDE.md`** - Comprehensive guide to writing orchestrations
- **`docs/provider-implementation-guide.md`** - Guide for implementing custom providers
- **`docs/cross-crate-registry-pattern.md`** - Guide for building reusable libraries

**Review criteria:**
- Read all docs, compare with code for accuracy and relevance
- Examples compile and use current API signatures
- Terminology is consistent with codebase
- Code patterns match working examples
- Instructions are prescriptive and actionable
- Examples are succinct but complete

### 3.2 API Documentation (Priority: High, MUST scan)
Review doc comments in public-facing modules:

- **`src/lib.rs`** - Core types: `OrchestrationContext`, `ActivityContext`, `Event`, `Action`
- **`src/runtime/mod.rs`** - `Runtime`, `RuntimeOptions`, `ObservabilityConfig`
- **`src/client/mod.rs`** - `Client` and all control-plane methods
- **`src/providers/mod.rs`** - `Provider` trait and related types
- **`src/runtime/registry.rs`** - `ActivityRegistry`, `OrchestrationRegistry`

**Review criteria:**
- Every public struct/enum has a doc comment
- Every public method has examples or usage notes
- Doc examples compile (`cargo test --doc`)
- Breaking changes are documented
- Related types are cross-referenced with `[`TypeName`]`

### 3.3 Examples (Priority: Medium)
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

### 3.4 Specialized Documentation (Priority: Low)
Review if major changes were made:

- **`docs/observability-guide.md`** - Observability features
- **`docs/metrics-specification.md`** - Complete metrics reference
- **`docs/provider-observability.md`** - Provider instrumentation
- **`docs/library-observability.md`** - Library developer guide
- **`docs/activity-context-design.md`** - ActivityContext design document
- **`sqlite-stress/README.md`** - Stress testing guide

## Execution Checklist

- [ ] Scan git changes (staged + unstaged)
- [ ] Propose additional tests for changes
- [ ] If metrics changed: Update metrics-specification.md
- [ ] If metrics changed: Perform OTel export audit
- [ ] If metrics changed: Add tests to observability_tests.rs
- [ ] Read through changed guides and fix outdated examples
- [ ] Update API signatures if they changed
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
7. **Metrics not in spec** - New metrics added but not documented
8. **Metrics not wired to OTel** - Instruments created but never recorded
9. **Tests missing for new code** - New features without corresponding tests

## Metrics Audit Template

When metrics are changed, use this template to audit each metric:

```
Metric: metric_name_here
Type: Counter/Histogram/Gauge
Labels: label1, label2, label3

Audit Checklist:
□ Instrument defined in observability.rs
□ Recording method exists with correct signature
□ Method called from runtime code (find call sites)
□ All labels present in call site
□ Histogram buckets match specification
□ Test exists in observability_tests.rs
□ Documented in metrics-specification.md

OTel Export Status: ✅/⚠️/❌
```

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
- Implementing more than 3 new tests

Then summarize the proposed changes and ask for confirmation before proceeding.

## Final Validation

After completing documentation and test updates:

**Documentation:**
1. Run `cargo test --doc` - All doctests should pass
2. Run `cargo build --examples` - All examples should compile
3. Spot-check by running 2-3 examples: `cargo run --example hello_world`
4. Search for outdated patterns: `rg "println!" examples/` (should mostly be output, not logging)
5. Verify cross-references work: Look for broken `[TypeName]` links

**Tests:**
1. Run `cargo test` - All tests should pass
2. Run `cargo test --features observability --test observability_tests` - Metrics tests pass
3. Check test coverage: New code should have corresponding tests
4. Verify no dead code warnings for new public methods

**Metrics (if applicable):**
1. Every metric in code is documented in `metrics-specification.md`
2. Every metric in spec has OTel Export status audited
3. Every metric has a test in `observability_tests.rs`
4. Metrics summary table is up to date with correct counts

