//! Shared policy for the global ingestion scheduler.
//!
//! The interval bounds belong to the source-neutral scheduler.  Legacy
//! scanner configuration delegates to these helpers so there is one policy
//! and one validation path while the v10 scanner API remains available.

use std::time::Duration;

pub(crate) const DEFAULT_INTERVAL: Duration = Duration::from_secs(300);
pub(crate) const MIN_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const MAX_INTERVAL: Duration = Duration::from_secs(3_600);

pub(crate) fn validate_interval(interval: Duration) -> bool {
    (MIN_INTERVAL..=MAX_INTERVAL).contains(&interval)
}
