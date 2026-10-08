package usage

import (
	"fmt"
	"math"
)

type NormalizedTokenUsage struct {
	InputTokens      int64
	CachedTokens     int64
	CacheWriteTokens *int64
	OutputTokens     int64
	ReasoningTokens  int64
	TotalTokens      int64
}

func Zero() NormalizedTokenUsage {
	cacheWrite := int64(0)
	return NormalizedTokenUsage{CacheWriteTokens: &cacheWrite}
}

func NewNormalizedTokenUsage(
	inputTokens int64,
	cachedTokens int64,
	cacheWriteTokens *int64,
	outputTokens int64,
	reasoningTokens int64,
	totalTokens int64,
) (NormalizedTokenUsage, error) {
	usage := NormalizedTokenUsage{
		InputTokens:      inputTokens,
		CachedTokens:     cachedTokens,
		CacheWriteTokens: cacheWriteTokens,
		OutputTokens:     outputTokens,
		ReasoningTokens:  reasoningTokens,
		TotalTokens:      totalTokens,
	}
	if err := usage.Validate(); err != nil {
		return NormalizedTokenUsage{}, err
	}
	return usage, nil
}

func (usage NormalizedTokenUsage) Validate() error {
	for _, value := range []struct {
		field string
		count int64
	}{
		{"input_tokens", usage.InputTokens},
		{"cached_tokens", usage.CachedTokens},
		{"output_tokens", usage.OutputTokens},
		{"reasoning_tokens", usage.ReasoningTokens},
		{"total_tokens", usage.TotalTokens},
	} {
		if value.count < 0 {
			return invalidValue(value.field, "must be non-negative")
		}
	}
	if usage.CacheWriteTokens != nil && *usage.CacheWriteTokens < 0 {
		return invalidValue("cache_write_tokens", "must be non-negative")
	}
	if usage.CachedTokens > usage.InputTokens {
		return invariantViolation("cached tokens must not exceed input")
	}
	if usage.ReasoningTokens > usage.OutputTokens {
		return invariantViolation("reasoning tokens must not exceed output")
	}
	derivedTotal, err := checkedAddInt64(usage.InputTokens, usage.OutputTokens)
	if err != nil {
		return invalidValue("total_tokens", err.Error())
	}
	if usage.TotalTokens != derivedTotal {
		return invariantViolation("total tokens must equal input plus output")
	}
	if usage.CacheWriteTokens != nil {
		cachedAndWritten, err := checkedAddInt64(usage.CachedTokens, *usage.CacheWriteTokens)
		if err != nil {
			return invalidValue("cache_write_tokens", err.Error())
		}
		if cachedAndWritten > usage.InputTokens {
			return invariantViolation("cached plus cache-write tokens must not exceed input")
		}
	}
	return nil
}

func (usage NormalizedTokenUsage) CheckedAdd(other NormalizedTokenUsage) (NormalizedTokenUsage, error) {
	inputTokens, err := checkedTokenAdd("input_tokens", usage.InputTokens, other.InputTokens)
	if err != nil {
		return NormalizedTokenUsage{}, err
	}
	cachedTokens, err := checkedTokenAdd("cached_tokens", usage.CachedTokens, other.CachedTokens)
	if err != nil {
		return NormalizedTokenUsage{}, err
	}
	outputTokens, err := checkedTokenAdd("output_tokens", usage.OutputTokens, other.OutputTokens)
	if err != nil {
		return NormalizedTokenUsage{}, err
	}
	reasoningTokens, err := checkedTokenAdd("reasoning_tokens", usage.ReasoningTokens, other.ReasoningTokens)
	if err != nil {
		return NormalizedTokenUsage{}, err
	}
	totalTokens, err := checkedTokenAdd("total_tokens", usage.TotalTokens, other.TotalTokens)
	if err != nil {
		return NormalizedTokenUsage{}, err
	}
	var cacheWriteTokens *int64
	if usage.CacheWriteTokens != nil && other.CacheWriteTokens != nil {
		value, err := checkedTokenAdd("cache_write_tokens", *usage.CacheWriteTokens, *other.CacheWriteTokens)
		if err != nil {
			return NormalizedTokenUsage{}, err
		}
		cacheWriteTokens = &value
	}
	return NewNormalizedTokenUsage(
		inputTokens,
		cachedTokens,
		cacheWriteTokens,
		outputTokens,
		reasoningTokens,
		totalTokens,
	)
}

func (usage NormalizedTokenUsage) CheckedSub(previous NormalizedTokenUsage) (NormalizedTokenUsage, error) {
	inputTokens, err := checkedTokenSub("input_tokens", usage.InputTokens, previous.InputTokens)
	if err != nil {
		return NormalizedTokenUsage{}, err
	}
	cachedTokens, err := checkedTokenSub("cached_tokens", usage.CachedTokens, previous.CachedTokens)
	if err != nil {
		return NormalizedTokenUsage{}, err
	}
	outputTokens, err := checkedTokenSub("output_tokens", usage.OutputTokens, previous.OutputTokens)
	if err != nil {
		return NormalizedTokenUsage{}, err
	}
	reasoningTokens, err := checkedTokenSub("reasoning_tokens", usage.ReasoningTokens, previous.ReasoningTokens)
	if err != nil {
		return NormalizedTokenUsage{}, err
	}
	totalTokens, err := checkedTokenSub("total_tokens", usage.TotalTokens, previous.TotalTokens)
	if err != nil {
		return NormalizedTokenUsage{}, err
	}
	var cacheWriteTokens *int64
	if usage.CacheWriteTokens != nil && previous.CacheWriteTokens != nil {
		value, err := checkedTokenSub("cache_write_tokens", *usage.CacheWriteTokens, *previous.CacheWriteTokens)
		if err != nil {
			return NormalizedTokenUsage{}, err
		}
		cacheWriteTokens = &value
	}
	return NewNormalizedTokenUsage(
		inputTokens,
		cachedTokens,
		cacheWriteTokens,
		outputTokens,
		reasoningTokens,
		totalTokens,
	)
}

func (usage NormalizedTokenUsage) UncachedInputTokens() *int64 {
	if usage.CacheWriteTokens == nil {
		return nil
	}
	uncached := usage.InputTokens - usage.CachedTokens - *usage.CacheWriteTokens
	return &uncached
}

func (usage NormalizedTokenUsage) OtherOutputTokens() int64 {
	return usage.OutputTokens - usage.ReasoningTokens
}

func (usage NormalizedTokenUsage) CacheHitRate() *float64 {
	if usage.InputTokens == 0 {
		return nil
	}
	rate := float64(usage.CachedTokens) / float64(usage.InputTokens)
	return &rate
}

func checkedAddInt64(a, b int64) (int64, error) {
	if (b > 0 && a > math.MaxInt64-b) || (b < 0 && a < math.MinInt64-b) {
		return 0, fmt.Errorf("arithmetic overflow")
	}
	return a + b, nil
}

func checkedSubInt64(a, b int64) (int64, error) {
	if (b > 0 && a < math.MinInt64+b) || (b < 0 && a > math.MaxInt64+b) {
		return 0, fmt.Errorf("arithmetic overflow")
	}
	if a < b {
		return 0, fmt.Errorf("negative delta")
	}
	return a - b, nil
}

func checkedTokenAdd(field string, a, b int64) (int64, error) {
	value, err := checkedAddInt64(a, b)
	if err != nil {
		return 0, invalidValue(field, err.Error())
	}
	return value, nil
}

func checkedTokenSub(field string, a, b int64) (int64, error) {
	value, err := checkedSubInt64(a, b)
	if err != nil {
		return 0, invalidValue(field, err.Error())
	}
	return value, nil
}

func invalidValue(field, reason string) error {
	return fmt.Errorf("invalid %s: %s", field, reason)
}

func invariantViolation(invariant string) error {
	return fmt.Errorf("invariant violated: %s", invariant)
}
