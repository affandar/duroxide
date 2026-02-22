# Toygres Improvements Roadmap

This document captures improvements to **toygres**, the test application built on duroxide that simulates a managed Postgres service. Toygres serves as an important feedback loop for duroxide development, exercising real-world orchestration patterns.

**Repository**: [affandar/toygres](https://github.com/affandar/toygres)

## Summary

| # | Feature | Issue | Description |
|---|---------|-------|-------------|
| 1 | [Replica Support](#1-replica-support) | [#24](https://github.com/microsoft/duroxide/issues/24) | Add read replicas with streaming replication |
| 2 | [Manual Failover](#2-manual-failover) | [#25](https://github.com/microsoft/duroxide/issues/25) | Support operator-initiated failover to replica |
| 3 | [Automatic Failover Monitoring](#3-automatic-failover-monitoring) | [#26](https://github.com/microsoft/duroxide/issues/26) | Instance actor monitors health and triggers automatic failover |
| 4 | [Backup & Restore](#4-backup--restore) | [#27](https://github.com/microsoft/duroxide/issues/27) | Point-in-time backup and restore capabilities |
| 5 | [Postgres Parameters](#5-postgres-parameters) | [#28](https://github.com/microsoft/duroxide/issues/28) | Configure and apply Postgres parameters dynamically |

---

## 1. Replica Support

### Current State

Toygres currently manages single-instance Postgres deployments with no replication.

### Proposed Features

**1.1 Add Replica to Instance:**

```rust
// API to add a replica
client.add_replica(AddReplicaRequest {
    instance_id: "pg-prod-1".into(),
    replica_id: "pg-prod-1-replica-1".into(),
    replica_config: ReplicaConfig {
        size: "small".into(),
        region: "us-east-1".into(),
        availability_zone: Some("us-east-1b".into()),
    },
}).await?;
```

**1.2 Replica Configuration:**

```rust
pub struct ReplicaConfig {
    /// Compute size for replica
    pub size: String,
    /// Region for replica (can differ from primary for geo-replication)
    pub region: String,
    /// Availability zone (for HA within region)
    pub availability_zone: Option<String>,
    /// Replication mode
    pub replication_mode: ReplicationMode,
    /// Max replication lag before alerts
    pub max_lag_bytes: Option<u64>,
}

pub enum ReplicationMode {
    /// Asynchronous streaming replication (default)
    Async,
    /// Synchronous replication (primary waits for replica confirmation)
    Sync,
}
```

**1.3 Orchestration Flow:**

```
AddReplica Orchestration
├── Provision replica VM/container
├── Configure replica Postgres
│   ├── Set up recovery.conf / standby.signal
│   ├── Configure primary_conninfo
│   └── Set replica identity
├── Take base backup from primary
├── Start replica in standby mode
├── Wait for initial sync
├── Register replica in instance state
└── Start replication lag monitoring
```

**1.4 List Replicas:**

```rust
let instance = client.get_instance("pg-prod-1").await?;
for replica in &instance.replicas {
    println!("{}: lag={}ms, state={:?}", 
        replica.id, 
        replica.replication_lag_ms,
        replica.state);
}
```

### Implementation Considerations

- Primary needs `wal_level = replica` and `max_wal_senders` configured
- Replica connection string stored securely
- Handle replica promotion (see Failover sections)
- Support removing replicas cleanly

---

## 2. Manual Failover

### Concept

Allow operators to manually trigger failover from primary to a designated replica. This is useful for:
- Planned maintenance on primary
- Testing disaster recovery procedures
- Upgrading primary with minimal downtime

### API

```rust
// Initiate manual failover
client.failover(FailoverRequest {
    instance_id: "pg-prod-1".into(),
    target_replica_id: "pg-prod-1-replica-1".into(),
    options: FailoverOptions {
        /// Wait for replica to catch up before failover
        wait_for_sync: true,
        /// Max time to wait for sync
        sync_timeout: Duration::from_secs(300),
        /// What to do with old primary after failover
        old_primary_action: OldPrimaryAction::ConvertToReplica,
    },
}).await?;

pub enum OldPrimaryAction {
    /// Convert old primary to replica of new primary
    ConvertToReplica,
    /// Stop old primary but keep data
    StopAndRetain,
    /// Terminate old primary completely
    Terminate,
}
```

### Orchestration Flow

```
ManualFailover Orchestration
├── Validate target replica exists and is healthy
├── If wait_for_sync:
│   ├── Pause writes on primary (optional)
│   ├── Wait for replica to catch up to primary LSN
│   └── Timeout if sync takes too long
├── Fence primary (prevent writes)
├── Promote replica to primary
│   ├── Create trigger file / promote command
│   ├── Wait for promotion complete
│   └── Update connection endpoint
├── Handle old primary per OldPrimaryAction
│   ├── ConvertToReplica: reconfigure as standby
│   ├── StopAndRetain: shutdown gracefully
│   └── Terminate: destroy instance
├── Update DNS / connection routing
├── Notify connected clients (if possible)
└── Update instance metadata (new primary, replica list)
```

### Failover Status

```rust
pub struct FailoverStatus {
    pub state: FailoverState,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub old_primary_id: String,
    pub new_primary_id: String,
    pub replication_lag_at_start: u64,
    pub data_loss_bytes: Option<u64>,  // If any
}

pub enum FailoverState {
    WaitingForSync,
    FencingPrimary,
    PromotingReplica,
    UpdatingRouting,
    CleaningUpOldPrimary,
    Completed,
    Failed { reason: String },
}
```

---

## 3. Automatic Failover Monitoring

### Concept

The instance actor (long-running orchestration managing instance lifecycle) monitors primary health and automatically triggers failover when issues are detected.

### Health Monitoring

```rust
pub struct HealthMonitorConfig {
    /// Interval between health checks
    pub check_interval: Duration,
    /// Number of consecutive failures before failover
    pub failure_threshold: u32,
    /// Timeout for each health check
    pub check_timeout: Duration,
    /// Minimum replica lag for automatic failover eligibility
    pub max_eligible_lag_bytes: u64,
}

pub struct HealthCheck {
    /// Can connect to Postgres
    pub connectivity: bool,
    /// Postgres is accepting queries
    pub query_responsive: bool,
    /// Replication is active (if replicas exist)
    pub replication_healthy: bool,
    /// Disk space OK
    pub disk_space_ok: bool,
    /// Custom health query passes
    pub custom_check_ok: bool,
}
```

### Instance Actor Integration

The instance actor uses durable timers and activities for monitoring:

```rust
async fn instance_actor(ctx: OrchestrationContext) -> Result<(), String> {
    let state: InstanceState = ctx.get_state()?;
    
    loop {
        // Wait for next check interval
        ctx.schedule_timer(state.health_config.check_interval).await;
        
        // Perform health check
        let health = ctx.schedule_activity("check_primary_health", &state.primary_id)
            .await?;
        
        if !health.is_healthy() {
            state.consecutive_failures += 1;
            
            if state.consecutive_failures >= state.health_config.failure_threshold {
                // Find best replica for failover
                if let Some(target) = select_failover_target(&state).await? {
                    // Trigger automatic failover
                    ctx.schedule_sub_orchestration(
                        "automatic_failover",
                        AutoFailoverInput {
                            instance_id: state.instance_id.clone(),
                            target_replica_id: target.id,
                            reason: "Health check failures exceeded threshold".into(),
                        },
                    ).await?;
                } else {
                    // No eligible replica - alert only
                    ctx.schedule_activity("send_alert", AlertInput {
                        severity: AlertSeverity::Critical,
                        message: "Primary unhealthy but no replica eligible for failover",
                    }).await?;
                }
            }
        } else {
            state.consecutive_failures = 0;
        }
        
        // Continue-as-new periodically to manage history size
        if ctx.should_continue_as_new() {
            ctx.continue_as_new(state)?;
        }
    }
}
```

### Failover Target Selection

```rust
fn select_failover_target(state: &InstanceState) -> Option<&Replica> {
    state.replicas
        .iter()
        .filter(|r| r.state == ReplicaState::Streaming)
        .filter(|r| r.replication_lag_bytes <= state.health_config.max_eligible_lag_bytes)
        .min_by_key(|r| r.replication_lag_bytes)
}
```

### Automatic Failover Safeguards

- **Cooldown period**: Minimum time between automatic failovers
- **Manual override**: Ability to disable automatic failover temporarily
- **Quorum check**: Ensure network partition isn't causing false positives
- **Notification**: Alert on-call before/during automatic failover
- **Audit log**: Record all automatic failover decisions and outcomes

---

## 4. Backup & Restore

### Backup Types

```rust
pub enum BackupType {
    /// Full base backup (pg_basebackup)
    Full,
    /// Incremental using WAL archiving
    Incremental,
    /// Logical backup (pg_dump)
    Logical { databases: Vec<String> },
}

pub struct BackupConfig {
    /// Where to store backups
    pub storage: BackupStorage,
    /// Retention policy
    pub retention: RetentionPolicy,
    /// Encryption settings
    pub encryption: Option<EncryptionConfig>,
    /// Compression level
    pub compression: CompressionLevel,
}

pub enum BackupStorage {
    AzureBlob { container: String, connection_string: String },
    S3 { bucket: String, region: String },
    Local { path: String },
}

pub struct RetentionPolicy {
    /// Keep daily backups for N days
    pub daily_retention_days: u32,
    /// Keep weekly backups for N weeks
    pub weekly_retention_weeks: u32,
    /// Keep monthly backups for N months
    pub monthly_retention_months: u32,
}
```

### Backup API

```rust
// Trigger manual backup
let backup = client.create_backup(CreateBackupRequest {
    instance_id: "pg-prod-1".into(),
    backup_type: BackupType::Full,
    label: Some("pre-migration-backup".into()),
}).await?;

// List backups
let backups = client.list_backups("pg-prod-1").await?;

// Schedule automatic backups
client.configure_backup_schedule(BackupScheduleConfig {
    instance_id: "pg-prod-1".into(),
    full_backup_cron: "0 2 * * 0".into(),  // Weekly Sunday 2am
    incremental_backup_cron: "0 2 * * *".into(),  // Daily 2am
    config: BackupConfig { .. },
}).await?;
```

### Restore API

```rust
// Restore to new instance
let restored = client.restore_backup(RestoreRequest {
    backup_id: "backup-123".into(),
    target: RestoreTarget::NewInstance {
        instance_id: "pg-prod-1-restored".into(),
        config: InstanceConfig { .. },
    },
    point_in_time: Some(datetime!(2024-01-15 14:30:00 UTC)),  // PITR
}).await?;

// Restore to existing instance (destructive)
client.restore_backup(RestoreRequest {
    backup_id: "backup-123".into(),
    target: RestoreTarget::ExistingInstance {
        instance_id: "pg-dev-1".into(),
        confirm_destructive: true,
    },
    point_in_time: None,  // Restore to backup time
}).await?;
```

### Backup Orchestration

```
CreateBackup Orchestration
├── Validate instance exists and is healthy
├── If Full backup:
│   ├── Start pg_basebackup on replica (preferred) or primary
│   ├── Stream to backup storage
│   └── Record backup metadata
├── If Incremental:
│   ├── Archive WAL segments since last backup
│   └── Update backup chain metadata
├── If Logical:
│   ├── Run pg_dump for specified databases
│   └── Stream to backup storage
├── Verify backup integrity
├── Apply retention policy (delete old backups)
└── Update backup catalog
```

### Point-in-Time Recovery (PITR)

- Continuous WAL archiving to backup storage
- Restore base backup + replay WAL to target timestamp
- Recovery target options: timestamp, transaction ID, named restore point

---

## 5. Postgres Parameters

### Concept

Allow users to configure Postgres parameters and apply them dynamically or with restart.

### Parameter Categories

```rust
pub enum ParameterCategory {
    /// Can be changed without restart (SET command)
    Dynamic,
    /// Requires reload (pg_reload_conf)
    Reload,
    /// Requires restart
    Restart,
    /// Cannot be changed after init
    Immutable,
}

pub struct ParameterDefinition {
    pub name: String,
    pub category: ParameterCategory,
    pub data_type: ParameterType,
    pub default_value: String,
    pub min_value: Option<String>,
    pub max_value: Option<String>,
    pub description: String,
}
```

### API

```rust
// Set parameters
client.set_parameters(SetParametersRequest {
    instance_id: "pg-prod-1".into(),
    parameters: vec![
        ("shared_buffers".into(), "4GB".into()),
        ("work_mem".into(), "256MB".into()),
        ("max_connections".into(), "200".into()),
        ("log_statement".into(), "all".into()),
    ],
    apply_mode: ApplyMode::Immediate,  // or Scheduled { at: datetime }
}).await?;

pub enum ApplyMode {
    /// Apply immediately (may restart if needed)
    Immediate,
    /// Apply at scheduled time
    Scheduled { at: DateTime<Utc> },
    /// Apply on next restart
    OnNextRestart,
}

// Get current parameters
let params = client.get_parameters("pg-prod-1").await?;
for (name, value, pending) in params {
    if let Some(pending_value) = pending {
        println!("{}: {} (pending: {})", name, value, pending_value);
    } else {
        println!("{}: {}", name, value);
    }
}

// Get parameter definition
let def = client.describe_parameter("shared_buffers").await?;
println!("{}: {} ({:?})", def.name, def.description, def.category);
```

### Apply Parameters Orchestration

```
SetParameters Orchestration
├── Validate parameter names and values
├── Classify parameters by category
├── For Dynamic parameters:
│   └── Execute SET commands via SQL
├── For Reload parameters:
│   ├── Update postgresql.conf
│   └── Execute pg_reload_conf()
├── For Restart parameters:
│   ├── Update postgresql.conf
│   ├── Schedule restart (if Immediate mode)
│   │   ├── Graceful connection draining
│   │   ├── Stop Postgres
│   │   ├── Start Postgres
│   │   └── Verify parameters applied
│   └── Or mark as pending (if OnNextRestart mode)
├── Replicate parameter changes to replicas
└── Update instance state with current/pending params
```

### Parameter Profiles

Pre-defined parameter profiles for common workloads:

```rust
pub enum ParameterProfile {
    /// Optimized for OLTP workloads
    Oltp,
    /// Optimized for analytics/OLAP
    Analytics,
    /// Optimized for mixed workloads
    GeneralPurpose,
    /// Minimal resources for development
    Development,
    /// Custom profile
    Custom { parameters: HashMap<String, String> },
}

// Apply a profile
client.apply_parameter_profile(ApplyProfileRequest {
    instance_id: "pg-prod-1".into(),
    profile: ParameterProfile::Oltp,
    override_params: vec![
        ("max_connections".into(), "500".into()),  // Override specific values
    ],
}).await?;
```

---

## Open Questions

1. **Replicas**: Should replicas be promotable by default, or require explicit configuration?
2. **Failover**: How to handle in-flight transactions during failover?
3. **Automatic Failover**: What quorum/witness mechanism prevents split-brain?
4. **Backup**: Should logical backups be schema-only by default for large databases?
5. **Parameters**: How to validate parameter combinations that may conflict?

