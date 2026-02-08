//! Capability filtering provider validation tests.
//!
//! These tests validate that providers correctly implement the capability filtering
//! contract: filtering orchestration items by pinned duroxide version, storing pinned
//! versions via ExecutionMetadata, and correctly handling deserialization errors.
//!
//! See `docs/proposals/provider-capability-filtering.md` test plan categories A, B, F, F2, I.

use super::ProviderFactory;
use crate::providers::{
    DispatcherCapabilityFilter, ExecutionMetadata, SemverRange, SemverVersion, WorkItem,
};
use crate::{Event, EventKind, INITIAL_EVENT_ID, INITIAL_EXECUTION_ID};
use std::time::Duration;

const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// Create a StartOrchestration work item for an instance.
fn start_item(instance: &str) -> WorkItem {
    WorkItem::StartOrchestration {
        instance: instance.to_string(),
        orchestration: "TestOrch".to_string(),
        input: "{}".to_string(),
        version: Some("1.0.0".to_string()),
        parent_instance: None,
        parent_id: None,
        execution_id: INITIAL_EXECUTION_ID,
    }
}

/// Create an OrchestrationStarted event with a specific duroxide_version.
fn orchestration_started_event(instance: &str, duroxide_version: &str) -> Event {
    let mut event = Event::with_event_id(
        INITIAL_EVENT_ID,
        instance,
        INITIAL_EXECUTION_ID,
        None,
        EventKind::OrchestrationStarted {
            name: "TestOrch".to_string(),
            version: "1.0.0".to_string(),
            input: "{}".to_string(),
            parent_instance: None,
            parent_id: None,
        },
    );
    event.duroxide_version = duroxide_version.to_string();
    event
}

/// Build a filter for the given inclusive range [min, max].
fn filter_for_range(min: SemverVersion, max: SemverVersion) -> DispatcherCapabilityFilter {
    DispatcherCapabilityFilter {
        supported_duroxide_versions: vec![SemverRange::new(min, max)],
    }
}

/// Seed an instance: enqueue start item, fetch, ack with the given pinned version
/// and immediately enqueue follow-up work in the same ack (via orchestrator_items).
/// This avoids race conditions when seeding multiple instances on the same provider.
async fn seed_instance_with_version(
    provider: &dyn crate::providers::Provider,
    instance: &str,
    pinned_version: SemverVersion,
) {
    // Enqueue start item
    provider
        .enqueue_for_orchestrator(start_item(instance), None)
        .await
        .unwrap();

    // Fetch — since we just enqueued and no other items exist for THIS instance,
    // we may still get a different instance's item. Use a targeted approach:
    // fetch everything, ack ours, abandon others.
    let lock_token;
    let mut abandoned_tokens = Vec::new();
    loop {
        let (item, token, _) = provider
            .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, None)
            .await
            .unwrap()
            .expect("Should have work available");
        if item.instance == instance {
            lock_token = token;
            break;
        }
        // Not our instance — remember it for later abandon
        abandoned_tokens.push(token);
    }

    let version_str = pinned_version.to_string();

    // Ack with pinned version AND enqueue follow-up work atomically
    let follow_up = WorkItem::ExternalRaised {
        instance: instance.to_string(),
        name: "ping".to_string(),
        data: "{}".to_string(),
    };
    provider
        .ack_orchestration_item(
            &lock_token,
            INITIAL_EXECUTION_ID,
            vec![orchestration_started_event(instance, &version_str)],
            vec![],
            vec![follow_up],
            ExecutionMetadata {
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
                pinned_duroxide_version: Some(pinned_version),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // Release any locks we acquired on other instances
    for token in abandoned_tokens {
        let _ = provider
            .abandon_orchestration_item(&token, None, true)
            .await;
    }
}

// ---------------------------------------------------------------------------
// Category A: Provider validation tests
// ---------------------------------------------------------------------------

/// Test #1: fetch_with_filter_none_returns_any_item
pub async fn test_fetch_with_filter_none_returns_any_item<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;
    let v = SemverVersion::new(1, 2, 3);
    seed_instance_with_version(&*provider, "inst-1", v).await;

    // Fetch with filter=None → should return the item (legacy behavior)
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, None)
        .await
        .unwrap();
    assert!(result.is_some(), "filter=None should return any item");
}

/// Test #2: fetch_with_compatible_filter_returns_item
pub async fn test_fetch_with_compatible_filter_returns_item<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;
    let v = SemverVersion::new(1, 2, 3);
    seed_instance_with_version(&*provider, "inst-2", v).await;

    let filter = filter_for_range(SemverVersion::new(1, 0, 0), SemverVersion::new(1, 9, 9));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(result.is_some(), "Compatible filter should return item");
}

/// Test #3: fetch_with_incompatible_filter_skips_item
pub async fn test_fetch_with_incompatible_filter_skips_item<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;
    let v = SemverVersion::new(1, 2, 3);
    seed_instance_with_version(&*provider, "inst-3", v).await;

    let filter = filter_for_range(SemverVersion::new(2, 0, 0), SemverVersion::new(2, 9, 9));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(result.is_none(), "Incompatible filter should return None");
}

/// Test #4: fetch_filter_skips_incompatible_selects_compatible
pub async fn test_fetch_filter_skips_incompatible_selects_compatible<F: ProviderFactory>(
    factory: &F,
) {
    let provider = factory.create_provider().await;
    seed_instance_with_version(&*provider, "inst-v1", SemverVersion::new(1, 0, 0)).await;
    seed_instance_with_version(&*provider, "inst-v2", SemverVersion::new(2, 0, 0)).await;

    // Filter for v2 only
    let filter_v2 = filter_for_range(SemverVersion::new(2, 0, 0), SemverVersion::new(2, 9, 9));
    let (item, lock_token, _) = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter_v2))
        .await
        .unwrap()
        .expect("Should return v2 instance");
    assert_eq!(item.instance, "inst-v2");
    provider
        .abandon_orchestration_item(&lock_token, None, true)
        .await
        .unwrap();

    // Filter for v1 only
    let filter_v1 = filter_for_range(SemverVersion::new(1, 0, 0), SemverVersion::new(1, 9, 9));
    let (item, _, _) = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter_v1))
        .await
        .unwrap()
        .expect("Should return v1 instance");
    assert_eq!(item.instance, "inst-v1");
}

/// Test #5: fetch_filter_does_not_lock_skipped_instances
pub async fn test_fetch_filter_does_not_lock_skipped_instances<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;
    seed_instance_with_version(&*provider, "inst-5", SemverVersion::new(1, 0, 0)).await;

    // Fetch with incompatible filter → None
    let incompatible =
        filter_for_range(SemverVersion::new(2, 0, 0), SemverVersion::new(2, 9, 9));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&incompatible))
        .await
        .unwrap();
    assert!(result.is_none());

    // Fetch with compatible filter → should still be available (not locked)
    let compatible = filter_for_range(SemverVersion::new(0, 0, 0), SemverVersion::new(1, 9, 9));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&compatible))
        .await
        .unwrap();
    assert!(
        result.is_some(),
        "Instance should not have been locked by incompatible fetch"
    );
}

/// Test #6: fetch_filter_null_pinned_version_always_compatible
pub async fn test_fetch_filter_null_pinned_version_always_compatible<F: ProviderFactory>(
    factory: &F,
) {
    let provider = factory.create_provider().await;

    // Create an instance WITHOUT setting pinned version (simulates pre-migration data)
    provider
        .enqueue_for_orchestrator(start_item("inst-null"), None)
        .await
        .unwrap();

    let (_item, lock_token, _) = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, None)
        .await
        .unwrap()
        .unwrap();

    // Ack without pinned version → NULL columns
    provider
        .ack_orchestration_item(
            &lock_token,
            INITIAL_EXECUTION_ID,
            vec![orchestration_started_event("inst-null", "0.0.0")],
            vec![],
            vec![],
            ExecutionMetadata {
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
                pinned_duroxide_version: None, // No pinned version
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // Enqueue work
    provider
        .enqueue_for_orchestrator(
            WorkItem::ExternalRaised {
                instance: "inst-null".to_string(),
                name: "ping".to_string(),
                data: "{}".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Fetch with any filter → should return (NULL = always compatible)
    let filter = filter_for_range(SemverVersion::new(99, 0, 0), SemverVersion::new(99, 9, 9));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(
        result.is_some(),
        "NULL pinned version should be always compatible"
    );
}

/// Test #7: fetch_filter_boundary_versions
pub async fn test_fetch_filter_boundary_versions<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;
    let filter = filter_for_range(SemverVersion::new(1, 0, 0), SemverVersion::new(1, 9, 99));

    // Test lower bound: 1.0.0 should be included
    seed_instance_with_version(&*provider, "v1-low", SemverVersion::new(1, 0, 0)).await;
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(result.is_some(), "v1.0.0 should be within [1.0.0, 1.9.99]");
    let (item, lock, _) = result.unwrap();
    assert_eq!(item.instance, "v1-low");
    provider.abandon_orchestration_item(&lock, None, true).await.unwrap();

    // Test upper bound: 1.9.99 should be included
    seed_instance_with_version(&*provider, "v1-high", SemverVersion::new(1, 9, 99)).await;
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(result.is_some(), "v1.9.99 should be within [1.0.0, 1.9.99]");
    let (_, lock, _) = result.unwrap();
    provider.abandon_orchestration_item(&lock, None, true).await.unwrap();

    // Test just outside: 2.0.0 should be excluded.
    // Use a separate provider to avoid interference from the v1 instances above.
    let provider2 = factory.create_provider().await;
    seed_instance_with_version(&*provider2, "v2-exact", SemverVersion::new(2, 0, 0)).await;

    let result = provider2
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(result.is_none(), "v2.0.0 should NOT be within [1.0.0, 1.9.99]");

    // Verify v2-exact IS fetchable without filter
    let result = provider2
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, None)
        .await
        .unwrap();
    assert!(result.is_some(), "v2-exact should be fetchable without filter");
}

/// Test #8: pinned_version_stored_via_ack_metadata
pub async fn test_pinned_version_stored_via_ack_metadata<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;
    let v = SemverVersion::new(3, 1, 4);
    seed_instance_with_version(&*provider, "inst-8", v).await;

    // Fetch with matching filter → confirms version was stored from metadata
    let filter = filter_for_range(SemverVersion::new(3, 0, 0), SemverVersion::new(3, 9, 9));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(
        result.is_some(),
        "Version stored via metadata should be filterable"
    );
}

/// Test #9: pinned_version_immutable_across_ack_cycles
pub async fn test_pinned_version_immutable_across_ack_cycles<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;
    let v = SemverVersion::new(1, 0, 0);
    seed_instance_with_version(&*provider, "inst-9", v).await;

    // Fetch and ack again (second turn, no pinned version in metadata)
    let filter = filter_for_range(SemverVersion::new(0, 0, 0), SemverVersion::new(1, 9, 9));
    let (item, lock_token, _) = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.instance, "inst-9");

    // Ack without pinned_duroxide_version (second turn)
    provider
        .ack_orchestration_item(
            &lock_token,
            INITIAL_EXECUTION_ID,
            vec![],
            vec![],
            vec![],
            ExecutionMetadata {
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // Enqueue more work
    provider
        .enqueue_for_orchestrator(
            WorkItem::ExternalRaised {
                instance: "inst-9".to_string(),
                name: "ping2".to_string(),
                data: "{}".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Still fetchable with same filter
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(
        result.is_some(),
        "Pinned version should persist across ack cycles"
    );
}

// ---------------------------------------------------------------------------
// Category B: ContinueAsNew execution isolation
// ---------------------------------------------------------------------------

/// Test #10 + #11: continue_as_new_execution_gets_own_pinned_version
///
/// After ContinueAsNew, the new execution's pinned version comes from the new
/// ExecutionMetadata, NOT inherited from the previous execution.
/// Verifies: v2 filter matches execution 2, v1 filter does NOT.
pub async fn test_continue_as_new_execution_gets_own_pinned_version<F: ProviderFactory>(
    factory: &F,
) {
    let provider = factory.create_provider().await;

    // Seed instance with execution 1 pinned at 1.0.0
    seed_instance_with_version(&*provider, "inst-can", SemverVersion::new(1, 0, 0)).await;

    // Fetch and ack as ContinuedAsNew → creates execution 2 pinned at 2.0.0
    let filter_v1 = filter_for_range(SemverVersion::new(0, 0, 0), SemverVersion::new(1, 9, 9));
    let (_item, lock_token, _) = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter_v1))
        .await
        .unwrap()
        .unwrap();

    let execution_2 = INITIAL_EXECUTION_ID + 1;
    provider
        .ack_orchestration_item(
            &lock_token,
            INITIAL_EXECUTION_ID,
            vec![Event::with_event_id(
                2,
                "inst-can",
                INITIAL_EXECUTION_ID,
                None,
                EventKind::OrchestrationContinuedAsNew {
                    input: "{}".to_string(),
                },
            )],
            vec![],
            vec![WorkItem::ContinueAsNew {
                instance: "inst-can".to_string(),
                orchestration: "TestOrch".to_string(),
                input: "{}".to_string(),
                version: Some("1.0.0".to_string()),
            }],
            ExecutionMetadata {
                status: Some("ContinuedAsNew".to_string()),
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // Now fetch and ack the ContinueAsNew to create execution 2 with pinned v2.0.0
    let (_item, lock_token2, _) = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, None)
        .await
        .unwrap()
        .unwrap();

    let mut started_event = orchestration_started_event("inst-can", "2.0.0");
    started_event.execution_id = execution_2;

    provider
        .ack_orchestration_item(
            &lock_token2,
            execution_2,
            vec![started_event],
            vec![],
            vec![],
            ExecutionMetadata {
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
                pinned_duroxide_version: Some(SemverVersion::new(2, 0, 0)),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // Enqueue work for the new execution
    provider
        .enqueue_for_orchestrator(
            WorkItem::ExternalRaised {
                instance: "inst-can".to_string(),
                name: "ping".to_string(),
                data: "{}".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Fetch with v2 filter → should return (uses execution 2's pinned version)
    let filter_v2 = filter_for_range(SemverVersion::new(2, 0, 0), SemverVersion::new(2, 9, 9));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter_v2))
        .await
        .unwrap();
    assert!(result.is_some(), "Should return item with v2 filter after ContinueAsNew");

    let (item, lock_token3, _) = result.unwrap();
    assert_eq!(item.instance, "inst-can");
    provider
        .abandon_orchestration_item(&lock_token3, None, true)
        .await
        .unwrap();

    // Fetch with v1 filter → should NOT return (execution 2 is pinned at 2.0.0,
    // proving the old v1.0.0 pinned version was NOT inherited)
    let filter_v1_only =
        filter_for_range(SemverVersion::new(1, 0, 0), SemverVersion::new(1, 9, 9));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter_v1_only))
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "v1.x filter must NOT match execution 2 — pinned version should be 2.0.0, not inherited 1.0.0"
    );
}

// ---------------------------------------------------------------------------
// Category F: Edge cases and error handling
// ---------------------------------------------------------------------------

/// Test #22: filter_with_empty_supported_versions_returns_nothing
pub async fn test_filter_with_empty_supported_versions_returns_nothing<F: ProviderFactory>(
    factory: &F,
) {
    let provider = factory.create_provider().await;
    seed_instance_with_version(&*provider, "inst-empty", SemverVersion::new(1, 0, 0)).await;

    let filter = DispatcherCapabilityFilter {
        supported_duroxide_versions: vec![],
    };
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "Empty supported versions should return None"
    );
}

/// Test #23: concurrent_filtered_fetch_no_double_lock
pub async fn test_concurrent_filtered_fetch_no_double_lock<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;
    seed_instance_with_version(&*provider, "inst-conc", SemverVersion::new(1, 0, 0)).await;

    let filter = filter_for_range(SemverVersion::new(0, 0, 0), SemverVersion::new(1, 9, 9));

    // First fetch should succeed
    let result1 = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(result1.is_some(), "First fetch should succeed");

    // Second fetch with same filter should return None (instance locked)
    let result2 = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(
        result2.is_none(),
        "Second fetch should return None (instance locked)"
    );
}

// ---------------------------------------------------------------------------
// Category F2: Additional provider contract validation tests
// ---------------------------------------------------------------------------

/// Test #45: ack_stores_pinned_version_via_metadata_update
pub async fn test_ack_stores_pinned_version_via_metadata_update<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;

    // Create an instance WITHOUT pinned version
    provider
        .enqueue_for_orchestrator(start_item("inst-backfill"), None)
        .await
        .unwrap();
    let (_item, lock_token, _) = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, None)
        .await
        .unwrap()
        .unwrap();

    // Ack without pinned version first (simulates pre-migration)
    provider
        .ack_orchestration_item(
            &lock_token,
            INITIAL_EXECUTION_ID,
            vec![orchestration_started_event("inst-backfill", "1.2.3")],
            vec![],
            vec![],
            ExecutionMetadata {
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
                pinned_duroxide_version: None,
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // Enqueue more work
    provider
        .enqueue_for_orchestrator(
            WorkItem::ExternalRaised {
                instance: "inst-backfill".to_string(),
                name: "ping".to_string(),
                data: "{}".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Fetch and ack WITH pinned version (backfill)
    let (_item, lock_token2, _) = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, None)
        .await
        .unwrap()
        .unwrap();
    provider
        .ack_orchestration_item(
            &lock_token2,
            INITIAL_EXECUTION_ID,
            vec![],
            vec![],
            vec![],
            ExecutionMetadata {
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
                pinned_duroxide_version: Some(SemverVersion::new(1, 2, 3)),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // Enqueue more work
    provider
        .enqueue_for_orchestrator(
            WorkItem::ExternalRaised {
                instance: "inst-backfill".to_string(),
                name: "ping2".to_string(),
                data: "{}".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Fetch with matching filter → should work now
    let filter = filter_for_range(SemverVersion::new(1, 0, 0), SemverVersion::new(1, 9, 9));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(
        result.is_some(),
        "Backfilled pinned version should be filterable"
    );
}

/// Test #46: provider_updates_pinned_version_when_told
///
/// The provider unconditionally updates the pinned version when `Some(v)` is provided
/// in `ExecutionMetadata`. Write-once semantics are enforced by the runtime (via
/// `debug_assert`), not the provider. This test validates the provider stores whatever
/// the runtime tells it.
pub async fn test_provider_updates_pinned_version_when_told<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;
    // Create with pinned version 1.0.0
    seed_instance_with_version(&*provider, "inst-update", SemverVersion::new(1, 0, 0)).await;

    // Fetch and ack with a DIFFERENT pinned version — provider should accept it
    let filter_v1 = filter_for_range(SemverVersion::new(0, 0, 0), SemverVersion::new(1, 9, 9));
    let (_item, lock_token, _) = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter_v1))
        .await
        .unwrap()
        .unwrap();

    provider
        .ack_orchestration_item(
            &lock_token,
            INITIAL_EXECUTION_ID,
            vec![],
            vec![],
            vec![],
            ExecutionMetadata {
                orchestration_name: Some("TestOrch".to_string()),
                orchestration_version: Some("1.0.0".to_string()),
                pinned_duroxide_version: Some(SemverVersion::new(2, 0, 0)),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // Enqueue more work
    provider
        .enqueue_for_orchestrator(
            WorkItem::ExternalRaised {
                instance: "inst-update".to_string(),
                name: "ping2".to_string(),
                data: "{}".to_string(),
            },
            None,
        )
        .await
        .unwrap();

    // Should now be fetchable with v2 filter (version was updated)
    let filter_v2 = filter_for_range(SemverVersion::new(2, 0, 0), SemverVersion::new(2, 0, 0));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter_v2))
        .await
        .unwrap();
    assert!(
        result.is_some(),
        "Provider should have updated pinned version to 2.0.0"
    );

    let (_, lock_token2, _) = result.unwrap();
    provider
        .abandon_orchestration_item(&lock_token2, None, true)
        .await
        .unwrap();

    // Should NOT be fetchable with v1-only filter anymore
    let filter_v1_only =
        filter_for_range(SemverVersion::new(1, 0, 0), SemverVersion::new(1, 0, 0));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter_v1_only))
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "Should no longer match v1 filter — version was updated to v2"
    );
}

// ---------------------------------------------------------------------------
// Category I: Provider deserialization contract tests
//
// These tests require direct SQL access to inject corrupted history data.
// They accept a `&SqliteProvider` to access the underlying pool for corruption.
// ---------------------------------------------------------------------------

/// Helper: seed an instance with a pinned version, then corrupt its history data via SQL.
async fn seed_and_corrupt_history(
    provider: &dyn crate::providers::Provider,
    sqlite: &crate::providers::sqlite::SqliteProvider,
    instance: &str,
    pinned_version: SemverVersion,
) {
    seed_instance_with_version(provider, instance, pinned_version).await;

    // Corrupt the history by replacing event_data with invalid JSON
    sqlx::query("UPDATE history SET event_data = 'NOT_VALID_JSON{{{' WHERE instance_id = ?")
        .bind(instance)
        .execute(sqlite.get_pool())
        .await
        .expect("Failed to corrupt history");
}

/// Test #39: fetch_corrupted_history_filtered_vs_unfiltered
///
/// Part A: Fetch with a filter that excludes the corrupted instance → Ok(None).
///         The provider must not attempt deserialization for filtered-out items.
/// Part B: Fetch with filter=None → returns Err(permanent) due to corrupted history.
pub async fn test_fetch_corrupted_history_filtered_vs_unfiltered(
    provider: &dyn crate::providers::Provider,
    sqlite: &crate::providers::sqlite::SqliteProvider,
) {
    seed_and_corrupt_history(provider, sqlite, "inst-corrupt-39", SemverVersion::new(1, 0, 0))
        .await;

    // Part A: Filter excludes v1.0.0 → should return Ok(None), no deserialization attempted
    let excluding_filter =
        filter_for_range(SemverVersion::new(2, 0, 0), SemverVersion::new(2, 9, 9));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&excluding_filter))
        .await;
    assert!(
        result.is_ok(),
        "Filtered fetch should not produce an error for excluded items"
    );
    assert!(
        result.unwrap().is_none(),
        "Filtered fetch should return None for excluded version"
    );

    // Part B: filter=None → provider tries to deserialize → permanent error
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, None)
        .await;
    match result {
        Err(e) => {
            assert!(
                !e.is_retryable(),
                "Deserialization failure should be a permanent (non-retryable) error, got: {e}"
            );
        }
        Ok(Some(_)) => panic!("Should not successfully return item with corrupted history"),
        Ok(None) => panic!("Should return an error for corrupted history, not None"),
    }
}

/// Test #41: fetch_deserialization_error_increments_attempt_count
///
/// Corrupted history → fetch returns permanent error → lock expires → fetch again →
/// attempt_count increments across cycles.
pub async fn test_fetch_deserialization_error_increments_attempt_count(
    provider: &dyn crate::providers::Provider,
    sqlite: &crate::providers::sqlite::SqliteProvider,
) {
    seed_and_corrupt_history(
        provider,
        sqlite,
        "inst-deser-41",
        SemverVersion::new(1, 0, 0),
    )
    .await;

    // Use a very short lock timeout so we can re-fetch quickly
    let short_lock = Duration::from_millis(50);

    // First fetch → permanent error (deserialization failure)
    let result1 = provider
        .fetch_orchestration_item(short_lock, Duration::ZERO, None)
        .await;
    assert!(
        result1.is_err(),
        "First fetch should fail with deserialization error"
    );

    // Wait for lock to expire
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second fetch → permanent error again, attempt_count should have incremented
    let result2 = provider
        .fetch_orchestration_item(short_lock, Duration::ZERO, None)
        .await;
    assert!(
        result2.is_err(),
        "Second fetch should also fail with deserialization error"
    );

    // Verify attempt_count has incremented by checking the queue directly
    let max_attempt: i64 =
        sqlx::query_scalar("SELECT MAX(attempt_count) FROM orchestrator_queue WHERE instance_id = ?")
            .bind("inst-deser-41")
            .fetch_one(sqlite.get_pool())
            .await
            .expect("Should be able to query attempt_count");
    assert!(
        max_attempt >= 2,
        "attempt_count should be >= 2 after two fetch cycles, got {max_attempt}"
    );
}

/// Test #42: fetch_deserialization_error_eventually_reaches_poison
///
/// This is a provider-level test that validates the attempt_count keeps incrementing
/// for corrupted history items. The full poison termination pipeline is tested
/// at the runtime level (Category C).
/// 
/// TODO : Actual poisoning is not implemented yet, so this test just verifies attempt_count increments
pub async fn test_fetch_deserialization_error_eventually_reaches_poison(
    provider: &dyn crate::providers::Provider,
    sqlite: &crate::providers::sqlite::SqliteProvider,
) {
    seed_and_corrupt_history(
        provider,
        sqlite,
        "inst-poison-42",
        SemverVersion::new(1, 0, 0),
    )
    .await;

    let short_lock = Duration::from_millis(50);
    let max_attempts: u32 = 5;

    // Repeatedly fetch → error → wait for lock expiry → fetch again
    for i in 0..max_attempts {
        let result = provider
            .fetch_orchestration_item(short_lock, Duration::ZERO, None)
            .await;
        assert!(
            result.is_err(),
            "Fetch #{} should fail with deserialization error",
            i + 1
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Verify attempt_count has reached max_attempts
    let max_attempt: i64 =
        sqlx::query_scalar("SELECT MAX(attempt_count) FROM orchestrator_queue WHERE instance_id = ?")
            .bind("inst-poison-42")
            .fetch_one(sqlite.get_pool())
            .await
            .expect("Should be able to query attempt_count");
    assert!(
        max_attempt >= max_attempts as i64,
        "attempt_count should be >= {max_attempts} after {max_attempts} fetch cycles, got {max_attempt}"
    );
}

// ---------------------------------------------------------------------------
// Category F2: Additional provider contract edge cases
// ---------------------------------------------------------------------------

/// Test #43: fetch_filter_applied_before_history_deserialization
///
/// Seed an instance pinned at v99.0.0 with corrupted history. Fetch with a filter
/// that excludes v99.0.0 → Ok(None). This proves the filter was applied BEFORE
/// any history deserialization was attempted (otherwise we'd get a permanent error).
pub async fn test_fetch_filter_applied_before_history_deserialization(
    provider: &dyn crate::providers::Provider,
    sqlite: &crate::providers::sqlite::SqliteProvider,
) {
    seed_and_corrupt_history(
        provider,
        sqlite,
        "inst-order-43",
        SemverVersion::new(99, 0, 0),
    )
    .await;

    // Filter excludes v99.0.0
    let filter = filter_for_range(SemverVersion::new(1, 0, 0), SemverVersion::new(2, 0, 0));
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await;

    // If filter was applied BEFORE deserialization → Ok(None)
    // If deserialization happened first → Err(permanent) — test fails
    assert!(
        result.is_ok(),
        "Filter should be applied before deserialization; got error: {:?}",
        result.err()
    );
    assert!(
        result.unwrap().is_none(),
        "Excluded version should not be returned"
    );

    // Sanity: fetch without filter → should error (history is corrupted)
    let unfiltered = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, None)
        .await;
    assert!(
        unfiltered.is_err(),
        "Unfiltered fetch should hit deserialization error for corrupted history"
    );
}

/// Test #44: fetch_single_range_only_uses_first_range
///
/// Phase 1 limitation: when multiple ranges are provided in
/// `supported_duroxide_versions`, only the first range is used.
pub async fn test_fetch_single_range_only_uses_first_range<F: ProviderFactory>(factory: &F) {
    let provider = factory.create_provider().await;

    // Instance A pinned at 1.0.0
    seed_instance_with_version(&*provider, "inst-range-a", SemverVersion::new(1, 0, 0)).await;
    // Instance B pinned at 3.0.0
    seed_instance_with_version(&*provider, "inst-range-b", SemverVersion::new(3, 0, 0)).await;

    // Multi-range filter: [1.0.0–1.5.0, 3.0.0–3.5.0]
    let filter = DispatcherCapabilityFilter {
        supported_duroxide_versions: vec![
            SemverRange::new(SemverVersion::new(1, 0, 0), SemverVersion::new(1, 5, 0)),
            SemverRange::new(SemverVersion::new(3, 0, 0), SemverVersion::new(3, 5, 0)),
        ],
    };

    // Fetch — Phase 1 only uses first range [1.0.0–1.5.0]
    let result = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    assert!(result.is_some(), "Should return instance A (first range)");
    let (item, lock, _) = result.unwrap();
    assert_eq!(
        item.instance, "inst-range-a",
        "Phase 1: only first range should be used, returning instance A"
    );
    provider
        .abandon_orchestration_item(&lock, None, true)
        .await
        .unwrap();

    // Instance B (v3.0.0) should NOT be returned despite being in the second range
    // because Phase 1 only uses the first range
    let result2 = provider
        .fetch_orchestration_item(LOCK_TIMEOUT, Duration::ZERO, Some(&filter))
        .await
        .unwrap();
    // inst-range-a may come back (we abandoned it), but inst-range-b should not
    if let Some((item2, _, _)) = &result2 {
        assert_eq!(
            item2.instance, "inst-range-a",
            "Phase 1: second range should be ignored; only inst-range-a (first range) should be returned"
        );
    }
}

