# Durable Actors: Integrating Duroxide with Actor Frameworks

**GitHub Issue**: [#29](https://github.com/microsoft/duroxide/issues/29)

> ⚠️ **Note**: The ideas in this document have not been vetted in detail by a human yet. They are meant to get the creative juices flowing and serve as a starting point for deeper exploration.

This document explores ideas for integrating duroxide with actor frameworks to create **durable actors** — actors with persistent state, reliable message delivery, and crash recovery semantics.

## Motivation

### The Actor Model

The actor model provides:
- **Encapsulation**: Each actor owns its state
- **Message passing**: Asynchronous communication between actors
- **Supervision**: Hierarchical failure handling
- **Location transparency**: Actors can be local or remote

### What's Missing

Traditional actor frameworks (Actix, Tokio actors, etc.) are in-memory:
- Actor state is lost on crash
- Messages in flight are lost
- Supervision restarts actors from scratch
- No replay of message history

### Durable Actors

Combine duroxide's durability with the actor model:
- **Persistent state**: Actor state survives crashes
- **Durable mailbox**: Messages are persisted and delivered exactly-once
- **Replay recovery**: Actors can replay their message history to rebuild state
- **Durable supervision**: Supervision decisions are recorded and replayed

---

## Concept 1: Actor as Orchestration

### Idea

Each actor is a long-running orchestration that:
- Uses `continue_as_new` to manage history size
- Receives messages via external events
- Maintains state across iterations
- Can spawn child actors (sub-orchestrations)

### Implementation Sketch

```rust
/// Durable actor trait
#[async_trait]
pub trait DurableActor: Sized + Serialize + DeserializeOwned {
    /// Actor's unique identifier
    fn actor_id(&self) -> &str;
    
    /// Handle a message, returning updated state
    async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError>;
    
    /// Called on actor startup
    async fn on_start(&mut self, ctx: &ActorContext) -> Result<(), ActorError> {
        Ok(())
    }
    
    /// Called before continue_as_new
    async fn on_checkpoint(&mut self, ctx: &ActorContext) -> Result<(), ActorError> {
        Ok(())
    }
}

/// Actor execution loop (orchestration)
async fn actor_loop<A: DurableActor>(ctx: OrchestrationContext) -> Result<(), String> {
    let mut actor: A = ctx.get_input_typed()?;
    
    // Startup hook (only on first execution, not replay)
    if ctx.is_first_execution() {
        actor.on_start(&ctx.into()).await?;
    }
    
    loop {
        // Wait for next message (external event)
        let msg: Message = ctx.schedule_wait_typed("inbox").await;
        
        // Handle message
        actor.handle(&ctx.into(), msg).await?;
        
        // Checkpoint periodically
        if ctx.should_continue_as_new() {
            actor.on_checkpoint(&ctx.into()).await?;
            ctx.continue_as_new(&actor)?;
        }
    }
}
```

### Sending Messages

```rust
impl ActorRef {
    /// Send message to actor (fire-and-forget)
    pub async fn send(&self, msg: impl Into<Message>) -> Result<(), SendError> {
        self.client
            .raise_event(&self.actor_id, "inbox", msg.into())
            .await
    }
    
    /// Send message and wait for response
    pub async fn ask<R>(&self, msg: impl Into<Message>) -> Result<R, AskError> {
        // Create response channel (another actor or callback)
        let response_id = generate_id();
        let msg = msg.into().with_reply_to(response_id);
        
        self.send(msg).await?;
        
        // Wait for response event
        self.client.wait_for_event(&response_id, "response").await
    }
}
```

### Pros & Cons

**Pros:**
- Simple mapping to existing duroxide concepts
- No new runtime components needed
- Full durability guarantees

**Cons:**
- External events as mailbox may have performance overhead
- No batching of messages
- Supervision requires separate orchestration

---

## Concept 2: Actor System as Provider Extension

### Idea

Extend the provider to support actor-specific operations:
- Persistent actor registry
- Durable mailboxes (message queues per actor)
- Actor state storage
- Message routing

### Provider Extensions

```rust
pub trait ActorProvider: OrchestrationProvider {
    /// Register an actor
    async fn register_actor(&self, actor_id: &str, actor_type: &str) -> Result<()>;
    
    /// Store actor state
    async fn save_actor_state(&self, actor_id: &str, state: &[u8]) -> Result<()>;
    
    /// Load actor state
    async fn load_actor_state(&self, actor_id: &str) -> Result<Option<Vec<u8>>>;
    
    /// Enqueue message to actor's mailbox
    async fn enqueue_message(&self, actor_id: &str, msg: ActorMessage) -> Result<MessageId>;
    
    /// Dequeue messages from mailbox
    async fn dequeue_messages(&self, actor_id: &str, limit: usize) -> Result<Vec<ActorMessage>>;
    
    /// Acknowledge message processing
    async fn ack_message(&self, actor_id: &str, msg_id: MessageId) -> Result<()>;
}

pub struct ActorMessage {
    pub id: MessageId,
    pub sender: Option<ActorId>,
    pub payload: Vec<u8>,
    pub reply_to: Option<ActorId>,
    pub timestamp: u64,
    pub headers: HashMap<String, String>,
}
```

### Actor Runtime

```rust
pub struct ActorSystem {
    provider: Arc<dyn ActorProvider>,
    registry: ActorRegistry,
    dispatchers: Vec<ActorDispatcher>,
}

impl ActorSystem {
    /// Spawn a new actor
    pub async fn spawn<A: DurableActor>(&self, actor: A) -> ActorRef {
        let actor_id = actor.actor_id().to_string();
        
        // Register actor
        self.provider.register_actor(&actor_id, A::TYPE_NAME).await?;
        
        // Save initial state
        let state = serialize(&actor)?;
        self.provider.save_actor_state(&actor_id, &state).await?;
        
        ActorRef::new(actor_id, self.clone())
    }
    
    /// Get reference to existing actor
    pub fn actor_ref(&self, actor_id: &str) -> ActorRef {
        ActorRef::new(actor_id.to_string(), self.clone())
    }
}
```

### Actor Dispatcher

Separate dispatcher for processing actor messages:

```rust
impl ActorDispatcher {
    async fn run(&self) {
        loop {
            // Poll for actors with pending messages
            let work = self.provider.poll_actor_work().await?;
            
            for actor_id in work {
                // Load actor state
                let state = self.provider.load_actor_state(&actor_id).await?;
                let mut actor = deserialize(&state)?;
                
                // Dequeue messages
                let messages = self.provider.dequeue_messages(&actor_id, BATCH_SIZE).await?;
                
                // Process messages
                for msg in messages {
                    actor.handle(&self.ctx, msg.clone()).await?;
                    self.provider.ack_message(&actor_id, msg.id).await?;
                }
                
                // Save updated state
                let new_state = serialize(&actor)?;
                self.provider.save_actor_state(&actor_id, &new_state).await?;
            }
        }
    }
}
```

### Pros & Cons

**Pros:**
- Optimized for actor patterns (batching, efficient mailbox)
- Clear separation of concerns
- Can leverage existing actor framework concepts

**Cons:**
- Requires provider changes
- More complex implementation
- Diverges from core orchestration model

---

## Concept 3: Hybrid Model — Actors with Orchestration Backing

### Idea

Actors run in-memory for performance, but backed by orchestrations for durability:
- Actor state changes emit events to backing orchestration
- On crash, orchestration replays to rebuild actor state
- Messages flow through actor system, persisted to orchestration history

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    In-Memory Actor Runtime                   │
│  ┌─────────┐    ┌─────────┐    ┌─────────┐                  │
│  │ Actor A │◄──►│ Actor B │◄──►│ Actor C │                  │
│  └────┬────┘    └────┬────┘    └────┬────┘                  │
│       │              │              │                        │
│       ▼              ▼              ▼                        │
│  ┌─────────────────────────────────────────────────────┐    │
│  │              Event Journal (in-memory)               │    │
│  └──────────────────────┬──────────────────────────────┘    │
└─────────────────────────┼───────────────────────────────────┘
                          │ async persist
                          ▼
┌─────────────────────────────────────────────────────────────┐
│                 Duroxide Orchestration                       │
│  ┌─────────────────────────────────────────────────────┐    │
│  │              Event History (durable)                 │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

### Event Sourcing for Actors

```rust
/// Events emitted by actors
pub enum ActorEvent {
    Spawned { actor_id: String, actor_type: String, initial_state: Vec<u8> },
    MessageReceived { actor_id: String, msg_id: MessageId, payload: Vec<u8> },
    MessageProcessed { actor_id: String, msg_id: MessageId },
    StateChanged { actor_id: String, new_state: Vec<u8> },
    MessageSent { from: String, to: String, msg_id: MessageId, payload: Vec<u8> },
    ActorStopped { actor_id: String, reason: String },
}

/// Replay events to rebuild actor state
fn rebuild_actor(events: &[ActorEvent]) -> HashMap<String, ActorState> {
    let mut actors = HashMap::new();
    
    for event in events {
        match event {
            ActorEvent::Spawned { actor_id, initial_state, .. } => {
                actors.insert(actor_id.clone(), deserialize(initial_state)?);
            }
            ActorEvent::StateChanged { actor_id, new_state } => {
                actors.insert(actor_id.clone(), deserialize(new_state)?);
            }
            ActorEvent::ActorStopped { actor_id, .. } => {
                actors.remove(actor_id);
            }
            _ => {}
        }
    }
    
    actors
}
```

### Checkpointing

Periodically snapshot actor state to avoid replaying entire history:

```rust
pub struct ActorCheckpoint {
    pub checkpoint_id: u64,
    pub timestamp: u64,
    pub actor_states: HashMap<String, Vec<u8>>,
    pub pending_messages: Vec<ActorMessage>,
}

// Recovery: load checkpoint + replay events after checkpoint
async fn recover_actor_system(provider: &dyn Provider) -> ActorSystem {
    let checkpoint = provider.load_latest_checkpoint().await?;
    let events_after = provider.load_events_after(checkpoint.checkpoint_id).await?;
    
    let mut actors = checkpoint.actor_states;
    for event in events_after {
        apply_event(&mut actors, event);
    }
    
    ActorSystem::from_state(actors)
}
```

### Pros & Cons

**Pros:**
- High performance (in-memory message passing)
- Full durability via event sourcing
- Familiar actor programming model
- Efficient recovery via checkpoints

**Cons:**
- Complex implementation
- Two consistency models to reason about
- Checkpoint management overhead

---

## Concept 4: Virtual Actors (Orleans-style)

### Idea

Inspired by Microsoft Orleans "virtual actors":
- Actors are **addressable by ID** — no explicit lifecycle management
- Actors are **activated on demand** when first messaged
- Actors are **deactivated** after idle timeout (state persisted)
- **Location transparent** — runtime decides where to run actor

### API

```rust
/// Define a virtual actor
#[duroxide::virtual_actor]
pub struct PlayerActor {
    pub player_id: String,
    pub score: u64,
    pub inventory: Vec<Item>,
}

#[duroxide::virtual_actor]
impl PlayerActor {
    /// Activation hook
    async fn on_activate(&mut self, ctx: &ActorContext) -> Result<()> {
        // Load state from storage, or initialize if new
        Ok(())
    }
    
    /// Deactivation hook
    async fn on_deactivate(&mut self, ctx: &ActorContext) -> Result<()> {
        // Save state to storage
        Ok(())
    }
    
    /// Message handlers
    pub async fn add_score(&mut self, ctx: &ActorContext, points: u64) -> Result<u64> {
        self.score += points;
        Ok(self.score)
    }
    
    pub async fn get_inventory(&self, ctx: &ActorContext) -> Result<Vec<Item>> {
        Ok(self.inventory.clone())
    }
}

// Usage — no spawn needed, just call by ID
let player = PlayerActor::get("player-123");
let score = player.add_score(100).await?;
```

### Runtime Management

```rust
pub struct VirtualActorRuntime {
    /// Active actors in memory
    active: DashMap<ActorId, ActiveActor>,
    /// Provider for persistence
    provider: Arc<dyn Provider>,
    /// Configuration
    config: VirtualActorConfig,
}

pub struct VirtualActorConfig {
    /// Deactivate actor after this idle time
    pub idle_timeout: Duration,
    /// Max actors to keep in memory
    pub max_active_actors: usize,
    /// Persistence mode
    pub persistence: PersistenceMode,
}

pub enum PersistenceMode {
    /// Save on every state change
    WriteThrough,
    /// Save periodically and on deactivation
    WriteBack { interval: Duration },
    /// Save only on deactivation
    Lazy,
}
```

### Actor Placement

For distributed scenarios:

```rust
pub trait ActorPlacement {
    /// Determine which node should host this actor
    fn place(&self, actor_id: &str) -> NodeId;
    
    /// Rebalance actors across nodes
    fn rebalance(&self, nodes: &[NodeId]) -> Vec<Migration>;
}

pub struct ConsistentHashPlacement {
    ring: HashRing<NodeId>,
}

impl ActorPlacement for ConsistentHashPlacement {
    fn place(&self, actor_id: &str) -> NodeId {
        self.ring.get(actor_id).clone()
    }
}
```

### Pros & Cons

**Pros:**
- Simplest programming model (just call actor by ID)
- Automatic lifecycle management
- Scales naturally (activate where needed)
- Familiar to Orleans/Dapr users

**Cons:**
- Implicit activation can surprise users
- State loading latency on activation
- Complex distributed placement logic

---

## Concept 5: Supervision Trees with Durable Recovery

### Idea

Implement Erlang/Akka-style supervision trees with durable recovery decisions.

### Supervision Strategies

```rust
pub enum SupervisionStrategy {
    /// Restart just the failed actor
    OneForOne {
        max_restarts: u32,
        within: Duration,
    },
    /// Restart all children if one fails
    OneForAll {
        max_restarts: u32,
        within: Duration,
    },
    /// Restart failed actor and all actors started after it
    RestForOne {
        max_restarts: u32,
        within: Duration,
    },
}

pub enum SupervisionDecision {
    /// Restart the actor with fresh state
    Restart,
    /// Restart the actor, preserving state
    Resume,
    /// Stop the actor permanently
    Stop,
    /// Escalate to parent supervisor
    Escalate,
}
```

### Durable Supervisor

```rust
/// Supervisor as orchestration
async fn supervisor_orchestration(ctx: OrchestrationContext) -> Result<(), String> {
    let config: SupervisorConfig = ctx.get_input_typed()?;
    let mut children: Vec<ChildActor> = vec![];
    let mut restart_counts: HashMap<String, Vec<Instant>> = HashMap::new();
    
    // Spawn initial children
    for child_spec in &config.children {
        let child = spawn_child(&ctx, child_spec).await?;
        children.push(child);
    }
    
    loop {
        // Wait for child event (failure, message, etc.)
        let event: SupervisorEvent = ctx.schedule_wait_typed("supervisor").await;
        
        match event {
            SupervisorEvent::ChildFailed { child_id, error } => {
                // Record in history (durable decision)
                let decision = decide_supervision(&config.strategy, &child_id, &mut restart_counts);
                
                match decision {
                    SupervisionDecision::Restart => {
                        // Restart child (recorded in history)
                        let child_spec = find_child_spec(&config, &child_id);
                        let new_child = spawn_child(&ctx, child_spec).await?;
                        replace_child(&mut children, &child_id, new_child);
                    }
                    SupervisionDecision::Stop => {
                        remove_child(&mut children, &child_id);
                    }
                    SupervisionDecision::Escalate => {
                        // Propagate to parent supervisor
                        return Err(format!("Child {} failed, escalating", child_id));
                    }
                    _ => {}
                }
            }
            SupervisorEvent::SpawnChild { spec } => {
                let child = spawn_child(&ctx, &spec).await?;
                children.push(child);
            }
            SupervisorEvent::StopChild { child_id } => {
                stop_child(&ctx, &child_id).await?;
                remove_child(&mut children, &child_id);
            }
        }
        
        if ctx.should_continue_as_new() {
            ctx.continue_as_new(SupervisorState { config, children, restart_counts })?;
        }
    }
}
```

### Recovery Replay

On system restart, the supervisor orchestration replays:
1. Which children were spawned
2. Which failures occurred
3. What supervision decisions were made
4. Current set of active children

This provides **durable supervision** — the supervision tree structure and decisions survive crashes.

---

## Integration with Existing Frameworks

### Actix Integration

```rust
/// Wrapper to make Actix actor durable
pub struct DurableActixActor<A: Actor> {
    inner: A,
    orchestration_id: String,
    client: DuroxideClient,
}

impl<A: Actor> Actor for DurableActixActor<A> {
    type Context = Context<Self>;
    
    fn started(&mut self, ctx: &mut Self::Context) {
        // Sync with backing orchestration
        self.sync_state(ctx);
    }
}
```

### Tokio Actors (tower-actor style)

```rust
/// Durable actor service
pub struct DurableActorService<A: DurableActor> {
    actor: A,
    receiver: mpsc::Receiver<Message>,
    journal: EventJournal,
}

impl<A: DurableActor> DurableActorService<A> {
    pub async fn run(mut self) {
        while let Some(msg) = self.receiver.recv().await {
            // Journal message receipt
            self.journal.append(ActorEvent::MessageReceived { .. }).await;
            
            // Handle message
            self.actor.handle(msg).await;
            
            // Journal state change
            self.journal.append(ActorEvent::StateChanged { .. }).await;
        }
    }
}
```

---

## Open Questions

1. **Performance**: What's the acceptable overhead for durability in actor message passing?
2. **Consistency**: How to handle actors that need transactional state updates?
3. **Distribution**: How does actor placement interact with duroxide's provider sharding?
4. **Framework Choice**: Should we build on an existing actor framework or create a new one?
5. **Mailbox Semantics**: FIFO vs. priority mailboxes? How does this interact with replay?
6. **Timers**: How do actor timers integrate with durable timers?
7. **Streaming**: How to handle actors that process streams of data?

---

## Recommended Starting Point

**Start with Concept 1 (Actor as Orchestration)** because:
- Minimal implementation effort
- Uses existing duroxide primitives
- Proves the concept before optimizing
- Can evolve to more sophisticated models based on learnings

Once patterns emerge, consider:
- Concept 4 (Virtual Actors) for easier programming model
- Concept 3 (Hybrid) for performance-critical use cases
- Concept 5 (Supervision Trees) for fault-tolerant systems

