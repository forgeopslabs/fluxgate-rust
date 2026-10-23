use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use crate::transport::OfrepTransport;
use crate::{Error, ErrorCode, EvaluationContext, FlagEvaluation, FlagValue, ResolvedFlag};

/// The state of a [`FlagSnapshot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnapshotStatus {
    /// The first load has not finished.
    NotReady,
    /// The flags are current.
    Ready,
    /// A refresh failed; the last loaded flags are still served.
    Stale,
    /// The first load failed; evaluations return defaults.
    Error,
}

/// Every flag of the client's team, evaluated for one context and kept up to date by polling.
///
/// Evaluations against it are synchronous and cost no network call, which suits code that
/// reads many flags for the same subject, such as a UI or a batch job. Create one with
/// [`FluxGateClient::watch`](crate::FluxGateClient::watch).
///
/// Polling sends the last version in `If-None-Match`, so an unchanged flag set costs a `304`
/// with no body. Clones share the same flags. Polling stops when the last clone is dropped or
/// when [`close`](Self::close) is called.
///
/// ```no_run
/// # async fn run(fluxgate: fluxgate::FluxGateClient) {
/// use fluxgate::EvaluationContext;
///
/// let flags = fluxgate.watch(EvaluationContext::new("user-1").with("country", "CA"));
/// flags.ready().await;
/// let theme = flags.get_string("checkout-theme", "classic");
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct FlagSnapshot {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    transport: Arc<OfrepTransport>,
    state: RwLock<State>,
    /// The revision, sent on every change of the flags or the status.
    revision: watch::Sender<u64>,
    /// The last context epoch whose first load has finished.
    loaded_epoch: watch::Sender<u64>,
    /// Restarts the poll timer after a manual refresh.
    reset_timer: Arc<Notify>,
    task: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Debug)]
struct State {
    flags: HashMap<String, ResolvedFlag>,
    version: Option<String>,
    context: EvaluationContext,
    status: SnapshotStatus,
    error: Option<Error>,
    /// Increases on every load; a load whose generation is no longer current is dropped.
    generation: u64,
    /// Increases on every context change.
    epoch: u64,
    revision: u64,
    closed: bool,
}

impl FlagSnapshot {
    pub(crate) fn start(
        transport: Arc<OfrepTransport>,
        context: EvaluationContext,
        poll_interval: Duration,
    ) -> Self {
        let reset_timer = Arc::new(Notify::new());
        let shared = Arc::new(Shared {
            transport,
            state: RwLock::new(State {
                flags: HashMap::new(),
                version: None,
                context,
                status: SnapshotStatus::NotReady,
                error: None,
                generation: 0,
                epoch: 1,
                revision: 0,
                closed: false,
            }),
            revision: watch::Sender::new(0),
            loaded_epoch: watch::Sender::new(0),
            reset_timer: Arc::clone(&reset_timer),
            task: Mutex::new(None),
        });
        let task = tokio::spawn(poll(Arc::downgrade(&shared), reset_timer, poll_interval));
        *shared
            .task
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(task);
        Self { shared }
    }

    /// The current status.
    pub fn status(&self) -> SnapshotStatus {
        self.shared.read().status
    }

    /// The error of the last load, if it failed.
    pub fn error(&self) -> Option<Error> {
        self.shared.read().error.clone()
    }

    /// The context the flags are evaluated for.
    pub fn context(&self) -> EvaluationContext {
        self.shared.read().context.clone()
    }

    /// Increases on every change of the flags or the status.
    pub fn revision(&self) -> u64 {
        self.shared.read().revision
    }

    /// `true` after [`close`](Self::close).
    pub fn is_closed(&self) -> bool {
        self.shared.read().closed
    }

    /// Waits until the first load for the current context has finished, successfully or not.
    pub async fn ready(&self) {
        let target = self.shared.read().epoch;
        let mut loaded = self.shared.loaded_epoch.subscribe();
        // The sender lives as long as `self`, so this only ends when the load finishes.
        let _ = loaded.wait_for(|epoch| *epoch >= target).await;
    }

    /// A receiver of the [`revision`](Self::revision), which changes whenever the flags or the
    /// status change. Use it to re-render or re-read flags.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.shared.revision.subscribe()
    }

    /// Evaluates a flag from the loaded set. The default value also sets the expected type; see
    /// [`FlagValue`].
    pub fn evaluate<T: FlagValue>(&self, key: &str, default_value: T) -> FlagEvaluation<T> {
        let state = self.shared.read();
        match state.status {
            SnapshotStatus::NotReady => FlagEvaluation::error(
                key,
                default_value,
                ErrorCode::ProviderNotReady,
                "Flags are still loading",
            ),
            SnapshotStatus::Error => {
                let (code, message) = state.error.as_ref().map_or_else(
                    || (ErrorCode::General, "Flags could not be loaded".to_owned()),
                    |error| (error.code(), error.to_string()),
                );
                FlagEvaluation::error(key, default_value, code, message)
            }
            SnapshotStatus::Ready | SnapshotStatus::Stale => match state.flags.get(key) {
                Some(flag) => flag.to_evaluation(default_value),
                None => FlagEvaluation::error(
                    key,
                    default_value,
                    ErrorCode::FlagNotFound,
                    format!("Flag '{key}' does not exist for this client"),
                ),
            },
        }
    }

    /// The flag as a boolean, or `default_value`.
    pub fn get_bool(&self, key: &str, default_value: bool) -> bool {
        self.evaluate(key, default_value).value
    }

    /// The flag as a string, or `default_value`.
    pub fn get_string(&self, key: &str, default_value: &str) -> String {
        self.evaluate(key, default_value.to_owned()).value
    }

    /// The flag as an integer, or `default_value`.
    pub fn get_i64(&self, key: &str, default_value: i64) -> i64 {
        self.evaluate(key, default_value).value
    }

    /// The flag as a number, or `default_value`.
    pub fn get_f64(&self, key: &str, default_value: f64) -> f64 {
        self.evaluate(key, default_value).value
    }

    /// The flag as JSON, or `default_value`. `Value::Null` accepts a flag of any type.
    pub fn get_value(&self, key: &str, default_value: Value) -> Value {
        self.evaluate(key, default_value).value
    }

    /// `true` only when the flag evaluates to the boolean `true`.
    pub fn is_enabled(&self, key: &str) -> bool {
        self.get_bool(key, false)
    }

    /// Keys of every loaded flag.
    pub fn keys(&self) -> Vec<String> {
        self.shared.read().flags.keys().cloned().collect()
    }

    /// Switches to another context, for example after sign-in, and loads its flags. Until the
    /// load finishes, evaluations use the previous context's flags.
    pub async fn set_context(&self, context: EvaluationContext) {
        {
            let mut state = self.shared.write();
            if state.closed {
                return;
            }
            state.context = context;
            state.version = None;
            state.epoch += 1;
        }
        self.refresh().await;
    }

    /// Fetches the flags now instead of waiting for the next poll.
    pub async fn refresh(&self) {
        self.shared.refresh().await;
        self.shared.reset_timer.notify_waiters();
    }

    /// Stops polling. Evaluations keep answering from the last loaded flags.
    pub fn close(&self) {
        self.shared.write().closed = true;
        self.shared.stop();
        self.shared.loaded_epoch.send_replace(u64::MAX);
    }
}

impl Shared {
    fn read(&self) -> RwLockReadGuard<'_, State> {
        self.state
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, State> {
        self.state
            .write()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn stop(&self) {
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            task.abort();
        }
    }

    async fn refresh(&self) {
        let (generation, context, version) = {
            let mut state = self.write();
            if state.closed {
                return;
            }
            state.generation += 1;
            (
                state.generation,
                state.context.clone(),
                state.version.clone(),
            )
        };
        let result = self
            .transport
            .evaluate_all(&context, version.as_deref())
            .await;

        let mut state = self.write();
        if state.generation != generation || state.closed {
            return; // A newer load superseded this one.
        }
        let previous = (state.status, state.revision);
        let mut changed = false;
        match result {
            Ok(bulk) => {
                state.error = None;
                state.status = SnapshotStatus::Ready;
                if !bulk.not_modified {
                    changed = state.flags != bulk.flags;
                    state.flags = bulk.flags;
                    state.version = bulk.version;
                }
            }
            Err(error) => {
                state.error = Some(error);
                state.status = if state.flags.is_empty() {
                    SnapshotStatus::Error
                } else {
                    SnapshotStatus::Stale
                };
            }
        }
        if changed || state.status != previous.0 {
            state.revision = previous.1 + 1;
        }
        let (epoch, revision) = (state.epoch, state.revision);
        drop(state);

        self.revision.send_if_modified(|current| {
            let modified = *current != revision;
            *current = revision;
            modified
        });
        self.loaded_epoch.send_if_modified(|loaded| {
            let modified = *loaded < epoch;
            if modified {
                *loaded = epoch;
            }
            modified
        });
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Loads the flags, then refreshes them every `interval` until the snapshot is dropped or
/// closed. Holds only a weak reference between loads.
async fn poll(shared: Weak<Shared>, reset_timer: Arc<Notify>, interval: Duration) {
    loop {
        match shared.upgrade() {
            Some(shared) => shared.refresh().await,
            None => return,
        }
        if interval.is_zero() {
            return;
        }
        // A manual refresh restarts the wait.
        while tokio::time::timeout(interval, reset_timer.notified())
            .await
            .is_ok()
        {}
    }
}
