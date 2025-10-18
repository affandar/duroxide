# Duroxide Macros

Procedural macros for duroxide - syntactic sugar for durable workflows.

## Status: Phase 1 Complete ✅

### Implemented
- ✅ Basic `#[activity]` macro (pass-through)
- ✅ Basic `#[orchestration]` macro (pass-through)
- ✅ Macro stubs for future phases
- ✅ 34 comprehensive tests
- ✅ Workspace integration
- ✅ Feature flag support
- ✅ Example working

### Phase 1 Deliverables
- Proc-macro crate structure
- Basic macro annotations
- Compilation tests
- Documentation
- Integration with main crate

## Testing

```bash
# Run macro tests
cargo test

# Run with duroxide integration
cd .. && cargo test --features macros

# Run example
cd .. && cargo run --example hello_world_macros --features macros
```

## Current Test Coverage

**34 tests across:**
- Basic activity annotations (8 tests)
- Basic orchestration annotations (7 tests)
- Attribute parsing (2 tests)
- E2E patterns (5 tests)
- Versioning (1 test)
- Complex types (2 tests)
- Nested modules (2 tests)
- Realistic scenarios (2 tests)
- Comprehensive combinations (5 tests)

All tests passing! ✅

## Next Steps (Phase 2+)

See `/docs/proposals/MACRO-FINAL-DESIGN.md` for complete implementation plan.

### Phase 2: Type extraction and code generation
- Implement typed parameter extraction
- Generate `.call()` methods
- Handle serialization/deserialization

### Phase 3: Invocation macros
- Implement `durable!()` macro
- Implement trace macros
- Implement system call macros

### Phase 4+: Advanced features
- Client helper generation
- Auto-discovery with linkme
- Runtime::builder() integration

