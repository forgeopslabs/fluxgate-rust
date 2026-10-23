use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::ErrorCode;

/// The outcome of one flag evaluation. On error, `value` is the default you passed.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct FlagEvaluation<T> {
    /// The flag key.
    pub key: String,
    /// The flag's value, or the default value on errors and when the flag is off.
    pub value: T,
    /// The variant the edge served, if the flag has variants.
    pub variant: Option<String>,
    /// The edge's reason, for example `TARGETING_MATCH`, `SPLIT`, `DISABLED` or `ERROR`.
    pub reason: String,
    /// Why the evaluation failed, if it did.
    pub error_code: Option<ErrorCode>,
    /// Details of the failure, if any.
    pub error_message: Option<String>,
    /// Flag metadata from the edge.
    pub metadata: Map<String, Value>,
}

impl<T> FlagEvaluation<T> {
    /// Whether the evaluation failed and `value` is the default.
    pub fn is_error(&self) -> bool {
        self.error_code.is_some()
    }

    pub(crate) fn error(
        key: &str,
        default_value: T,
        code: ErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            key: key.to_owned(),
            value: default_value,
            variant: None,
            reason: "ERROR".to_owned(),
            error_code: Some(code),
            error_message: Some(message.into()),
            metadata: Map::new(),
        }
    }
}

/// One flag as the edge resolved it, before it is converted to the caller's type.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ResolvedFlag {
    /// The flag key.
    pub key: String,
    /// The value; `None` when the edge answered with the code default.
    pub value: Option<Value>,
    /// The variant, if the flag has variants.
    pub variant: Option<String>,
    /// The edge's reason.
    pub reason: String,
    /// Set when the edge could not evaluate this flag.
    pub error_code: Option<ErrorCode>,
    /// The edge's description of the error.
    pub error_details: Option<String>,
    /// Flag metadata.
    pub metadata: Map<String, Value>,
}

impl ResolvedFlag {
    /// Whether this is the edge's answer for a disabled or unassigned flag of any type: `false`
    /// with no variant.
    pub(crate) fn is_off_marker(&self) -> bool {
        self.value == Some(Value::Bool(false)) && self.variant.is_none()
    }

    /// Converts the flag to a typed evaluation; see [`FlagValue`] for the type rules.
    ///
    /// The edge answers a disabled or unassigned flag of any type with `false` and no variant.
    /// That yields the default with the edge's reason, not a type mismatch.
    pub(crate) fn to_evaluation<T: FlagValue>(&self, default_value: T) -> FlagEvaluation<T> {
        if let Some(code) = self.error_code {
            let mut evaluation = FlagEvaluation::error(
                &self.key,
                default_value,
                code,
                self.error_details.clone().unwrap_or_default(),
            );
            evaluation.error_message = self.error_details.clone();
            evaluation.metadata = self.metadata.clone();
            return evaluation;
        }
        let (value, variant) = match &self.value {
            // The edge resolved to the code default.
            None => (default_value, self.variant.clone()),
            Some(value) => match T::from_flag(value, &default_value) {
                Some(typed) => (typed, self.variant.clone()),
                None if self.is_off_marker() => (default_value, None),
                None => {
                    let message = format!(
                        "Flag '{}' has a {} value, expected {}",
                        self.key,
                        json_type(value),
                        default_value.type_name()
                    );
                    let mut evaluation = FlagEvaluation::error(
                        &self.key,
                        default_value,
                        ErrorCode::TypeMismatch,
                        message,
                    );
                    evaluation.metadata = self.metadata.clone();
                    return evaluation;
                }
            },
        };
        FlagEvaluation {
            key: self.key.clone(),
            value,
            variant,
            reason: self.reason.clone(),
            error_code: None,
            error_message: None,
            metadata: self.metadata.clone(),
        }
    }
}

/// The result of a bulk evaluation.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct BulkEvaluation {
    /// `true` when the flags have not changed since the version passed in.
    pub not_modified: bool,
    /// Version of the flag set (the edge's `ETag`), to pass back next time.
    pub version: Option<String>,
    /// Flags by key; empty when `not_modified`.
    pub flags: HashMap<String, ResolvedFlag>,
}

mod sealed {
    pub trait Sealed {}
}

/// A type a flag can be read as: `bool`, `String`, `i64`, `f64` or [`serde_json::Value`].
///
/// The default value sets the expected type. A `bool` default accepts only a boolean, a
/// `String` only a string, an `i64` only an integer, an `f64` any number. A
/// [`Value`] default accepts a value of its own JSON type; `Value::Null` accepts any value,
/// and an object or array default accepts an object or array.
///
/// This trait is sealed.
pub trait FlagValue: sealed::Sealed + Sized {
    #[doc(hidden)]
    fn from_flag(value: &Value, default_value: &Self) -> Option<Self>;
    #[doc(hidden)]
    fn type_name(&self) -> &'static str;
}

impl sealed::Sealed for bool {}
impl FlagValue for bool {
    fn from_flag(value: &Value, _: &Self) -> Option<Self> {
        value.as_bool()
    }
    fn type_name(&self) -> &'static str {
        "boolean"
    }
}

impl sealed::Sealed for String {}
impl FlagValue for String {
    fn from_flag(value: &Value, _: &Self) -> Option<Self> {
        value.as_str().map(str::to_owned)
    }
    fn type_name(&self) -> &'static str {
        "string"
    }
}

impl sealed::Sealed for i64 {}
impl FlagValue for i64 {
    fn from_flag(value: &Value, _: &Self) -> Option<Self> {
        value.as_i64()
    }
    fn type_name(&self) -> &'static str {
        "integer"
    }
}

impl sealed::Sealed for f64 {}
impl FlagValue for f64 {
    fn from_flag(value: &Value, _: &Self) -> Option<Self> {
        value.as_f64()
    }
    fn type_name(&self) -> &'static str {
        "number"
    }
}

impl sealed::Sealed for Value {}
impl FlagValue for Value {
    fn from_flag(value: &Value, default_value: &Self) -> Option<Self> {
        let matches = match default_value {
            Value::Null => true,
            Value::Object(_) | Value::Array(_) => value.is_object() || value.is_array(),
            Value::Bool(_) => value.is_boolean(),
            Value::Number(_) => value.is_number(),
            Value::String(_) => value.is_string(),
        };
        matches.then(|| value.clone())
    }
    fn type_name(&self) -> &'static str {
        json_type(self)
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn flag(value: Option<Value>, variant: Option<&str>) -> ResolvedFlag {
        ResolvedFlag {
            key: "f".to_owned(),
            value,
            variant: variant.map(str::to_owned),
            reason: "TARGETING_MATCH".to_owned(),
            error_code: None,
            error_details: None,
            metadata: Map::new(),
        }
    }

    #[test]
    fn matching_type_returns_the_value() {
        let evaluation =
            flag(Some(json!("express")), Some("express")).to_evaluation("classic".to_owned());
        assert_eq!(evaluation.value, "express");
        assert_eq!(evaluation.variant.as_deref(), Some("express"));
        assert_eq!(evaluation.reason, "TARGETING_MATCH");
        assert!(!evaluation.is_error());
    }

    #[test]
    fn off_marker_returns_the_default_without_error() {
        let evaluation = flag(Some(json!(false)), None).to_evaluation("classic".to_owned());
        assert_eq!(evaluation.value, "classic");
        assert!(evaluation.error_code.is_none());
        assert!(evaluation.variant.is_none());
    }

    #[test]
    fn other_types_are_a_mismatch() {
        let evaluation = flag(Some(json!(true)), Some("on")).to_evaluation("classic".to_owned());
        assert_eq!(evaluation.value, "classic");
        assert_eq!(evaluation.error_code, Some(ErrorCode::TypeMismatch));
        assert_eq!(
            evaluation.error_message.as_deref(),
            Some("Flag 'f' has a boolean value, expected string")
        );
    }

    #[test]
    fn code_default_returns_the_default() {
        let evaluation = flag(None, None).to_evaluation(7_i64);
        assert_eq!(evaluation.value, 7);
        assert!(!evaluation.is_error());
    }

    #[test]
    fn numbers() {
        assert_eq!(flag(Some(json!(3)), None).to_evaluation(0_i64).value, 3);
        assert_eq!(
            flag(Some(json!(2.5)), None).to_evaluation(0.0_f64).value,
            2.5
        );
        assert!(flag(Some(json!(2.5)), None).to_evaluation(0_i64).is_error());
    }

    #[test]
    fn json_values() {
        let object = json!({"limit": 5});
        assert_eq!(
            flag(Some(object.clone()), None)
                .to_evaluation(Value::Null)
                .value,
            object
        );
        assert_eq!(
            flag(Some(object.clone()), None)
                .to_evaluation(json!({}))
                .value,
            object
        );
        assert!(
            flag(Some(json!("x")), Some("x"))
                .to_evaluation(json!({}))
                .is_error()
        );
    }

    #[test]
    fn edge_errors_keep_the_default() {
        let mut resolved = flag(None, None);
        resolved.error_code = Some(ErrorCode::FlagNotFound);
        resolved.error_details = Some("no such flag".to_owned());
        let evaluation = resolved.to_evaluation(true);
        assert!(evaluation.value);
        assert_eq!(evaluation.reason, "ERROR");
        assert_eq!(evaluation.error_code, Some(ErrorCode::FlagNotFound));
        assert_eq!(evaluation.error_message.as_deref(), Some("no such flag"));
    }
}
