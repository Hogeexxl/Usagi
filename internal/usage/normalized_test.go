package usage

import (
	"math"
	"testing"
)

func TestTokenZero(t *testing.T) {
	zero := Zero()
	if zero.InputTokens != 0 || zero.CachedTokens != 0 || zero.CacheWriteTokens == nil || *zero.CacheWriteTokens != 0 ||
		zero.OutputTokens != 0 || zero.ReasoningTokens != 0 || zero.TotalTokens != 0 {
		t.Fatalf("Zero() = %+v", zero)
	}
}

func TestTokenCheckedAddUnknown(t *testing.T) {
	known := standardTokenUsage(t, int64Pointer(2_000))
	unknown := standardTokenUsage(t, nil)
	sum, err := known.CheckedAdd(known)
	if err != nil {
		t.Fatal(err)
	}
	if sum.InputTokens != 20_000 || sum.CacheWriteTokens == nil || *sum.CacheWriteTokens != 4_000 {
		t.Fatalf("known sum = %+v", sum)
	}
	for _, pair := range [][2]NormalizedTokenUsage{{known, unknown}, {unknown, known}} {
		sum, err := pair[0].CheckedAdd(pair[1])
		if err != nil {
			t.Fatal(err)
		}
		if sum.CacheWriteTokens != nil {
			t.Errorf("unknown cache-write sum = %d, want nil", *sum.CacheWriteTokens)
		}
	}
}

func TestTokenCheckedSubUnknownAndNegative(t *testing.T) {
	current := standardTokenUsage(t, int64Pointer(2_000))
	previous := validTokenUsage(t, 8_000, 5_500, int64Pointer(500), 1_000, 300, 9_000)
	delta, err := current.CheckedSub(previous)
	if err != nil {
		t.Fatal(err)
	}
	if delta.InputTokens != 2_000 || delta.CachedTokens != 500 || delta.CacheWriteTokens == nil || *delta.CacheWriteTokens != 1_500 ||
		delta.OutputTokens != 500 || delta.ReasoningTokens != 200 || delta.TotalTokens != 2_500 {
		t.Fatalf("delta = %+v", delta)
	}
	unknown, err := standardTokenUsage(t, nil).CheckedSub(previous)
	if err != nil {
		t.Fatal(err)
	}
	if unknown.CacheWriteTokens != nil {
		t.Errorf("unknown cache-write delta = %d, want nil", *unknown.CacheWriteTokens)
	}
	if _, err := previous.CheckedSub(current); err == nil {
		t.Fatal("CheckedSub accepted a negative token delta")
	}
}

func TestTokenOverflow(t *testing.T) {
	max, err := NewNormalizedTokenUsage(math.MaxInt64, 0, nil, 0, 0, math.MaxInt64)
	if err != nil {
		t.Fatal(err)
	}
	one, err := NewNormalizedTokenUsage(1, 0, nil, 0, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := max.CheckedAdd(one); err == nil {
		t.Fatal("CheckedAdd accepted int64 overflow")
	}
	if _, err := NewNormalizedTokenUsage(math.MaxInt64, 0, nil, 1, 0, 0); err == nil {
		t.Fatal("Validate accepted total-token overflow")
	}
}

func TestTokenDerivedValues(t *testing.T) {
	known := standardTokenUsage(t, int64Pointer(2_000))
	if value := known.UncachedInputTokens(); value == nil || *value != 2_000 {
		t.Fatalf("UncachedInputTokens() = %v", value)
	}
	if got := known.OtherOutputTokens(); got != 1_000 {
		t.Errorf("OtherOutputTokens() = %d, want 1000", got)
	}
	if value := known.CacheHitRate(); value == nil || *value != 0.6 {
		t.Errorf("CacheHitRate() = %v, want 0.6", value)
	}
	unknown := standardTokenUsage(t, nil)
	if value := unknown.UncachedInputTokens(); value != nil {
		t.Errorf("unknown UncachedInputTokens() = %v, want nil", *value)
	}
	if value := Zero().CacheHitRate(); value != nil {
		t.Errorf("zero CacheHitRate() = %v, want nil", *value)
	}
}

func standardTokenUsage(t *testing.T, cacheWrite *int64) NormalizedTokenUsage {
	t.Helper()
	return validTokenUsage(t, 10_000, 6_000, cacheWrite, 1_500, 500, 11_500)
}

func validTokenUsage(t *testing.T, input, cached int64, cacheWrite *int64, output, reasoning, total int64) NormalizedTokenUsage {
	t.Helper()
	usage, err := NewNormalizedTokenUsage(input, cached, cacheWrite, output, reasoning, total)
	if err != nil {
		t.Fatal(err)
	}
	return usage
}

func int64Pointer(value int64) *int64 {
	return &value
}
