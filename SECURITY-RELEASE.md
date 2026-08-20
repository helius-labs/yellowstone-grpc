# Publishing the h2 security update

This branch adds an `h2 >=0.4.17,<0.5` dependency floor to the Rust client and the proto crate's Tonic feature. Version 0.4.16 is not used because it has a regression.

## Versions

| Published crate | Current | Publish as |
|-|-|-|
| `laserstream-core-proto` | 11.2.0 | 11.2.1 |
| `laserstream-core-client` | 11.1.0 | 11.1.1 |

Publish the proto crate first. The client release must depend on `laserstream-core-proto = "11.2.1"`.

## Preparation

The in-tree packages remain named `yellowstone-grpc-proto` and `yellowstone-grpc-client`. Prepare the published manifests with the same manual rename process used for the current releases:

1. Start the proto manifest from the published 11.2.0 manifest so its dependency constraints are preserved.
2. Rename the proto package to `laserstream-core-proto`, set its version to 11.2.1, and preserve the existing `go_package` rewrite for the Laserstream SDK.
3. Keep the optional `h2` dependency and its inclusion in both the `tonic` and `plugin` features.
4. Package and inspect `laserstream-core-proto` before publishing it.
5. Rename the client package to `laserstream-core-client` and set its version to 11.1.1.
6. Preserve the dependency alias as `yellowstone-grpc-proto = { package = "laserstream-core-proto", version = "11.2.1", default-features = false, features = ["tonic", "tonic-compression"] }`.
7. Keep the direct `h2 >=0.4.17,<0.5` dependency.
8. Package and inspect `laserstream-core-client` before publishing it.

Publishing is not part of this security PR and requires separate authorization from a crates.io owner.
