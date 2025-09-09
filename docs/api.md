# API Reference

stonr exposes a minimal HTTP and WebSocket interface compatible with NIP-01.

## HTTP

### `GET /healthz`
Returns `{ "status": "ok" }` when the relay is running.

### `GET /query`
Returns matching events as newline-delimited JSON (NDJSON).
Parameters mirror Nostr filter fields:

- `authors` – comma-separated list of public keys
- `kinds` – comma-separated list of kind numbers
- `d` / `t` – single `#d` or `#t` tag value
- `since` / `until` – Unix timestamps bounding `created_at`
- `limit` – maximum number of events to return

Example:

```bash
curl "http://localhost:7777/query?authors=npub1&kinds=1,30023&limit=5"
```

## WebSocket

Connect to the `BIND_WS` address and speak NIP-01 messages.
A typical subscription looks like:

```text
["REQ","sub1",{"authors":["npub1"],"kinds":[1]}]
```

For each matching event the server replies:

```text
["EVENT","sub1",{...event...}]
```

Once all events have been sent an end-of-stored-events marker is emitted:

```text
["EOSE","sub1"]
```

Close the TCP connection to terminate the subscription; `CLOSE` messages are ignored.
