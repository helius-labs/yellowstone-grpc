# Publishing laserstream-core-proto / -client for the notify_on REMOVAL

This branch (`cameron/remove-notify-on`, PR #51) **removes** the `notify_on`
field and `NotifyOn` enum from the proto (tag 31 reserved). To get the removal
into the public Helius SDK, the two core crates must be re-published from a ref
that contains this change. There is **no publish automation** on this repo —
it's a manual `cargo publish` by whoever holds crates.io rights (Het).

> ⚠️ This supersedes the earlier deprecation runbook (PR #47, "publish 11.1.0,
> minor"). That path is now WRONG: 11.1.0 and 11.2.0 are already published, and
> this change **removes public Rust symbols** (`notify_on`, `NotifyOn`), so it
> is **source-breaking**. It must NOT ship on the 11.x line — a Cargo user
> accepting a compatible `11.x` update would get compile failures. Publish a
> **major** version instead.

## Versions to publish

Removing exported symbols from a `>=1.0` crate is a **breaking** API change →
**major** bump:

| Published crate            | Current | Publish as |
|----------------------------|---------|------------|
| `laserstream-core-proto`   | 11.2.0  | **12.0.0** |
| `laserstream-core-client`  | 11.2.0  | **12.0.0** |

Publish in lockstep (proto first, then client) — the client's published
`Cargo.toml.orig` pins `laserstream-core-proto = { version = "12.0.0", ... }`.

> In-tree these crates are named `yellowstone-grpc-proto` / `-client`. The
> publish step renames them to `laserstream-core-*` and reversions to the 12.x
> line in `Cargo.toml.orig`, exactly as the 11.x line was cut. The in-tree
> number is not the published number.

## Publish steps (mirror how the 11.x line was cut)

1. `laserstream-core-proto` → 12.0.0: from `yellowstone-grpc-proto`, rename in
   `Cargo.toml`, set `version = "12.0.0"`, keep the `go_package` rewrite to
   `github.com/helius-labs/laserstream-sdk/go/proto`, then `cargo publish`.
2. `laserstream-core-client` → 12.0.0: from `yellowstone-grpc-client`, rename,
   set `version = "12.0.0"`, pin `laserstream-core-proto = "12.0.0"`, `cargo publish`.

## After publish — retarget the SDK

The public SDK (`laserstream-sdk` PR #118) is marked `[DNM]` pending this
release. Once 12.0.0 is live:

1. Bump **both** `rust/Cargo.toml` and `javascript/Cargo.toml` to
   `laserstream-core-proto = "12.0.0"` / `laserstream-core-client = "12.0.0"`.
2. Regenerate both `Cargo.lock`s.
3. Bump the SDK's own major (breaking removal for its consumers too) and lift
   the DNM.
