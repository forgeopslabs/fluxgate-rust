use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::cache::EvaluationCache;
use crate::transport::OfrepTransport;
use crate::{
    BulkEvaluation, ConfigError, Error, ErrorCode, EvaluationContext, FlagEvaluation, FlagSnapshot,
    FlagValue, ResolvedFlag,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(30);
const DEFAULT_CACHE_MAX_SIZE: usize = 1000;
pub(crate) const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Evaluates FluxGate flags against an edge server over OFREP.
///
/// ```no_run
/// use fluxgate::{EvaluationContext, FluxGateClient};
///
/// # async fn run() -> Result<(), fluxgate::ConfigError> {
/// let fluxgate = FluxGateClient::new("https://edge.example.com", &std::env::var("FLUXGATE_SDK_KEY").unwrap())?;
/// let user = EvaluationContext::new("user-1").with("country", "CA");
/// if fluxgate.is_enabled("new-checkout", &user).await {
///     // ...
/// }
/// # Ok(())
/// # }
/// ```
///
/// Evaluations never fail: on any error they return the default value with an
/// [`error_code`](FlagEvaluation::error_code). Single-flag results are cached briefly per flag
/// and context. To keep every flag for one context in memory, use [`watch`](Self::watch).
///
/// The client is cheap to clone; clones share the connection pool and the cache. It needs a
/// Tokio runtime. Without one, use [`blocking::FluxGateClient`](crate::blocking::FluxGateClient)
/// (feature `blocking`).
#[derive(Clone, Debug)]
pub struct FluxGateClient {
    transport: Arc<OfrepTransport>,
    cache: Option<Arc<EvaluationCache>>,
}

impl FluxGateClient {
    /// A client with default settings.
    ///
    /// `sdk_key` is the SDK key of a FluxGate client, `<clientId>.<apiKey>`.
    ///
    /// # Errors
    ///
    /// When the base URL is not an `http://` or `https://` URL or the SDK key is malformed.
    pub fn new(base_url: &str, sdk_key: &str) -> Result<Self, ConfigError> {
        Self::builder(base_url, sdk_key).build()
    }

    /// A builder for a client with custom timeouts, retries or caching.
    pub fn builder(
        base_url: impl Into<String>,
        sdk_key: impl Into<String>,
    ) -> FluxGateClientBuilder {
        FluxGateClientBuilder {
            base_url: base_url.into(),
            sdk_key: sdk_key.into(),
            timeout: DEFAULT_TIMEOUT,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            cache_enabled: true,
            cache_ttl: DEFAULT_CACHE_TTL,
            cache_max_size: DEFAULT_CACHE_MAX_SIZE,
        }
    }

    /// Evaluates one flag. The default value is returned on any error, and its type is the
    /// expected type of the flag; see [`FlagValue`].
    pub async fn evaluate<T: FlagValue>(
        &self,
        key: &str,
        default_value: T,
        context: &EvaluationContext,
    ) -> FlagEvaluation<T> {
        match self.resolve(key, context).await {
            Ok(flag) => flag.to_evaluation(default_value),
            Err(error) => {
                FlagEvaluation::error(key, default_value, error.code(), error.to_string())
            }
        }
    }

    /// The flag as a boolean, or `default_value`.
    pub async fn get_bool(
        &self,
        key: &str,
        default_value: bool,
        context: &EvaluationContext,
    ) -> bool {
        self.evaluate(key, default_value, context).await.value
    }

    /// The flag as a string, or `default_value`.
    pub async fn get_string(
        &self,
        key: &str,
        default_value: &str,
        context: &EvaluationContext,
    ) -> String {
        self.evaluate(key, default_value.to_owned(), context)
            .await
            .value
    }

    /// The flag as an integer, or `default_value`.
    pub async fn get_i64(&self, key: &str, default_value: i64, context: &EvaluationContext) -> i64 {
        self.evaluate(key, default_value, context).await.value
    }

    /// The flag as a number, or `default_value`.
    pub async fn get_f64(&self, key: &str, default_value: f64, context: &EvaluationContext) -> f64 {
        self.evaluate(key, default_value, context).await.value
    }

    /// The flag as JSON, or `default_value`. `Value::Null` accepts a flag of any type.
    pub async fn get_value(
        &self,
        key: &str,
        default_value: Value,
        context: &EvaluationContext,
    ) -> Value {
        self.evaluate(key, default_value, context).await.value
    }

    /// `true` only when the flag evaluates to the boolean `true`.
    pub async fn is_enabled(&self, key: &str, context: &EvaluationContext) -> bool {
        self.get_bool(key, false, context).await
    }

    /// Evaluates every flag of the client's team in one request. Pass the previous result's
    /// [`version`](BulkEvaluation::version) to get `not_modified` when nothing changed.
    ///
    /// # Errors
    ///
    /// Unlike [`evaluate`](Self::evaluate), this fails when the edge cannot be reached or rejects
    /// the call.
    pub async fn evaluate_all(
        &self,
        context: &EvaluationContext,
        version: Option<&str>,
    ) -> Result<BulkEvaluation, Error> {
        self.transport.evaluate_all(context, version).await
    }

    /// Loads every flag for `context` and keeps them current by polling every 30 seconds. Use
    /// the snapshot for synchronous evaluations.
    ///
    /// # Panics
    ///
    /// When called outside a Tokio runtime.
    pub fn watch(&self, context: EvaluationContext) -> FlagSnapshot {
        self.watch_with_interval(context, DEFAULT_POLL_INTERVAL)
    }

    /// Like [`watch`](Self::watch), polling at the given interval. `Duration::ZERO` turns
    /// polling off.
    ///
    /// # Panics
    ///
    /// When called outside a Tokio runtime.
    pub fn watch_with_interval(
        &self,
        context: EvaluationContext,
        poll_interval: Duration,
    ) -> FlagSnapshot {
        FlagSnapshot::start(Arc::clone(&self.transport), context, poll_interval)
    }

    /// `true` when the edge answers `GET /health` with 2xx.
    pub async fn is_healthy(&self) -> bool {
        self.transport.health().await
    }

    /// Drops cached single-flag evaluations.
    pub fn clear_cache(&self) {
        if let Some(cache) = &self.cache {
            cache.clear();
        }
    }

    /// Resolves one flag without converting it, through the cache.
    pub(crate) async fn resolve(
        &self,
        key: &str,
        context: &EvaluationContext,
    ) -> Result<ResolvedFlag, Error> {
        if key.is_empty() {
            return Err(Error::new(ErrorCode::General, "Flag key must not be empty"));
        }
        if !context.has_targeting_key() {
            // The edge rejects an empty targeting key; save the round trip.
            return Err(Error::new(
                ErrorCode::TargetingKeyMissing,
                "Evaluation context has no targeting key",
            ));
        }
        if let Some(flag) = self
            .cache
            .as_ref()
            .and_then(|cache| cache.get(key, context))
        {
            return Ok(flag);
        }
        let flag = self.transport.evaluate(key, context).await?;
        if let Some(cache) = &self.cache {
            if flag.error_code.is_none() {
                cache.insert(key, context, flag.clone());
            }
        }
        Ok(flag)
    }
}

/// Settings for a [`FluxGateClient`]. Create it with [`FluxGateClient::builder`].
#[derive(Clone)]
#[must_use]
pub struct FluxGateClientBuilder {
    base_url: String,
    sdk_key: String,
    timeout: Duration,
    max_attempts: u32,
    initial_backoff: Duration,
    cache_enabled: bool,
    cache_ttl: Duration,
    cache_max_size: usize,
}

impl std::fmt::Debug for FluxGateClientBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the SDK key.
        f.debug_struct("FluxGateClientBuilder")
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .field("max_attempts", &self.max_attempts)
            .field("initial_backoff", &self.initial_backoff)
            .field("cache_enabled", &self.cache_enabled)
            .field("cache_ttl", &self.cache_ttl)
            .field("cache_max_size", &self.cache_max_size)
            .finish_non_exhaustive()
    }
}

impl FluxGateClientBuilder {
    /// Timeout for one edge request. Default 5 seconds.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Attempts per call, counting the first. Default 3; `0` counts as 1.
    pub fn max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts.max(1);
        self
    }

    /// Pause before the first retry, doubled on each retry. Default 100 ms.
    pub fn initial_backoff(mut self, initial_backoff: Duration) -> Self {
        self.initial_backoff = initial_backoff;
        self
    }

    /// How long a cached single-flag evaluation is reused. Default 30 seconds.
    pub fn cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Maximum number of cached evaluations. Default 1000; `0` counts as 1.
    pub fn cache_max_size(mut self, max_size: usize) -> Self {
        self.cache_max_size = max_size.max(1);
        self
    }

    /// Turns the single-flag cache off.
    pub fn disable_cache(mut self) -> Self {
        self.cache_enabled = false;
        self
    }

    /// Creates the client.
    ///
    /// # Errors
    ///
    /// When the base URL is not an `http://` or `https://` URL, the SDK key is malformed, or the
    /// HTTP client cannot be created.
    pub fn build(self) -> Result<FluxGateClient, ConfigError> {
        let base_url = self.base_url.trim().trim_end_matches('/').to_owned();
        let scheme_ok = base_url.starts_with("http://") || base_url.starts_with("https://");
        if !scheme_ok || base_url.ends_with("://") {
            return Err(ConfigError::InvalidBaseUrl(self.base_url));
        }
        match self.sdk_key.split_once('.') {
            Some((client_id, api_key)) if !client_id.is_empty() && !api_key.is_empty() => {}
            _ => return Err(ConfigError::InvalidSdkKey),
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("fluxgate-rust/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| ConfigError::HttpClient(Box::new(error)))?;
        let transport = OfrepTransport {
            http,
            base_url,
            authorization: format!("Bearer {}", self.sdk_key),
            timeout: if self.timeout.is_zero() {
                DEFAULT_TIMEOUT
            } else {
                self.timeout
            },
            max_attempts: self.max_attempts,
            initial_backoff: self.initial_backoff,
        };
        let cache = self
            .cache_enabled
            .then(|| Arc::new(EvaluationCache::new(self.cache_ttl, self.cache_max_size)));
        Ok(FluxGateClient {
            transport: Arc::new(transport),
            cache,
        })
    }

    /// Creates a [`blocking::FluxGateClient`](crate::blocking::FluxGateClient).
    ///
    /// # Errors
    ///
    /// Like [`build`](Self::build), and when the client's runtime cannot start.
    #[cfg(feature = "blocking")]
    #[cfg_attr(docsrs, doc(cfg(feature = "blocking")))]
    pub fn build_blocking(self) -> Result<crate::blocking::FluxGateClient, ConfigError> {
        crate::blocking::FluxGateClient::from_parts(self.build()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_settings() {
        assert!(FluxGateClient::new("http://edge.test/", "client.key").is_ok());
        assert!(matches!(
            FluxGateClient::new("edge.test", "client.key"),
            Err(ConfigError::InvalidBaseUrl(_))
        ));
        assert!(matches!(
            FluxGateClient::new("https://", "client.key"),
            Err(ConfigError::InvalidBaseUrl(_))
        ));
        for key in ["", "nodot", ".key", "client."] {
            assert!(matches!(
                FluxGateClient::new("http://edge.test", key),
                Err(ConfigError::InvalidSdkKey)
            ));
        }
    }

    #[test]
    fn debug_output_hides_the_sdk_key() {
        let builder = FluxGateClient::builder("http://edge.test", "client.secret-key");
        assert!(!format!("{builder:?}").contains("secret-key"));
        let client = builder.build().unwrap();
        assert!(!format!("{client:?}").contains("secret-key"));
    }
}
