# Stonr

Stonr is a file-backed [Nostr](https://github.com/nostr-protocol/nostr) relay implemented in Rust. It stores each event as a standalone JSON file and serves them over HTTP and NIP-01 WebSockets. Optional upstream relays can be siphoned through a Tor SOCKS proxy.

## Configuration

Configuration is provided via an `.env` file:

```
STORE_ROOT=/srv/stonr
BIND_HTTP=127.0.0.1:7777
BIND_WS=127.0.0.1:7778
RELAYS_UPSTREAM=ws://relay.example
TOR_SOCKS=127.0.0.1:9050
FILTER_AUTHORS=npub1...
FILTER_KINDS=1,30023
FILTER_TAG_T=essay,philosophy
FILTER_SINCE_MODE=cursor
VERIFY_SIG=0
```

## CLI

```
stonr init --env .env
stonr ingest events/*.json
stonr reindex --env .env
stonr serve --env .env
stonr verify --env .env --sample 1000
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

See [docs/onion.md](docs/onion.md) for Tor deployment details.
