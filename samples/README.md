# FluxGate Rust SDK samples

Three small programs that evaluate flags against a FluxGate stack running on your machine, and a
Docker Compose file that starts that stack.

| Sample | Shows | Address |
|---|---|---|
| [cli-sample](cli-sample/) | `FluxGateClient`, a `FlagSnapshot`, and the OpenFeature provider. Prints the flags of four sample users. | (console) |
| [blocking-sample](blocking-sample/) | `fluxgate::blocking` from a plain `fn main`, including a snapshot that switches users. | (console) |
| [axum-sample](axum-sample/) | One shared client in an axum service, evaluating flags for each request's user. | http://localhost:38180 |

You need Docker, Rust 1.85 or later, `curl`, `jq` and `openssl`. Run the commands from the
repository root.

## 1. Start a local FluxGate stack

[docker-compose.yml](docker-compose.yml) runs Postgres, the backend, the admin UI and the edge
server on their own ports and volume, so it does not clash with another FluxGate install:

| Service | Address |
|---|---|
| Admin UI | http://localhost:38000 |
| Backend REST API | http://localhost:38080/api/v1 (gRPC on 35051) |
| Edge server | http://localhost:38081 (the samples talk to this) |

```bash
docker compose -f samples/docker-compose.yml up -d
```

On Apple Silicon, use the native images instead of x86 emulation:

```bash
printf 'FLUXGATE_BACKEND_TAG=v1.2.2-arm64\nFLUXGATE_EDGE_TAG=v1.2.2-arm64\n' >> samples/.env
docker compose -f samples/docker-compose.yml up -d
```

## 2. Create the sample data

[setup.sh](setup.sh) sets up the fresh stack through the backend REST API:

- the first admin `sample-admin`, the team **Samples** and the environment **Development**,
- the context keys `country` and `user_tier`,
- a **Backend** client `rust-sdk-samples`, used by the samples and the edge server,
- an approval policy and a second user `sample-approver`, because a deployment needs an approver
  other than the requester,
- two flags, with their Development stage requested, approved and deployed:

| Flag | Type | Development rules |
|---|---|---|
| `new-checkout` | Simple (boolean) | `true` when `country` equals `CA`, otherwise off. |
| `checkout-theme` | Contextual, string variants `classic` and `express` | `express` when `country` equals `CA` and `user_tier` equals `plus`; everyone else gets an 80/20 split of `classic` and `express`. |

```bash
samples/setup.sh
```

The script writes the SDK key (`FLUXGATE_SDK_KEY`) and the sample users' passwords to
`samples/.env`. The file is git-ignored and never printed. Sign in to the admin UI as
`sample-admin` with the password from that file to look at the flags and change their rules.

## 3. Start the edge server

The edge uses the same Backend client, read from `FLUXGATE_SDK_KEY`, as its own identity:

```bash
docker compose -f samples/docker-compose.yml --profile edge up -d edge
curl -s -o /dev/null -w '%{http_code}\n' http://localhost:38081/health
```

`200` means the edge is connected to the backend.

## 4. Run the samples

The samples read `samples/.env`. Environment variables override it: `FLUXGATE_BASE_URL`
(default `http://localhost:38081`) and `FLUXGATE_SDK_KEY`.

### CLI

```bash
cargo run -p cli-sample
FLUXGATE_FLAG=checkout-theme cargo run -p cli-sample
```

```text
FluxGateClient, flag 'checkout-theme':
  user-1  CA/plus -> value="express" variant=express reason=TARGETING_MATCH
  user-2  US/free -> value="classic" variant=classic reason=TARGETING_MATCH
  ...
```

### Blocking

```bash
cargo run -p blocking-sample
```

### axum

```bash
cargo run -p axum-sample
curl 'http://localhost:38180/checkout?user=user-1&country=CA&tier=plus'
```

```json
{"checkout":"new","theme":"express","user":"user-1","details":{...}}
```

## Assignments are sticky

The edge remembers the variant it first served to a targeting key. If you change a flag's rules,
or a user's attributes, a user who already got an answer can keep it. Use new user IDs to see the
new rules.

## Start over

```bash
docker compose -f samples/docker-compose.yml --profile edge down -v
rm samples/.env
```

## Troubleshooting

| Symptom | Cause |
|---|---|
| `error=UNAUTHORIZED` | Wrong SDK key, or the client was deleted. |
| `error=GENERAL` with "Could not reach the edge" | The edge is not running, or `FLUXGATE_BASE_URL` is wrong. |
| `error=FLAG_NOT_FOUND` | No flag with that key in the client's team. |
| `reason=DISABLED` for everyone | The flag's Development stage is not deployed. |
| `setup.sh` fails at `POST /admins` | The stack is not fresh. Start over (see above). |
