# Duroxide Documentation

Welcome to the Duroxide documentation! This guide will help you understand and use the Duroxide deterministic orchestration framework.

## 📚 Documentation Structure

### Getting Started
- **[Quick Start Guide](../QUICK_START.md)** - Get up and running in 5 minutes
- **[Examples](../examples/)** - Complete, runnable examples with explanations
- **[Orchestration Guide](ORCHESTRATION-GUIDE.md)** - Complete guide to writing orchestrations
- **[Troubleshooting](troubleshooting.md)** - Common issues and solutions
- **[External Events](external-events.md)** - Working with external events
- **[ContinueAsNew](continue-as-new.md)** - Long-running orchestration patterns
- **[Sub-Orchestrations](sub-orchestrations.md)** - Composing orchestrations

### Architecture & Design
- **[Architecture Overview](architecture.md)** - System architecture and components
- **[Execution Model](polling-and-execution-model.md)** - How orchestrations execute
- **[Design Invariants](design-invariants.md)** - Core design principles
- **[Consistency Guidelines](consistency-guidelines.md)** - API consistency rules

### Advanced Topics
- **[Reliability & Queues](reliability-queue-and-history.md)** - Reliability guarantees

### Observability & Monitoring
- **[Metrics Specification](metrics-specification.md)** - Complete metrics reference with labels and categories
- **[Observability Guide](observability-guide.md)** - Production monitoring and debugging
- **[Telemetry Spec](duroxide-telemetry-spec.md)** - Detailed telemetry requirements (desired state)
- **[Active Orchestrations Metric](duroxide-active-orchestrations-metric-spec.md)** - Gauge metric specification
- **[Provider Observability](provider-observability.md)** - Instrumenting custom providers
- **[Library Observability](library-observability.md)** - Observability for library developers

### Development
- **[Persistence Analysis](persistence-failure-analysis.md)** - Failure mode analysis
- **[Transaction Example](transaction-example.md)** - Transaction implementation
- **[DTF Runtime Design](dtf-runtime-design.md)** - Runtime implementation details
- **[Provider Implementation Guide](provider-implementation-guide.md)** - How to implement a custom provider
- **[Provider Testing Guide](provider-testing-guide.md)** - How to test custom providers
- **[Cross-Crate Registry Pattern](cross-crate-registry-pattern.md)** - Organizing orchestrations across crates

## 🎯 Learning Path

### For New Users:
1. Start with the **[Quick Start Guide](../QUICK_START.md)**
2. Run the **[Hello World Example](../examples/hello_world.rs)**
3. Read the **[Orchestration Guide](ORCHESTRATION-GUIDE.md)** to understand how Duroxide works
4. Explore **[External Events](external-events.md)** and **[ContinueAsNew](continue-as-new.md)** for advanced patterns

### For Developers:
1. Review the **[Orchestration Guide](ORCHESTRATION-GUIDE.md)** for complete API reference
2. Study the **[Architecture](architecture.md)**
3. Understand the **[Execution Model](polling-and-execution-model.md)**
4. Check **[Design Invariants](design-invariants.md)** before contributing

### For Production Use:
1. Understand **[Reliability Guarantees](reliability-queue-and-history.md)**
2. Learn about **[ContinueAsNew](continue-as-new.md)** for long-running workflows
3. Plan for **[External Events](external-events.md)** integration
4. Set up **[Observability](observability-guide.md)** with metrics and logging
5. Review **[Metrics Specification](metrics-specification.md)** for monitoring dashboards

## 📖 Key Concepts Summary

- **Orchestrations**: Long-running workflows written as async Rust functions
- **Activities**: Stateless functions that perform actual work
- **Deterministic Replay**: Orchestrations replay from history to ensure consistency
- **Turn-Based Execution**: Progress happens through discrete turns triggered by completions
- **Correlation IDs**: Every operation has a unique ID for reliable matching

## 🔧 Common Use Cases

Duroxide is ideal for:
- **Order Processing**: Multi-step workflows with compensation
- **Data Pipelines**: Fan-out/fan-in processing with error handling
- **Approval Workflows**: Human-in-the-loop with timeouts
- **Monitoring**: Long-running monitoring with periodic checks
- **Batch Processing**: Large-scale data processing with checkpointing

## 🤝 Contributing

When contributing to documentation:
1. Update affected docs when changing behavior
2. Add new documents to this index
3. Follow the existing structure and style
4. Include code examples where appropriate
5. Test all code examples

## 🆘 Getting Help

- Start with the examples - they demonstrate real patterns
- Check the test suite for working code
- Review architecture docs for deep understanding
- Open an issue for bugs or clarifications

---

*This documentation is for Duroxide v0.1.0 - an experimental project exploring AI-assisted framework development.*