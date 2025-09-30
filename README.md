# Stonr

Stonr is a file-backed [Nostr](https://github.com/nostr-protocol/nostr) relay implemented in Rust. It stores each event as a standalone JSON file and serves them over HTTP and NIP-01 WebSockets. Optional upstream relays can be mirrored through a Tor SOCKS proxy.

## How it works

Events are stored as individual JSON files on disk. Plain-text index files and
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

Running any command will bootstrap `.env` with sensible defaults if it is missing,
placing `stonr-data/` next to the config file.

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
| `RELAYS_UPSTREAM` | Comma‑separated upstream relays to mirror | `wss://relay.example` | _none_ |
| `TOR_SOCKS` | Tor SOCKS proxy address ([details](docs/onion.md)) | `127.0.0.1:9050` | _none_ |
| `FILTER_AUTHORS` | Authors to mirror, comma‑separated | `npub1...,npub2...` | _none_ |
| `FILTER_KINDS` | Kind numbers to mirror | `1,30023` | _none_ |
| `FILTER_TAG_T` | `#t` tag values to mirror | `essay,philosophy` | _none_ |
| `FILTER_SINCE_MODE` | `cursor` or `fixed:<unix>` start time | `fixed:1700000000` | `cursor` |

Example `.env`:

```
STORE_ROOT=/srv/stonr
BIND_HTTP=127.0.0.1:7777
BIND_WS=127.0.0.1:7778
RELAYS_UPSTREAM=wss://relay.example
FILTER_KINDS=1
FILTER_SINCE_MODE=cursor
```

## Mirroring upstream relays

Set `RELAYS_UPSTREAM` to a comma-separated list of relay URLs in your `.env`
file. Optional `FILTER_AUTHORS`, `FILTER_KINDS`, `FILTER_TAG_T`, and
`FILTER_SINCE_MODE` values narrow the subscription. After updating the config,
run `stonr serve --env .env` to connect and start ingesting mirrored events. You
can also manage entries with `stonr mirror add <url>` (which verifies the relay
before saving it) and `stonr mirror remove <url>`. See
[docs/mirroring.md](docs/mirroring.md) for detailed examples.

## CLI

```
stonr init --env .env
stonr ingest events/*.json
stonr reindex --env .env
stonr serve --env .env
stonr verify --env .env --sample 1000
stonr mirror add wss://relay.example
stonr mirror remove wss://relay.example
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
