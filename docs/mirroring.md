# Mirroring upstream relays

Stonr can subscribe to other relays and write received events into the local
store. Each upstream connection can host multiple independent Nostr
subscriptions (`REQ`s), and every subscription maintains its own resume cursor
so it can pick up where it left off.

## Managing relays and requests

Use the `stonr mirror` subcommands to manage upstream configuration while the
server is running:

```bash
# Register an upstream relay
stonr mirror add-relay wss://relay.example

# Create a subscription named "timeline" that only mirrors specific authors
stonr mirror add-request wss://relay.example timeline \
  --author npub1abc... --author npub1def... --kind 1 --tag t=nostr

# Add a second REQ on the same connection that tracks replaceable events
stonr mirror add-request wss://relay.example profiles \
  --kind 0 --no-cursor --limit 100

# Inspect the current configuration
stonr mirror list
```

Requests can be updated or removed with `update-request` and `remove-request`.
All configuration is stored under `STORE_ROOT/mirror/relays/`, so changes are
picked up automatically by a running `stonr serve` process.

## Starting the mirror

Once relays and requests are configured, start the server as usual. The mirror
manager reloads configuration every few seconds and applies any pending
changes:

```bash
stonr serve --env .env
```

## Mirroring through Tor

Set `TOR_SOCKS` in the `.env` file to proxy upstream connections through Tor:

```bash
TOR_SOCKS=127.0.0.1:9050
```

See [docs/onion.md](onion.md) for more on running stonr as a Tor hidden service.
