//! Source-neutral scheduling and ingestion orchestration.
//!
//! The v10 database still has one global scan lifecycle.  This module owns
//! the scheduling seam while adapters own source-specific readiness and
//! execution.  Source child rows are intentionally deferred to the v11
//! lifecycle cutover.

pub(crate) mod coordinator;
pub(crate) mod policy;

pub use crate::scanner::LegacyCodexSourceAdapter;
pub use coordinator::{
    IngestionConfig, IngestionConfigError, IngestionCoordinator, IngestionStartError,
};
