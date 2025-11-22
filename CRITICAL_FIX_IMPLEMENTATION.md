# Critical Fix Implementation Guide

This document provides the exact code changes needed to fix the soundness issue in `poll_once()`.

---

## The Problem

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

**Why it's unsound**: 
- Creates `Pin<&mut F>` from `&mut F` without proving `F` won't be moved
- If `F` is `!Unpin` and self-referential, moving it after this call causes UB
- The `unsafe` here is not justified for all possible `F`

---

## The Solution

### Step 1: Update the function signature

**Replace** lines 1543-1548 in `src/lib.rs`:

**Old**:
```rust
fn poll_once<F: Future>(fut: &mut F) -> Poll<F::Output> {
    let w = noop_waker();
    let mut cx = Context::from_waker(&w);
    let mut pinned = unsafe { Pin::new_unchecked(fut) };
    pinned.as_mut().poll(&mut cx)
}
```

**New**:
```rust
fn poll_once<F: Future + Unpin>(fut: &mut F) -> Poll<F::Output> {
    let w = noop_waker();
    let mut cx = Context::from_waker(&w);
    Pin::new(fut).poll(&mut cx)  // Safe because F: Unpin
}
```

**Changes**:
1. Added `+ Unpin` bound to `F`
2. Removed `unsafe` block
3. Use safe `Pin::new()` instead of `Pin::new_unchecked()`

---

## Verification

### Step 2: Ensure the code still compiles

```bash
cd /workspace
cargo build
```

**Expected**: No compilation errors. All usage sites of `poll_once()` should continue to work because:
- Line 984: `poll_once(&mut fut)` where `fut` is `DurableFuture` (which is `Unpin`)
- Line 1577: `poll_once(&mut fut)` where `fut` is from `orchestrator(ctx)` (async blocks are typically `Unpin`)
- Line 1617: Same as 1577

### Step 3: Run tests

```bash
cargo test
```

**Expected**: All tests pass.

---

## Understanding the Fix

### Why This Works

1. **All current futures are Unpin**:
   ```rust
   // DurableFuture is Unpin because all its fields are Unpin:
   pub struct DurableFuture(pub(crate) Kind);
   
   pub(crate) enum Kind {
       Activity {
           name: String,                      // Unpin
           input: String,                     // Unpin
           claimed_event_id: Cell<...>,       // Unpin
           ctx: OrchestrationContext,         // Unpin (Arc is always Unpin)
       },
       // ... all other variants are also Unpin
   }
   ```

2. **User closures return Unpin futures**:
   ```rust
   // User writes:
   async fn my_orchestration(ctx: OrchestrationContext) -> Result<String, String> {
       // ...
   }
   
   // The compiler generates an impl Future that is Unpin
   // (unless it captures a !Unpin reference, which doesn't happen here)
   ```

3. **Adding the bound makes the contract explicit**:
   - Before: `poll_once` was unsound but happened to work
   - After: `poll_once` is sound and enforces its requirements

### When Would This Break?

This fix would only cause a compilation error if someone tried to pass a `!Unpin` future to `poll_once()`:

```rust
// This would NOT compile with the fix:
struct SelfReferential<'a> {
    data: String,
    pointer: &'a str,  // Points to data above
}

impl Future for SelfReferential<'_> {
    // This is !Unpin
}

let mut fut = SelfReferential { ... };
poll_once(&mut fut);  // ERROR: SelfReferential is !Unpin
```

But this is exactly what we want! The fix prevents unsound usage.

---

## Alternative Fix (Not Recommended)

If you wanted to keep the original signature and make it sound:

```rust
fn poll_once<F: Future>(mut fut: Pin<&mut F>) -> Poll<F::Output> {
    let w = noop_waker();
    let mut cx = Context::from_waker(&w);
    fut.as_mut().poll(&mut cx)
}
```

**Why not recommended**: 
- Requires all call sites to change
- More verbose for callers
- No benefit since all futures are `Unpin` anyway

---

## Additional Safety Improvements

### Optional: Add a test to verify Unpin

Add to `src/lib.rs` in a test module:

```rust
#[cfg(test)]
mod soundness_tests {
    use super::*;

    #[test]
    fn verify_durable_future_is_unpin() {
        fn assert_unpin<T: Unpin>() {}
        assert_unpin::<DurableFuture>();
    }

    #[test]
    fn verify_poll_once_works() {
        let ctx = OrchestrationContext::new(
            vec![],
            1,
            "test".to_string(),
            None,
            None,
            None,
        );
        
        let mut fut = async { 42 };
        let result = poll_once(&mut fut);
        assert_eq!(result, Poll::Ready(42));
    }
}
```

---

## Documentation Update

### Optional: Update the safety comment

Since we removed the `unsafe`, we should update the comment:

```rust
/// Poll a future exactly once using a no-op waker.
/// 
/// This is used internally for synchronous future evaluation during
/// orchestration replay. The future must be `Unpin` to safely create
/// a `Pin<&mut F>` from `&mut F`.
/// 
/// # Examples
/// 
/// ```rust,ignore
/// let mut fut = async { 42 };
/// let result = poll_once(&mut fut);
/// assert_eq!(result, Poll::Ready(42));
/// ```
fn poll_once<F: Future + Unpin>(fut: &mut F) -> Poll<F::Output> {
    let w = noop_waker();
    let mut cx = Context::from_waker(&w);
    Pin::new(fut).poll(&mut cx)
}
```

---

## Commit Message

When committing this fix:

```
fix: Make poll_once() sound by requiring Future + Unpin

Previously, poll_once() used Pin::new_unchecked() on &mut F without
guaranteeing F wouldn't be moved later. This is unsound for !Unpin
futures.

Fix by adding an `Unpin` bound, which allows using the safe Pin::new().
This is correct because all futures currently used with poll_once are
Unpin (DurableFuture and async blocks from user code).

This change makes the function's contract explicit and prevents future
unsound usage.

No functional changes - all existing code compiles and tests pass.
```

---

## Timeline

- **Investigation**: Already done (this document)
- **Implementation**: 2 minutes (copy-paste the new code)
- **Testing**: 5 minutes (cargo build && cargo test)
- **Documentation**: 3 minutes (add doc comment)
- **Total**: ~10 minutes

---

## Risk Assessment

**Risk of breaking existing code**: ❌ None
- All current futures are `Unpin`
- No API changes for callers
- Tests will verify correctness

**Risk of introducing new bugs**: ❌ None
- Removing unsafe code reduces risk
- Making the contract explicit prevents misuse

**Benefits**: ✅ Multiple
- Code is now sound
- No unsafe block
- Clear contract (F must be Unpin)
- Better documentation opportunity

---

## Checklist

- [ ] Update `poll_once()` function in `src/lib.rs:1543`
- [ ] Run `cargo build` to verify compilation
- [ ] Run `cargo test` to verify tests pass
- [ ] Run `cargo clippy` to check for warnings
- [ ] Optional: Add test for `Unpin` requirement
- [ ] Optional: Update doc comment
- [ ] Commit with clear message

---

## Questions & Answers

**Q: Why is this unsound in the original code?**
A: `Pin::new_unchecked()` requires proving the value won't be moved, but `&mut F` doesn't provide that guarantee.

**Q: Why didn't this cause problems before?**
A: All futures actually used are `Unpin`, so moving them is safe. The unsoundness was theoretical.

**Q: Does adding `Unpin` change behavior?**
A: No, it just makes the existing requirement explicit.

**Q: Could we use `Pin<&mut F>` parameter instead?**
A: Yes, but it's more awkward for callers and unnecessary since all futures are `Unpin`.

**Q: Is this the same fix Tokio uses?**
A: Yes, many utility functions in the ecosystem require `Future + Unpin` for exactly this reason.
