# Publishing laserstream-core-proto / -client for the notify_on deprecation

This branch (`feat/deprecate-notify-on`) adds `#[deprecated]` to the generated
`notify_on` field and `NotifyOn` enum (see PR #47). To get the deprecation into
the public Helius SDK, the two core crates must be re-published from a ref that
contains this change. There is **no publish automation** on this repo — it's a
manual `cargo publish` by whoever holds crates.io rights (Het).

## Versions to publish

Deprecation is a non-breaking change → **patch** bump on the published line:

| Published crate            | Current | Publish as |
|----------------------------|---------|------------|
| `laserstream-core-proto`   | 11.0.0  | **11.0.1** |
| `laserstream-core-client`  | 11.0.0  | **11.0.1** |

Publish in lockstep (proto first, then client) — the client's published
`Cargo.toml.orig` pins `laserstream-core-proto = { version = "11.0.1", ... }`.

> In-tree these crates are named `yellowstone-grpc-proto` / `-client` at
> `9.0.2` (bumped on this branch from 9.0.1 for coherence). The publish step
> renames them to `laserstream-core-*` and reversions to the 11.x line in
> `Cargo.toml.orig`, exactly as 11.0.0 was cut. The 9.0.2 in-tree number is
> not the published number.

## Publish steps (mirrors how 11.0.0 was cut)

1. `laserstream-core-proto` → 11.0.1: from `yellowstone-grpc-proto`, rename in
   `Cargo.toml`, set `version = "11.0.1"`, keep the `go_package` rewrite to
   `github.com/helius-labs/laserstream-sdk/go/proto`, then `cargo publish`.
2. `laserstream-core-client` → 11.0.1: from `yellowstone-grpc-client`, rename,
   set `version = "11.0.1"`, pin `laserstream-core-proto = "11.0.1"`, `cargo publish`.

## After publish — retarget the SDK

The public SDK (`laserstream-sdk` PR #107) currently points its two git-deps at
this branch and is marked `[DNM]`. Once 11.0.1 is live, retarget both
`rust/Cargo.toml` and `javascript/Cargo.toml` from the git branch back to
`laserstream-core-proto = "11.0.1"` / `laserstream-core-client = "11.0.1"`,
regenerate both `Cargo.lock`s, and lift the DNM.
