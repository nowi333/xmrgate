# Contributing

Thanks for helping. Bug reports, fixes and integrations for other shop platforms are welcome.

## Getting started

```bash
cargo test                  # offline suite: scanner, payment engine, HTTP server
cargo run --example demo    # every screen on a simulated chain, no funds needed
```

The web interface lives in `web/` (plain HTML, CSS and JavaScript, no build step) and is
compiled into the binary. Restart the demo to see a change.

More checks, used before a release:

```bash
scripts/regtest-e2e.sh /path/to/monero   # real Monero software on a private chain
scripts/test-integrations.sh             # PHP, Node.js and Python example shops
```

## Pull requests

- Keep changes focused, one topic per pull request.
- Add or update a test for any change to payment detection or the API.
- Explain the user-visible effect in the description.

For security issues, see [SECURITY.md](SECURITY.md) instead.
