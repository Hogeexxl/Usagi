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
	})
}

func (a *usageAccumulator) AddRow(row query.UsageRow) {
	a.Totals.Calls++
	if row.Status >= 400 || row.Error != "" {
		a.Totals.Errors++
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
	return usage
}
