# Mixing Auto-Discovery and Manual Registration

## Yes! Fully Supported

The `RuntimeBuilder` is designed to support **any combination** of auto-discovery and manual registration.

---

## All Supported Combinations

### Option 1: Discovery + Manual Registration

```rust
// Some activities use macros
#[activity(typed)]
async fn new_activity(input: String) -> Result<String, String> {
    Ok(format!("New: {}", input))
}

// Some activities use old style
let legacy_activity = |input: String| async move {
    Ok(format!("Legacy: {}", input))
};

let rt = Runtime::builder()
    .store(store)
    .discover_activities()                          // Get macro-annotated
    .register_activity("LegacyActivity", legacy_activity)  // Add manual
    .discover_orchestrations()
    .start()
    .await;
```

### Option 2: Manual Registration + Discovery

```rust
let rt = Runtime::builder()
    .store(store)
    .register_activity("SpecialActivity", special_fn)  // Manual first
    .discover_activities()                             // Then auto-discover
    .register_orchestration("CustomOrch", custom_orch)
    .discover_orchestrations()
    .start()
    .await;
```

**Order doesn't matter!** The builder accumulates everything.

### Option 3: Only Discovery (New Projects)

```rust
let rt = Runtime::builder()
    .store(store)
    .discover_activities()
    .discover_orchestrations()
    .start()
    .await;
```

### Option 4: Only Manual (Legacy Projects)

```rust
let rt = Runtime::builder()
    .store(store)
    .register_activity("Activity1", fn1)
    .register_activity("Activity2", fn2)
    .register_orchestration("Orch1", orch1)
    .start()
    .await;
```

### Option 5: Pre-Built Registries (Full Control)

```rust
// Build manually
let activities = ActivityRegistry::builder()
    .register("Manual1", fn1)
    .register("Manual2", fn2)
    .build();

// Override everything
let rt = Runtime::builder()
    .store(store)
    .activities(activities)      // Replaces any discovery/registration
    .discover_orchestrations()   // Can still discover orchestrations
    .start()
    .await;
```

---

## RuntimeBuilder Implementation

### Complete API

```rust
impl RuntimeBuilder {
    // ===== Auto-Discovery =====
    
    #[cfg(feature = "macros")]
    pub fn discover_activities(mut self) -> Self {
        // Collect from linkme distributed slice
        let mut builder = self.take_activity_builder();
        
        for descriptor in crate::__internal::ACTIVITIES {
            builder = builder.register(descriptor.name, descriptor.invoke);
        }
        
        self.activities = Some(builder.build());
        self
    }
    
    #[cfg(feature = "macros")]
    pub fn discover_orchestrations(mut self) -> Self {
        // Collect from linkme distributed slice
        let mut builder = self.take_orchestration_builder();
        
        for descriptor in crate::__internal::ORCHESTRATIONS {
            builder = builder.register_versioned(
                descriptor.name,
                descriptor.version,
                descriptor.invoke,
            );
        }
        
        self.orchestrations = Some(builder.build());
        self
    }
    
    // ===== Manual Registration (Additive) =====
    
    pub fn register_activity<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        let mut builder = self.take_activity_builder();
        builder = builder.register(name, f);
        self.activities = Some(builder.build());
        self
    }
    
    pub fn register_orchestration<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(OrchestrationContext, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        let mut builder = self.take_orchestration_builder();
        builder = builder.register(name, f);
        self.orchestrations = Some(builder.build());
        self
    }
    
    // ===== Full Override =====
    
    pub fn activities(mut self, activities: ActivityRegistry) -> Self {
        self.activities = Some(activities);
        self
    }
    
    pub fn orchestrations(mut self, orchestrations: OrchestrationRegistry) -> Self {
        self.orchestrations = Some(orchestrations);
        self
    }
    
    // ===== Helpers =====
    
    fn take_activity_builder(&mut self) -> ActivityRegistryBuilder {
        if let Some(existing) = self.activities.take() {
            ActivityRegistryBuilder::from_registry(&existing)
        } else {
            ActivityRegistry::builder()
        }
    }
    
    fn take_orchestration_builder(&mut self) -> OrchestrationRegistryBuilder {
        if let Some(existing) = self.orchestrations.take() {
            OrchestrationRegistryBuilder::from_registry(&existing)
        } else {
            OrchestrationRegistry::builder()
        }
    }
}
```

**Key insight:** Each method gets the existing registry, adds to it, and stores it back.

---

## Migration Scenarios

### Scenario 1: Gradual Migration (Most Common)

**Week 1:**
```rust
// All old style
let rt = Runtime::start_with_store(store, Arc::new(activities), orchestrations).await;
```

**Week 2:**
```rust
// Start using builder, no macros yet
let rt = Runtime::builder()
    .store(store)
    .activities(activities)        // Pass existing registries
    .orchestrations(orchestrations)
    .start()
    .await;
```

**Week 3:**
```rust
// Add first macro-annotated function
#[activity(typed)]
async fn new_activity(input: String) -> Result<String, String> { ... }

let rt = Runtime::builder()
    .store(store)
    .activities(manually_built_activities)  // Old ones
    .discover_activities()                  // + new one (but no conflict - already have activities!)
    .orchestrations(manually_built_orchestrations)
    .start()
    .await;
```

Wait - that won't work because `.activities()` replaces! Let's fix:

**Week 3 (corrected):**
```rust
#[activity(typed)]
async fn new_activity(input: String) -> Result<String, String> { ... }

let rt = Runtime::builder()
    .store(store)
    .discover_activities()                  // Get new_activity
    .register_activity("OldActivity1", old1)  // Add old ones
    .register_activity("OldActivity2", old2)
    .discover_orchestrations()
    .start()
    .await;
```

**Week 4-N:**
Migrate one activity at a time, remove from `.register_activity()` calls.

**Final state:**
```rust
let rt = Runtime::builder()
    .store(store)
    .discover_activities()
    .discover_orchestrations()
    .start()
    .await;
```

---

### Scenario 2: Library Crates + Main App

**Library crate uses old style:**
```rust
// my-workflows/src/lib.rs

pub fn build_activities() -> ActivityRegistry {
    ActivityRegistry::builder()
        .register("LegacyActivity1", fn1)
        .register("LegacyActivity2", fn2)
        .build()
}
```

**Main app uses new style:**
```rust
// app/src/main.rs

use my_workflows;

#[activity(typed)]
async fn new_activity(input: String) -> Result<String, String> { ... }

#[orchestration]
async fn my_orch(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    // Can call both old and new activities!
    let old = ctx.schedule_activity("LegacyActivity1", input.clone()).into_activity().await?;
    let new = durable!(new_activity(input)).await?;
    Ok(format!("{} {}", old, new))
}

let rt = Runtime::builder()
    .store(store)
    .activities(my_workflows::build_activities())  // Old from library
    .discover_activities()                          // New from main app
    .discover_orchestrations()
    .start()
    .await;
```

**Wait - `.activities()` would replace!** Need a merge:

```rust
let rt = Runtime::builder()
    .store(store)
    .discover_activities()                          // Get macro-annotated from main
    // Now add the library ones
    .register_activity("LegacyActivity1", /* from lib */)
    .register_activity("LegacyActivity2", /* from lib */)
    .discover_orchestrations()
    .start()
    .await;
```

Or better, have the library export individual registrations:

```rust
// my-workflows/src/lib.rs
pub fn register_legacy_activities(builder: RuntimeBuilder) -> RuntimeBuilder {
    builder
        .register_activity("LegacyActivity1", fn1)
        .register_activity("LegacyActivity2", fn2)
}

// app/src/main.rs
let rt = Runtime::builder()
    .store(store)
    .discover_activities()
    .pipe(my_workflows::register_legacy_activities)  // Chainable!
    .discover_orchestrations()
    .start()
    .await;
```

---

## Best Practice: Accumulative Builder

The builder should be **accumulative** by default:

```rust
impl RuntimeBuilder {
    pub fn discover_activities(mut self) -> Self {
        let mut builder = self.take_activity_builder();  // Get existing
        
        // Add discovered ones
        for descriptor in crate::__internal::ACTIVITIES {
            builder = builder.register(descriptor.name, descriptor.invoke);
        }
        
        self.activities = Some(builder.build());
        self
    }
    
    pub fn register_activity<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self {
        let mut builder = self.take_activity_builder();  // Get existing
        
        // Add this one
        builder = builder.register(name, f);
        
        self.activities = Some(builder.build());
        self
    }
    
    // .activities() is a full REPLACE (opt-out of accumulation)
    pub fn activities(mut self, activities: ActivityRegistry) -> Self {
        self.activities = Some(activities);  // Replace entirely
        self
    }
}
```

**Pattern:**
- `.discover_*()` - Accumulates
- `.register_*()` - Accumulates
- `.activities()` / `.orchestrations()` - Replaces

---

## Example: All Three Together

```rust
use duroxide::prelude::*;

// Macro style
#[activity(typed)]
async fn macro_activity(input: String) -> Result<String, String> {
    Ok(format!("Macro: {}", input))
}

// Old closure style
let closure_activity = |input: String| async move {
    Ok(format!("Closure: {}", input))
};

// Pre-built from library
let library_activities = my_library::build_activities();

let rt = Runtime::builder()
    .store(store)
    .discover_activities()                          // 1. Get macro_activity
    .register_activity("ClosureActivity", closure_activity)  // 2. Add closure
    // How to add library activities? Need to merge...
    
    // Option A: Provide merge helper
    .merge_activities(library_activities)
    
    // Option B: Extract and re-register
    // (Library would need to expose individual functions)
    
    .start()
    .await;
```

**Better pattern:** Library exports registration function:

```rust
// my-library/src/lib.rs
pub fn register_activities(builder: RuntimeBuilder) -> RuntimeBuilder {
    builder
        .register_activity("LibActivity1", fn1)
        .register_activity("LibActivity2", fn2)
}

// app/src/main.rs
let rt = my_library::register_activities(
    Runtime::builder()
        .store(store)
        .discover_activities()                     // Macro-annotated from app
).discover_orchestrations()
 .start()
 .await;
```

---

## Recommended Migration Path

### For Existing Projects

1. **Keep old code working** - Change nothing
2. **Add builder alongside** - Use `Runtime::builder()` instead of `Runtime::start_with_store()`
3. **Add macros gradually** - New code uses macros, old code stays
4. **Mix via builder** - Use `.discover_*()` + `.register_*()`
5. **Eventually migrate everything** - When ready, pure `.discover_*()`

### For New Projects

Just use discovery:
```rust
#[duroxide::main]
async fn main() {
    // Everything auto-discovered!
}
```

---

## Summary

**✅ Can mix and match:**
- Auto-discovery (`.discover_*()`)
- Manual registration (`.register_*()`)
- Pre-built registries (`.activities()`, `.orchestrations()`)

**✅ Accumulative by default:**
- Each `.discover_*()` or `.register_*()` adds to existing
- `.activities()` / `.orchestrations()` replaces entirely

**✅ Zero breaking changes:**
- Old `Runtime::start_with_store()` still works
- New `Runtime::builder()` is additive
- Can migrate gradually

**The design supports any migration strategy!** 🎉


