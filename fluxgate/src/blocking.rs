//! A synchronous client, for code without an async runtime.
//!
//! ```no_run
//! use fluxgate::EvaluationContext;
//! use fluxgate::blocking::FluxGateClient;
//!
//! # fn run() -> Result<(), fluxgate::ConfigError> {
//! let fluxgate = FluxGateClient::new("https://edge.example.com", &std::env::var("FLUXGATE_SDK_KEY").unwrap())?;
//! let user = EvaluationContext::new("user-1").with("country", "CA");
//! let theme = fluxgate.get_string("checkout-theme", "classic", &user);
//! # Ok(())
//! # }
//! ```
//!
//! The client runs the async client on a private Tokio runtime with one worker thread, which
//! also polls [`FlagSnapshot`]s. Do not call it, or drop its last clone, from inside an async
//! runtime: Tokio panics on blocking calls there. In async code, use
//! [`crate::FluxGateClient`] instead.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::runtime::Runtime;

use crate::client::DEFAULT_POLL_INTERVAL;
use crate::{
    BulkEvaluation, ConfigError, Error, EvaluationContext, FlagEvaluation, FlagValue,
    FluxGateClientBuilder, SnapshotStatus,
};

/// Synchronous version of [`crate::FluxGateClient`]. Cheap to clone; clones share the runtime,
/// the connection pool and the cache.
#[derive(Clone, Debug)]
pub struct FluxGateClient {
    inner: crate::FluxGateClient,
    runtime: Arc<Runtime>,
}

impl FluxGateClient {
    /// A client with default settings. `sdk_key` is `<clientId>.<apiKey>`.
    ///
    /// # Errors
    ///
    /// When the base URL or the SDK key is malformed, or the runtime cannot start.
    pub fn new(base_url: &str, sdk_key: &str) -> Result<Self, ConfigError> {
        crate::FluxGateClient::builder(base_url, sdk_key).build_blocking()
    }

    /// A builder for a client with custom settings; finish it with
    /// [`build_blocking`](FluxGateClientBuilder::build_blocking).
    pub fn builder(
        base_url: impl Into<String>,
        sdk_key: impl Into<String>,
    ) -> FluxGateClientBuilder {
        crate::FluxGateClient::builder(base_url, sdk_key)
    }

    pub(crate) fn from_parts(inner: crate::FluxGateClient) -> Result<Self, ConfigError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("fluxgate")
            .enable_all()
            .build()
            .map_err(ConfigError::Runtime)?;
        Ok(Self {
            inner,
            runtime: Arc::new(runtime),
        })
    }

    /// Evaluates one flag; see [`crate::FluxGateClient::evaluate`].
    pub fn evaluate<T: FlagValue>(
        &self,
        key: &str,
        default_value: T,
        context: &EvaluationContext,
    ) -> FlagEvaluation<T> {
        self.runtime
            .block_on(self.inner.evaluate(key, default_value, context))
    }

    /// The flag as a boolean, or `default_value`.
    pub fn get_bool(&self, key: &str, default_value: bool, context: &EvaluationContext) -> bool {
        self.evaluate(key, default_value, context).value
    }

    /// The flag as a string, or `default_value`.
    pub fn get_string(
        &self,
        key: &str,
        default_value: &str,
        context: &EvaluationContext,
    ) -> String {
        self.evaluate(key, default_value.to_owned(), context).value
    }

    /// The flag as an integer, or `default_value`.
    pub fn get_i64(&self, key: &str, default_value: i64, context: &EvaluationContext) -> i64 {
        self.evaluate(key, default_value, context).value
    }

    /// The flag as a number, or `default_value`.
    pub fn get_f64(&self, key: &str, default_value: f64, context: &EvaluationContext) -> f64 {
        self.evaluate(key, default_value, context).value
    }

    /// The flag as JSON, or `default_value`. `Value::Null` accepts a flag of any type.
    pub fn get_value(&self, key: &str, default_value: Value, context: &EvaluationContext) -> Value {
        self.evaluate(key, default_value, context).value
    }

    /// `true` only when the flag evaluates to the boolean `true`.
    pub fn is_enabled(&self, key: &str, context: &EvaluationContext) -> bool {
        self.get_bool(key, false, context)
    }

    /// Evaluates every flag in one request; see [`crate::FluxGateClient::evaluate_all`].
    ///
    /// # Errors
    ///
    /// When the edge cannot be reached or rejects the call.
    pub fn evaluate_all(
        &self,
        context: &EvaluationContext,
        version: Option<&str>,
    ) -> Result<BulkEvaluation, Error> {
        self.runtime
            .block_on(self.inner.evaluate_all(context, version))
    }

    /// Loads every flag for `context` and keeps them current by polling every 30 seconds.
    pub fn watch(&self, context: EvaluationContext) -> FlagSnapshot {
        self.watch_with_interval(context, DEFAULT_POLL_INTERVAL)
    }

    /// Like [`watch`](Self::watch), polling at the given interval. `Duration::ZERO` turns
    /// polling off.
    pub fn watch_with_interval(
        &self,
        context: EvaluationContext,
        poll_interval: Duration,
    ) -> FlagSnapshot {
        let _guard = self.runtime.enter();
        FlagSnapshot {
            inner: self.inner.watch_with_interval(context, poll_interval),
            runtime: Arc::clone(&self.runtime),
        }
    }

    /// `true` when the edge answers `GET /health` with 2xx.
    pub fn is_healthy(&self) -> bool {
        self.runtime.block_on(self.inner.is_healthy())
    }

    /// Drops cached single-flag evaluations.
    pub fn clear_cache(&self) {
        self.inner.clear_cache();
    }
}

/// Synchronous version of [`crate::FlagSnapshot`].
#[derive(Clone, Debug)]
pub struct FlagSnapshot {
    inner: crate::FlagSnapshot,
    runtime: Arc<Runtime>,
}

impl FlagSnapshot {
    /// Waits until the first load for the current context has finished, successfully or not.
    pub fn wait_ready(&self) {
        self.runtime.block_on(self.inner.ready());
    }

    /// Switches to another context and waits for its flags.
    pub fn set_context(&self, context: EvaluationContext) {
        self.runtime.block_on(self.inner.set_context(context));
    }

    /// Fetches the flags now instead of waiting for the next poll.
    pub fn refresh(&self) {
        self.runtime.block_on(self.inner.refresh());
    }

    /// The current status.
    pub fn status(&self) -> SnapshotStatus {
        self.inner.status()
    }

    /// The error of the last load, if it failed.
    pub fn error(&self) -> Option<Error> {
        self.inner.error()
    }

    /// The context the flags are evaluated for.
    pub fn context(&self) -> EvaluationContext {
        self.inner.context()
    }

    /// Increases on every change of the flags or the status.
    pub fn revision(&self) -> u64 {
        self.inner.revision()
    }

    /// Evaluates a flag from the loaded set.
    pub fn evaluate<T: FlagValue>(&self, key: &str, default_value: T) -> FlagEvaluation<T> {
        self.inner.evaluate(key, default_value)
    }

    /// The flag as a boolean, or `default_value`.
    pub fn get_bool(&self, key: &str, default_value: bool) -> bool {
        self.inner.get_bool(key, default_value)
    }

    /// The flag as a string, or `default_value`.
    pub fn get_string(&self, key: &str, default_value: &str) -> String {
        self.inner.get_string(key, default_value)
    }

    /// The flag as an integer, or `default_value`.
    pub fn get_i64(&self, key: &str, default_value: i64) -> i64 {
        self.inner.get_i64(key, default_value)
    }

    /// The flag as a number, or `default_value`.
    pub fn get_f64(&self, key: &str, default_value: f64) -> f64 {
        self.inner.get_f64(key, default_value)
    }

    /// The flag as JSON, or `default_value`.
    pub fn get_value(&self, key: &str, default_value: Value) -> Value {
        self.inner.get_value(key, default_value)
    }

    /// `true` only when the flag evaluates to the boolean `true`.
    pub fn is_enabled(&self, key: &str) -> bool {
        self.inner.is_enabled(key)
    }

    /// Keys of every loaded flag.
    pub fn keys(&self) -> Vec<String> {
        self.inner.keys()
    }

    /// Stops polling.
    pub fn close(&self) {
        self.inner.close();
    }
}
