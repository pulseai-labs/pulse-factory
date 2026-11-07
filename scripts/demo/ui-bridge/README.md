# The test bridge

The test bridge is `pf-client-bridge`, a binary of the `pf-client` crate (`crates/pf-client`). It was a
Node script here until `r1.s2.w4`.

It serves the built UI bundle and the bridge protocol of `docs/contracts/ui-transport.md` on
`127.0.0.1`, and it makes the pinned TLS call to the controller with the real client. It stands in
for the Mac app's Rust side where the Mac app cannot run, such as headless Chromium on draco-desk
(the `desktop-bridge` fake in `SPINE.md`). It is for `scripts/demo/ui-pair.sh` (d12) and tests only.
The Mac app does not use it.

```sh
pf-client-bridge --ui <dir> --state <dir>   # prints: listening http://127.0.0.1:<port>
```

`scripts/install.sh` installs it beside `pulse-factory`. `scripts/demo/ui-pair.sh` finds it through
`PF_BRIDGE_BIN`, then next to `PF_BIN`, then on `PATH`.
