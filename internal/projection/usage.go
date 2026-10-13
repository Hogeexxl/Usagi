package projection

import "github.com/Hogeexxl/Usagi/internal/query"

type usageAccumulator struct {
	Totals query.UsageTotals
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
	return usageFromTotals(a.Totals)
}

func mergeUsage(a, b UsageView) UsageView {
	return deriveUsage(UsageView{
		Calls:        a.Calls + b.Calls,
		Errors:       a.Errors + b.Errors,
		Input:        a.Input + b.Input,
		Output:       a.Output + b.Output,
		CacheRead:    a.CacheRead + b.CacheRead,
		CacheWrite:   a.CacheWrite + b.CacheWrite,
		CacheWrite1h: a.CacheWrite1h + b.CacheWrite1h,
		Reasoning:    a.Reasoning + b.Reasoning,
		Cost:         a.Cost + b.Cost,
		Unpriced:     a.Unpriced + b.Unpriced,
		Retried:      a.Retried + b.Retried,
		DecodeMs:     a.DecodeMs + b.DecodeMs,
		DecodeOut:    a.DecodeOut + b.DecodeOut,
		DecodeN:      a.DecodeN + b.DecodeN,
	})
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
