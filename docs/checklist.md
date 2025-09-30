# Release checklist

- [ ] `stonr init` succeeds from a clean checkout and creates a default `.env`.
- [ ] Default `.env` points `STORE_ROOT` to `stonr-data` next to the config file.
- [ ] `stonr -v` prints the crate version.
- [ ] Mirroring only activates when `RELAYS_UPSTREAM` is set.
- [ ] `stonr mirror add <url>` connects successfully before updating `RELAYS_UPSTREAM`.
- [ ] `stonr mirror remove <url>` removes the relay from the configuration.
- [ ] Update docs if configuration variables change.
