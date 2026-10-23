//! An [OpenFeature](https://openfeature.dev) provider.
//!
//! ```no_run
//! use fluxgate::FluxGateClient;
//! use fluxgate::openfeature::FluxGateProvider;
//! use open_feature::{EvaluationContext, OpenFeature};
//!
//! # async fn run() -> Result<(), fluxgate::ConfigError> {
//! let fluxgate = FluxGateClient::new("https://edge.example.com", &std::env::var("FLUXGATE_SDK_KEY").unwrap())?;
//! let mut api = OpenFeature::singleton_mut().await;
//! api.set_provider(FluxGateProvider::new(fluxgate)).await;
//! let client = api.create_client();
//! drop(api);
//!
//! let user = EvaluationContext::default().with_targeting_key("user-1").with_custom_field("country", "CA");
//! let enabled = client.get_bool_value("new-checkout", Some(&user), None).await.unwrap_or(false);
//! # Ok(())
//! # }
//! ```
//!
//! Every evaluation is one OFREP call for that context, cached briefly by the
//! [`FluxGateClient`].
//!
//! OpenFeature providers in Rust receive no default value, so a flag the edge answers with its
//! "off" marker (`false` with no variant, for a disabled or unassigned flag) resolves to an
//! error for every type but `bool`. OpenFeature then returns your default value. The error
//! code is `GENERAL` with the reason, for example `DISABLED`, as its text.

use open_feature::provider::{FeatureProvider, ProviderMetadata, ResolutionDetails};
use open_feature::{
    EvaluationContext as OpenFeatureContext, EvaluationContextFieldValue, EvaluationError,
    EvaluationErrorCode, EvaluationReason, EvaluationResult, FlagMetadata, StructValue,
    async_trait,
};
use serde_json::{Map, Number, Value};

use crate::{ErrorCode, EvaluationContext, FluxGateClient, ResolvedFlag};

/// OpenFeature provider backed by a [`FluxGateClient`].
#[derive(Clone, Debug)]
pub struct FluxGateProvider {
    client: FluxGateClient,
    metadata: ProviderMetadata,
}

impl FluxGateProvider {
    /// A provider that evaluates through `client`.
    pub fn new(client: FluxGateClient) -> Self {
        Self {
            client,
            metadata: ProviderMetadata::new("FluxGate"),
        }
    }

    /// The underlying client, for evaluations outside OpenFeature.
    pub fn client(&self) -> &FluxGateClient {
        &self.client
    }

    async fn resolve<T>(
        &self,
        key: &str,
        context: &OpenFeatureContext,
        expected: &str,
        convert: impl FnOnce(&Value) -> Option<T>,
    ) -> EvaluationResult<ResolutionDetails<T>> {
        let flag = self
            .client
            .resolve(key, &to_fluxgate_context(context))
            .await
            .map_err(|error| evaluation_error(error.code(), error.to_string()))?;
        if let Some(code) = flag.error_code {
            return Err(evaluation_error(
                code,
                flag.error_details.clone().unwrap_or_default(),
            ));
        }
        let Some(value) = &flag.value else {
            return Err(no_value(&flag, "resolved to the code default"));
        };
        match convert(value) {
            Some(typed) => Ok(resolution(typed, &flag)),
            None if flag.is_off_marker() => Err(no_value(&flag, "is off for this context")),
            None => Err(evaluation_error(
                ErrorCode::TypeMismatch,
                format!("Flag '{key}' is not {expected}"),
            )),
        }
    }
}

#[async_trait]
impl FeatureProvider for FluxGateProvider {
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn resolve_bool_value(
        &self,
        flag_key: &str,
        context: &OpenFeatureContext,
    ) -> EvaluationResult<ResolutionDetails<bool>> {
        self.resolve(flag_key, context, "a boolean", Value::as_bool)
            .await
    }

    async fn resolve_int_value(
        &self,
        flag_key: &str,
        context: &OpenFeatureContext,
    ) -> EvaluationResult<ResolutionDetails<i64>> {
        self.resolve(flag_key, context, "an integer", Value::as_i64)
            .await
    }

    async fn resolve_float_value(
        &self,
        flag_key: &str,
        context: &OpenFeatureContext,
    ) -> EvaluationResult<ResolutionDetails<f64>> {
        self.resolve(flag_key, context, "a number", Value::as_f64)
            .await
    }

    async fn resolve_string_value(
        &self,
        flag_key: &str,
        context: &OpenFeatureContext,
    ) -> EvaluationResult<ResolutionDetails<String>> {
        self.resolve(flag_key, context, "a string", |value| {
            value.as_str().map(str::to_owned)
        })
        .await
    }

    async fn resolve_struct_value(
        &self,
        flag_key: &str,
        context: &OpenFeatureContext,
    ) -> EvaluationResult<ResolutionDetails<StructValue>> {
        self.resolve(
            flag_key,
            context,
            "an object",
            |value| match to_open_feature_value(value)? {
                open_feature::Value::Struct(value) => Some(value),
                _ => None,
            },
        )
        .await
    }
}

/// Turns an OpenFeature evaluation context into a FluxGate one. Date-times are sent as
/// RFC 3339 strings; custom structs are sent when they are a [`serde_json::Value`] and
/// skipped otherwise.
fn to_fluxgate_context(context: &OpenFeatureContext) -> EvaluationContext {
    let mut result = EvaluationContext::default();
    if let Some(key) = &context.targeting_key {
        result.set_targeting_key(key.clone());
    }
    for (name, field) in &context.custom_fields {
        let value = match field {
            EvaluationContextFieldValue::Bool(value) => Value::Bool(*value),
            EvaluationContextFieldValue::Int(value) => Value::from(*value),
            EvaluationContextFieldValue::Float(value) => match Number::from_f64(*value) {
                Some(number) => Value::Number(number),
                None => continue,
            },
            EvaluationContextFieldValue::String(value) => Value::String(value.clone()),
            EvaluationContextFieldValue::DateTime(value) => {
                match value.format(&time::format_description::well_known::Rfc3339) {
                    Ok(text) => Value::String(text),
                    Err(_) => continue,
                }
            }
            EvaluationContextFieldValue::Struct(value) => match value.downcast_ref::<Value>() {
                Some(value) => value.clone(),
                None => continue,
            },
        };
        result.set(name.clone(), value);
    }
    result
}

fn to_open_feature_value(value: &Value) -> Option<open_feature::Value> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(value) => open_feature::Value::Bool(*value),
        Value::Number(number) => match number.as_i64() {
            Some(value) => open_feature::Value::Int(value),
            None => open_feature::Value::Float(number.as_f64()?),
        },
        Value::String(value) => open_feature::Value::String(value.clone()),
        Value::Array(items) => open_feature::Value::Array(
            items
                .iter()
                .map(to_open_feature_value)
                .collect::<Option<_>>()?,
        ),
        Value::Object(fields) => {
            let mut result = StructValue::default();
            for (name, value) in fields {
                result.add_field(name.clone(), to_open_feature_value(value)?);
            }
            open_feature::Value::Struct(result)
        }
    })
}

fn resolution<T>(value: T, flag: &ResolvedFlag) -> ResolutionDetails<T> {
    ResolutionDetails {
        value,
        variant: flag.variant.clone(),
        reason: Some(to_reason(&flag.reason)),
        flag_metadata: to_flag_metadata(&flag.metadata),
    }
}

fn no_value(flag: &ResolvedFlag, what: &str) -> EvaluationError {
    EvaluationError {
        code: EvaluationErrorCode::General(flag.reason.clone()),
        message: Some(format!(
            "Flag '{}' {what}; the default value applies",
            flag.key
        )),
    }
}

fn evaluation_error(code: ErrorCode, message: String) -> EvaluationError {
    let code = match code {
        ErrorCode::FlagNotFound => EvaluationErrorCode::FlagNotFound,
        ErrorCode::TypeMismatch => EvaluationErrorCode::TypeMismatch,
        ErrorCode::TargetingKeyMissing => EvaluationErrorCode::TargetingKeyMissing,
        ErrorCode::InvalidContext => EvaluationErrorCode::InvalidContext,
        ErrorCode::ProviderNotReady => EvaluationErrorCode::ProviderNotReady,
        ErrorCode::ParseError => EvaluationErrorCode::ParseError,
        ErrorCode::Unauthorized | ErrorCode::General => {
            EvaluationErrorCode::General(code.as_str().to_owned())
        }
    };
    EvaluationError {
        code,
        message: (!message.is_empty()).then_some(message),
    }
}

fn to_reason(reason: &str) -> EvaluationReason {
    match reason {
        "STATIC" => EvaluationReason::Static,
        "DEFAULT" => EvaluationReason::Default,
        "TARGETING_MATCH" => EvaluationReason::TargetingMatch,
        "SPLIT" => EvaluationReason::Split,
        "CACHED" => EvaluationReason::Cached,
        "DISABLED" => EvaluationReason::Disabled,
        "UNKNOWN" => EvaluationReason::Unknown,
        "ERROR" => EvaluationReason::Error,
        other => EvaluationReason::Other(other.to_owned()),
    }
}

/// OpenFeature flag metadata allows only strings, numbers and booleans.
fn to_flag_metadata(metadata: &Map<String, Value>) -> Option<FlagMetadata> {
    let mut result = FlagMetadata::default();
    for (name, value) in metadata {
        match value {
            Value::Bool(value) => result.add_value(name.clone(), *value),
            Value::String(value) => result.add_value(name.clone(), value.clone()),
            Value::Number(number) => match (number.as_i64(), number.as_f64()) {
                (Some(value), _) => result.add_value(name.clone(), value),
                (None, Some(value)) => result.add_value(name.clone(), value),
                (None, None) => {}
            },
            Value::Null | Value::Array(_) | Value::Object(_) => {}
        }
    }
    (!result.values.is_empty()).then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn converts_contexts() {
        let date = time::macros::datetime!(2026-10-23 12:00 UTC);
        let context = OpenFeatureContext::default()
            .with_targeting_key("user-1")
            .with_custom_field("country", "CA")
            .with_custom_field("age", 42)
            .with_custom_field("signed_up", EvaluationContextFieldValue::DateTime(date))
            .with_custom_field(
                "prefs",
                EvaluationContextFieldValue::Struct(Arc::new(serde_json::json!({"a": 1}))),
            )
            .with_custom_field("opaque", EvaluationContextFieldValue::new_struct(7_u8));
        let result = to_fluxgate_context(&context);
        assert_eq!(result.targeting_key(), Some("user-1"));
        assert_eq!(result.attribute("country"), Some(&Value::from("CA")));
        assert_eq!(result.attribute("age"), Some(&Value::from(42)));
        assert_eq!(
            result.attribute("signed_up"),
            Some(&Value::from("2026-10-23T12:00:00Z"))
        );
        assert_eq!(
            result.attribute("prefs"),
            Some(&serde_json::json!({"a": 1}))
        );
        assert_eq!(result.attribute("opaque"), None);
    }

    #[test]
    fn converts_metadata() {
        let metadata =
            serde_json::json!({"team": "a", "count": 2, "ratio": 0.5, "on": true, "list": [1]});
        let result = to_flag_metadata(metadata.as_object().unwrap()).unwrap();
        assert_eq!(result.values.len(), 4);
        assert!(to_flag_metadata(&Map::new()).is_none());
    }
}
