# LLM Integration with Duroxide

This document captures ideas for integrating Large Language Models into the duroxide framework, enabling AI-powered orchestrations and developer tooling.

## Summary

| # | Feature | Issue | Description |
|---|---------|-------|-------------|
| 1 | [LLM Provider](#1-llm-provider) | [#21](https://github.com/microsoft/duroxide/issues/21) | Replay-safe LLM operations on orchestration context (generate, if_true, extract, etc.) |
| 2 | [Dynamic Orchestration Construction](#2-dynamic-orchestration-construction) | [#22](https://github.com/microsoft/duroxide/issues/22) | LLM-driven orchestration that constructs itself dynamically using tools |
| 3 | [LLM Build Step for Visualization](#3-llm-build-step-for-visualization) | [#23](https://github.com/microsoft/duroxide/issues/23) | Cargo build integration to generate orchestration diagrams from code |

---

## 1. LLM Provider

### Concept

Add an **LLM provider** alongside the storage provider. This provider exposes replay-safe methods on the orchestration context for LLM operations. All LLM calls are recorded in history and replayed deterministically.

### Architecture

```
┌─────────────────────────────────────────────┐
│              Duroxide Runtime               │
├─────────────────────┬───────────────────────┤
│  Storage Provider   │    LLM Provider       │
│  (SQLite, Postgres) │  (OpenAI, Azure, etc.)│
└─────────────────────┴───────────────────────┘
```

### Core API

```rust
impl OrchestrationContext {
    /// Generate text completion
    /// Returns: Generated text
    pub async fn generate(&self, prompt: impl Into<String>) -> Result<String, LlmError>;
    
    /// Generate with system prompt
    pub async fn generate_with_system(
        &self,
        system: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Result<String, LlmError>;
    
    /// Yes/no decision based on prompt
    /// Returns: true/false
    pub async fn if_true(&self, prompt: impl Into<String>) -> Result<bool, LlmError>;
    
    /// Extract structured features from text
    /// Returns: Key-value pairs
    pub async fn extract_features(
        &self,
        text: impl Into<String>,
        features: &[&str],
    ) -> Result<HashMap<String, String>, LlmError>;
    
    /// Classify text into one of the provided categories
    pub async fn classify(
        &self,
        text: impl Into<String>,
        categories: &[&str],
    ) -> Result<String, LlmError>;
    
    /// Generate structured output matching a schema
    pub async fn generate_structured<T: DeserializeOwned>(
        &self,
        prompt: impl Into<String>,
        schema: &str, // JSON Schema
    ) -> Result<T, LlmError>;
    
    /// Summarize text to specified length
    pub async fn summarize(
        &self,
        text: impl Into<String>,
        max_words: usize,
    ) -> Result<String, LlmError>;
    
    /// Sentiment analysis
    pub async fn sentiment(&self, text: impl Into<String>) -> Result<Sentiment, LlmError>;
}

#[derive(Debug, Clone)]
pub enum Sentiment {
    Positive(f32),  // confidence 0.0-1.0
    Negative(f32),
    Neutral(f32),
}
```

### Automatic Context Injection

Optionally inject orchestration context into LLM prompts automatically:

```rust
pub struct LlmOptions {
    /// Include execution history in LLM context
    pub include_history: bool,
    /// Include current orchestration state
    pub include_state: bool,
    /// Include activity results from this execution
    pub include_activity_results: bool,
    /// Custom context to always include
    pub custom_context: Option<String>,
    /// Max tokens for context (truncates oldest first)
    pub max_context_tokens: usize,
}

impl OrchestrationContext {
    /// Generate with automatic context injection
    pub async fn generate_with_context(
        &self,
        prompt: impl Into<String>,
        options: LlmOptions,
    ) -> Result<String, LlmError>;
}
```

**Context includes:**
- Orchestration name and version
- Current execution ID
- Recent activity results (success/failure)
- Timer events and external events received
- Custom state set by orchestration

### Additional Goodies

**Retry with Refinement:**
```rust
/// Generate with automatic retry and refinement on validation failure
pub async fn generate_validated<T, F>(
    &self,
    prompt: impl Into<String>,
    validator: F,
    max_attempts: u32,
) -> Result<T, LlmError>
where
    T: DeserializeOwned,
    F: Fn(&T) -> Result<(), String>;
```

**Tool Descriptions:**
```rust
/// Describe available activities as tools for the LLM
pub fn describe_tools(&self) -> Vec<ToolDescription>;

pub struct ToolDescription {
    pub name: String,
    pub description: String,
    pub parameters: String, // JSON Schema
}
```

### History Events

```rust
EventKind::LlmRequested {
    operation: String,      // "generate", "if_true", "extract", etc.
    prompt_hash: String,    // Hash of prompt for dedup
    options: String,        // Serialized options
}

EventKind::LlmCompleted {
    source_event_id: u64,
    result: String,         // Serialized result
    tokens_used: u64,
    model: String,
}
```

### Provider Trait

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Generate completion
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError>;
    
    /// Provider name for logging/metrics
    fn name(&self) -> &str;
    
    /// Model being used
    fn model(&self) -> &str;
}

pub struct CompletionRequest {
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub response_format: Option<ResponseFormat>,
}

pub enum ResponseFormat {
    Text,
    Json { schema: Option<String> },
}
```

### Provider Implementations

- `OpenAiProvider` — OpenAI API (GPT-4, etc.)
- `AzureOpenAiProvider` — Azure OpenAI Service
- `AnthropicProvider` — Claude models
- `OllamaProvider` — Local models via Ollama
- `MockLlmProvider` — For testing (returns canned responses)

---

## 2. Dynamic Orchestration Construction

### Concept

An LLM-driven orchestration that **constructs itself dynamically** based on intent. Instead of writing orchestration logic in code, the LLM decides which tools (activities) to call based on the current state and goal.

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Meta Orchestration                        │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  Loop (with continue_as_new each iteration):        │    │
│  │                                                     │    │
│  │  1. Gather context (history, tool results, errors)  │    │
│  │  2. Call LLM with intent + tools + context          │    │
│  │  3. LLM outputs execution plan (JSON)               │    │
│  │  4. Execute tools (activities) per plan             │    │
│  │  5. Collect results                                 │    │
│  │  6. continue_as_new with updated context            │    │
│  │                                                     │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

### Execution Plan Schema

LLM outputs a JSON execution plan:

```json
{
  "thought": "The CPU is high, I should check what processes are running before restarting",
  "actions": [
    {
      "tool": "get_top_processes",
      "params": { "count": 10 },
      "id": "step1"
    }
  ],
  "parallel": false,
  "done": false,
  "done_reason": null
}
```

**Parallel execution:**
```json
{
  "thought": "Need to check both CPU and memory metrics simultaneously",
  "actions": [
    { "tool": "get_cpu_metrics", "params": {}, "id": "cpu" },
    { "tool": "get_memory_metrics", "params": {}, "id": "mem" }
  ],
  "parallel": true,
  "done": false
}
```

**Completion:**
```json
{
  "thought": "VM has been restarted and metrics are back to normal",
  "actions": [],
  "done": true,
  "done_reason": "success",
  "summary": "Resolved high CPU by restarting the container that was stuck in a loop"
}
```

### Tool Registry

Activities are exposed as "tools" to the LLM:

```rust
pub struct ToolRegistry {
    tools: HashMap<String, ToolDefinition>,
}

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_schema: String,  // JSON Schema
    pub returns_schema: String,     // JSON Schema
    pub examples: Vec<ToolExample>,
}

pub struct ToolExample {
    pub description: String,
    pub input: String,
    pub output: String,
}
```

**Example tools for VM remediation:**
```rust
let tools = vec![
    ToolDefinition {
        name: "get_metrics".into(),
        description: "Get current CPU, memory, disk metrics for a VM".into(),
        parameters_schema: r#"{"type":"object","properties":{"vm_id":{"type":"string"}}}"#.into(),
        ..
    },
    ToolDefinition {
        name: "restart_vm".into(),
        description: "Restart a virtual machine (takes 2-3 minutes)".into(),
        ..
    },
    ToolDefinition {
        name: "restart_container".into(),
        description: "Restart a specific container on a VM".into(),
        ..
    },
    ToolDefinition {
        name: "reset_network".into(),
        description: "Reset network configuration on a VM".into(),
        ..
    },
    ToolDefinition {
        name: "get_logs".into(),
        description: "Get recent logs from a service".into(),
        ..
    },
];
```

### Meta Orchestration Implementation

```rust
async fn llm_driven_orchestration(ctx: OrchestrationContext) -> Result<String, String> {
    // Get input: intent and initial context
    let input: LlmOrchInput = ctx.get_input_typed()?;
    
    // Build context from previous execution (if continue_as_new)
    let mut context = input.context.unwrap_or_default();
    
    // Get available tools
    let tools = ctx.describe_tools();
    
    // Build prompt with intent, tools, and context
    let prompt = build_prompt(&input.intent, &tools, &context);
    
    // Call LLM to get execution plan
    let plan: ExecutionPlan = ctx.generate_structured(prompt, PLAN_SCHEMA).await?;
    
    // Check if done
    if plan.done {
        return Ok(plan.summary.unwrap_or("Complete".into()));
    }
    
    // Execute actions
    let results = if plan.parallel {
        execute_parallel(&ctx, &plan.actions).await?
    } else {
        execute_sequential(&ctx, &plan.actions).await?
    };
    
    // Update context with results
    context.add_step(plan.thought, results);
    
    // Continue as new with updated context
    ctx.continue_as_new(LlmOrchInput {
        intent: input.intent,
        context: Some(context),
    })?;
    
    unreachable!()
}
```

### Safety Guardrails

```rust
pub struct LlmOrchestrationConfig {
    /// Maximum iterations before forcing completion
    pub max_iterations: u32,
    /// Maximum total cost (in tokens or dollars)
    pub max_cost: Cost,
    /// Tools that require human approval
    pub approval_required: Vec<String>,
    /// Tools that are completely forbidden
    pub forbidden_tools: Vec<String>,
    /// Timeout for entire orchestration
    pub timeout: Duration,
}
```

**Human-in-the-loop:**
```rust
// If action requires approval, pause for external event
if config.approval_required.contains(&action.tool) {
    let approval = ctx.schedule_wait_typed::<Approval>("approval").await;
    if !approval.approved {
        context.add_rejection(action.tool, approval.reason);
        continue;
    }
}
```

### Use Cases

- **Automated remediation**: "Fix the high CPU on vm-123"
- **Incident response**: "Investigate and resolve the alert for service-xyz"
- **Data pipeline repair**: "The ETL job failed, diagnose and fix it"
- **Infrastructure provisioning**: "Set up a new dev environment like prod"

---

## 3. LLM Build Step for Visualization

### Concept

Integrate an LLM-powered build step into Cargo that analyzes orchestration code and generates visual diagrams. These diagrams are encoded as strings (Mermaid, DOT, etc.) within the code or as separate artifacts.

### Build Integration

```toml
# Cargo.toml
[package.metadata.duroxide]
generate_diagrams = true
diagram_format = "mermaid"  # or "dot", "plantuml"
output_dir = "docs/diagrams"
```

### How It Works

1. **Parse orchestration code** using `syn` or rust-analyzer
2. **Extract flow information**:
   - Activity calls and their order
   - Timer/delay usage
   - External event waits
   - Sub-orchestration calls
   - Conditional branches (if detectable)
   - Parallel execution (fan-out/fan-in)
3. **Send to LLM** with prompt to generate diagram
4. **Output diagram** as embedded string or file

### Generated Artifacts

**Mermaid diagram:**
```rust
// Auto-generated by duroxide-diagram
// DO NOT EDIT - regenerate with `cargo duroxide diagram`
pub const ORDER_WORKFLOW_DIAGRAM: &str = r#"
flowchart TD
    A[Start] --> B[validate_order]
    B --> C{Valid?}
    C -->|Yes| D[reserve_inventory]
    C -->|No| E[Return Error]
    D --> F[process_payment]
    F --> G{Payment OK?}
    G -->|Yes| H[ship_order]
    G -->|No| I[release_inventory]
    H --> J[send_confirmation]
    I --> E
    J --> K[End]
"#;
```

**Sequence diagram for complex flows:**
```rust
pub const SAGA_WORKFLOW_SEQUENCE: &str = r#"
sequenceDiagram
    participant O as Orchestration
    participant A as Activity: reserve_flight
    participant B as Activity: reserve_hotel
    participant C as Activity: charge_card
    
    O->>A: reserve_flight()
    A-->>O: flight_id
    O->>B: reserve_hotel()
    B-->>O: hotel_id
    O->>C: charge_card()
    alt Payment Success
        C-->>O: confirmation
        O->>O: Complete
    else Payment Failed
        C-->>O: error
        O->>B: cancel_hotel(hotel_id)
        O->>A: cancel_flight(flight_id)
        O->>O: Compensated
    end
"#;
```

### CLI Commands

```bash
# Generate diagrams for all orchestrations
cargo duroxide diagram

# Generate for specific orchestration
cargo duroxide diagram --name order_workflow

# Output to specific format
cargo duroxide diagram --format dot --output ./diagrams/

# Preview in terminal (requires mermaid-cli)
cargo duroxide diagram --preview
```

### Build Script Integration

```rust
// build.rs
fn main() {
    duroxide_build::generate_diagrams()
        .format(DiagramFormat::Mermaid)
        .output_dir("src/generated")
        .run()
        .expect("Failed to generate diagrams");
}
```

### Diagram Attributes

Annotate orchestrations for better diagrams:

```rust
#[duroxide::orchestration(
    name = "order_workflow",
    version = "1.0.0",
    diagram_title = "Order Processing Workflow",
    diagram_description = "Handles end-to-end order processing with payment and shipping"
)]
async fn order_workflow(ctx: OrchestrationContext) -> Result<String, String> {
    // ...
}

#[duroxide::activity(
    name = "process_payment",
    diagram_label = "💳 Process Payment",
    diagram_color = "green"
)]
async fn process_payment(ctx: ActivityContext, input: String) -> Result<String, String> {
    // ...
}
```

### Integration with Tooling

Generated diagrams can be used in:
- **Documentation**: Auto-embed in README or docs
- **Management UI**: Render workflow visualization
- **IDE plugins**: Show diagram alongside code
- **CI/CD**: Generate and publish on each build

---

## Open Questions

1. **LLM Provider**: How to handle rate limiting and cost management across orchestrations?
2. **LLM Provider**: Should we cache LLM responses for identical prompts (within same execution)?
3. **Dynamic Orchestration**: How to handle LLM "hallucinating" non-existent tools?
4. **Dynamic Orchestration**: What's the right balance between autonomy and human oversight?
5. **Visualization**: Can we infer branching logic from code without explicit annotations?
6. **Visualization**: Should diagrams be validated against actual code structure?

