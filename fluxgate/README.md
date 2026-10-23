# fluxgate

Rust client for the FluxGate feature toggle edge server. It evaluates flags over
[OFREP](https://openfeature.dev/specification/appendix-c) (the OpenFeature Remote Evaluation
Protocol), authenticated with a FluxGate client's SDK key.

```toml
[dependencies]
fluxgate = "1"
```

```rust
use fluxgate::{EvaluationContext, FluxGateClient};

let fluxgate = FluxGateClient::new("https://edge.example.com", &std::env::var("FLUXGATE_SDK_KEY").unwrap())?;
let user = EvaluationContext::new("user-1").with("country", "CA").with("user_tier", "plus");

if fluxgate.is_enabled("new-checkout", &user).await {
    // ...
}

let theme = fluxgate.evaluate("checkout-theme", "classic".to_owned(), &user).await;
println!("{} (variant {:?}, reason {})", theme.value, theme.variant, theme.reason);
```

## What's in the crate

- **`FluxGateClient`**: one OFREP call per flag and context, with retries, timeouts and a
  30-second cache. Evaluations never fail. On any error they return your default value with an
  `error_code`. The client needs a Tokio runtime and is cheap to clone.
- **`FlagSnapshot`**, from `client.watch(context)`: loads every flag for one context in one call
  and polls every 30 seconds with `If-None-Match`, so an unchanged flag set costs a `304`. Reads
  are synchronous. `subscribe()` notifies you of changes.
- **`fluxgate::blocking`** (feature `blocking`): the same API for code without an async runtime.
- **`fluxgate::openfeature::FluxGateProvider`** (feature `openfeature`): an
  [OpenFeature](https://openfeature.dev) provider for the `open-feature` crate.

| Feature | Default | |
|---|---|---|
| `rustls` | yes | TLS through rustls. |
| `native-tls` | no | TLS through the operating system's library. Use with `default-features = false`. |
| `blocking` | no | The synchronous client. |
| `openfeature` | no | The OpenFeature provider. |

Rust 1.85 or later.

## SDK keys

Create a **Backend** client in the FluxGate admin UI (**Clients**). Its SDK key is
`<clientId>.<apiKey>`. Keep it secret.

## How evaluation works

- Every evaluation needs a targeting key, the ID that decides the user's bucket in percentage
  splits. `EvaluationContext::new("user-1")` sets it.
- The default value sets the expected type: a `bool`, `String`, `i64`, `f64` or
  `serde_json::Value` (`Value::Null` accepts any flag). A boolean flag read with a string default
  is a `TYPE_MISMATCH`.
- A flag that is disabled or not deployed for the client's environment answers `false` with no
  variant, whatever its type. For non-boolean reads that yields your default value.
- The edge remembers the variant it first served to a targeting key. After you change a flag's
  rules, a user who already got an answer can keep it.

See the [repository](https://github.com/forgeopslabs/fluxgate-rust) for samples that run against a
local FluxGate stack.

## License

Apache License 2.0
