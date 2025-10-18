# Macro Crate Structure

## Required Structure

Rust **requires** procedural macros to be in a separate crate with `proc-macro = true`. This is a language requirement, not a choice.

---

## Crate Organization

```
duroxide/
├── Cargo.toml                      # Workspace manifest
├── duroxide/                       # Main library crate
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── runtime/
│   │   ├── providers/
│   │   └── ...
│   └── ...
├── duroxide-macros/               # Proc-macro crate (REQUIRED to be separate)
│   ├── Cargo.toml                 # [lib] proc-macro = true
│   ├── src/
│   │   └── lib.rs                 # Macro implementations
│   └── tests/
│       └── expand.rs
└── examples/
    ├── hello_world.rs             # Old style
    └── hello_world_macros.rs      # New style
```

---

## Workspace Cargo.toml

```toml
# Cargo.toml (workspace root)

[workspace]
members = [
    ".",                    # Main duroxide crate
    "duroxide-macros",      # Proc-macro crate
]
resolver = "2"

[workspace.dependencies]
# Shared dependencies
serde = { version = "1.0", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
```

---

## Main Crate: `duroxide`

```toml
# duroxide/Cargo.toml (or just Cargo.toml if at root)

[package]
name = "duroxide"
version = "0.1.0"
edition = "2021"

[dependencies]
# ... existing dependencies ...
serde = "1.0"
tokio = "1"
async-trait = "0.1"
# ... etc

# Macro dependency (optional)
duroxide-macros = { path = "./duroxide-macros", optional = true }
linkme = { version = "0.3", optional = true }

[features]
default = []
macros = ["duroxide-macros", "linkme"]

[dev-dependencies]
# ... existing dev dependencies ...
```

**Key points:**
- `duroxide-macros` is an **optional** dependency
- Behind `macros` feature flag
- Users can opt-in or out

---

## Proc-Macro Crate: `duroxide-macros`

```toml
# duroxide-macros/Cargo.toml

[package]
name = "duroxide-macros"
version = "0.1.0"
edition = "2021"

[lib]
proc-macro = true          # ← REQUIRED for proc-macros

[dependencies]
proc-macro2 = "1.0"
quote = "1.0"
syn = { version = "2.0", features = ["full", "parsing", "extra-traits"] }

[dev-dependencies]
trybuild = "1.0"           # For testing macro expansion
```

**Key points:**
- **MUST** have `proc-macro = true`
- Only exports proc-macros (can't export regular code)
- Minimal dependencies (just syn, quote, proc-macro2)

---

## Why Separate Crates?

### Rust Language Requirement

```toml
# ❌ This is NOT allowed:
[lib]
proc-macro = true
crate-type = ["lib"]      # Can't have both!

# ✅ Proc-macro crate can ONLY export proc-macros
[lib]
proc-macro = true
```

Proc-macro crates:
- ✅ Can export procedural macros
- ❌ Cannot export types, functions, traits
- ❌ Cannot be used as regular dependencies

### Compilation Separation

```
Build process:
1. Build duroxide-macros (proc-macro crate)
   → Produces a dynamic library loaded by rustc
   
2. Build duroxide (main crate)
   → Uses duroxide-macros during compilation
   → Macros expand to regular Rust code
   
3. Build application
   → Uses duroxide library
   → Macros already expanded (not in final binary)
```

---

## User Perspective

### Without Macros (Default)

```toml
# User's Cargo.toml
[dependencies]
duroxide = "0.1"  # Just the main crate
```

They get:
- Core duroxide functionality
- No macros
- Smaller dependency tree

### With Macros (Opt-in)

```toml
# User's Cargo.toml
[dependencies]
duroxide = { version = "0.1", features = ["macros"] }
```

They get:
- Core duroxide functionality
- Macro support
- Auto-discovery via linkme
- Syntactic sugar

The user **never directly depends** on `duroxide-macros` - it's a transitive dependency through the `macros` feature.

---

## Publishing Strategy

### To crates.io

Both crates get published:

```bash
# Publish proc-macro crate first
cd duroxide-macros
cargo publish

# Then main crate
cd ..
cargo publish
```

```toml
# duroxide/Cargo.toml (published version)
[dependencies]
duroxide-macros = { version = "0.1", optional = true }  # From crates.io now
linkme = { version = "0.3", optional = true }
```

### Versioning

Keep versions in sync:
- `duroxide` version `0.1.0` → requires `duroxide-macros` version `0.1.0`
- `duroxide` version `0.2.0` → requires `duroxide-macros` version `0.2.0`

Or allow range:
```toml
duroxide-macros = { version = ">=0.1, <0.2", optional = true }
```

---

## Directory Structure (Two Options)

### Option A: Nested (Workspace)

```
duroxide/
├── Cargo.toml                  # Workspace
├── Cargo.toml                  # Main crate (same dir)
├── src/                        # Main crate source
│   └── ...
├── duroxide-macros/            # Nested proc-macro crate
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
└── examples/
```

**Workspace Cargo.toml:**
```toml
[workspace]
members = [".", "duroxide-macros"]
```

**Main Cargo.toml (same directory):**
```toml
[package]
name = "duroxide"

[dependencies]
duroxide-macros = { path = "./duroxide-macros", optional = true }
```

### Option B: Sibling (Cleaner)

```
duroxide-workspace/
├── Cargo.toml                  # Workspace only
├── duroxide/                   # Main crate
│   ├── Cargo.toml
│   ├── src/
│   │   └── ...
│   └── examples/
└── duroxide-macros/            # Proc-macro crate
    ├── Cargo.toml
    ├── src/
    │   └── lib.rs
    └── tests/
```

**Workspace Cargo.toml:**
```toml
[workspace]
members = ["duroxide", "duroxide-macros"]
resolver = "2"
```

**duroxide/Cargo.toml:**
```toml
[package]
name = "duroxide"

[dependencies]
duroxide-macros = { path = "../duroxide-macros", optional = true }
```

**Recommendation:** Use **Option A (Nested)** for simpler paths and single-crate feel.

---

## What Users See

### Installation

```toml
# User just adds one dependency
[dependencies]
duroxide = { version = "0.1", features = ["macros"] }
```

### Usage

```rust
// All from duroxide crate
use duroxide::prelude::*;

#[activity(typed)]      // From duroxide-macros, re-exported by duroxide
async fn my_activity(input: String) -> Result<String, String> {
    Ok(input)
}

#[orchestration]        // From duroxide-macros, re-exported by duroxide  
async fn my_orch(ctx: OrchestrationContext, input: String) -> Result<String, String> {
    let result = durable!(my_activity(input)).await?;
    Ok(result)
}
```

**Users never directly import `duroxide_macros`** - everything is re-exported through `duroxide::prelude`.

---

## Benefits of Separate Crate

### 1. Optional Dependency

Users who don't want macros don't pay for them:

```toml
# No macros = smaller dependency tree
[dependencies]
duroxide = "0.1"  # Doesn't pull in syn, quote, proc-macro2
```

### 2. Faster Rebuilds

Proc-macro crates rarely change - users' rebuild times are faster.

### 3. Clear Separation

- `duroxide` = runtime functionality
- `duroxide-macros` = syntactic sugar

### 4. Feature Flag Control

```toml
[features]
default = []           # No macros by default
macros = ["duroxide-macros", "linkme"]
```

Users opt-in to macros when ready.

---

## Comparison with Other Crates

### Tokio Pattern

```
tokio/
├── tokio/              # Main crate
└── tokio-macros/       # Proc-macro crate

# Users:
[dependencies]
tokio = { version = "1", features = ["macros"] }

# Usage:
#[tokio::main]          # Re-exported from tokio, implemented in tokio-macros
async fn main() { }
```

### Serde Pattern

```
serde/
├── serde/              # Main crate
└── serde_derive/       # Proc-macro crate

# Users:
[dependencies]
serde = { version = "1", features = ["derive"] }

# Usage:
#[derive(Serialize)]    # Re-exported from serde, implemented in serde_derive
struct MyStruct { }
```

### Duroxide Pattern (Same!)

```
duroxide/
├── duroxide/           # Main crate (or at root)
└── duroxide-macros/    # Proc-macro crate

# Users:
[dependencies]
duroxide = { version = "0.1", features = ["macros"] }

# Usage:
#[activity(typed)]      # Re-exported from duroxide, implemented in duroxide-macros
async fn my_activity(...) { }
```

**We follow the same pattern as Tokio and Serde!**

---

## Summary

**Must be separate crate:**
- ✅ Rust language requirement
- ✅ Proc-macro crates must have `proc-macro = true`
- ✅ Can only export macros, not regular code

**Structure:**
- `duroxide-macros/` - Proc-macro crate
- `duroxide/` (or root) - Main crate
- Main crate re-exports macros

**User experience:**
- Only depends on `duroxide` with `features = ["macros"]`
- Never directly imports `duroxide_macros`
- Everything through `duroxide::prelude::*`

**Just like Tokio and Serde!** 🎯
