//! Codex rollout normalization and canonical parser-version history.

use crate::domain::DomainError;
use crate::usage::normalized::NormalizedTokenUsage;

/// Parser version used by the current Codex rollout consumer.
pub const USAGE_PARSER_VERSION: i64 = 11;

pub(crate) const USAGE_CANONICAL_ALGORITHM_VERSION: i64 = 5;

/// Return the canonical algorithm associated with a Codex parser version.
pub(crate) const fn canonical_algorithm_for(parser_version: i64) -> Option<i64> {
    match parser_version {
        4 | 5 => Some(4),
        6 | 7 | 8 | 9 | 10 | USAGE_PARSER_VERSION => Some(USAGE_CANONICAL_ALGORITHM_VERSION),
        _ => None,
    }
}

/// Stable fingerprint used by Codex source-state proofs.  Keep the byte
/// order and algorithm-version prefix identical to the established v5 form.
pub(crate) fn usage_fingerprint(value: &NormalizedTokenUsage) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(8 * 7 + 1);
    bytes.extend_from_slice(&USAGE_CANONICAL_ALGORITHM_VERSION.to_be_bytes());
    bytes.extend_from_slice(&value.input_tokens.to_be_bytes());
    bytes.extend_from_slice(&value.cached_tokens.to_be_bytes());
    match value.cache_write_tokens {
        Some(token_count) => {
            bytes.push(1);
            bytes.extend_from_slice(&token_count.to_be_bytes());
        }
        None => bytes.push(0),
    }
    bytes.extend_from_slice(&value.output_tokens.to_be_bytes());
    bytes.extend_from_slice(&value.reasoning_tokens.to_be_bytes());
    bytes.extend_from_slice(&value.total_tokens.to_be_bytes());
    *blake3::hash(&bytes).as_bytes()
}

/// Raw names are intentionally kept at this boundary because they mirror the
/// Codex rollout JSONL wire format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexRawTokenUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_write_input_tokens: Option<i64>,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

pub struct CodexRolloutAdapter;

impl CodexRolloutAdapter {
    pub fn normalize(raw: CodexRawTokenUsage) -> Result<NormalizedTokenUsage, DomainError> {
        NormalizedTokenUsage::new(
            raw.input_tokens,
            raw.cached_input_tokens,
            raw.cache_write_input_tokens,
            raw.output_tokens,
            raw.reasoning_output_tokens,
            raw.total_tokens,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(write: Option<i64>) -> CodexRawTokenUsage {
        CodexRawTokenUsage {
            input_tokens: 10_000,
            cached_input_tokens: 6_000,
            cache_write_input_tokens: write,
            output_tokens: 1_500,
            reasoning_output_tokens: 500,
            total_tokens: 11_500,
        }
    }

    #[test]
    fn t_dc_011_to_015_maps_and_validates_raw_values() {
        let normalized = CodexRolloutAdapter::normalize(raw(Some(2_000))).unwrap();
        assert_eq!(normalized.cached_tokens, 6_000);
        assert_eq!(normalized.cache_write_tokens, Some(2_000));
        assert_eq!(normalized.reasoning_tokens, 500);
        assert_eq!(
            CodexRolloutAdapter::normalize(raw(Some(0)))
                .unwrap()
                .cache_write_tokens,
            Some(0)
        );
        assert!(CodexRolloutAdapter::normalize(raw(None)).is_ok());
        assert!(
            CodexRolloutAdapter::normalize(CodexRawTokenUsage {
                cached_input_tokens: 11_000,
                ..raw(None)
            })
            .is_err()
        );
        assert!(
            CodexRolloutAdapter::normalize(CodexRawTokenUsage {
                reasoning_output_tokens: 1_501,
                ..raw(None)
            })
            .is_err()
        );
        assert!(
            CodexRolloutAdapter::normalize(CodexRawTokenUsage {
                total_tokens: 1,
                ..raw(None)
            })
            .is_err()
        );
        assert!(
            CodexRolloutAdapter::normalize(CodexRawTokenUsage {
                cache_write_input_tokens: Some(5_000),
                ..raw(None)
            })
            .is_err()
        );
    }

    #[test]
    fn t_dc_010_fingerprint_is_v5_and_distinguishes_states() {
        assert_eq!(USAGE_PARSER_VERSION, 11);
        assert_eq!(USAGE_CANONICAL_ALGORITHM_VERSION, 5);
        assert_eq!(canonical_algorithm_for(1), None);
        assert_eq!(canonical_algorithm_for(2), None);
        assert_eq!(canonical_algorithm_for(3), None);
        assert_eq!(canonical_algorithm_for(4), Some(4));
        assert_eq!(canonical_algorithm_for(5), Some(4));
        assert_eq!(canonical_algorithm_for(6), Some(5));
        assert_eq!(canonical_algorithm_for(8), Some(5));
        assert_eq!(canonical_algorithm_for(9), Some(5));
        assert_eq!(canonical_algorithm_for(10), Some(5));
        assert_eq!(canonical_algorithm_for(11), Some(5));
        assert_eq!(
            usage_fingerprint(&CodexRolloutAdapter::normalize(raw(Some(2_000))).unwrap()),
            usage_fingerprint(&CodexRolloutAdapter::normalize(raw(Some(2_000))).unwrap())
        );
        assert_ne!(
            usage_fingerprint(&CodexRolloutAdapter::normalize(raw(Some(2_000))).unwrap()),
            usage_fingerprint(&CodexRolloutAdapter::normalize(raw(None)).unwrap())
        );
        assert_ne!(
            usage_fingerprint(&CodexRolloutAdapter::normalize(raw(Some(2_000))).unwrap()),
            usage_fingerprint(
                &CodexRolloutAdapter::normalize(CodexRawTokenUsage {
                    cache_write_input_tokens: Some(2_001),
                    ..raw(None)
                })
                .unwrap()
            )
        );
    }
}
