# Stonr specification additions

## Mirroring configuration
- Upstream relays are managed at runtime with `stonr mirror` subcommands.
- Each relay may define multiple named requests; every request corresponds to a
  Nostr `REQ` message issued on the relay's WebSocket connection.
- Request filters support authors, kinds, tag selectors, time bounds, limits,
  and optional cursor persistence. Filters are stored under
  `STORE_ROOT/mirror/relays/<relay-hash>/relay.json`.
- `stonr serve` watches the configuration directory and reloads requests
  without restarting the server. Cursor files are tracked per relay/request pair
  under `cursor/`.

## Event storage layout
- Ingested events are stored at
  `STORE_ROOT/events/<YYYY>/<MM>/<DD>/<event-id>.json`, grouped by the day they
  were received.
- A `events/by-id/<event-id>.path` mapping file records the relative path for
  each event so legacy hashed layouts can still be located.
- Mirror symlinks reference the absolute event path for authors and kinds.
