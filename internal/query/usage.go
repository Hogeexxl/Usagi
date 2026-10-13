package query

import (
	"cmp"
	"context"
	"slices"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/pricing"
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
		case ProjectKindProjectless, ProjectKindUnknown:
			if project.Path != "" {
				return UsageFilter{}, ErrInvalidFilter
			}
		default:
			return UsageFilter{}, ErrInvalidFilter
		}
	}
	filter.Agents = normalizeValues(filter.Agents)
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

func (s *Service) runRequests(ctx context.Context, r DateRange, filter UsageFilter, workspace *queryWorkspace, visit func(magpie.UsageRequest, SessionRef, magpie.Session, ProjectRef)) error {
	s.syncGeneration()
	buckets, err := s.store.rows(ctx, r.From, r.To)
	if err != nil {
		return err
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	priceCatalog, err := pricing.Load(ctx, tx)
	if err != nil {
		return err
	}
	if err := tx.Commit(); err != nil {
		return err
	}
	modelSelections := make(map[ModelSelection]struct{}, len(filter.Models))
	for _, selection := range filter.Models {
		modelSelections[selection] = struct{}{}
	}
	for _, bucket := range buckets {
		for _, row := range bucket.rows {
			if isRejected(row) {
				continue
			}
			row.Cost = 0
			row.Priced = false
			if row.Input+row.Output > 0 {
				if price, ok := priceCatalog.PriceAt(row.Time, row.Provider, row.Model, row.Served); ok {
					row.Cost = price.Cost(int64(row.Input), int64(row.Output), int64(row.CacheRead), int64(row.CacheWrite), int64(row.CacheWrite1h))
					row.Priced = true
				}
			}
			if len(filter.Agents) > 0 && !slices.Contains(filter.Agents, row.Agent) {
				continue
			}
			if len(filter.Models) > 0 {
				if _, ok := modelSelections[ModelSelection{ProviderID: row.Provider, Model: ModelName(row.Model)}]; !ok {
					continue
				}
			}
			var ref SessionRef
			var session magpie.Session
			var project ProjectRef
			if workspace != nil {
				ref, session, err = workspace.resolveSession(ctx, row)
				if err != nil {
					return err
				}
				project = projectOf(session, ref.Matched)
				if !matchesProjects(project, filter.Projects) {
					continue
				}
			}
			visit(row, ref, session, project)
		}
	}
	return nil
}

func (s *Service) Usage(ctx context.Context, q UsageQuery) (UsageResult, error) {
	filter, err := normalizeUsageInput(q.Range, q.Filter)
	if err != nil {
		return UsageResult{}, err
	}
	join := len(filter.Projects) > 0
	switch q.GroupBy {
	case GroupNone, GroupAgent, GroupModel, GroupModelEffort:
	case GroupProject, GroupProjectModel, GroupProjectEffort:
		join = true
	default:
		return UsageResult{}, ErrInvalidGroupBy
	}
	var workspace *queryWorkspace
	if join {
		if err := s.ensureSessions(ctx); err != nil {
			return UsageResult{}, err
		}
		workspace = s.newWorkspace()
	}
	var result UsageResult
	groups := make(map[[3]string]UsageGroup)
	err = s.runRequests(ctx, q.Range, filter, workspace, func(row magpie.UsageRequest, _ SessionRef, _ magpie.Session, project ProjectRef) {
		totals := totalsOf(row)
		result.Totals.add(totals)
		if q.GroupBy == GroupNone {
			return
		}
		key, group := groupOf(row, project, q.GroupBy)
		if existing, ok := groups[key]; ok {
			group = existing
		}
		group.Totals.add(totals)
		groups[key] = group
	})
	if err != nil {
		return UsageResult{}, err
	}
	keys := make([][3]string, 0, len(groups))
	for key := range groups {
		keys = append(keys, key)
	}
	slices.SortFunc(keys, func(a, b [3]string) int {
		for i := range a {
			if c := cmp.Compare(a[i], b[i]); c != 0 {
				return c
			}
		}
		return 0
	})
	for _, key := range keys {
		result.Groups = append(result.Groups, groups[key])
	}
	return result, nil
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

func groupOf(row magpie.UsageRequest, project ProjectRef, by UsageGroupBy) ([3]string, UsageGroup) {
	var key [3]string
	var group UsageGroup
	switch by {
	case GroupAgent:
		key[0] = row.Agent
		group.Agent, group.AgentName = row.Agent, row.AgentName
	case GroupModel:
		key[0] = ModelName(row.Model)
		group.Model = key[0]
	case GroupModelEffort:
		key[0], key[1] = ModelName(row.Model), row.Effort
		group.Model, group.Effort = key[0], row.Effort
	case GroupProject, GroupProjectModel, GroupProjectEffort:
		key[0], key[1] = string(project.Kind), project.Path
		group.Project = project
		if by == GroupProjectModel {
			key[2] = ModelName(row.Model)
			group.Model = key[2]
		}
		if by == GroupProjectEffort {
			key[2], group.Effort = row.Effort, row.Effort
		}
	}
	return key, group
}

func (s *Service) UsageRows(ctx context.Context, q UsageRowsQuery) (UsageRowsResult, error) {
	filter, err := normalizeUsageInput(q.Range, q.Filter)
	if err != nil {
		return UsageRowsResult{}, err
	}
	if err := s.ensureSessions(ctx); err != nil {
		return UsageRowsResult{}, err
	}
	workspace := s.newWorkspace()
	var result UsageRowsResult
	err = s.runRequests(ctx, q.Range, filter, workspace, func(row magpie.UsageRequest, ref SessionRef, _ magpie.Session, project ProjectRef) {
		result.Rows = append(result.Rows, UsageRow{
			Time: row.Time, Agent: row.Agent, AgentName: row.AgentName, Provider: row.Provider,
			Model: row.Model, Effort: row.Effort, Session: row.Session, NativeSession: row.NativeSession,
			Subagent: row.Subagent, ParentAgent: row.ParentAgent, Kind: row.Kind, Origin: row.Source,
			Status: row.Status, Error: row.Error, ResolvedSession: ref, Project: project,
			Tries: row.Tries, Millis: row.Millis, TTFTMs: row.TTFTMs, FirstTextMs: row.FirstTextMs, FlowMs: row.FlowMs,
			Input: int64(row.Input), Output: int64(row.Output), CacheRead: int64(row.CacheRead),
			CacheWrite: int64(row.CacheWrite), CacheWrite1h: int64(row.CacheWrite1h),
			Reasoning: int64(row.Reasoning), Cost: row.Cost, Priced: row.Priced,
		})
	})
	if err != nil {
		return UsageRowsResult{}, err
	}
	slices.SortStableFunc(result.Rows, func(a, b UsageRow) int {
		if c := a.Time.Compare(b.Time); c != 0 {
			return c
		}
		left := [5]string{a.Agent, a.Session, a.Subagent, a.Model, a.Effort}
		right := [5]string{b.Agent, b.Session, b.Subagent, b.Model, b.Effort}
		for i := range left {
			if c := cmp.Compare(left[i], right[i]); c != 0 {
				return c
			}
		}
		return 0
	})
	return result, nil
}
