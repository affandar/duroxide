//! Serde boundary tests for replay engine versioning.
//!
//! These tests verify that EventKind deserialization correctly rejects
//! unknown variant JSON when the corresponding feature flag is not enabled.
//! This is the foundation of duroxide's version isolation guarantee:
//! old binaries structurally cannot parse new event types.
//!
//! This test is NOT feature-gated — it runs in every `cargo nextest run` invocation.
//! When `replay-version-test` is enabled, the v2 variants exist and deserialization
//! succeeds (tested in tests/scenarios/replay_versioning.rs).
//! When the feature is off, deserialization fails (tested here).

use duroxide::EventKind;

/// Without the replay-version-test feature, v2 event JSON must fail deserialization.
/// This proves that a duroxide binary without the v2 code cannot parse v2 history.
///
/// With the feature enabled, deserialization succeeds — that path is covered by
/// `replay_versioning::v2_events_deserialize_with_feature_flag`.
#[test]
fn serde_boundary_v2_events() {
    let subscribed2_json = r#"{"type": "ExternalSubscribed2", "name": "x", "topic": "y"}"#;
    let event2_json =
        r#"{"type": "ExternalEvent2", "name": "x", "topic": "y", "data": "payload"}"#;

    #[cfg(not(feature = "replay-version-test"))]
    {
        assert!(
            serde_json::from_str::<EventKind>(subscribed2_json).is_err(),
            "ExternalSubscribed2 should fail deserialization without feature flag"
        );
        assert!(
            serde_json::from_str::<EventKind>(event2_json).is_err(),
            "ExternalEvent2 should fail deserialization without feature flag"
        );
    }

    #[cfg(feature = "replay-version-test")]
    {
        // When feature is on, v2 variants exist — just verify they parse (detailed checks in replay_versioning.rs)
        assert!(
            serde_json::from_str::<EventKind>(subscribed2_json).is_ok(),
            "ExternalSubscribed2 should succeed with feature flag"
        );
        assert!(
            serde_json::from_str::<EventKind>(event2_json).is_ok(),
            "ExternalEvent2 should succeed with feature flag"
        );
    }
}
