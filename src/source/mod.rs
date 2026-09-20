//! Source-neutral domain and adapter contracts.

pub use crate::domain::SessionIdentity;

mod adapter;
mod id;
mod registry;

pub use adapter::{
    AdapterAvailability, CanonicalUsageEventWrite, SourceAdapter, SourceAdapterError,
    SourceContextError, SourceRunContext, SourceRunReport, SourceRunResult, SourceRunState,
    SourceStorage, SourceStorageError, SourceWriteTxn, UsageWriteTarget,
};
pub(crate) use adapter::SourceStorageFactory;
pub use id::{SourceId, SourceIdError};
pub use registry::{SourceDescriptor, SourceDescriptorError, SourceRegistry, SourceRegistryError};
