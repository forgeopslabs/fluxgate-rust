#![cfg(feature = "blocking")]

mod common;

use std::time::Duration;

use common::{FakeEdge, SDK_KEY};
use fluxgate::blocking::FluxGateClient;
use fluxgate::{EvaluationContext, SnapshotStatus};
use serde_json::json;
use wiremock::matchers::path;
use wiremock::{Mock, ResponseTemplate};

/// The mock server needs a runtime; the blocking client must run outside it.
fn with_edge(test: impl FnOnce(String, FakeEdge) + Send + 'static) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (server, edge) = runtime.block_on(async {
        let (server, edge) = FakeEdge::start().await;
        Mock::given(path("/ofrep/v1/evaluate/flags/new-checkout"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"key": "new-checkout", "value": true})),
            )
            .mount(&server)
            .await;
        (server, edge)
    });
    let uri = server.uri();
    std::thread::spawn(move || test(uri, edge)).join().unwrap();
    drop(server);
}

#[test]
fn evaluates_without_an_async_runtime() {
    with_edge(|uri, _| {
        let fluxgate = FluxGateClient::builder(uri, SDK_KEY)
            .build_blocking()
            .unwrap();
        let user = EvaluationContext::new("user-1");
        assert!(fluxgate.is_enabled("new-checkout", &user));
        assert_eq!(fluxgate.get_string("other", "fallback", &user), "fallback");
        assert!(
            fluxgate
                .evaluate_all(&user, None)
                .unwrap()
                .flags
                .contains_key("checkout-theme")
        );
    });
}

#[test]
fn snapshots_load_and_switch_context() {
    with_edge(|uri, edge| {
        let fluxgate = FluxGateClient::new(&uri, SDK_KEY).unwrap();
        let flags = fluxgate.watch_with_interval(
            EvaluationContext::new("user-1").with("country", "CA"),
            Duration::from_secs(60),
        );
        flags.wait_ready();
        assert_eq!(flags.status(), SnapshotStatus::Ready);
        assert!(flags.is_enabled("new-checkout"));

        flags.set_context(EvaluationContext::new("user-2").with("country", "US"));
        assert!(!flags.is_enabled("new-checkout"));
        assert_eq!(edge.calls(), 2);
    });
}
