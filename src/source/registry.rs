//! Runtime source registry.

use std::{fmt, sync::Arc};

use super::{SourceAdapter, SourceId};

/// Stable source metadata used by the registry and by source-run contexts.
///
/// The display name is presentation metadata only; it is never part of a
/// canonical identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceDescriptor {
    pub id: SourceId,
    pub display_name: &'static str,
}

impl SourceDescriptor {
    pub const fn new(id: SourceId, display_name: &'static str) -> Self {
        Self { id, display_name }
    }

    pub const fn codex() -> Self {
        Self::new(SourceId::CODEX, "Codex")
    }

    pub fn try_new(
        id: SourceId,
        display_name: &'static str,
    ) -> Result<Self, SourceDescriptorError> {
        let descriptor = Self::new(id, display_name);
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn validate(&self) -> Result<(), SourceDescriptorError> {
        self.id
            .validate()
            .map_err(SourceDescriptorError::InvalidId)?;
        if self.display_name.trim().is_empty() {
            return Err(SourceDescriptorError::EmptyDisplayName);
        }
        if self.display_name.chars().any(char::is_control) {
            return Err(SourceDescriptorError::ControlCharacterInDisplayName);
        }
        Ok(())
    }
}

/// Failure while constructing or registering source metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceDescriptorError {
    InvalidId(super::SourceIdError),
    EmptyDisplayName,
    ControlCharacterInDisplayName,
}

impl fmt::Display for SourceDescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(error) => write!(formatter, "invalid source descriptor id: {error}"),
            Self::EmptyDisplayName => formatter.write_str("source display name must not be empty"),
            Self::ControlCharacterInDisplayName => {
                formatter.write_str("source display name must not contain control characters")
            }
        }
    }
}

impl std::error::Error for SourceDescriptorError {}

/// Runtime registry of adapters, ordered by their stable [`SourceId`].
///
/// A registry owns adapters through `Arc` so a snapshot can be handed to a
/// coordinator without changing adapter identity or registration order.
#[derive(Clone, Default)]
pub struct SourceRegistry {
    adapters: std::collections::BTreeMap<SourceId, Arc<dyn SourceAdapter>>,
}

impl fmt::Debug for SourceRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceRegistry")
            .field("source_ids", &self.source_ids().collect::<Vec<_>>())
            .finish()
    }
}

impl SourceRegistry {
    pub const fn new() -> Self {
        Self {
            adapters: std::collections::BTreeMap::new(),
        }
    }

    /// Register one adapter. Duplicate source ids are rejected.
    pub fn register<A>(&mut self, adapter: A) -> Result<(), SourceRegistryError>
    where
        A: SourceAdapter,
    {
        self.register_shared(Arc::new(adapter))
    }

    /// Register an already shared adapter.
    pub fn register_shared(
        &mut self,
        adapter: Arc<dyn SourceAdapter>,
    ) -> Result<(), SourceRegistryError> {
        let descriptor = adapter.descriptor();
        descriptor
            .validate()
            .map_err(SourceRegistryError::InvalidDescriptor)?;
        let source_id = descriptor.id.clone();
        if self.adapters.contains_key(&source_id) {
            return Err(SourceRegistryError::DuplicateSource(source_id));
        }
        self.adapters.insert(source_id, adapter);
        Ok(())
    }

    pub fn get(&self, source: &SourceId) -> Option<&dyn SourceAdapter> {
        self.adapters.get(source).map(Arc::as_ref)
    }

    pub fn contains(&self, source: &SourceId) -> bool {
        self.adapters.contains_key(source)
    }

    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    /// Iterate adapters in deterministic source-id order.
    pub fn iter(&self) -> impl Iterator<Item = (&SourceId, &dyn SourceAdapter)> {
        self.adapters
            .iter()
            .map(|(source, adapter)| (source, adapter.as_ref()))
    }

    /// Iterate descriptors in deterministic source-id order.
    pub fn descriptors(&self) -> impl Iterator<Item = &SourceDescriptor> {
        self.adapters.values().map(|adapter| adapter.descriptor())
    }

    /// Return source ids in deterministic order.
    pub fn source_ids(&self) -> impl Iterator<Item = &SourceId> {
        self.adapters.keys()
    }
}

/// Failure while changing a source registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceRegistryError {
    DuplicateSource(SourceId),
    InvalidDescriptor(SourceDescriptorError),
}

impl fmt::Display for SourceRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateSource(source) => {
                write!(formatter, "source already registered: {source}")
            }
            Self::InvalidDescriptor(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SourceRegistryError {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::source::{SourceRunContext, SourceRunResult};

    struct TestAdapter {
        descriptor: SourceDescriptor,
    }

    impl TestAdapter {
        fn new(id: SourceId, display_name: &'static str) -> Self {
            Self {
                descriptor: SourceDescriptor::new(id, display_name),
            }
        }
    }

    impl SourceAdapter for TestAdapter {
        fn descriptor(&self) -> &SourceDescriptor {
            &self.descriptor
        }

        fn availability(
            &self,
        ) -> Result<crate::source::AdapterAvailability, crate::source::SourceAdapterError> {
            Ok(crate::source::AdapterAvailability::Available)
        }

        fn run_scan(
            &self,
            _context: &SourceRunContext,
            _cancellation: &AtomicBool,
        ) -> SourceRunResult {
            Ok(())
        }
    }

    #[test]
    fn registry_rejects_duplicates_and_orders_ids() {
        let mut registry = SourceRegistry::new();
        registry
            .register(TestAdapter::new(SourceId::new("zeta").unwrap(), "Zeta"))
            .unwrap();
        registry
            .register(TestAdapter::new(SourceId::CODEX, "Codex"))
            .unwrap();
        assert_eq!(
            registry
                .source_ids()
                .map(SourceId::as_str)
                .collect::<Vec<_>>(),
            ["codex", "zeta"]
        );
        assert!(matches!(
            registry.register(TestAdapter::new(SourceId::CODEX, "Codex")),
            Err(SourceRegistryError::DuplicateSource(source)) if source == SourceId::CODEX
        ));
    }
}
