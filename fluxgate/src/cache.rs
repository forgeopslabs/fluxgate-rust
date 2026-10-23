use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{EvaluationContext, ResolvedFlag};

/// Short-lived cache of successful single-flag evaluations, keyed by flag key and context.
/// The oldest entry is dropped first when it is full.
#[derive(Debug)]
pub(crate) struct EvaluationCache {
    ttl: Duration,
    max_size: usize,
    entries: Mutex<HashMap<String, Entry>>,
}

#[derive(Debug)]
struct Entry {
    flag: ResolvedFlag,
    expires_at: Instant,
}

impl EvaluationCache {
    pub(crate) fn new(ttl: Duration, max_size: usize) -> Self {
        Self {
            ttl,
            max_size,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn get(&self, key: &str, context: &EvaluationContext) -> Option<ResolvedFlag> {
        let id = cache_key(key, context)?;
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match entries.get(&id) {
            Some(entry) if entry.expires_at > Instant::now() => Some(entry.flag.clone()),
            Some(_) => {
                entries.remove(&id);
                None
            }
            None => None,
        }
    }

    pub(crate) fn insert(&self, key: &str, context: &EvaluationContext, flag: ResolvedFlag) {
        let Some(id) = cache_key(key, context) else {
            return;
        };
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let expires_at = Instant::now() + self.ttl;
        entries.insert(id, Entry { flag, expires_at });
        if entries.len() > self.max_size {
            // Every entry lives for the same TTL, so the earliest expiry is the oldest entry.
            let oldest = entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
    }

    pub(crate) fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }
}

/// The flag key and the context's JSON. Attribute names are sorted, so equal contexts give
/// equal keys.
fn cache_key(key: &str, context: &EvaluationContext) -> Option<String> {
    let context = serde_json::to_string(context).ok()?;
    Some(format!("{key}\u{0}{context}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn flag(key: &str) -> ResolvedFlag {
        ResolvedFlag {
            key: key.to_owned(),
            value: Some(true.into()),
            variant: None,
            reason: "STATIC".to_owned(),
            error_code: None,
            error_details: None,
            metadata: Map::new(),
        }
    }

    #[test]
    fn keys_by_flag_and_context() {
        let cache = EvaluationCache::new(Duration::from_secs(30), 10);
        let ca = EvaluationContext::new("u").with("country", "CA");
        cache.insert("a", &ca, flag("a"));
        assert!(cache.get("a", &ca).is_some());
        assert!(cache.get("a", &EvaluationContext::new("u")).is_none());
        assert!(cache.get("b", &ca).is_none());
        cache.clear();
        assert!(cache.get("a", &ca).is_none());
    }

    #[test]
    fn drops_the_oldest_entry_when_full() {
        let cache = EvaluationCache::new(Duration::from_secs(30), 2);
        let context = EvaluationContext::new("u");
        for key in ["a", "b", "c"] {
            cache.insert(key, &context, flag(key));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(cache.get("a", &context).is_none());
        assert!(cache.get("b", &context).is_some());
        assert!(cache.get("c", &context).is_some());
    }

    #[test]
    fn expires_entries() {
        let cache = EvaluationCache::new(Duration::from_millis(1), 10);
        let context = EvaluationContext::new("u");
        cache.insert("a", &context, flag("a"));
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get("a", &context).is_none());
    }
}
