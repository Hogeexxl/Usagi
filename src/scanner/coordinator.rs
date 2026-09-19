//! Deprecated compatibility path for the scheduler.
//!
//! Runtime scheduling lives in [`crate::ingestion::coordinator`]. This module
//! intentionally contains no scheduler implementation; it remains only so
//! older in-crate imports can be migrated without duplicating lifecycle logic.

pub(crate) use crate::ingestion::coordinator::{ScanWorker, WorkerResult};
