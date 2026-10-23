//! Evaluates flags for four sample users with `FluxGateClient`, a `FlagSnapshot` and
//! OpenFeature.
//!
//! Settings come from environment variables; `samples/.env` is read when they are not set:
//!   FLUXGATE_BASE_URL  edge server URL, default http://localhost:38081
//!   FLUXGATE_SDK_KEY   SDK key of a Backend client, <clientId>.<apiKey> (required)
//!   FLUXGATE_FLAG      flag key to evaluate, default new-checkout
//!
//! Exits with status 1 when an evaluation fails.

use std::process::ExitCode;

use fluxgate::openfeature::FluxGateProvider;
use fluxgate::{EvaluationContext, FluxGateClient, Value};
use open_feature::OpenFeature;

const USERS: [(&str, &str, &str); 4] = [
    ("user-1", "CA", "plus"),
    ("user-2", "US", "free"),
    ("user-3", "US", "plus"),
    ("user-4", "DE", "free"),
];

#[tokio::main]
async fn main() -> ExitCode {
    let base_url =
        setting("FLUXGATE_BASE_URL").unwrap_or_else(|| "http://localhost:38081".to_owned());
    let flag = setting("FLUXGATE_FLAG").unwrap_or_else(|| "new-checkout".to_owned());
    let Some(sdk_key) = setting("FLUXGATE_SDK_KEY") else {
        eprintln!(
            "Set FLUXGATE_SDK_KEY to the SDK key of a FluxGate Backend client (<clientId>.<apiKey>)."
        );
        return ExitCode::from(2);
    };
    let fluxgate = match FluxGateClient::new(&base_url, &sdk_key) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    println!("Edge {base_url} healthy: {}", fluxgate.is_healthy().await);
    let mut failed = false;

    println!("\nFluxGateClient, flag '{flag}':");
    for (key, country, tier) in USERS {
        // A null default accepts any flag type: boolean, string, number or JSON.
        let evaluation = fluxgate
            .evaluate(&flag, Value::Null, &user(key, country, tier))
            .await;
        println!(
            "  {key:<7} {country}/{tier:<4} -> value={} variant={} reason={}{}",
            evaluation.value,
            evaluation.variant.as_deref().unwrap_or("-"),
            evaluation.reason,
            evaluation
                .error_code
                .map(|code| format!(" error={code}"))
                .unwrap_or_default(),
        );
        failed |= evaluation.is_error();
    }

    // A snapshot loads every flag for one user in one call; reads are then synchronous.
    let (key, country, tier) = USERS[0];
    let snapshot = fluxgate.watch(user(key, country, tier));
    snapshot.ready().await;
    println!("\nFlagSnapshot for {key}: status {:?}", snapshot.status());
    let mut keys = snapshot.keys();
    keys.sort();
    for flag_key in keys {
        let evaluation = snapshot.evaluate(&flag_key, Value::Null);
        println!(
            "  {flag_key:<15} -> value={} variant={}",
            evaluation.value,
            evaluation.variant.as_deref().unwrap_or("-")
        );
    }
    failed |= snapshot.error().is_some();
    snapshot.close();

    let mut api = OpenFeature::singleton_mut().await;
    api.set_provider(FluxGateProvider::new(fluxgate)).await;
    let client = api.create_client();
    drop(api);

    println!("\nOpenFeature, flags 'new-checkout' and 'checkout-theme':");
    for (key, country, tier) in USERS {
        let context = open_feature::EvaluationContext::default()
            .with_targeting_key(key)
            .with_custom_field("country", country)
            .with_custom_field("user_tier", tier);
        let enabled = client
            .get_bool_details("new-checkout", Some(&context), None)
            .await;
        let theme = client
            .get_string_details("checkout-theme", Some(&context), None)
            .await;
        match (enabled, theme) {
            (Ok(enabled), Ok(theme)) => println!(
                "  {key:<7} -> new-checkout={} checkout-theme={} (variant {})",
                enabled.value,
                theme.value,
                theme.variant.as_deref().unwrap_or("-")
            ),
            (enabled, theme) => {
                failed = true;
                println!("  {key:<7} -> {:?} {:?}", enabled.err(), theme.err());
            }
        }
    }

    if failed {
        eprintln!(
            "\nSome evaluations failed. Check FLUXGATE_SDK_KEY, the flag key and the edge logs."
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn user(key: &str, country: &str, tier: &str) -> EvaluationContext {
    EvaluationContext::new(key)
        .with("country", country)
        .with("user_tier", tier)
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
