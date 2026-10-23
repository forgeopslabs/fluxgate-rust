use std::time::Duration;

use fluxgate::{ErrorCode, EvaluationContext, FluxGateClient};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SDK_KEY: &str = "client-1.api-key";

fn client(server: &MockServer) -> FluxGateClient {
    FluxGateClient::builder(server.uri(), SDK_KEY)
        .initial_backoff(Duration::ZERO)
        .build()
        .unwrap()
}

fn user() -> EvaluationContext {
    EvaluationContext::new("user-1").with("country", "CA")
}

async fn answer(server: &MockServer, key: &str, status: u16, body: Value) {
    Mock::given(method("POST"))
        .and(path(format!("/ofrep/v1/evaluate/flags/{key}")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

async fn requests(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

#[tokio::test]
async fn sends_the_sdk_key_and_the_context() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ofrep/v1/evaluate/flags/new-checkout"))
        .and(header("authorization", format!("Bearer {SDK_KEY}")))
        .and(header("content-type", "application/json"))
        .and(body_json(
            json!({"context": {"targetingKey": "user-1", "country": "CA"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"key": "new-checkout", "value": true, "reason": "TARGETING_MATCH"}),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let evaluation = client(&server)
        .evaluate("new-checkout", false, &user())
        .await;
    assert!(evaluation.value);
    assert_eq!(evaluation.reason, "TARGETING_MATCH");
    assert!(evaluation.error_code.is_none());
}

#[tokio::test]
async fn returns_string_variants_with_metadata() {
    let server = MockServer::start().await;
    answer(
        &server,
        "checkout-theme",
        200,
        json!({"key": "checkout-theme", "value": "express", "variant": "express", "reason": "SPLIT", "metadata": {"team": "web"}}),
    )
    .await;

    let evaluation = client(&server)
        .evaluate("checkout-theme", "classic".to_owned(), &user())
        .await;
    assert_eq!(evaluation.value, "express");
    assert_eq!(evaluation.variant.as_deref(), Some("express"));
    assert_eq!(evaluation.reason, "SPLIT");
    assert_eq!(evaluation.metadata.get("team"), Some(&json!("web")));
}

#[tokio::test]
async fn treats_the_off_marker_as_the_default() {
    let server = MockServer::start().await;
    answer(
        &server,
        "checkout-theme",
        200,
        json!({"key": "checkout-theme", "value": false, "reason": "DISABLED"}),
    )
    .await;

    let fluxgate = client(&server);
    let evaluation = fluxgate
        .evaluate("checkout-theme", "classic".to_owned(), &user())
        .await;
    assert_eq!(evaluation.value, "classic");
    assert_eq!(evaluation.reason, "DISABLED");
    assert!(evaluation.error_code.is_none());
    assert_eq!(fluxgate.get_i64("checkout-theme", 3, &user()).await, 3);
}

#[tokio::test]
async fn reports_a_type_mismatch() {
    let server = MockServer::start().await;
    answer(
        &server,
        "limit",
        200,
        json!({"key": "limit", "value": "ten", "variant": "ten", "reason": "STATIC"}),
    )
    .await;

    let evaluation = client(&server).evaluate("limit", 5_i64, &user()).await;
    assert_eq!(evaluation.value, 5);
    assert_eq!(evaluation.error_code, Some(ErrorCode::TypeMismatch));
    assert_eq!(evaluation.reason, "ERROR");
}

#[tokio::test]
async fn reads_numbers_and_json() {
    let server = MockServer::start().await;
    answer(
        &server,
        "limit",
        200,
        json!({"key": "limit", "value": 10, "variant": "ten", "reason": "STATIC"}),
    )
    .await;
    answer(
        &server,
        "ratio",
        200,
        json!({"key": "ratio", "value": 0.25, "variant": "low", "reason": "STATIC"}),
    )
    .await;
    answer(
        &server,
        "config",
        200,
        json!({"key": "config", "value": {"a": 1}, "variant": "v1", "reason": "STATIC"}),
    )
    .await;

    let fluxgate = client(&server);
    assert_eq!(fluxgate.get_i64("limit", 0, &user()).await, 10);
    assert_eq!(fluxgate.get_f64("limit", 0.0, &user()).await, 10.0);
    assert_eq!(fluxgate.get_f64("ratio", 0.0, &user()).await, 0.25);
    assert_eq!(
        fluxgate.get_value("config", Value::Null, &user()).await,
        json!({"a": 1})
    );
    assert_eq!(
        fluxgate.get_value("config", json!({}), &user()).await,
        json!({"a": 1})
    );
    assert_eq!(
        fluxgate.get_value("limit", json!({}), &user()).await,
        json!({})
    );
}

#[tokio::test]
async fn returns_the_default_for_the_code_default() {
    let server = MockServer::start().await;
    answer(
        &server,
        "limit",
        200,
        json!({"key": "limit", "reason": "DEFAULT"}),
    )
    .await;

    let evaluation = client(&server).evaluate("limit", 7_i64, &user()).await;
    assert_eq!(evaluation.value, 7);
    assert_eq!(evaluation.reason, "DEFAULT");
    assert!(!evaluation.is_error());
}

#[tokio::test]
async fn fails_locally_without_a_targeting_key_or_flag_key() {
    let server = MockServer::start().await;
    let fluxgate = client(&server);

    let evaluation = fluxgate
        .evaluate("new-checkout", true, &EvaluationContext::default())
        .await;
    assert!(evaluation.value);
    assert_eq!(evaluation.error_code, Some(ErrorCode::TargetingKeyMissing));

    let evaluation = fluxgate.evaluate("", true, &user()).await;
    assert_eq!(evaluation.error_code, Some(ErrorCode::General));
    assert_eq!(requests(&server).await, 0);
}

#[tokio::test]
async fn maps_http_errors() {
    let server = MockServer::start().await;
    answer(
        &server,
        "missing",
        404,
        json!({"errorCode": "FLAG_NOT_FOUND", "errorDetails": "no such flag"}),
    )
    .await;
    answer(&server, "denied", 401, json!({})).await;
    answer(&server, "bad", 400, json!({"errorCode": "INVALID_CONTEXT"})).await;

    let fluxgate = client(&server);
    let evaluation = fluxgate.evaluate("missing", false, &user()).await;
    assert_eq!(evaluation.error_code, Some(ErrorCode::FlagNotFound));
    assert_eq!(
        evaluation.error_message.as_deref(),
        Some("Edge returned HTTP 404: no such flag")
    );
    assert_eq!(
        fluxgate.evaluate("denied", false, &user()).await.error_code,
        Some(ErrorCode::Unauthorized)
    );
    assert_eq!(
        fluxgate.evaluate("bad", false, &user()).await.error_code,
        Some(ErrorCode::InvalidContext)
    );
}

#[tokio::test]
async fn retries_502_503_504_then_succeeds() {
    let server = MockServer::start().await;
    for status in [502, 503] {
        Mock::given(path("/ofrep/v1/evaluate/flags/f"))
            .respond_with(ResponseTemplate::new(status))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
    }
    answer(
        &server,
        "f",
        200,
        json!({"key": "f", "value": true, "reason": "STATIC"}),
    )
    .await;

    assert!(client(&server).is_enabled("f", &user()).await);
    assert_eq!(requests(&server).await, 3);
}

#[tokio::test]
async fn gives_up_after_max_attempts() {
    let server = MockServer::start().await;
    answer(&server, "f", 503, json!({})).await;

    let fluxgate = FluxGateClient::builder(server.uri(), SDK_KEY)
        .initial_backoff(Duration::ZERO)
        .max_attempts(2)
        .build()
        .unwrap();
    let evaluation = fluxgate.evaluate("f", false, &user()).await;
    assert_eq!(evaluation.error_code, Some(ErrorCode::ProviderNotReady));
    assert_eq!(requests(&server).await, 2);
}

#[tokio::test]
async fn does_not_retry_other_errors() {
    let server = MockServer::start().await;
    answer(&server, "f", 500, json!({})).await;

    let evaluation = client(&server).evaluate("f", false, &user()).await;
    assert_eq!(evaluation.error_code, Some(ErrorCode::General));
    assert_eq!(requests(&server).await, 1);
}

#[tokio::test]
async fn reports_network_errors() {
    // A port that was free a moment ago, so nothing listens on it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let fluxgate = FluxGateClient::builder(format!("http://127.0.0.1:{port}"), SDK_KEY)
        .initial_backoff(Duration::ZERO)
        .max_attempts(2)
        .build()
        .unwrap();
    let evaluation = fluxgate.evaluate("f", true, &user()).await;
    assert!(evaluation.value);
    assert_eq!(evaluation.error_code, Some(ErrorCode::General));
    assert!(
        evaluation
            .error_message
            .unwrap()
            .starts_with("Could not reach the edge"),
        "network errors name the cause"
    );
}

#[tokio::test]
async fn times_out_slow_answers() {
    let server = MockServer::start().await;
    Mock::given(path("/ofrep/v1/evaluate/flags/f"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"key": "f", "value": true}))
                .set_delay(Duration::from_millis(500)),
        )
        .mount(&server)
        .await;

    let fluxgate = FluxGateClient::builder(server.uri(), SDK_KEY)
        .timeout(Duration::from_millis(50))
        .max_attempts(1)
        .build()
        .unwrap();
    let evaluation = fluxgate.evaluate("f", false, &user()).await;
    assert_eq!(evaluation.error_code, Some(ErrorCode::General));
    assert_eq!(
        evaluation.error_message.as_deref(),
        Some("Edge did not answer within 50 ms")
    );
}

#[tokio::test]
async fn reports_a_body_that_is_not_json_as_a_parse_error() {
    let server = MockServer::start().await;
    Mock::given(path("/ofrep/v1/evaluate/flags/f"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&server)
        .await;

    let evaluation = client(&server).evaluate("f", false, &user()).await;
    assert_eq!(evaluation.error_code, Some(ErrorCode::ParseError));
}

#[tokio::test]
async fn passes_a_flag_level_error_through() {
    let server = MockServer::start().await;
    answer(
        &server,
        "f",
        200,
        json!({"key": "f", "errorCode": "TARGETING_KEY_MISSING", "errorDetails": "need a key"}),
    )
    .await;

    let evaluation = client(&server).evaluate("f", false, &user()).await;
    assert_eq!(evaluation.error_code, Some(ErrorCode::TargetingKeyMissing));
    assert_eq!(evaluation.error_message.as_deref(), Some("need a key"));
}

#[tokio::test]
async fn caches_results_per_flag_and_context() {
    let server = MockServer::start().await;
    answer(
        &server,
        "f",
        200,
        json!({"key": "f", "value": true, "reason": "STATIC"}),
    )
    .await;

    let fluxgate = client(&server);
    fluxgate.is_enabled("f", &user()).await;
    fluxgate.is_enabled("f", &user()).await;
    assert_eq!(requests(&server).await, 1);

    fluxgate
        .is_enabled("f", &user().with("country", "US"))
        .await;
    assert_eq!(requests(&server).await, 2);

    fluxgate.clear_cache();
    fluxgate.is_enabled("f", &user()).await;
    assert_eq!(requests(&server).await, 3);
}

#[tokio::test]
async fn does_not_cache_errors_or_when_disabled() {
    let server = MockServer::start().await;
    answer(&server, "missing", 404, json!({})).await;
    answer(&server, "f", 200, json!({"key": "f", "value": true})).await;

    let fluxgate = client(&server);
    fluxgate.is_enabled("missing", &user()).await;
    fluxgate.is_enabled("missing", &user()).await;
    assert_eq!(requests(&server).await, 2);

    let uncached = FluxGateClient::builder(server.uri(), SDK_KEY)
        .disable_cache()
        .build()
        .unwrap();
    uncached.is_enabled("f", &user()).await;
    uncached.is_enabled("f", &user()).await;
    assert_eq!(requests(&server).await, 4);
}

#[tokio::test]
async fn evaluates_all_flags_with_etag_and_not_modified() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ofrep/v1/evaluate/flags"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304).insert_header("etag", "\"v1\""))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ofrep/v1/evaluate/flags"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("etag", "\"v1\"").set_body_json(json!({"flags": [
                {"key": "new-checkout", "value": true, "reason": "TARGETING_MATCH"},
                {"key": "checkout-theme", "value": "classic", "variant": "classic", "reason": "SPLIT"},
            ]})),
        )
        .mount(&server)
        .await;

    let fluxgate = client(&server);
    let bulk = fluxgate.evaluate_all(&user(), None).await.unwrap();
    assert!(!bulk.not_modified);
    assert_eq!(bulk.version.as_deref(), Some("\"v1\""));
    assert_eq!(bulk.flags.len(), 2);
    assert_eq!(
        bulk.flags["checkout-theme"].variant.as_deref(),
        Some("classic")
    );

    let again = fluxgate
        .evaluate_all(&user(), bulk.version.as_deref())
        .await
        .unwrap();
    assert!(again.not_modified);
    assert!(again.flags.is_empty());
    assert_eq!(again.version.as_deref(), Some("\"v1\""));
}

#[tokio::test]
async fn bulk_falls_back_to_metadata_version_and_fails_without_flags() {
    let server = MockServer::start().await;
    Mock::given(path("/ofrep/v1/evaluate/flags"))
        .and(body_json(
            json!({"context": {"targetingKey": "user-1", "country": "CA"}}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"flags": [], "metadata": {"version": "m1"}})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/ofrep/v1/evaluate/flags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nope": true})))
        .mount(&server)
        .await;

    let fluxgate = client(&server);
    let bulk = fluxgate.evaluate_all(&user(), None).await.unwrap();
    assert_eq!(bulk.version.as_deref(), Some("m1"));

    let error = fluxgate
        .evaluate_all(&EvaluationContext::new("other"), None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ParseError);
}

#[tokio::test]
async fn bulk_returns_mapped_errors() {
    let server = MockServer::start().await;
    Mock::given(path("/ofrep/v1/evaluate/flags"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"errorCode": "UNAUTHORIZED"})),
        )
        .mount(&server)
        .await;

    let error = client(&server)
        .evaluate_all(&user(), None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::Unauthorized);
    assert_eq!(error.status(), Some(401));
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn checks_health_without_retrying() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let fluxgate = client(&server);
    assert!(fluxgate.is_healthy().await);
    assert!(!fluxgate.is_healthy().await);
    assert_eq!(requests(&server).await, 2);
}
