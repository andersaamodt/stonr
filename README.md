# Stonr

Stonr is a file-backed [Nostr](https://github.com/nostr-protocol/nostr) relay implemented in Rust. It stores each event as a standalone JSON file and serves them over HTTP and NIP-01 WebSockets. Optional upstream relays can be mirrored through a Tor SOCKS proxy.

## How it works

Events are stored as individual JSON files on disk, grouped by the day they
were received (`events/<YYYY>/<MM>/<DD>/<id>.json`). Plain-text index files and
symlink “mirrors” allow quick lookup by author, kind, or tag without scanning
the entire tree. The pieces fit together like this:

```
         +-----------+       +-------+
 client  | HTTP / WS | <---> | Store |
   ^     +-----------+       +-------+
   |            ^               ^
   |            |               |
   +-------- Mirror ------------+
```

- **HTTP** serves `/healthz`, `/query`, and a NIP‑11 relay info document.
- **WebSocket** handles minimal NIP‑01 `REQ` → `EVENT` → `EOSE` flows.
- **Mirror** connects to upstream relays (optionally through Tor) and writes
  received events into the store.

### Quick workflow

```bash
# 1. Create the storage directory structure
stonr init --env .env

# 2. Ingest a sample event into the store
stonr ingest sample.json

# 3. Start the HTTP and WebSocket servers
stonr serve --env .env

# 4. Query events over HTTP
curl "http://localhost:7777/query?authors=npub1&kinds=1"
```

See [docs/api.md](docs/api.md) for HTTP and WebSocket details,
[docs/mirroring.md](docs/mirroring.md) for mirroring setups, and
[docs/onion.md](docs/onion.md) for Tor deployment.

## Configuration
Runtime settings are read from a `.env` file:

| Variable | Description | Example | Default |
| --- | --- | --- | --- |
| `STORE_ROOT` | Directory to store event files | `/srv/stonr` | _required_ |
| `BIND_HTTP` | HTTP listen address | `127.0.0.1:7777` | _required_ |
| `BIND_WS` | WebSocket listen address | `127.0.0.1:7778` | _required_ |
| `VERIFY_SIG` | `1` to verify Schnorr signatures on ingest | `1` | `0` |
| `RELAYS_UPSTREAM` | Optional bootstrap list of upstream relays | `wss://relay.example` | _none_ |
| `TOR_SOCKS` | Tor SOCKS proxy address ([details](docs/onion.md)) | `127.0.0.1:9050` | _none_ |

Example `.env`:

```
STORE_ROOT=/srv/stonr
BIND_HTTP=127.0.0.1:7777
BIND_WS=127.0.0.1:7778
```

Mirroring configuration—relay URLs, subscriptions, and filters—is managed at
runtime through the CLI:

```bash
stonr mirror add-relay wss://relay.example
stonr mirror add-request wss://relay.example default \
  --author npub1example --kind 1 --tag t=nostr --since 1700000000
stonr mirror list
```

## CLI

```
stonr init --env .env
stonr ingest events/*.json
stonr reindex --env .env
stonr serve --env .env
stonr verify --env .env --sample 1000
stonr mirror --help
```

## Build and Test

```
cargo build
cargo test
cargo tarpaulin --timeout 120 --out Lcov
```

The ports above are examples; when running behind Tor the [onion guide](docs/onion.md)
uses `3000/3001` to match the `torrc` example. Choose values that align across
your configuration.
