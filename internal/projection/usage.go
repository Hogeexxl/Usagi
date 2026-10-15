package projection

import "github.com/Hogeexxl/Usagi/internal/query"

type usageAccumulator struct {
	Totals         query.UsageTotals
	requestMetrics requestMetricsTotals
}

func usageFromTotals(t query.UsageTotals) UsageView {
	return deriveUsage(UsageView{
		Calls:        t.Calls,
		Errors:       t.Errors,
		Input:        t.Input,
		Output:       t.Output,
		CacheRead:    t.CacheRead,
		CacheWrite:   t.CacheWrite,
		CacheWrite1h: t.CacheWrite1h,
		Reasoning:    t.Reasoning,
		Cost:         t.Cost,
		Unpriced:     t.Unpriced,
		Retried:      t.Retried,
		DecodeMs:     t.DecodeMs,
		DecodeOut:    t.DecodeOut,
		DecodeN:      t.DecodeN,
	})
}

func (a *usageAccumulator) AddRow(row query.UsageRow) {
	a.Totals.Calls++
	a.requestMetrics.addRow(row)
	failed := query.RequestFailed(row.Status, row.Error)
	if failed {
		a.Totals.Errors++
	} else if row.Tries > 1 {
		a.Totals.Retried++
	}
	if sample, ok := query.DecodeOf(query.DecodeInput{
		Failed: failed, Output: row.Output, Reasoning: row.Reasoning,
		Millis: row.Millis, TTFTMs: row.TTFTMs, FirstTextMs: row.FirstTextMs, FlowMs: row.FlowMs,
	}); ok {
		a.Totals.DecodeMs += sample.Ms
		a.Totals.DecodeOut += sample.Tokens
		a.Totals.DecodeN++
	}
	a.Totals.Input += row.Input
	a.Totals.Output += row.Output
	a.Totals.CacheRead += row.CacheRead
	a.Totals.CacheWrite += row.CacheWrite
	a.Totals.CacheWrite1h += row.CacheWrite1h
	a.Totals.Reasoning += row.Reasoning
	a.Totals.Cost += row.Cost
	if row.Input+row.Output > 0 && !row.Priced {
		a.Totals.Unpriced++
	}
}

func (a usageAccumulator) View() UsageView {
	view := usageFromTotals(a.Totals)
	view.requestMetrics = a.requestMetrics
	return view
}

func mergeUsage(a, b UsageView) UsageView {
	return deriveUsage(UsageView{
		Calls:          a.Calls + b.Calls,
		Errors:         a.Errors + b.Errors,
		Input:          a.Input + b.Input,
		Output:         a.Output + b.Output,
		CacheRead:      a.CacheRead + b.CacheRead,
		CacheWrite:     a.CacheWrite + b.CacheWrite,
		CacheWrite1h:   a.CacheWrite1h + b.CacheWrite1h,
		Reasoning:      a.Reasoning + b.Reasoning,
		Cost:           a.Cost + b.Cost,
		Unpriced:       a.Unpriced + b.Unpriced,
		Retried:        a.Retried + b.Retried,
		DecodeMs:       a.DecodeMs + b.DecodeMs,
		DecodeOut:      a.DecodeOut + b.DecodeOut,
		DecodeN:        a.DecodeN + b.DecodeN,
		requestMetrics: a.requestMetrics.add(b.requestMetrics),
	})
}

type requestMetricsTotals struct {
	calls        int64
	errors       int64
	retried      int64
	decodeMs     int64
	decodeOut    int64
	speedSamples int64
	ttftMs       int64
	ttftSamples  int64
}

func (a *requestMetricsTotals) addRow(row query.UsageRow) {
	failed := query.RequestFailed(row.Status, row.Error)
	if !row.HandedOn {
		a.calls++
		if failed {
			a.errors++
		} else if row.Tries > 1 {
			a.retried++
		}
	}
	if !failed && row.TTFTMs > 0 {
		a.ttftMs += row.TTFTMs
		a.ttftSamples++
	}
	if tokens, window, ok := requestDecodeOf(row); ok {
		a.decodeMs += window
		a.decodeOut += tokens
		a.speedSamples++
	}
}

func (a requestMetricsTotals) add(b requestMetricsTotals) requestMetricsTotals {
	return requestMetricsTotals{
		calls: a.calls + b.calls, errors: a.errors + b.errors, retried: a.retried + b.retried,
		decodeMs: a.decodeMs + b.decodeMs, decodeOut: a.decodeOut + b.decodeOut,
		speedSamples: a.speedSamples + b.speedSamples, ttftMs: a.ttftMs + b.ttftMs,
		ttftSamples: a.ttftSamples + b.ttftSamples,
	}
}

func (a requestMetricsTotals) view() RequestMetricsView {
	result := RequestMetricsView{
		Calls: a.calls, Errors: a.errors, Retried: a.retried, SpeedSamples: a.speedSamples,
		TTFTSamples: a.ttftSamples,
	}
	if a.speedSamples > 0 && a.decodeMs > 0 {
		speed := float64(a.decodeOut) * 1000 / float64(a.decodeMs)
		result.OutputSpeed = &speed
	}
	if a.ttftSamples > 0 {
		mean := float64(a.ttftMs) / float64(a.ttftSamples) / 1000
		result.MeanTTFTSeconds = &mean
	}
	return result
}

func requestDecodeOf(row query.UsageRow) (int64, int64, bool) {
	if query.RequestFailed(row.Status, row.Error) || row.TTFTMs <= 0 {
		return 0, 0, false
	}
	if row.ReasoningHidden && row.FirstTextMs <= 0 {
		return 0, 0, false
	}

	tokens, start := row.Output, row.TTFTMs
	if row.Reasoning > 0 {
		if row.FirstTextMs > 0 {
			tokens, start = row.Output-row.Reasoning, row.FirstTextMs
		} else if row.FirstReasoningMs >= row.TTFTMs && row.FlowMs > 0 && row.Reasoning <= row.Output {
			start = row.FirstReasoningMs
		} else {
			return 0, 0, false
		}
	}
	window := row.Millis - start
	if tokens <= 0 || start <= 0 || window < 100 || tokens*1000 > 10000*window {
		return 0, 0, false
	}
	if row.FlowMs > 0 && row.FlowMs*4 < window {
		return 0, 0, false
	}
	if row.Reasoning > 0 && row.FirstTextMs > 0 && tokens*(row.FirstTextMs-row.TTFTMs) > 20*row.Reasoning*window {
		return 0, 0, false
	}
	if row.Origin == "log" && row.Agent == "codex" && (window < 1000 || tokens < 100) {
		return 0, 0, false
	}
	return tokens, window, true
}

func deriveUsage(usage UsageView) UsageView {
	usage.Total = usage.Input + usage.CacheRead + usage.CacheWrite + usage.Output
	usage.CacheHitRate = nil
	prompt := usage.Input + usage.CacheRead + usage.CacheWrite
	if prompt > 0 {
		rate := float64(usage.CacheRead) / float64(prompt)
		usage.CacheHitRate = &rate
	}
	usage.OutputSpeed = nil
	if usage.DecodeMs > 0 {
		speed := float64(usage.DecodeOut) * 1000 / float64(usage.DecodeMs)
		usage.OutputSpeed = &speed
	}
	return usage
}
