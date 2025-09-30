# stonr Spec

## Configuration bootstrap
- The CLI accepts an `--env` flag (default `.env`).
- When the referenced `.env` file is missing, stonr creates it with sane defaults:
  - `STORE_ROOT` pointing to `stonr-data` beside the env file
  - `BIND_HTTP=127.0.0.1:7777`
  - `BIND_WS=127.0.0.1:7778`
  - `VERIFY_SIG=0`
  - empty `RELAYS_UPSTREAM`, `FILTER_AUTHORS`, `FILTER_KINDS`, `FILTER_TAG_T`, and `TOR_SOCKS`
  - `FILTER_SINCE_MODE=cursor`
- After bootstrap the CLI loads settings from the env file for every subcommand.

## CLI behaviour
- `stonr init` ensures the storage directory structure exists.
- `stonr serve` reuses the same settings, starting mirroring only when `RELAYS_UPSTREAM` has entries.
- `stonr serve --verbose`/`-v` prints HTTP requests, WebSocket subscriptions, and mirror progress.
- `stonr -v` prints the package version.
- `stonr mirror add <url>` verifies the upstream relay is reachable before adding it to `RELAYS_UPSTREAM`.
- `stonr mirror remove <url>` removes the relay from `RELAYS_UPSTREAM`.

## Mirroring
- Configure mirroring via `RELAYS_UPSTREAM` and optional `FILTER_*` variables in the `.env` file.
- `stonr serve` connects to each upstream relay, applying the filters and keeping cursors per relay.
