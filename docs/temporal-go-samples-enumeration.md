# Temporal Go Samples Enumeration

## Repository
**Source**: https://github.com/temporalio/samples-go

## Sample Workflows

### 1. **Hello World** (`hello`)
**Location**: `hello/`
**Description**: Basic introduction to Temporal workflows
**Features**:
- Simple workflow that calls a single activity
- Demonstrates basic workflow and activity registration
- Shows workflow execution and result retrieval

**Key Concepts**:
- Workflow definition
- Activity invocation
- Worker setup
- Client usage

**Portability to Duroxide**: ✅ High - Simple pattern, direct mapping

---

### 2. **Money Transfer** (`money`)
**Location**: `money/`
**Description**: Demonstrates saga pattern with compensation
**Features**:
- Multi-step transaction workflow
- Compensation logic for rollback
- Error handling and retries
- Activity failures trigger compensation

**Key Concepts**:
- Saga pattern
- Compensation activities
- Error handling
- Transaction rollback

**Portability to Duroxide**: ✅ High - Saga pattern well-supported

---

### 3. **Expense Report** (`expense`)
**Location**: `expense/`
**Description**: Human-in-the-loop workflow with external events
**Features**:
- Workflow waits for approval signals
- External event handling
- Timeout handling for approvals
- State management across events

**Key Concepts**:
- Signals (external events)
- Timeouts
- Human-in-the-loop patterns
- State persistence

**Portability to Duroxide**: ⚠️ Medium - Requires `schedule_wait().into_event()` for external events

---

### 4. **Cron** (`cron`)
**Location**: `cron/`
**Description**: Scheduled/recurring workflow execution
**Features**:
- Cron-style scheduling
- Recurring workflow execution
- Continue-as-new pattern
- Schedule management

**Key Concepts**:
- Scheduled workflows
- Continue-as-new
- Recurring patterns
- Schedule expressions

**Portability to Duroxide**: ⚠️ Medium - Requires external scheduler, continue-as-new supported

---

### 5. **File Processing** (`fileprocessing`)
**Location**: `fileprocessing/`
**Description**: Fan-out/fan-in pattern for parallel processing
**Features**:
- Process multiple files in parallel
- Aggregate results
- Error handling per file
- Progress tracking

**Key Concepts**:
- Fan-out/fan-in
- Parallel activity execution
- Result aggregation
- Partial failure handling

**Portability to Duroxide**: ✅ High - `ctx.join()` and `ctx.select()` support parallel execution

---

### 6. **Subscription** (`subscription`)
**Location**: `subscription/`
**Description**: Long-running subscription management workflow
**Features**:
- Subscription lifecycle management
- Periodic billing
- Renewal handling
- Cancellation support

**Key Concepts**:
- Long-running workflows
- Periodic activities
- State management
- Lifecycle events

**Portability to Duroxide**: ✅ High - Long-running workflows well-supported

---

### 7. **Booking** (`booking`)
**Location**: `booking/`
**Description**: Multi-step reservation workflow
**Features**:
- Sequential activity execution
- Resource reservation
- Confirmation handling
- Cancellation support

**Key Concepts**:
- Sequential workflows
- Resource management
- Confirmation patterns
- State transitions

**Portability to Duroxide**: ✅ High - Sequential patterns well-supported

---

### 8. **Polling** (`polling`)
**Location**: `polling/`
**Description**: Polling pattern for external service status
**Features**:
- Periodic polling of external service
- Conditional completion
- Timeout handling
- Status checking

**Key Concepts**:
- Polling patterns
- Conditional loops
- Timeouts
- External service integration

**Portability to Duroxide**: ✅ High - Timers and loops well-supported

---

### 9. **Child Workflow** (`child`)
**Location**: `child/`
**Description**: Parent-child workflow relationships
**Features**:
- Parent workflow spawns child workflows
- Child workflow execution
- Result aggregation from children
- Error handling in child workflows

**Key Concepts**:
- Sub-workflows
- Parent-child relationships
- Result aggregation
- Error propagation

**Portability to Duroxide**: ✅ High - `schedule_sub_orchestration()` supports this

---

### 10. **Query** (`query`)
**Location**: `query/`
**Description**: Query workflow state without modification
**Features**:
- Query workflow state
- Read-only state access
- State inspection
- Monitoring support

**Key Concepts**:
- Queries
- State inspection
- Read-only access
- Monitoring

**Portability to Duroxide**: ⚠️ Medium - Requires client-side history reading, no direct query API

---

### 11. **Signal** (`signal`)
**Location**: `signal/`
**Description**: External event signaling to workflows
**Features**:
- Send signals to running workflows
- Handle signals in workflow
- Signal-based state updates
- Dynamic workflow control

**Key Concepts**:
- Signals
- External events
- Dynamic control
- State updates

**Portability to Duroxide**: ✅ High - `schedule_wait().into_event()` supports external events

---

### 12. **Timer** (`timer`)
**Location**: `timer/`
**Description**: Timer and delay patterns
**Features**:
- Workflow delays
- Timer creation
- Timer cancellation
- Timeout handling

**Key Concepts**:
- Timers
- Delays
- Timeouts
- Cancellation

**Portability to Duroxide**: ✅ High - `schedule_timer()` fully supported

---

### 13. **Retry** (`retry`)
**Location**: `retry/`
**Description**: Activity retry patterns and policies
**Features**:
- Retry policies
- Exponential backoff
- Maximum attempts
- Error handling

**Key Concepts**:
- Retry policies
- Backoff strategies
- Error handling
- Resilience patterns

**Portability to Duroxide**: ⚠️ Medium - Duroxide has fixed retry logic, may need workarounds

---

### 14. **Search Attributes** (`searchattributes`)
**Location**: `searchattributes/`
**Description**: Workflow search and filtering
**Features**:
- Searchable workflow attributes
- Filtering workflows
- Metadata management
- Visibility features

**Key Concepts**:
- Search attributes
- Filtering
- Metadata
- Visibility

**Portability to Duroxide**: ⚠️ Low - Duroxide doesn't have search attributes, would need custom implementation

---

### 15. **Update** (`update`)
**Location**: `update/`
**Description**: Update running workflows
**Features**:
- Update workflow state
- Handle update requests
- State modification
- Dynamic updates

**Key Concepts**:
- Workflow updates
- State modification
- Dynamic changes
- Update handlers

**Portability to Duroxide**: ⚠️ Low - Duroxide doesn't support workflow updates, would need continue-as-new workaround

---

## Summary by Portability

### ✅ High Portability (Direct Mapping)
- Hello World
- Money Transfer (Saga)
- File Processing (Fan-out/Fan-in)
- Subscription
- Booking
- Polling
- Child Workflow
- Signal
- Timer

### ⚠️ Medium Portability (Workarounds Needed)
- Expense Report (External events via `schedule_wait`)
- Cron (External scheduler needed)
- Query (Client-side history reading)
- Retry (Fixed retry policies)

### ❌ Low Portability (Significant Changes or Not Supported)
- Search Attributes (Not supported)
- Update (Not supported, would need continue-as-new)

## Recommended Samples for Scenario Tests

Based on complexity and portability:

1. **Hello World** - Simplest, good for basic patterns
2. **Money Transfer** - Saga pattern, important pattern
3. **File Processing** - Fan-out/fan-in, common pattern
4. **Polling** - Practical pattern, well-supported
5. **Child Workflow** - Sub-orchestration pattern
6. **Timer** - Basic but fundamental pattern
7. **Signal** - External events, important pattern

## Notes

- Most Temporal samples use Go-specific patterns that translate well to Rust
- Activity registration differs (Temporal registers separately, Duroxide uses Registry)
- Error handling patterns are similar (Go error → Rust Result)
- State management is similar (both use deterministic replay)
- External events require `schedule_wait()` in Duroxide vs Signals in Temporal

