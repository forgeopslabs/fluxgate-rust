//! An axum web service that evaluates flags for each request's user.
//!
//!   GET /checkout?user=user-1&country=CA&tier=plus
//!
//! answers which checkout and theme that user gets. The service listens on
//! http://localhost:38180.
//!
//! Settings come from environment variables; `samples/.env` is read when they are not set:
//!   FLUXGATE_BASE_URL  edge server URL, default http://localhost:38081
//!   FLUXGATE_SDK_KEY   SDK key of a Backend client, <clientId>.<apiKey> (required)

use std::process::ExitCode;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use fluxgate::{EvaluationContext, FluxGateClient};
use serde::Deserialize;
use serde_json::{Value, json};

const ADDRESS: &str = "127.0.0.1:38180";

#[derive(Deserialize)]
struct CheckoutQuery {
    user: String,
    country: Option<String>,
    tier: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let base_url =
        setting("FLUXGATE_BASE_URL").unwrap_or_else(|| "http://localhost:38081".to_owned());
    let Some(sdk_key) = setting("FLUXGATE_SDK_KEY") else {
        eprintln!(
            "Set FLUXGATE_SDK_KEY to the SDK key of a FluxGate Backend client (<clientId>.<apiKey>)."
        );
        return ExitCode::from(2);
    };
    // One client for the whole service; clones share its connections and cache.
    let fluxgate = match FluxGateClient::new(&base_url, &sdk_key) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/checkout", get(checkout))
        .with_state(fluxgate);
    let listener = match tokio::net::TcpListener::bind(ADDRESS).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("Cannot listen on {ADDRESS}: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("Listening on http://{ADDRESS}, flags from {base_url}");
    println!("Try http://{ADDRESS}/checkout?user=user-1&country=CA&tier=plus");
    if let Err(error) = axum::serve(listener, app).await {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn index() -> &'static str {
    "GET /checkout?user=<id>&country=<CA|US|DE>&tier=<free|plus>\n"
}

async fn checkout(
    State(fluxgate): State<FluxGateClient>,
    Query(query): Query<CheckoutQuery>,
) -> Json<Value> {
    let mut user = EvaluationContext::new(query.user);
    if let Some(country) = query.country {
        user.set("country", country);
    }
    if let Some(tier) = query.tier {
        user.set("user_tier", tier);
    }

    let (new_checkout, theme) = tokio::join!(
        fluxgate.evaluate("new-checkout", false, &user),
        fluxgate.evaluate("checkout-theme", "classic".to_owned(), &user),
    );
    Json(json!({
        "user": user.targeting_key(),
        "checkout": if new_checkout.value { "new" } else { "classic" },
        "theme": theme.value,
        "details": {
            "new-checkout": { "reason": new_checkout.reason, "error": new_checkout.error_code.map(|code| code.as_str()) },
            "checkout-theme": {
                "variant": theme.variant,
                "reason": theme.reason,
                "error": theme.error_code.map(|code| code.as_str()),
            },
        },
    }))
}

/// An environment variable, or else its value in samples/.env.
fn setting(name: &str) -> Option<String> {
    if let Some(value) = std::env::var(name).ok().filter(|value| !value.is_empty()) {
        return Some(value);
    }
    let env_file = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../.env")).ok()?;
    env_file
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
        .map(str::to_owned)
}
