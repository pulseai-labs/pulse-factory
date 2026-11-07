# apps/api-client

This directory holds only the generated TypeScript types of the controller's network API
(`docs/contracts/api.md`, ADR-0012, ADR-0013). It holds no package and no client code yet: `r1.s2.w3`
adds the package.

The files in `src/gen/` are written by `ts-rs` from the Rust types in `crates/pf-api/src/types.rs`.
Do not edit them by hand. A cargo test fails when they differ from what `ts-rs` renders:

```bash
cargo test -p pf-api --test ts_types
```

After a change to a wire type, rewrite the files from the repository root, then commit them with
the change:

```bash
TS_RS_EXPORT_DIR=apps/api-client/src/gen cargo test -p pf-api --test ts_types -- --ignored write
```
