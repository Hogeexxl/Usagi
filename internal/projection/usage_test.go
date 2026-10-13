package projection

import (
	"reflect"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/query"
)

func TestUsageFromTotals(t *testing.T) {
	rate := 0.2
	got := usageFromTotals(query.UsageTotals{
		Calls: 3, Errors: 1, Input: 10, Output: 4, CacheRead: 20, CacheWrite: 70,
		CacheWrite1h: 30, Reasoning: 3, Cost: 0.25, Unpriced: 2,
	})
	want := UsageView{
		Calls: 3, Errors: 1, Input: 10, Output: 4, CacheRead: 20, CacheWrite: 70,
		CacheWrite1h: 30, Reasoning: 3, Total: 104, CacheHitRate: &rate, Cost: 0.25, Unpriced: 2,
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("usageFromTotals() = %+v, want %+v", got, want)
	}

	zeroHitRate := usageFromTotals(query.UsageTotals{Input: 5})
	if zeroHitRate.CacheHitRate == nil || *zeroHitRate.CacheHitRate != 0 {
		t.Fatalf("CacheHitRate with prompt and no cache read = %v, want 0", zeroHitRate.CacheHitRate)
	}
	if noPrompt := usageFromTotals(query.UsageTotals{}); noPrompt.CacheHitRate != nil {
		t.Fatalf("CacheHitRate with no prompt tokens = %v, want nil", noPrompt.CacheHitRate)
	}
}

func TestUsageAccumulator(t *testing.T) {
	var accumulator usageAccumulator
	accumulator.AddRow(query.UsageRow{
		Input: 10, Output: 2, CacheRead: 3, CacheWrite: 4, CacheWrite1h: 2,
		Reasoning: 1, Cost: 0.5, Priced: true,
	})
	accumulator.AddRow(query.UsageRow{
		Status: 400, Error: "failed", Input: 5, Output: 1, CacheRead: 2,
		CacheWrite: 3, CacheWrite1h: 1, Reasoning: 2, Cost: 0.25,
	})
	accumulator.AddRow(query.UsageRow{CacheRead: 9})

	want := query.UsageTotals{
		Calls: 3, Errors: 1, Input: 15, Output: 3, CacheRead: 14, CacheWrite: 7,
		CacheWrite1h: 3, Reasoning: 3, Cost: 0.75, Unpriced: 1,
	}
	if accumulator.Totals != want {
		t.Fatalf("accumulator totals = %+v, want %+v", accumulator.Totals, want)
	}
}

func TestUsageMerge(t *testing.T) {
	a := usageFromTotals(query.UsageTotals{
		Calls: 1, Input: 10, Output: 10, CacheRead: 20, CacheWrite: 10,
		CacheWrite1h: 4, Reasoning: 3, Cost: 0.5, Unpriced: 1,
	})
	b := usageFromTotals(query.UsageTotals{
		Calls: 2, Errors: 1, Input: 10, Output: 5, Reasoning: 5, Cost: 0.25,
	})
	rate := 0.4
	want := UsageView{
		Calls: 3, Errors: 1, Input: 20, Output: 15, CacheRead: 20, CacheWrite: 10,
		CacheWrite1h: 4, Reasoning: 8, Total: 65, CacheHitRate: &rate, Cost: 0.75, Unpriced: 1,
	}
	if got := mergeUsage(a, b); !reflect.DeepEqual(got, want) {
		t.Fatalf("mergeUsage() = %+v, want %+v", got, want)
	}
}

func TestSessionFieldsOutputSpeedAndRetries(t *testing.T) {
	view := usageFromTotals(query.UsageTotals{Calls: 3, Retried: 1, DecodeMs: 10_000, DecodeOut: 600, DecodeN: 2})
	if view.Retried != 1 || view.DecodeN != 2 || view.OutputSpeed == nil || *view.OutputSpeed != 60 {
		t.Fatalf("usageFromTotals() = %+v, want retried 1, 2 samples, 60 tokens/s", view)
	}
	if none := usageFromTotals(query.UsageTotals{Calls: 1}); none.OutputSpeed != nil {
		t.Fatalf("OutputSpeed = %v, want nil without a measurable reply", *none.OutputSpeed)
	}

	merged := mergeUsage(view, usageFromTotals(query.UsageTotals{Calls: 1, Retried: 2, DecodeMs: 10_000, DecodeOut: 1_400, DecodeN: 1}))
	if merged.Retried != 3 || merged.DecodeN != 3 || merged.OutputSpeed == nil || *merged.OutputSpeed != 100 {
		t.Fatalf("mergeUsage() = %+v, want speed recomputed from summed tokens and ms", merged)
	}

	var acc usageAccumulator
	acc.AddRow(query.UsageRow{Output: 500, Millis: 10_000, TTFTMs: 2_000})
	acc.AddRow(query.UsageRow{Output: 100, Millis: 3_000, TTFTMs: 1_000, Tries: 2})
	acc.AddRow(query.UsageRow{Output: 100, Millis: 3_000, TTFTMs: 1_000, Tries: 2, Status: 502})
	if got := acc.Totals; got.Retried != 1 || got.Errors != 1 || got.DecodeN != 2 || got.DecodeOut != 600 || got.DecodeMs != 10_000 {
		t.Fatalf("AddRow totals = %+v", got)
	}
}
