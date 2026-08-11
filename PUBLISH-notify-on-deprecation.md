# Publishing laserstream-core-proto / -client for the notify_on deprecation

This branch (`feat/deprecate-notify-on`) adds `#[deprecated]` to the generated
`notify_on` field and `NotifyOn` enum (see PR #47). To get the deprecation into
the public Helius SDK, the two core crates must be re-published from a ref that
contains this change. There is **no publish automation** on this repo — it's a
manual `cargo publish` by whoever holds crates.io rights (Het).

## Versions to publish

Deprecation is a non-breaking, backward-compatible API change. The published
crates are ≥1.0, so under standard semver this is a **minor** bump:

| Published crate            | Current | Publish as |
|----------------------------|---------|------------|
| `laserstream-core-proto`   | 11.0.0  | **11.1.0** |
| `laserstream-core-client`  | 11.0.0  | **11.1.0** |

Publish in lockstep (proto first, then client) — the client's published
`Cargo.toml.orig` pins `laserstream-core-proto = { version = "11.1.0", ... }`.

> In-tree these crates are named `yellowstone-grpc-proto` / `-client` at
> `9.1.0` (bumped on this branch from 9.0.1 — minor, matching the deprecation).
> The publish step renames them to `laserstream-core-*` and reversions to the
> 11.x line in `Cargo.toml.orig`, exactly as 11.0.0 was cut. The 9.1.0 in-tree
> number is not the published number.

## Publish steps (mirrors how 11.0.0 was cut)

1. `laserstream-core-proto` → 11.1.0: from `yellowstone-grpc-proto`, rename in
   `Cargo.toml`, set `version = "11.1.0"`, keep the `go_package` rewrite to
   `github.com/helius-labs/laserstream-sdk/go/proto`, then `cargo publish`.
2. `laserstream-core-client` → 11.1.0: from `yellowstone-grpc-client`, rename,
   set `version = "11.1.0"`, pin `laserstream-core-proto = "11.1.0"`, `cargo publish`.

## After publish — retarget the SDK

The public SDK (`laserstream-sdk` PR #107) currently points its two git-deps at
this branch and is marked `[DNM]`. Once 11.1.0 is live, retarget both
`rust/Cargo.toml` and `javascript/Cargo.toml` from the git branch back to
`laserstream-core-proto = "11.1.0"` / `laserstream-core-client = "11.1.0"`,
regenerate both `Cargo.lock`s, and lift the DNM.
