
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

Restart `tor`. The generated onion hostname will be written to
`HiddenServiceDir/hostname`.

## Binding stonr services

stonr requires a few environment variables—none have built-in defaults:

- `STORE_ROOT` – directory to store events
- `BIND_HTTP` – HTTP listening address
- `BIND_WS` – WebSocket listening address
- `TOR_SOCKS` – Tor SOCKS proxy address

Create a `.env` file matching the ports in your `torrc`:

```
STORE_ROOT=/srv/stonr
BIND_HTTP=127.0.0.1:3000
BIND_WS=127.0.0.1:3001
TOR_SOCKS=127.0.0.1:9050
```

Start the relay using this configuration:

```bash
stonr serve --env .env
```

stonr will now accept connections from Tor via the hidden service.
