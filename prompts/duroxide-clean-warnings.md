# Clean Up Compiler and Linter Warnings

## Objective
Eliminate all compiler warnings, clippy lints, and formatting issues across the entire codebase without taking shortcuts.

## Scope
- Main library (`src/`)
- All tests (`tests/`)
- All examples (`examples/`)
- Workspace members (`sqlite-stress/`)
- Documentation tests

## Tools to Run

### 1. Cargo Build (All Targets)
```bash
cargo build --all-targets
```

Look for:
- Unused imports
- Unused variables
- Unused functions/types
- Dead code
- Deprecated API usage

### 2. Cargo Clippy (Comprehensive)
```bash
cargo clippy --all-targets --all-features
```

Common clippy warnings to address:
- `needless_lifetimes` - Remove unnecessary explicit lifetimes
- `derivable_impls` - Replace manual `impl Default` with `#[derive(Default)]`
- `question_mark` - Use `?` operator instead of manual error checking
- `field_reassign_with_default` - Initialize structs directly instead of mutation
- `redundant_pattern_matching` - Simplify match expressions
- `manual_map` - Replace match with `.map()`
- `useless_conversion` - Remove `.into()` when type already matches

### 3. Cargo Format
```bash
cargo fmt --all
```

Ensures consistent formatting across all code.

### 4. Doctest Verification
```bash
cargo test --doc
```

Ensures all doc examples compile or are properly marked as `ignore`.

## Handling Unused Code

### ❌ DO NOT
- Add `#[allow(unused)]` or `#[allow(dead_code)]` without understanding why
- Prefix variables with `_` to silence warnings unless they're truly meant to be ignored
- Remove code that's part of public API or used in feature-gated code
- Remove error handling just to simplify code

### ✅ DO
1. **Investigate first**: Understand why the code is unused
2. **Check feature gates**: Code might be used under `#[cfg(feature = "...")]`
3. **Check tests**: Code might only be used in test scenarios
4. **Remove genuinely unused code**: If it's truly not needed, delete it
5. **For intentionally unused parameters**: Use `_name` pattern when the parameter is required by a trait but not used in a specific implementation

## Example Workflow

```bash
# 1. Build and capture warnings
cargo build --all-targets 2>&1 | tee build-warnings.txt

# 2. Run clippy
cargo clippy --all-targets --all-features 2>&1 | tee clippy-warnings.txt

# 3. Review warnings
cat build-warnings.txt | grep "warning:"
cat clippy-warnings.txt | grep "warning:"

# 4. Fix warnings iteratively
# ... make fixes ...

# 5. Verify fixes
cargo build --all-targets
cargo clippy --all-targets --all-features

# 6. Format
cargo fmt --all

# 7. Test everything still works
cargo test --all
cargo test --doc
cargo build --examples
```

## Common Warning Fixes

### 1. Unused Import
```rust
// ❌ Before
use std::collections::HashMap;  // warning: unused import

// ✅ After - Remove if truly unused
// (import removed)
```

### 2. Unused Variable
```rust
// ❌ Wrong fix
let _result = expensive_operation();  // Misleading - operation still runs

// ✅ Correct - If value is genuinely not needed, remove the binding
expensive_operation();

// ✅ Correct - If required by trait but unused in this impl
fn process(&self, _ctx: Context) { }  // Trait requires ctx parameter
```

### 3. Dead Code
```rust
// If function is truly unused:
// ❌ Don't suppress
#[allow(dead_code)]
fn unused_helper() { }

// ✅ Remove it
// (function removed)

// If used in tests/features:
// ✅ Add appropriate cfg
#[cfg(test)]
fn test_helper() { }

#[cfg(feature = "advanced")]
fn advanced_feature() { }
```

### 4. Clippy: Derivable Impls
```rust
// ❌ Before
impl Default for Config {
    fn default() -> Self {
        Self {
            value: 0,
            enabled: false,
        }
    }
}

// ✅ After
#[derive(Default)]
struct Config {
    #[default]  // If one variant should be default
    value: i32,
    enabled: bool,
}
```

### 5. Clippy: Question Mark
```rust
// ❌ Before
if result.is_err() {
    return result;
}

// ✅ After
result?;
```

## Special Cases

### Feature-Gated Code
Code may appear unused but is actually behind a feature flag:
```rust
#[cfg(feature = "observability")]
pub fn setup_metrics() { }  // Not unused when feature enabled
```

**Action**: Verify the code is used when the feature is enabled.

### Test-Only Code
```rust
#[cfg(test)]
pub fn test_helper() { }
```

**Action**: Ensure helper is actually used in tests; remove if not.

### Public API
```rust
pub fn rarely_used_api() { }  // No warnings for public items in libraries
```

**Action**: Keep unless you're certain no external code uses it.

## Validation Checklist

Before considering the cleanup complete:

- [ ] `cargo build --all-targets` produces zero warnings
- [ ] `cargo clippy --all-targets --all-features` produces zero warnings
- [ ] `cargo fmt --all --check` produces no diff
- [ ] `cargo test --all` passes completely
- [ ] `cargo test --doc` passes completely
- [ ] `cargo build --examples` succeeds
- [ ] Spot-check: Run 2-3 examples to ensure they work

## When to Ask

Stop and ask the user if:
- You need to remove a large amount of code (>100 lines)
- Warning fix requires changing public API
- You're unsure if code is used in production scenarios
- Fix would require significant refactoring

## Anti-Patterns

**Don't do these:**
- Blindly adding `#[allow(dead_code)]` everywhere
- Prefixing everything with `_` to silence warnings
- Removing error handling to eliminate unused Result
- Deleting code you don't understand
- Changing APIs just to reduce warnings
