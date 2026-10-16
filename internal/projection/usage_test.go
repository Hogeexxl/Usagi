package projection

import (
	"math"
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

func TestRequestMetricsCountTTFTAndAggregateSpeed(t *testing.T) {
	rows := []query.UsageRow{
		{Output: 100, Millis: 1100, TTFTMs: 100, Status: 200},
		{Output: 100, Millis: 2100, TTFTMs: 100, Status: 200},
		{Output: 50, Millis: 199, TTFTMs: 100, Status: 200},
		{Output: 100, Millis: 1100, TTFTMs: 100, Status: 400},
		{Output: 0, TTFTMs: 300, Status: 200, Tries: 2},
		{Output: 50, Millis: 1500, TTFTMs: 500, Status: 200, Tries: 2, HandedOn: true},
	}
	var accumulator usageAccumulator
	for _, row := range rows {
		accumulator.AddRow(row)
	}

	metrics := accumulator.View().requestMetrics.view()
	if metrics.Calls != 5 || metrics.Errors != 1 || metrics.Retried != 1 || metrics.SpeedSamples != 3 || metrics.TTFTSamples != 5 {
		t.Fatalf("request metrics = %+v, want 5 calls, 1 error, 1 retry, 3 speed samples and 5 TTFT samples", metrics)
	}
	if metrics.OutputSpeed == nil || math.Abs(*metrics.OutputSpeed-62.5) > 1e-9 || metrics.MeanTTFTSeconds == nil || math.Abs(*metrics.MeanTTFTSeconds-0.22) > 1e-9 {
		t.Fatalf("request speed/mean TTFT = %v/%v, want weighted 62.5 token/s and 0.22 seconds", metrics.OutputSpeed, metrics.MeanTTFTSeconds)
	}
	if accumulator.Totals.Calls != 6 || accumulator.Totals.Errors != 1 || accumulator.Totals.Retried != 2 || accumulator.Totals.Output != 400 {
		t.Fatalf("existing usage totals changed = %+v; want 6 calls, 1 error, 2 retries and 400 output tokens", accumulator.Totals)
	}
}

func TestRequestMetricsMagpieDecodeBoundaries(t *testing.T) {
	base := query.UsageRow{Agent: "claude", Status: 200, Output: 100, Millis: 1100, TTFTMs: 100}
	tests := []struct {
		name   string
		row    query.UsageRow
		wantN  int64
		wantMS int64
	}{
		{name: "ordinary reply", row: base, wantN: 100, wantMS: 1000},
		{name: "minimum window", row: query.UsageRow{Agent: "claude", Status: 200, Output: 1, Millis: 200, TTFTMs: 100}, wantN: 1, wantMS: 100},
		{name: "short window", row: query.UsageRow{Agent: "claude", Status: 200, Output: 1, Millis: 199, TTFTMs: 100}},
		{name: "maximum speed", row: query.UsageRow{Agent: "claude", Status: 200, Output: 1000, Millis: 200, TTFTMs: 100}, wantN: 1000, wantMS: 100},
		{name: "over maximum speed", row: query.UsageRow{Agent: "claude", Status: 200, Output: 1001, Millis: 200, TTFTMs: 100}},
		{name: "flow at one quarter", row: query.UsageRow{Agent: "claude", Status: 200, Output: 100, Millis: 1100, TTFTMs: 100, FlowMs: 250}, wantN: 100, wantMS: 1000},
		{name: "flow burst", row: query.UsageRow{Agent: "claude", Status: 200, Output: 100, Millis: 1100, TTFTMs: 100, FlowMs: 249}},
		{name: "visible reasoning with first text", row: query.UsageRow{Agent: "claude", Status: 200, Output: 150, Reasoning: 50, Millis: 1200, TTFTMs: 100, FirstTextMs: 200}, wantN: 100, wantMS: 1000},
		{name: "visible reasoning without text", row: query.UsageRow{Agent: "claude", Status: 200, Output: 150, Reasoning: 50, Millis: 1200, TTFTMs: 100, FirstReasoningMs: 200, FlowMs: 1000}, wantN: 150, wantMS: 1000},
		{name: "missing reasoning timing", row: query.UsageRow{Agent: "claude", Status: 200, Output: 150, Reasoning: 50, Millis: 1200, TTFTMs: 100}},
		{name: "hidden reasoning without text", row: query.UsageRow{Agent: "claude", Status: 200, Output: 150, Reasoning: 50, Millis: 1200, TTFTMs: 100, FirstReasoningMs: 200, FlowMs: 1000, ReasoningHidden: true}},
		{name: "hidden reasoning with text", row: query.UsageRow{Agent: "claude", Status: 200, Output: 150, Reasoning: 50, Millis: 1200, TTFTMs: 100, FirstTextMs: 200, ReasoningHidden: true}, wantN: 100, wantMS: 1000},
		{name: "reasoning burst", row: query.UsageRow{Agent: "claude", Status: 200, Output: 210, Reasoning: 10, Millis: 3000, TTFTMs: 100, FirstTextMs: 2000}},
		{name: "codex log full sample", row: query.UsageRow{Agent: "codex", Origin: "log", Status: 200, Output: 100, Millis: 1100, TTFTMs: 100}, wantN: 100, wantMS: 1000},
		{name: "codex log short window", row: query.UsageRow{Agent: "codex", Origin: "log", Status: 200, Output: 100, Millis: 1099, TTFTMs: 100}},
		{name: "codex log short answer", row: query.UsageRow{Agent: "codex", Origin: "log", Status: 200, Output: 99, Millis: 1100, TTFTMs: 100}},
		{name: "codex gateway has no log gate", row: query.UsageRow{Agent: "codex", Status: 200, Output: 50, Millis: 1099, TTFTMs: 100}, wantN: 50, wantMS: 999},
		{name: "other agent log has no codex gate", row: query.UsageRow{Agent: "claude", Origin: "log", Status: 200, Output: 50, Millis: 1099, TTFTMs: 100}, wantN: 50, wantMS: 999},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			tokens, window, ok := requestDecodeOf(test.row)
			if test.wantN == 0 {
				if ok {
					t.Fatalf("decode = %d/%d ms, want no sample", tokens, window)
				}
				return
			}
			if !ok || tokens != test.wantN || window != test.wantMS {
				t.Fatalf("decode = %d/%d ms, %t; want %d/%d ms", tokens, window, ok, test.wantN, test.wantMS)
			}
		})
	}
}

// usageWithRequestCalls builds the expected view for accumulated rows that all
// count as plain request-metric calls.
func usageWithRequestCalls(totals query.UsageTotals, calls int64) UsageView {
	usage := usageFromTotals(totals)
	usage.requestMetrics.calls = calls
	return usage
}
