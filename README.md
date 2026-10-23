# FluxGate Rust SDK

Rust client for the FluxGate feature toggle edge server, published as the
[`fluxgate`](https://crates.io/crates/fluxgate) crate. It evaluates flags over
[OFREP](https://openfeature.dev/specification/appendix-c) (the OpenFeature Remote Evaluation
Protocol), authenticated with a FluxGate client's SDK key.

```toml
[dependencies]
fluxgate = "1"
```

```rust
use fluxgate::{EvaluationContext, FluxGateClient};

let fluxgate = FluxGateClient::new("https://edge.example.com", &sdk_key)?;
let user = EvaluationContext::new("user-1").with("country", "CA");

if fluxgate.is_enabled("new-checkout", &user).await {
    // ...
}
```

- `FluxGateClient`: async, with retries, timeouts and a short cache. Evaluations never fail; on
  errors they return your default value with an error code.
- `FlagSnapshot` (`client.watch(context)`): every flag for one context in memory, polled with
  ETags, read synchronously.
- `fluxgate::blocking` (feature `blocking`): a synchronous client.
- `fluxgate::openfeature` (feature `openfeature`): an OpenFeature provider.

See the [crate README](fluxgate/README.md) for details.

[samples/](samples/) holds a CLI, a blocking program and an axum service that run against a local
FluxGate stack, which `samples/docker-compose.yml` starts.

## Building

```bash
cargo test --workspace --all-features
```

Rust 1.85 or later for the library; the tests and samples follow current stable. CI runs on Linux,
macOS and Windows, checks every feature on its own, and builds the library with Rust 1.85.

## Releasing

`.github/workflows/release.yml` publishes the crate to crates.io when a GitHub release is published.
The tag sets the version: tag `v1.2.0` publishes `1.2.0`. The crate in the repository stays on
`0.0.0-dev`.

Publishing authenticates in one of two ways:

- **`CARGO_REGISTRY_TOKEN` repository secret:** a crates.io API token with the `publish-update`
  scope (and `publish-new` for the first release) for the `fluxgate` crate.
- **crates.io [trusted publishing](https://crates.io/docs/trusted-publishing):** without the
  secret, the workflow gets a short-lived token through GitHub OIDC. Configure this repository and
  `release.yml` as the crate's trusted publisher on crates.io first; that needs the crate to exist,
  so the first release uses a token.

A re-run of a release whose version is already on crates.io skips the publish.

## License

Apache License 2.0
