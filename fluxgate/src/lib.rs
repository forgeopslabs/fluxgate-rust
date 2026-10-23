//! Client for the [FluxGate](https://github.com/forgeopslabs) feature toggle edge server.
//!
//! It evaluates flags over [OFREP](https://openfeature.dev/specification/appendix-c), the
//! OpenFeature Remote Evaluation Protocol, authenticated with a FluxGate client's SDK key.
//!
//! ```no_run
//! use fluxgate::{EvaluationContext, FluxGateClient};
//!
//! # async fn run() -> Result<(), fluxgate::ConfigError> {
//! let fluxgate = FluxGateClient::new("https://edge.example.com", &std::env::var("FLUXGATE_SDK_KEY").unwrap())?;
//! let user = EvaluationContext::new("user-1").with("country", "CA").with("user_tier", "plus");
//!
//! if fluxgate.is_enabled("new-checkout", &user).await {
//!     // ...
//! }
//! let theme = fluxgate.evaluate("checkout-theme", "classic".to_owned(), &user).await;
//! println!("{} (variant {:?}, reason {})", theme.value, theme.variant, theme.reason);
//! # Ok(())
//! # }
//! ```
//!
//! - [`FluxGateClient`] makes one OFREP call per flag and context, cached for 30 seconds.
//!   Evaluations never fail: on any error they return your default value with an
//!   [`ErrorCode`].
//! - [`FlagSnapshot`], from [`FluxGateClient::watch`], loads every flag for one context in one
//!   call and polls for changes; evaluations against it are synchronous.
//! - [`blocking`] (feature `blocking`) is a synchronous client for code without an async
//!   runtime.
//! - [`openfeature`] (feature `openfeature`) is an OpenFeature provider.
//!
//! # Feature flags
//!
//! - `rustls` (default): TLS through rustls.
//! - `native-tls`: TLS through the operating system's library.
//! - `blocking`: the [`blocking`] module.
//! - `openfeature`: the [`openfeature`] module.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod cache;
mod client;
mod context;
mod error;
mod evaluation;
mod snapshot;
mod transport;

#[cfg(feature = "blocking")]
#[cfg_attr(docsrs, doc(cfg(feature = "blocking")))]
pub mod blocking;
#[cfg(feature = "openfeature")]
#[cfg_attr(docsrs, doc(cfg(feature = "openfeature")))]
pub mod openfeature;

pub use client::{FluxGateClient, FluxGateClientBuilder};
pub use context::EvaluationContext;
pub use error::{ConfigError, Error, ErrorCode};
pub use evaluation::{BulkEvaluation, FlagEvaluation, FlagValue, ResolvedFlag};
pub use snapshot::{FlagSnapshot, SnapshotStatus};

/// JSON values, as flags and context attributes hold them.
pub use serde_json::Value;
