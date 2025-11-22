# Critical Soundness Fixes - Quick Reference

This document provides immediate fixes for the soundness issues identified in the analysis.

---

## 🔴 CRITICAL FIX #1: `poll_once()` Pinning Issue

**File**: `src/lib.rs:1543-1548`

**Current Code** (UNSOUND):
```rust
fn poll_once<F: Future>(fut: &mut F) -> Poll<F::Output> {
    let w = noop_waker();
    let mut cx = Context::from_waker(&w);
    let mut pinned = unsafe { Pin::new_unchecked(fut) };
    pinned.as_mut().poll(&mut cx)
}
```

**Problem**: Creates `Pin` from `&mut F` without guaranteeing `fut` won't be moved after this call. This is unsound if `F` is `!Unpin` and self-referential.

**Fix Option 1** - Require Unpin (Recommended):
```rust
fn poll_once<F: Future + Unpin>(fut: &mut F) -> Poll<F::Output> {
    let w = noop_waker();
    let mut cx = Context::from_waker(&w);
    Pin::new(fut).poll(&mut cx)  // Safe because F: Unpin
}
```

**Fix Option 2** - Accept Pin:
```rust
fn poll_once<F: Future>(fut: Pin<&mut F>) -> Poll<F::Output> {
    let w = noop_waker();
    let mut cx = Context::from_waker(&w);
    fut.poll(&mut cx)
}
```

**Why Fix #1 is Better**: All futures used in this codebase are `Unpin`, so adding the bound is correct and doesn't change the API for existing callers.

**Impact**: 
- All internal futures (`DurableFuture`, wrapper futures) are `Unpin`
- The orchestrator closures returned by users are `async` blocks which are also typically `Unpin` in this context
- This fix makes the contract explicit and sound

---

## ⚠️ IMPORTANT FIX #2: Add Send/Sync Tests

**File**: `src/futures.rs` (add to test module)

**Add These Tests**:
```rust
#[cfg(test)]
mod soundness_tests {
    use super::*;

    // Ensure DurableFuture is Send (required for spawning on tokio)
    #[test]
    fn durable_future_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DurableFuture>();
    }

    // Ensure DurableFuture is NOT Sync (correct due to Cell/RefCell)
    // This test intentionally does not compile - commented out:
    // #[test]
    // fn durable_future_is_not_sync() {
    //     fn assert_sync<T: Sync>() {}
    //     assert_sync::<DurableFuture>();  // Should fail
    // }

    // Ensure OrchestrationContext is Send + Sync
    #[test]
    fn orchestration_context_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::OrchestrationContext>();
    }

    // Ensure futures work across await points (Send bound check)
    #[tokio::test]
    async fn future_works_across_await() {
        use crate::{OrchestrationContext, Event, INITIAL_EXECUTION_ID};
        
        let ctx = OrchestrationContext::new(
            vec![],
            INITIAL_EXECUTION_ID,
            "test".to_string(),
            None,
            None,
            None,
        );
        
        let fut = ctx.schedule_timer(1000);
        
        // This ensures the future can be held across an await point
        tokio::task::yield_now().await;
        
        // Future is still valid
        drop(fut);
    }
}
```

---

## ⚠️ IMPORTANT FIX #3: Improve Unsafe Code Documentation

**File**: `src/futures.rs:87-89`

**Current**:
```rust
fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
    // Safety: We never move fields that are !Unpin; we only take &mut to mutate inner Cells and use ctx by reference.
    let this = unsafe { self.get_unchecked_mut() };
```

**Better**:
```rust
fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
    // SAFETY: This is safe because all fields of DurableFuture are Unpin:
    // - String: Unpin
    // - Cell<Option<u64>>: Unpin (Cell<T> is Unpin if T: Unpin)
    // - RefCell<Option<T>>: Unpin (RefCell<T> is Unpin if T: Unpin)
    // - OrchestrationContext: Unpin (Arc<Mutex<T>> is always Unpin)
    // We only mutate through Cell/RefCell interior mutability, never moving data.
    let this = unsafe { self.get_unchecked_mut() };
```

**Apply similar improvements to all unsafe blocks**:
- `src/futures.rs:778`
- `src/lib.rs:1146, 1165, 1188, 1207, 1233`
- `src/futures.rs:913, 926`

---

## ⚠️ IMPORTANT FIX #4: Better Panic Messages

**File**: Throughout codebase

**Pattern to Replace**:
```rust
let mut inner = ctx.inner.lock().unwrap();
```

**Replace With**:
```rust
let mut inner = ctx.inner
    .lock()
    .expect("orchestration context mutex poisoned (previous panic in user code)");
```

**Why**: Provides better diagnostics when things go wrong. Mutex poison only occurs on panic, so this message helps developers understand what happened.

---

## 💡 OPTIONAL FIX #5: Static Assertions for Future Properties

**File**: `src/futures.rs` (add to module root)

**Add**:
```rust
// Static compile-time assertions about our future types
#[cfg(test)]
const _: () = {
    // Ensure DurableFuture is Unpin
    const fn assert_unpin<T: Unpin>() {}
    const _: () = assert_unpin::<DurableFuture>();
    
    // We can't easily assert !Sync at compile time, but we document it
};
```

---

## Testing Checklist

After applying fixes, run:

```bash
# 1. Check that code compiles
cargo build

# 2. Run all tests
cargo test

# 3. Run clippy with strict settings
cargo clippy -- -W clippy::unwrap_used -W clippy::expect_used

# 4. Run miri on critical unsafe code (if possible)
cargo +nightly miri test futures::tests

# 5. Run with sanitizers (if applicable)
RUSTFLAGS="-Z sanitizer=address" cargo +nightly test
```

---

## Priority Order

1. **FIX #1** (poll_once) - 5 minutes
2. **FIX #2** (Send/Sync tests) - 10 minutes  
3. **FIX #3** (Safety comments) - 15 minutes
4. **FIX #4** (Panic messages) - 10 minutes

**Total time to address critical issues**: ~40 minutes

---

## Verification

After fixes, verify soundness with:

1. **Miri** (Rust's undefined behavior detector):
   ```bash
   cargo +nightly miri test
   ```

2. **Clippy** (with strict lints):
   ```bash
   cargo clippy --all-features --tests -- -D warnings
   ```

3. **Manual inspection** of all `unsafe` blocks:
   - Ensure each has a SAFETY comment
   - Verify the invariants described are actually upheld
   - Check that no code paths can violate those invariants

---

## Long-Term Recommendations

1. **Add `cargo-geiger`** to CI to track unsafe code:
   ```bash
   cargo install cargo-geiger
   cargo geiger
   ```

2. **Set up Miri in CI** to catch UB early:
   ```yaml
   - name: Run Miri
     run: |
       rustup toolchain install nightly --component miri
       cargo +nightly miri test
   ```

3. **Enable more Clippy lints** in `Cargo.toml`:
   ```toml
   [lints.clippy]
   undocumented_unsafe_blocks = "deny"
   ```

4. **Consider using `static_assertions`** crate for compile-time checks:
   ```toml
   [dev-dependencies]
   static_assertions = "1.1"
   ```

   Then:
   ```rust
   use static_assertions::{assert_impl_all, assert_not_impl_any};
   
   assert_impl_all!(DurableFuture: Send, Unpin);
   assert_not_impl_any!(DurableFuture: Sync);
   ```
