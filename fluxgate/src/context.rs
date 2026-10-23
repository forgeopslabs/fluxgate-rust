use std::collections::BTreeMap;

use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::Value;

const TARGETING_KEY: &str = "targetingKey";

/// Who a flag is evaluated for.
///
/// The targeting key identifies the user (or other subject) and decides their bucket in
/// percentage splits; the edge requires it. Every other attribute is a targeting attribute,
/// such as `country` or `user_tier`.
///
/// ```
/// use fluxgate::EvaluationContext;
///
/// let context = EvaluationContext::new("user-1")
///     .with("country", "CA")
///     .with("user_tier", "plus");
/// assert_eq!(context.targeting_key(), Some("user-1"));
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EvaluationContext {
    targeting_key: Option<String>,
    attributes: BTreeMap<String, Value>,
}

impl EvaluationContext {
    /// A context for the given targeting key, with no attributes.
    pub fn new(targeting_key: impl Into<String>) -> Self {
        Self {
            targeting_key: Some(targeting_key.into()),
            attributes: BTreeMap::new(),
        }
    }

    /// Adds or replaces an attribute. The name `targetingKey` sets the targeting key when the
    /// value is a string.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.set(name, value);
        self
    }

    /// Adds or replaces an attribute. The name `targetingKey` sets the targeting key when the
    /// value is a string.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<Value>) {
        let name = name.into();
        let value = value.into();
        if name == TARGETING_KEY {
            if let Value::String(key) = value {
                self.targeting_key = Some(key);
            }
            return;
        }
        self.attributes.insert(name, value);
    }

    /// Sets the targeting key.
    pub fn set_targeting_key(&mut self, targeting_key: impl Into<String>) {
        self.targeting_key = Some(targeting_key.into());
    }

    /// The targeting key, if set.
    pub fn targeting_key(&self) -> Option<&str> {
        self.targeting_key.as_deref()
    }

    /// One attribute.
    pub fn attribute(&self, name: &str) -> Option<&Value> {
        self.attributes.get(name)
    }

    /// Every attribute except the targeting key, sorted by name.
    pub fn attributes(&self) -> &BTreeMap<String, Value> {
        &self.attributes
    }

    /// Whether the targeting key is set and not empty.
    pub(crate) fn has_targeting_key(&self) -> bool {
        self.targeting_key
            .as_deref()
            .is_some_and(|key| !key.is_empty())
    }
}

/// The OFREP shape: `targetingKey` first, then the attributes. Attribute names are sorted, so
/// equal contexts serialize to equal strings.
impl Serialize for EvaluationContext {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.attributes.len() + 1))?;
        map.serialize_entry(TARGETING_KEY, self.targeting_key.as_deref().unwrap_or(""))?;
        for (name, value) in &self.attributes {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_targeting_key_first() {
        let context = EvaluationContext::new("user-1")
            .with("country", "CA")
            .with("age", 42);
        assert_eq!(
            serde_json::to_string(&context).unwrap(),
            r#"{"targetingKey":"user-1","age":42,"country":"CA"}"#
        );
    }

    #[test]
    fn targeting_key_attribute_sets_the_key() {
        let context = EvaluationContext::default().with("targetingKey", "user-2");
        assert_eq!(context.targeting_key(), Some("user-2"));
        assert!(context.attributes().is_empty());
        assert_eq!(
            serde_json::to_value(&context).unwrap(),
            json!({"targetingKey": "user-2"})
        );
    }

    #[test]
    fn empty_key_is_missing() {
        assert!(!EvaluationContext::new("").has_targeting_key());
        assert!(!EvaluationContext::default().has_targeting_key());
    }
}
