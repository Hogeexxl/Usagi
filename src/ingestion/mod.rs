//! Source-neutral scheduling and ingestion orchestration.
//!
//! The v10 database still has one global scan lifecycle.  This module owns
//! the scheduling seam while adapters own source-specific readiness and
//! execution.  Source child rows are intentionally deferred to the v11
//! lifecycle cutover.

pub(crate) mod coordinator;
pub(crate) mod policy;

pub use crate::domain::ScanTrigger;
pub use coordinator::{
    CommitFailureKind, IngestionConfig, IngestionConfigError, IngestionCoordinator,
    IngestionStartError, RequestDisposition, ScanHandle, ScanRequestError, ScanShutdownError,
    ScanStartError,
};
