# Implementation checklist additions

- [ ] `stonr mirror` subcommands exist for adding, listing, updating, and
      removing relays and requests.
- [ ] Mirroring runtime loads multiple requests per relay and issues individual
      `REQ`s on each connection.
- [ ] Cursor files are tracked per relay/request and resume correctly.
- [ ] Events are written under `events/<YYYY>/<MM>/<DD>/` with a matching
      `events/by-id/<id>.path` mapping.
- [ ] Mirror symlinks reference the absolute event file path rather than hash
      prefixes.
