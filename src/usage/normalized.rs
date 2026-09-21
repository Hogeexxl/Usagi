//! The single canonical token usage value used after provider adapters.

use crate::domain::DomainError;

/// Provider-independent token counts.  All values are validated before they
/// cross the adapter boundary and all arithmetic is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedTokenUsage {
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
}

impl NormalizedTokenUsage {
    pub fn zero() -> Self {
        Self {
            input_tokens: 0,
            cached_tokens: 0,
            cache_write_tokens: Some(0),
            output_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 0,
        }
    }

    pub fn new(
        input_tokens: i64,
        cached_tokens: i64,
        cache_write_tokens: Option<i64>,
        output_tokens: i64,
        reasoning_tokens: i64,
        total_tokens: i64,
    ) -> Result<Self, DomainError> {
        let usage = Self {
            input_tokens,
            cached_tokens,
            cache_write_tokens,
            output_tokens,
            reasoning_tokens,
            total_tokens,
        };
        usage.validate()?;
        Ok(usage)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        for (field, value) in [
            ("input_tokens", self.input_tokens),
            ("cached_tokens", self.cached_tokens),
            ("output_tokens", self.output_tokens),
            ("reasoning_tokens", self.reasoning_tokens),
            ("total_tokens", self.total_tokens),
        ] {
            if value < 0 {
                return Err(DomainError::InvalidValue {
                    field,
                    reason: "must be non-negative".to_owned(),
                });
            }
        }
        if let Some(value) = self.cache_write_tokens
            && value < 0
        {
            return Err(DomainError::InvalidValue {
                field: "cache_write_tokens",
                reason: "must be non-negative".to_owned(),
            });
        }
        if self.cached_tokens > self.input_tokens {
            return Err(DomainError::InvariantViolation {
                invariant: "cached tokens must not exceed input",
            });
        }
        if let Some(cache_write) = self.cache_write_tokens {
            let cached_and_written =
                self.cached_tokens.checked_add(cache_write).ok_or_else(|| {
                    DomainError::InvalidValue {
                        field: "cache_write_tokens",
                        reason: "arithmetic overflow".to_owned(),
                    }
                })?;
            if cached_and_written > self.input_tokens {
                return Err(DomainError::InvariantViolation {
                    invariant: "cached plus cache-write tokens must not exceed input",
                });
            }
        }
        if self.reasoning_tokens > self.output_tokens {
            return Err(DomainError::InvariantViolation {
                invariant: "reasoning tokens must not exceed output",
            });
        }
        let derived_total = self
            .input_tokens
            .checked_add(self.output_tokens)
            .ok_or_else(|| DomainError::InvalidValue {
                field: "total_tokens",
                reason: "arithmetic overflow".to_owned(),
            })?;
        if self.total_tokens != derived_total {
            return Err(DomainError::InvariantViolation {
                invariant: "total tokens must equal input plus output",
            });
        }
        Ok(())
    }

    pub fn checked_add(&self, other: &Self) -> Result<Self, DomainError> {
        let add = |field: &'static str, left: i64, right: i64| {
            left.checked_add(right)
                .ok_or_else(|| DomainError::InvalidValue {
                    field,
                    reason: "arithmetic overflow".to_owned(),
                })
        };
        Self::new(
            add("input_tokens", self.input_tokens, other.input_tokens)?,
            add("cached_tokens", self.cached_tokens, other.cached_tokens)?,
            match (self.cache_write_tokens, other.cache_write_tokens) {
                (Some(left), Some(right)) => Some(add("cache_write_tokens", left, right)?),
                _ => None,
            },
            add("output_tokens", self.output_tokens, other.output_tokens)?,
            add(
                "reasoning_tokens",
                self.reasoning_tokens,
                other.reasoning_tokens,
            )?,
            add("total_tokens", self.total_tokens, other.total_tokens)?,
        )
    }

    pub fn checked_sub(&self, previous: &Self) -> Result<Self, DomainError> {
        let sub = |field: &'static str, current: i64, old: i64| {
            let value = current
                .checked_sub(old)
                .ok_or_else(|| DomainError::InvalidValue {
                    field,
                    reason: "arithmetic overflow".to_owned(),
                })?;
            if value < 0 {
                return Err(DomainError::InvalidValue {
                    field,
                    reason: "negative delta".to_owned(),
                });
            }
            Ok(value)
        };
        let cache_write_tokens = match (self.cache_write_tokens, previous.cache_write_tokens) {
            (Some(current), Some(old)) => {
                let value = current
                    .checked_sub(old)
                    .ok_or_else(|| DomainError::InvalidValue {
                        field: "cache_write_tokens",
                        reason: "arithmetic overflow".to_owned(),
                    })?;
                if value < 0 {
                    return Err(DomainError::InvalidValue {
                        field: "cache_write_tokens",
                        reason: "cache-write delta must not be negative".to_owned(),
                    });
                }
                Some(value)
            }
            _ => None,
        };
        Self::new(
            sub("input_tokens", self.input_tokens, previous.input_tokens)?,
            sub("cached_tokens", self.cached_tokens, previous.cached_tokens)?,
            cache_write_tokens,
            sub("output_tokens", self.output_tokens, previous.output_tokens)?,
            sub(
                "reasoning_tokens",
                self.reasoning_tokens,
                previous.reasoning_tokens,
            )?,
            sub("total_tokens", self.total_tokens, previous.total_tokens)?,
        )
    }

    pub fn uncached_input_tokens(&self) -> Option<i64> {
        self.cache_write_tokens
            .map(|cache_write| self.input_tokens - self.cached_tokens - cache_write)
    }

    pub fn other_output_tokens(&self) -> i64 {
        self.output_tokens - self.reasoning_tokens
    }

    pub fn cache_hit_rate(&self) -> Option<f64> {
        (self.input_tokens > 0).then(|| self.cached_tokens as f64 / self.input_tokens as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standard(write: Option<i64>) -> NormalizedTokenUsage {
        NormalizedTokenUsage::new(10_000, 6_000, write, 1_500, 500, 11_500).unwrap()
    }

    #[test]
    fn t_dc_001_to_007_canonical_and_derived_rules() {
        assert_eq!(standard(Some(2_000)).input_tokens, 10_000);
        for (input, cached, write, output, reasoning, total) in [
            (-1, 0, None, 0, 0, 0),
            (0, -1, None, 0, 0, 0),
            (0, 0, Some(-1), 0, 0, 0),
            (0, 0, None, -1, 0, 0),
            (0, 0, None, 0, -1, 0),
            (0, 0, None, 0, 0, -1),
        ] {
            assert!(
                NormalizedTokenUsage::new(input, cached, write, output, reasoning, total).is_err()
            );
        }
        assert!(standard(None).validate().is_ok());
        assert!(NormalizedTokenUsage::new(1, 2, None, 0, 0, 1).is_err());
        assert!(NormalizedTokenUsage::new(10, 8, Some(3), 0, 0, 10).is_err());
        assert!(NormalizedTokenUsage::new(10, 0, None, 4, 5, 14).is_err());
        assert!(NormalizedTokenUsage::new(10, 0, None, 4, 0, 15).is_err());
        assert_ne!(standard(Some(0)), standard(None));
        assert_eq!(standard(Some(2_000)).uncached_input_tokens(), Some(2_000));
        assert_eq!(standard(Some(2_000)).other_output_tokens(), 1_000);
        assert_eq!(standard(Some(2_000)).cache_hit_rate(), Some(0.6));
        assert_eq!(standard(None).uncached_input_tokens(), None);
        assert_eq!(standard(None).cache_hit_rate(), Some(0.6));
        assert_eq!(NormalizedTokenUsage::zero().cache_hit_rate(), None);
    }

    #[test]
    fn t_dc_008_checked_add_and_overflow() {
        let sum = standard(Some(2_000))
            .checked_add(&standard(Some(2_000)))
            .unwrap();
        assert_eq!(sum.input_tokens, 20_000);
        assert_eq!(sum.cache_write_tokens, Some(4_000));
        assert_eq!(
            standard(None)
                .checked_add(&standard(Some(2_000)))
                .unwrap()
                .cache_write_tokens,
            None
        );
        let max = NormalizedTokenUsage::new(i64::MAX, 0, None, 0, 0, i64::MAX).unwrap();
        assert!(max.checked_add(&NormalizedTokenUsage::zero()).is_ok());
        assert!(
            max.checked_add(&NormalizedTokenUsage::new(0, 0, None, 1, 0, 1).unwrap())
                .is_err()
        );
    }

    #[test]
    fn t_dc_009_checked_sub_and_unknown_propagation() {
        let current = standard(Some(2_000));
        let previous =
            NormalizedTokenUsage::new(8_000, 5_500, Some(500), 1_000, 300, 9_000).unwrap();
        let delta = current.checked_sub(&previous).unwrap();
        assert_eq!(
            delta,
            NormalizedTokenUsage::new(2_000, 500, Some(1_500), 500, 200, 2_500).unwrap()
        );
        let higher =
            NormalizedTokenUsage::new(11_000, 6_000, Some(2_000), 1_500, 500, 12_500).unwrap();
        assert_eq!(
            current.checked_sub(&higher).unwrap_err(),
            DomainError::InvalidValue {
                field: "input_tokens",
                reason: "negative delta".to_owned()
            }
        );
        assert_eq!(
            standard(None)
                .checked_sub(&previous)
                .unwrap()
                .cache_write_tokens,
            None
        );
        let lower =
            NormalizedTokenUsage::new(10_000, 6_000, Some(1_999), 1_500, 500, 11_500).unwrap();
        assert!(lower.checked_sub(&current).is_err());
    }
}
