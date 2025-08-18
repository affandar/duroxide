# Typed Futures Adapters (Unpin) – Design Proposal

## Motivation
- We introduced typed I/O for activities, external events, and (sub-)orchestrations.
- Naively implementing typed adapters with `async move { ... }` produces futures that are `!Unpin`, which conflicts with `futures::future::select` (function) and requires boilerplate (`pin_mut!`, `select!`, `fuse()`) at callsites.
- We want:
  - Back-compat String APIs to remain unchanged and ergonomic
  - Typed adapters that are Unpin, so they work seamlessly with `select` without pinning
  - No runtime cost: decode only on success, inside `poll` (allocation-free)

## Design Overview
- Keep String APIs and futures as-is:
  - `schedule_activity`, `schedule_wait`, `schedule_sub_orchestration`, `schedule_orchestration`
  - `into_activity() -> Future<Output = Result<String, String>>`
  - `into_event() -> Future<Output = String>`
  - `into_sub_orchestration() -> Future<Output = Result<String, String>>`
- Add typed `_typed` adapters that wrap the String futures with named Future structs:
  - `into_activity_typed<Out>() -> Future<Output = Result<Out, String>>`
  - `into_event_typed<T>() -> Future<Output = T>`
  - `into_sub_orchestration_typed<Out>() -> Future<Output = Result<Out, String>>`
- Each typed adapter is implemented as a simple struct that implements `Future` by delegating to the underlying String-future and decoding its output in `poll`.
- Because these wrappers are plain structs with Unpin fields, they are Unpin and compatible with `select` without pinning.

## Detailed Design
- Example: external events (String → T)
```rust
pub struct IntoEventStringFuture { inner: DurableFuture }
impl Future for IntoEventStringFuture {
    type Output = String;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<String> {
        match Pin::new(&mut self.inner).poll(cx) {
            Poll::Ready(DurableOutput::External(s)) => Poll::Ready(s),
            Poll::Ready(other) => panic!("expected External, got {:?}", other),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub struct DecodeFuture<T, F> { inner: F, _pd: PhantomData<T> }
impl<T, F> Future for DecodeFuture<T, F>
where F: Future<Output = String> + Unpin, T: DeserializeOwned {
    type Output = T;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        match Pin::new(&mut self.inner).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(s) => Poll::Ready(Json::decode::<T>(&s).expect("decode")),
        }
    }
}

impl DurableFuture {
    pub fn into_event(self) -> IntoEventStringFuture { IntoEventStringFuture { inner: self } }
    pub fn into_event_typed<T: DeserializeOwned>(self) -> DecodeFuture<T, IntoEventStringFuture> {
        DecodeFuture { inner: self.into_event(), _pd: PhantomData }
    }
}
```
- Activities/sub-orchestrations use `DecodeResultFuture` with `Future<Output = Result<String, String>>` and map to `Result<T, String>`.

## Error Semantics
- Decoding failures surface as `Err(String)` for activities/sub-orchestrations and as a panic for events (optional: switch to `Result<T, String>` for events in future for symmetry).
- The String path remains unchanged; typed wrappers decode only on success.

## Alternatives Considered
- Pin at callsites with `pin_mut!` and use `select!`: works, but adds boilerplate everywhere and reduces readability.
- Keep typed async-block adapters: requires `Unpin` at callsites; not ergonomic.

## Migration Plan
- Keep existing String APIs and tests unchanged.
- Add `_typed` adapters; update docs and samples to demonstrate both paths.
- Optional: Over time, convert orchestrations/activities to typed registration by adding `register_typed` and `start_orchestration_typed`.

## Work Items
- Implement Unpin wrapper futures for `into_event_typed`, `into_activity_typed`, `into_sub_orchestration_typed` (String futures already exist).
- Ensure typed wrappers are allocation-free and only decode on `Ready`.
- Add docs (this page) and link from TODO.md.
