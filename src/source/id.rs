//! Stable identifiers for source products.

use std::{borrow::Cow, fmt, str::FromStr};

/// A validated, opaque identifier for a product/source that contributes
/// canonical sessions and usage.
///
/// Source identifiers intentionally use a small ASCII slug grammar.  The
/// value is an identity, not a display label and must not be inferred from a
/// canonical thread id.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceId(Cow<'static, str>);

impl SourceId {
    /// The source id reserved for the existing Codex ingestion path.
    pub const CODEX: Self = Self(Cow::Borrowed("codex"));

    /// The source id reserved for the Antigravity ingestion path.
    pub const ANTIGRAVITY: Self = Self(Cow::Borrowed("antigravity"));

    /// Construct a source id after validating its stable slug grammar.
    pub fn new(value: impl Into<String>) -> Result<Self, SourceIdError> {
        let value = value.into();
        validate_source_id(&value)?;
        Ok(Self(Cow::Owned(value)))
    }

    /// Return the source's stable slug.
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }

    /// Return the source's owned slug.
    pub fn into_string(self) -> String {
        self.0.into_owned()
    }

    /// Return the Codex source id.
    pub const fn codex() -> Self {
        Self::CODEX
    }

    /// Return the Antigravity source id.
    pub const fn antigravity() -> Self {
        Self::ANTIGRAVITY
    }

    pub fn validate(&self) -> Result<(), SourceIdError> {
        validate_source_id(self.as_str())
    }
}

impl Default for SourceId {
    fn default() -> Self {
        Self::CODEX
    }
}

impl AsRef<str> for SourceId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SourceId {
    type Err = SourceIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.to_owned())
    }
}

impl TryFrom<&str> for SourceId {
    type Error = SourceIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value.to_owned())
    }
}

impl TryFrom<String> for SourceId {
    type Error = SourceIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<&SourceId> for SourceId {
    fn from(value: &SourceId) -> Self {
        value.clone()
    }
}

/// Validation failure for a [`SourceId`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceIdError {
    reason: &'static str,
}

impl SourceIdError {
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for SourceIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for SourceIdError {}

fn validate_source_id(value: &str) -> Result<(), SourceIdError> {
    if value.is_empty() {
        return Err(SourceIdError {
            reason: "source id must not be empty",
        });
    }
    if value.len() > 64 {
        return Err(SourceIdError {
            reason: "source id must be at most 64 bytes",
        });
    }
    let mut bytes = value.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase()) {
        return Err(SourceIdError {
            reason: "source id must start with an ASCII lowercase letter",
        });
    }
    if !bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
    }) {
        return Err(SourceIdError {
            reason: "source id must contain only lowercase ASCII letters, digits, '_' or '-'",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_is_the_stable_default_id() {
        assert_eq!(SourceId::CODEX.as_str(), "codex");
        assert_eq!(SourceId::codex(), SourceId::CODEX);
        assert_eq!("codex".parse::<SourceId>().unwrap(), SourceId::CODEX);
        assert_eq!(SourceId::ANTIGRAVITY.as_str(), "antigravity");
        assert_eq!(SourceId::antigravity(), SourceId::ANTIGRAVITY);
        assert_eq!(
            "antigravity".parse::<SourceId>().unwrap(),
            SourceId::ANTIGRAVITY
        );
    }

    #[test]
    fn ids_accept_the_documented_slug_shape() {
        for value in ["antigravity", "maka2", "my_source-v2"] {
            assert!(SourceId::new(value).is_ok(), "{value}");
        }
    }

    #[test]
    fn ids_reject_non_slug_values() {
        for value in ["", "Codex", "1source", "source:", "source name", "é"] {
            assert!(SourceId::new(value).is_err(), "{value:?}");
        }
        assert!(SourceId::new("a".repeat(65)).is_err());
    }
}
