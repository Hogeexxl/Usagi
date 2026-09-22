//! Source-neutral domain and adapter contracts.

pub use crate::domain::SessionIdentity;

pub(crate) mod adapter;
mod id;
mod registry;

pub(crate) use adapter::SourceStorageFactory;
pub use adapter::{
    AdapterAvailability, CanonicalUsageEventWrite, SourceAdapter, SourceAdapterError,
    SourceContextError, SourceRunContext, SourceRunReport, SourceRunResult, SourceRunState,
    SourceStorage, SourceStorageError, SourceWriteTxn, UsageWriteTarget,
};
pub(crate) use adapter::{
    CanonicalUsageEventMatch, CanonicalWriteOutcome, SessionMutationOutcome, UsageActivationOutcome,
};
pub use id::{SourceId, SourceIdError};
pub use registry::{SourceDescriptor, SourceDescriptorError, SourceRegistry, SourceRegistryError};
