# Migration Guide

This guide helps you migrate between Duroxide versions and handle orchestration versioning.

## Orchestration Versioning

Duroxide supports versioning to handle code evolution while maintaining compatibility with running instances.

### When to Version

You need to version your orchestration when:

1. **Adding/removing activities**: Changes the execution flow
2. **Reordering operations**: Affects correlation IDs
3. **Changing conditional logic**: Alters execution paths
4. **Modifying data structures**: Input/output format changes

You DON'T need to version when:

1. **Fixing bugs in activities**: Activities are stateless
2. **Improving activity performance**: No behavior change
3. **Adding logging**: Using `ctx.trace_*` is replay-safe
4. **Refactoring activity internals**: Interface remains the same

### Versioning Strategy

```rust
// Version 1.0.0
let orchestration_v1 = |ctx: OrchestrationContext, input: String| async move {
    let result = ctx.schedule_activity("ProcessV1", input).into_activity().await?;
    Ok(result)
};

// Version 2.0.0 - Added validation step
let orchestration_v2 = |ctx: OrchestrationContext, input: String| async move {
    // New validation step
    let validated = ctx.schedule_activity("Validate", &input).into_activity().await?;
    let result = ctx.schedule_activity("ProcessV2", validated).into_activity().await?;
    Ok(result)
};

// Register both versions
let orchestrations = OrchestrationRegistry::builder()
    .register_versioned("MyOrchestration", "1.0.0", orchestration_v1)
    .register_versioned("MyOrchestration", "2.0.0", orchestration_v2)
    .with_version_policy(VersionPolicy::Latest) // New instances use latest
    .build();
```

### Version Policies

1. **Latest** (default): New instances use the latest registered version
2. **Exact**: Must specify exact version when starting
3. **Compatible**: Use semantic versioning rules

### Handling Running Instances

When you deploy a new version:

1. **Running instances continue with their version**: Pinned at start
2. **New instances use the latest version**: Based on policy
3. **ContinueAsNew can change versions**: Explicitly specify

```rust
// Migrate running instance to new version via ContinueAsNew
ctx.continue_as_new_versioned("2.0.0", new_input);
```

## Breaking Changes Between Versions

### Duroxide 0.1.0 → 0.2.0 (Hypothetical)

#### API Changes

1. **Activity Registration**:
   ```rust
   // Old (0.1.0)
   .register("MyActivity", |input: String| async move { Ok(result) })
   
   // New (0.2.0) - Explicit error type
   .register("MyActivity", |input: String| async move -> Result<String, ActivityError> { 
       Ok(result) 
   })
   ```

2. **Orchestration Context**:
   ```rust
   // Old (0.1.0)
   ctx.new_guid() // Removed
   
   // New (0.2.0)
   ctx.system_new_guid().await // Async system activity
   ```

3. **Runtime Creation**:
   ```rust
   // Old (0.1.0)
   Runtime::start(activities, orchestrations).await
   
   // New (0.2.0) - Explicit store
   Runtime::start_with_store(store, activities, orchestrations).await
   ```

#### Migration Steps

1. **Update Dependencies**:
   ```toml
   [dependencies]
   duroxide = "0.2"
   ```

2. **Update Activity Signatures**:
   - Add explicit error types
   - Update return types if changed

3. **Update Orchestration Code**:
   - Replace deprecated methods
   - Update to new async APIs

4. **Test Thoroughly**:
   - Run existing tests
   - Test with production-like data
   - Verify determinism

## Data Migration

### Handling Input/Output Format Changes

When changing data structures:

1. **Support both formats temporarily**:
   ```rust
   #[derive(Serialize, Deserialize)]
   #[serde(untagged)]
   enum InputCompat {
       V1(InputV1),
       V2(InputV2),
   }
   
   let orchestration = |ctx: OrchestrationContext, input_json: String| async move {
       let input: InputCompat = serde_json::from_str(&input_json)?;
       
       match input {
           InputCompat::V1(v1) => {
               // Handle old format
               let v2 = migrate_v1_to_v2(v1);
               process_v2(ctx, v2).await
           }
           InputCompat::V2(v2) => {
               // Handle new format
               process_v2(ctx, v2).await
           }
       }
   };
   ```

2. **Gradual migration**:
   - Deploy version supporting both formats
   - Migrate data at your pace
   - Remove old format support later

### Storage Provider Migration

When switching providers:

```rust
// 1. Export from old provider
let old_store = InMemoryHistoryStore::new();
let instances = old_store.list_instances().await;

for instance in instances {
    let history = old_store.read(&instance).await;
    // Save history to new provider
}

// 2. Import to new provider
let new_store = SqliteProvider::new("sqlite:./data.db", None).await?;
for (instance, history) in saved_data {
    // Recreate instance in new store
    new_store.create_instance(&instance).await?;
    new_store.append(&instance, history).await?;
}

// 3. Switch runtime to new provider
let rt = Runtime::start_with_store(Arc::new(new_store), activities, orchestrations).await;
```

## Best Practices for Versioning

1. **Semantic Versioning**: Use major.minor.patch
   - Major: Breaking changes
   - Minor: New features, backward compatible
   - Patch: Bug fixes

2. **Deployment Strategy**:
   - Deploy new version alongside old
   - Monitor both versions
   - Gradually migrate instances
   - Remove old version when safe

3. **Testing Strategy**:
   ```rust
   #[test]
   async fn test_version_compatibility() {
       // Test that v1 instances complete successfully
       let v1_result = run_with_version("1.0.0", v1_input).await;
       
       // Test that v2 instances work with new features
       let v2_result = run_with_version("2.0.0", v2_input).await;
       
       // Test migration path
       let migrated = migrate_v1_to_v2(v1_result);
       assert_eq!(migrated, expected);
   }
   ```

4. **Documentation**:
   - Document what changed
   - Provide migration examples
   - List breaking changes clearly
   - Include compatibility matrix

## Rollback Strategy

If issues arise after deployment:

1. **Leave running instances**: They continue with their pinned version
2. **Revert new instances**: Change version policy or registration
3. **Monitor and fix**: Address issues without affecting running work

```rust
// Emergency rollback configuration
let orchestrations = OrchestrationRegistry::builder()
    .register_versioned("MyOrchestration", "1.0.0", orchestration_v1)
    .register_versioned("MyOrchestration", "2.0.0", orchestration_v2)
    .with_version_policy(VersionPolicy::Exact("1.0.0")) // Force v1 for new instances
    .build();
```

## Future Compatibility

To make future migrations easier:

1. **Use typed inputs/outputs** with serde
2. **Version your APIs** from the start
3. **Keep orchestrations simple** - complex logic in activities
4. **Document assumptions** and invariants
5. **Test with multiple versions** in CI/CD

## Getting Help

For migration assistance:

1. Review the [changelog](../CHANGELOG.md) for detailed changes
2. Check [examples](../examples/) for updated patterns
3. Run tests to verify compatibility
4. Open an issue for migration problems
