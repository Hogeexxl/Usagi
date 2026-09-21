#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CodexBindingStatus {
    Unbound,
    Ready,
    SourceChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CodexBindingOutcome {
    BoundNow,
    Ready,
    SourceChanged,
}

impl CodexBindingStatus {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "unbound" => Some(Self::Unbound),
            "ready" => Some(Self::Ready),
            "source_changed" => Some(Self::SourceChanged),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "binding/tests.rs"]
mod tests;
