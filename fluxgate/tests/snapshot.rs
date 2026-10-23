mod common;

use std::time::Duration;

use common::{FakeEdge, SDK_KEY};
use fluxgate::{ErrorCode, EvaluationContext, FluxGateClient, SnapshotStatus};
use serde_json::json;

fn client(uri: String) -> FluxGateClient {
    FluxGateClient::builder(uri, SDK_KEY)
        .max_attempts(1)
        .build()
        .unwrap()
}

fn plus_user() -> EvaluationContext {
    EvaluationContext::new("user-1")
        .with("country", "CA")
        .with("user_tier", "plus")
}

#[tokio::test]
async fn is_not_ready_until_the_first_load_then_evaluates_synchronously() {
    let (server, edge) = FakeEdge::start().await;
    let flags = client(server.uri()).watch_with_interval(plus_user(), Duration::ZERO);

    let early = flags.evaluate("checkout-theme", "classic".to_owned());
    assert_eq!(early.value, "classic");
    assert_eq!(early.error_code, Some(ErrorCode::ProviderNotReady));

    flags.ready().await;
    assert_eq!(flags.status(), SnapshotStatus::Ready);
    assert!(flags.is_enabled("new-checkout"));
    let theme = flags.evaluate("checkout-theme", "classic".to_owned());
    assert_eq!(theme.value, "express");
    assert_eq!(theme.variant.as_deref(), Some("express"));
    assert_eq!(
        flags.evaluate("missing", false).error_code,
        Some(ErrorCode::FlagNotFound)
    );
    let mut keys = flags.keys();
    keys.sort();
    assert_eq!(keys, ["checkout-theme", "new-checkout"]);
    assert_eq!(
        edge.contexts(),
        [json!({"targetingKey": "user-1", "country": "CA", "user_tier": "plus"})]
    );
}

#[tokio::test]
async fn reports_error_when_the_first_load_fails() {
    let (server, edge) = FakeEdge::start().await;
    edge.fail_with(Some(401));
    let flags = client(server.uri()).watch_with_interval(plus_user(), Duration::ZERO);
    flags.ready().await;

    assert_eq!(flags.status(), SnapshotStatus::Error);
    assert_eq!(flags.error().unwrap().code(), ErrorCode::Unauthorized);
    let evaluation = flags.evaluate("new-checkout", true);
    assert!(evaluation.value);
    assert_eq!(evaluation.error_code, Some(ErrorCode::Unauthorized));
}

#[tokio::test]
async fn polls_with_if_none_match_and_keeps_flags_when_a_refresh_fails() {
    let (server, edge) = FakeEdge::start().await;
    let flags = client(server.uri()).watch_with_interval(plus_user(), Duration::from_millis(40));
    let mut changes = flags.subscribe();
    flags.ready().await;
    let first_revision = flags.revision();

    // Unchanged flags: polls send the ETag and get 304s, with no change notification.
    tokio::time::sleep(Duration::from_millis(130)).await;
    assert!(edge.calls() >= 3);
    assert!(edge.if_none_match()[1..].iter().all(Option::is_some));
    assert_eq!(flags.revision(), first_revision);

    // Changed flags reach the snapshot on the next poll.
    changes.borrow_and_update();
    edge.bump();
    tokio::time::timeout(Duration::from_secs(2), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(flags.get_i64("limit", 0), 1);

    // A failed refresh keeps the flags and reports Stale.
    edge.fail_with(Some(500));
    tokio::time::timeout(Duration::from_secs(2), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(flags.status(), SnapshotStatus::Stale);
    assert_eq!(flags.get_i64("limit", 0), 1);
    assert_eq!(flags.error().unwrap().code(), ErrorCode::General);

    edge.fail_with(None);
    tokio::time::timeout(Duration::from_secs(2), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(flags.status(), SnapshotStatus::Ready);
    assert!(flags.error().is_none());

    flags.close();
    let calls = edge.calls();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(edge.calls(), calls, "no polls after close");
}

#[tokio::test]
async fn reloads_for_a_new_context() {
    let (server, edge) = FakeEdge::start().await;
    let flags = client(server.uri()).watch_with_interval(plus_user(), Duration::ZERO);
    flags.ready().await;
    assert!(flags.is_enabled("new-checkout"));

    flags
        .set_context(EvaluationContext::new("user-2").with("country", "US"))
        .await;
    assert!(!flags.is_enabled("new-checkout"));
    assert_eq!(flags.get_string("checkout-theme", "x"), "classic");
    assert_eq!(flags.context().targeting_key(), Some("user-2"));
    // A new context starts without a version, so it never gets a 304 for the old flags.
    assert_eq!(edge.if_none_match()[1], None);
}

#[tokio::test]
async fn manual_refresh_fetches_now() {
    let (server, edge) = FakeEdge::start().await;
    let flags = client(server.uri()).watch_with_interval(plus_user(), Duration::ZERO);
    flags.ready().await;
    edge.bump();
    flags.refresh().await;
    assert_eq!(flags.get_i64("limit", 0), 1);
    assert_eq!(edge.calls(), 2);
}

#[tokio::test]
async fn stops_polling_when_dropped() {
    let (server, edge) = FakeEdge::start().await;
    let flags = client(server.uri()).watch_with_interval(plus_user(), Duration::from_millis(20));
    flags.ready().await;
    drop(flags);
    let calls = edge.calls();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(edge.calls(), calls);
}
