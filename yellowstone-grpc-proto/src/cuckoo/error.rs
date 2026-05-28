//! Error types for the `cuckoo` module.
//!
//! Errors are scoped to the method that produces them — see [`CuckooBuildError`]
//! for build-time failures and [`TableFullError`] for `insert` failures.
//! Callers only pattern-match on variants the method can actually produce,
//! avoiding leaky union error types.
//!
//! These impl `Display`/`Error` by hand rather than via `thiserror`, because the
//! cuckoo module is always compiled while `thiserror` is an optional dependency
//! of this crate (only enabled by the `plugin` feature).

use {super::constants::MAX_KICKS, std::fmt};

/// Build-time error for [`CuckooFilter`] and [`CompressedAccountFilterSet`] construction.
///
/// Returned by [`CuckooFilter::with_capacity`], [`CuckooFilter::with_capacity_and_hasher`],
/// and [`CompressedAccountFilterSet::with_capacity`].
///
/// [`CuckooFilter`]: super::filter::CuckooFilter
/// [`CuckooFilter::with_capacity`]: super::filter::CuckooFilter::with_capacity
/// [`CuckooFilter::with_capacity_and_hasher`]: super::filter::CuckooFilter::with_capacity_and_hasher
/// [`CompressedAccountFilterSet`]: super::set::CompressedAccountFilterSet
/// [`CompressedAccountFilterSet::with_capacity`]: super::set::CompressedAccountFilterSet::with_capacity
#[derive(Debug, PartialEq, Eq)]
pub enum CuckooBuildError {
    /// The requested capacity could not be allocated.
    ///
    /// Either the next power-of-two bucket count would exceed `usize::MAX`,
    /// or the system rejected the allocation (e.g., not enough memory).
    CapacityOverflow,
}

impl fmt::Display for CuckooBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityOverflow => {
                f.write_str("capacity overflow: requested capacity exceeds maximum")
            }
        }
    }
}

impl std::error::Error for CuckooBuildError {}

/// Error returned when [`CuckooFilter::insert`] or [`CompressedAccountFilterSet::insert`] cannot
/// accommodate a new item.
///
/// Indicates the filter reached its load limit and could not relocate an
/// existing fingerprint after `MAX_KICKS` attempts. Typically means the filter
/// was under-sized for the workload; rebuild with a larger `max_capacity`.
///
/// On error, the inserting type's state is unchanged.
///
/// [`CuckooFilter::insert`]: super::filter::CuckooFilter::insert
/// [`CompressedAccountFilterSet::insert`]: super::set::CompressedAccountFilterSet::insert
#[derive(Debug, PartialEq, Eq)]
pub struct TableFullError;

impl fmt::Display for TableFullError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cuckoo table full after {MAX_KICKS} kicks")
    }
}

impl std::error::Error for TableFullError {}
