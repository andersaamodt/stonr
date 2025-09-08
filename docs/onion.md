# Running stonr over Tor

stonr can expose its HTTP and WebSocket interfaces as Tor hidden services.

## torrc configuration

Add this to your `torrc`:

```
HiddenServiceDir /var/lib/tor/stonr/
HiddenServiceVersion 3
HiddenServicePort 80 127.0.0.1:3000
HiddenServicePort 443 127.0.0.1:3001
```

Restart `tor`. The generated hostname will be placed in the `HiddenServiceDir`.

## Binding stonr services

stonr reads several environment variables to bind its services:

- `BIND_HTTP` – HTTP listening address (default `127.0.0.1:3000`)
- `BIND_WS` – WebSocket listening address (default `127.0.0.1:3001`)
- `TOR_SOCKS` – Tor SOCKS proxy address (default `127.0.0.1:9050`)

Run `stonr` with these variables set to match the ports configured in `torrc`:

```bash
BIND_HTTP=127.0.0.1:3000 \
BIND_WS=127.0.0.1:3001 \
TOR_SOCKS=127.0.0.1:9050 \
stonr
```

Now stonr will accept connections from Tor via the hidden service.
