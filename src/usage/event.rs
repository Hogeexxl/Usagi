//! Source-neutral kinds of canonical usage events.

/// The semantic kind of a canonical usage event.
///
/// Provider-specific readers may produce these values, but the enum itself is
/// intentionally source-neutral so canonical storage and analytics do not
/// depend on a provider implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    Normal,
    Recovered,
    TurnCompensation,
}

impl EventKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Recovered => "recovered",
            Self::TurnCompensation => "turn_compensation",
        }
    }
}
