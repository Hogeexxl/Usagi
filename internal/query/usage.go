package query

import (
	"cmp"
	"slices"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

func normalizeUsageInput(r DateRange, filter UsageFilter) (UsageFilter, error) {
	from, err := time.Parse(time.DateOnly, r.From)
	if err != nil {
		return UsageFilter{}, ErrInvalidRange
	}
	to, err := time.Parse(time.DateOnly, r.To)
	if err != nil || from.After(to) {
		return UsageFilter{}, ErrInvalidRange
	}
	for _, project := range filter.Projects {
		switch project.Kind {
		case ProjectKindProject:
			if project.Path == "" {
				return UsageFilter{}, ErrInvalidFilter
			}
		case ProjectKindWorkspace, ProjectKindProjectless, ProjectKindUnknown:
			if project.Path != "" {
				return UsageFilter{}, ErrInvalidFilter
			}
		default:
			return UsageFilter{}, ErrInvalidFilter
		}
	}
	filter.Agents = normalizeValues(filter.Agents)
	for index := range filter.Agents {
		filter.Agents[index] = agentFamily(filter.Agents[index])
	}
	slices.Sort(filter.Agents)
	filter.Agents = slices.Compact(filter.Agents)
	filter.Models = normalizeModelSelections(filter.Models)
	return filter, nil
}

func normalizeValues(values []string) []string {
	var normalized []string
	for _, value := range values {
		if value != "" {
			normalized = append(normalized, value)
		}
	}
	slices.Sort(normalized)
	return slices.Compact(normalized)
}

func normalizeModelSelections(values []ModelSelection) []ModelSelection {
	normalized := make([]ModelSelection, 0, len(values))
	for _, value := range values {
		if value.ProviderID != "" && value.Model != "" {
			normalized = append(normalized, value)
		}
	}
	slices.SortFunc(normalized, func(a, b ModelSelection) int {
		if c := cmp.Compare(a.ProviderID, b.ProviderID); c != 0 {
			return c
		}
		return cmp.Compare(a.Model, b.Model)
	})
	return slices.Compact(normalized)
}

func isRejected(row magpie.UsageRequest) bool {
	return row.Rejected || (row.Provider == "" && row.Status >= 400)
}

func totalsOf(row magpie.UsageRequest) UsageTotals {
	totals := UsageTotals{
		Calls: 1, Input: int64(row.Input), Output: int64(row.Output),
		CacheRead: int64(row.CacheRead), CacheWrite: int64(row.CacheWrite),
		CacheWrite1h: int64(row.CacheWrite1h), Reasoning: int64(row.Reasoning), Cost: row.Cost,
	}
	failed := RequestFailed(row.Status, row.Error)
	if failed {
		totals.Errors = 1
	}
	if row.Input+row.Output > 0 && !row.Priced {
		totals.Unpriced = 1
	}
	if !failed && row.Tries > 1 {
		totals.Retried = 1
	}
	if sample, ok := DecodeOf(DecodeInput{
		Failed: failed, Output: int64(row.Output), Reasoning: int64(row.Reasoning),
		Millis: row.Millis, TTFTMs: row.TTFTMs, FirstTextMs: row.FirstTextMs, FlowMs: row.FlowMs,
	}); ok {
		totals.DecodeMs, totals.DecodeOut, totals.DecodeN = sample.Ms, sample.Tokens, 1
	}
	return totals
}

// RequestFailed reports whether a request ended in an error.
func RequestFailed(status int, err string) bool {
	return status >= 400 || err != ""
}

// DecodeInput is what DecodeOf needs from one request row.
type DecodeInput struct {
	Failed      bool
	Output      int64
	Reasoning   int64
	Millis      int64
	TTFTMs      int64
	FirstTextMs int64
	FlowMs      int64
}

// DecodeSample is the tokens a reply wrote and the ms it took from its first
// token, the span its output speed is measured over.
type DecodeSample struct {
	Tokens int64
	Ms     int64
}

// DecodeOf mirrors how Magpie decides a request tells an output speed: a reply
// that reasoned counts its answer from its first text, and one held back and
// released in a burst tells none. It reports false when the row tells no speed.
func DecodeOf(in DecodeInput) (DecodeSample, bool) {
	if in.Failed || in.TTFTMs <= 0 {
		return DecodeSample{}, false
	}
	thinking := in.Reasoning > 0
	tokens, from := in.Output, in.TTFTMs
	if thinking {
		tokens, from = in.Output-in.Reasoning, in.FirstTextMs
	}
	window := in.Millis - from
	if tokens <= 0 || from <= 0 || window < 100 || tokens*1000 > 10000*window {
		return DecodeSample{}, false
	}
	if in.FlowMs > 0 && in.FlowMs*4 < window {
		return DecodeSample{}, false
	}
	if thinking && tokens*(in.FirstTextMs-in.TTFTMs) > 20*in.Reasoning*window {
		return DecodeSample{}, false
	}
	return DecodeSample{Tokens: tokens, Ms: window}, true
}

func (t *UsageTotals) add(other UsageTotals) {
	t.Calls += other.Calls
	t.Errors += other.Errors
	t.Input += other.Input
	t.Output += other.Output
	t.CacheRead += other.CacheRead
	t.CacheWrite += other.CacheWrite
	t.CacheWrite1h += other.CacheWrite1h
	t.Reasoning += other.Reasoning
	t.Cost += other.Cost
	t.Unpriced += other.Unpriced
	t.Retried += other.Retried
	t.DecodeMs += other.DecodeMs
	t.DecodeOut += other.DecodeOut
	t.DecodeN += other.DecodeN
}
