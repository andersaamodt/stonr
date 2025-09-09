# Mirroring upstream relays

stonr can subscribe to other relays and write received events into the local store.

## Basic mirroring

Example `.env` that mirrors two relays and filters by kind and author:

```
STORE_ROOT=/srv/stonr
BIND_HTTP=127.0.0.1:7777
BIND_WS=127.0.0.1:7778
RELAYS_UPSTREAM=wss://relay1.example,wss://relay2.example
FILTER_AUTHORS=npub1abc...,npub1def...
FILTER_KINDS=1,30023
FILTER_TAG_T=nostr,rust
FILTER_SINCE_MODE=cursor
```

`RELAYS_UPSTREAM` lists the relays to subscribe to. The optional `FILTER_*`
variables restrict which events are requested:

- `FILTER_AUTHORS` limits to specific pubkeys
- `FILTER_KINDS` limits to event kinds
- `FILTER_TAG_T` limits to `#t` tag values
- `FILTER_SINCE_MODE` chooses the starting point. `cursor` resumes from the last
  saved cursor; `fixed:<unix>` starts from a fixed timestamp.

Start mirroring with:

```bash
stonr serve --env .env
```

## Mirroring through Tor

Add a Tor SOCKS proxy to route upstream connections through Tor:

```
TOR_SOCKS=127.0.0.1:9050
RELAYS_UPSTREAM=wss://relay.onion
```

See [docs/onion.md](onion.md) for more on running stonr as a Tor hidden service.
