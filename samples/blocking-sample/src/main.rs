//! Evaluates the sample flags with the blocking client, from a plain `fn main` without an
//! async runtime.
//!
//! Settings come from environment variables; `samples/.env` is read when they are not set:
//!   FLUXGATE_BASE_URL  edge server URL, default http://localhost:38081
//!   FLUXGATE_SDK_KEY   SDK key of a Backend client, <clientId>.<apiKey> (required)
//!
//! Exits with status 1 when an evaluation fails.

use std::process::ExitCode;

use fluxgate::EvaluationContext;
use fluxgate::blocking::FluxGateClient;

fn main() -> ExitCode {
    let base_url =
        setting("FLUXGATE_BASE_URL").unwrap_or_else(|| "http://localhost:38081".to_owned());
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
    let mut failed = false;

    println!("Blocking client:");
    for (key, country, tier) in [("user-1", "CA", "plus"), ("user-2", "US", "free")] {
        let user = EvaluationContext::new(key)
            .with("country", country)
            .with("user_tier", tier);
        let checkout = fluxgate.evaluate("new-checkout", false, &user);
        let theme = fluxgate.evaluate("checkout-theme", "classic".to_owned(), &user);
        println!(
            "  {key} {country}/{tier} -> new-checkout={} checkout-theme={}",
            checkout.value, theme.value
        );
        failed |= checkout.is_error() || theme.is_error();
    }

    // The snapshot polls on the client's background thread.
    let snapshot = fluxgate.watch(EvaluationContext::new("user-3").with("country", "CA"));
    snapshot.wait_ready();
    println!(
        "\nSnapshot for user-3 (CA): status {:?}, new-checkout={}",
        snapshot.status(),
        snapshot.is_enabled("new-checkout")
    );
    // For example after another user signs in. (The edge keeps each user's first answer, so
    // changing only the country of user-3 would not change its result.)
    snapshot.set_context(EvaluationContext::new("user-4").with("country", "DE"));
    println!(
        "After switching to user-4 (DE): new-checkout={}",
        snapshot.is_enabled("new-checkout")
    );
    failed |= snapshot.error().is_some();

    if failed {
        eprintln!("\nSome evaluations failed. Check FLUXGATE_SDK_KEY and the edge logs.");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
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
