//! Compressed account filtering via cuckoo filters.
//!
//! This module provides a probabilistic account filter for subscribe requests,
//! aimed at clients tracking large pubkey sets (e.g., hundreds of thousands to
//! millions). Instead of uploading an explicit pubkey list every few seconds,
//! the client uploads a compact cuckoo filter — typically ~3 bytes per pubkey
//! at 95% load — and the server matches accounts against it.
//!
//! # Primary API
//!
//! - [`CompressedAccountFilterSet`] — safe, tracked client-side wrapper. Use this
//!   to build filters. Requires the `convert` feature (enabled by default).
//! - [`CuckooFilter`] — raw filter used for server-side matching. Has a `remove`
//!   footgun; clients should prefer [`CompressedAccountFilterSet`].
//!
//! # Example — build, subscribe, then update without resubmitting the full list
//!
//! ```no_run
//! use {
//!     solana_pubkey::Pubkey,
//!     yellowstone_grpc_proto::{cuckoo::CompressedAccountFilterSet, geyser::SubscribeRequest},
//! };
//!
//! // 1. Build the cuckoo map for the accounts we track
//! let mut accounts = CompressedAccountFilterSet::with_capacity(2_000_000).unwrap();
//! for pk in my_tracked_pubkeys() {
//!     accounts.insert(pk).unwrap();
//! }
//!
//! // 2. Attach the compact filter to the subscribe request under a name we choose
//! let mut req = SubscribeRequest::default();
//! accounts.insert_into_subscribe_request(&mut req, "tracked_accounts");
//! //    send `req` to the server...
//!
//! // 3. Mutate the tracked set as it changes
//! accounts.insert(Pubkey::new_from_array([7u8; 32])).unwrap();
//! accounts.remove(Pubkey::new_from_array([3u8; 32]));
//!
//! // 4. Only re-send when something changed — the filter is tiny, so a full
//! //    re-send is cheap (no incremental-update protocol needed)
//! if accounts.take_dirty() {
//!     accounts.insert_into_subscribe_request(&mut req, "tracked_accounts");
//!     // re-send `req` on the existing stream sink
//! }
//!
//! # fn my_tracked_pubkeys() -> Vec<Pubkey> { vec![] }
//! ```
//!
//! # Handling updates
//!
//! Account updates flowing in from the server may include false positives
//! (bounded at <1% at full load). Filter locally with [`CompressedAccountFilterSet::contains`]
//! for an exact check:
//!
//! ```no_run
//! # use {
//! #     solana_pubkey::Pubkey,
//! #     yellowstone_grpc_proto::cuckoo::CompressedAccountFilterSet,
//! # };
//! # let accounts: CompressedAccountFilterSet = CompressedAccountFilterSet::with_capacity(100).unwrap();
//! # let incoming_pubkey = Pubkey::new_from_array([0u8; 32]);
//! if accounts.contains(incoming_pubkey) {
//!     // definitely a tracked account
//! } else {
//!     // false positive from the server-side cuckoo check — ignore
//! }
//! ```

mod constants;
mod error;
mod filter;
mod hasher;
// `set` provides the client-side builder and needs `solana-pubkey`, which is
// supplied by the `convert` feature. The server only needs the raw filter below.
#[cfg(feature = "convert")]
mod set;

pub use {
    constants::DEFAULT_HASH_SEED,
    error::{CuckooBuildError, TableFullError},
    filter::CuckooFilter,
    hasher::YellowstoneHasherBuilder,
};

#[cfg(feature = "convert")]
pub use set::CompressedAccountFilterSet;
