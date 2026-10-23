#![cfg(feature = "openfeature")]

use fluxgate::FluxGateClient;
use fluxgate::openfeature::FluxGateProvider;
use open_feature::provider::FeatureProvider;
use open_feature::{EvaluationContext, EvaluationErrorCode, EvaluationReason, OpenFeature};
use serde_json::json;
use wiremock::matchers::{body_json, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn edge() -> MockServer {
    let server = MockServer::start().await;
    let flags = [
        (
            "new-checkout",
            json!({"key": "new-checkout", "value": true, "reason": "TARGETING_MATCH"}),
        ),
        (
            "checkout-theme",
            json!({"key": "checkout-theme", "value": "express", "variant": "express", "reason": "SPLIT", "metadata": {"team": "web"}}),
        ),
        (
            "limit",
            json!({"key": "limit", "value": 10, "variant": "ten", "reason": "STATIC"}),
        ),
        (
            "config",
            json!({"key": "config", "value": {"a": 1, "b": [true]}, "variant": "v1", "reason": "STATIC"}),
        ),
        (
            "off",
            json!({"key": "off", "value": false, "reason": "DISABLED"}),
        ),
    ];
    for (key, body) in flags {
        Mock::given(path(format!("/ofrep/v1/evaluate/flags/{key}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }
    Mock::given(path("/ofrep/v1/evaluate/flags/missing"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"errorCode": "FLAG_NOT_FOUND"})),
        )
        .mount(&server)
        .await;
    server
}

fn provider(server: &MockServer) -> FluxGateProvider {
    FluxGateProvider::new(FluxGateClient::new(&server.uri(), "client-1.api-key").unwrap())
}

fn user() -> EvaluationContext {
    EvaluationContext::default()
        .with_targeting_key("user-1")
        .with_custom_field("country", "CA")
}

#[tokio::test]
async fn resolves_every_type() {
    let server = edge().await;
    let provider = provider(&server);

    let enabled = provider
        .resolve_bool_value("new-checkout", &user())
        .await
        .unwrap();
    assert!(enabled.value);
    assert_eq!(enabled.reason, Some(EvaluationReason::TargetingMatch));

    let theme = provider
        .resolve_string_value("checkout-theme", &user())
        .await
        .unwrap();
    assert_eq!(theme.value, "express");
    assert_eq!(theme.variant.as_deref(), Some("express"));
    assert_eq!(theme.reason, Some(EvaluationReason::Split));
    assert_eq!(theme.flag_metadata.unwrap().values.len(), 1);

    assert_eq!(
        provider
            .resolve_int_value("limit", &user())
            .await
            .unwrap()
            .value,
        10
    );
    assert_eq!(
        provider
            .resolve_float_value("limit", &user())
            .await
            .unwrap()
            .value,
        10.0
    );

    let config = provider
        .resolve_struct_value("config", &user())
        .await
        .unwrap()
        .value;
    assert_eq!(config.fields["a"], open_feature::Value::Int(1));
}

#[tokio::test]
async fn maps_errors() {
    let server = edge().await;
    let provider = provider(&server);

    let missing = provider
        .resolve_bool_value("missing", &user())
        .await
        .unwrap_err();
    assert_eq!(missing.code, EvaluationErrorCode::FlagNotFound);

    let mismatch = provider
        .resolve_int_value("checkout-theme", &user())
        .await
        .unwrap_err();
    assert_eq!(mismatch.code, EvaluationErrorCode::TypeMismatch);

    let no_key = provider
        .resolve_bool_value("new-checkout", &EvaluationContext::default())
        .await
        .unwrap_err();
    assert_eq!(no_key.code, EvaluationErrorCode::TargetingKeyMissing);
}

#[tokio::test]
async fn the_off_marker_is_false_for_booleans_and_the_default_otherwise() {
    let server = edge().await;
    let provider = provider(&server);

    let off = provider.resolve_bool_value("off", &user()).await.unwrap();
    assert!(!off.value);
    assert_eq!(off.reason, Some(EvaluationReason::Disabled));

    let error = provider
        .resolve_string_value("off", &user())
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        EvaluationErrorCode::General("DISABLED".to_owned())
    );
}

#[tokio::test]
async fn works_through_the_open_feature_api() {
    let server = edge().await;
    Mock::given(path("/ofrep/v1/evaluate/flags/country-check"))
        .and(body_json(
            json!({"context": {"targetingKey": "user-1", "country": "CA"}}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"key": "country-check", "value": "CA", "variant": "ca"})),
        )
        .mount(&server)
        .await;

    let mut api = OpenFeature::default();
    api.set_provider(provider(&server)).await;
    let client = api.create_client();

    assert_eq!(
        client
            .get_string_value("country-check", Some(&user()), None)
            .await
            .unwrap(),
        "CA"
    );
    assert_eq!(
        client
            .get_string_value("off", Some(&user()), None)
            .await
            .unwrap_or_else(|_| "classic".to_owned()),
        "classic"
    );
}
