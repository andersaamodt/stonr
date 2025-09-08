# Tor Onion Deployment

To expose Stonr's HTTP and WebSocket services via Tor hidden services, configure `torrc` with separate service directories:

```conf
HiddenServiceDir /var/lib/tor/stonr_ws/
HiddenServiceVersion 3
HiddenServicePort 80 127.0.0.1:7778

HiddenServiceDir /var/lib/tor/stonr_http/
HiddenServiceVersion 3
HiddenServicePort 80 127.0.0.1:7777
```

With Tor running, set the `TOR_SOCKS` environment variable so Stonr uses the proxy for upstream siphoning:

```bash
TOR_SOCKS=127.0.0.1:9050
```

`BIND_HTTP` and `BIND_WS` must match the ports referenced in the `HiddenServicePort` directives.
