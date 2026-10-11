package projection

import (
	"cmp"
	"context"
	"fmt"
	"slices"
	"time"

	"github.com/Hogeexxl/Usagi/internal/query"
)

type threadProjection struct {
	Threads   []SessionThreadView
	byKey     map[ThreadKey]int
	bySession map[threadSessionKey][]int
}

type threadSessionKey struct {
	Agent     string
	SessionID string
}

type modelEffortKey struct {
	Model  string
	Effort string
}

type threadUsage struct {
	totals  usageAccumulator
	models  map[modelEffortKey]*usageAccumulator
	first   time.Time
	last    time.Time
	hasRows bool
}

func (u *threadUsage) addRow(row query.UsageRow) {
	u.totals.AddRow(row)
	if !u.hasRows {
		u.first, u.last, u.hasRows = row.Time, row.Time, true
	} else {
		if row.Time.Before(u.first) {
			u.first = row.Time
		}
		if row.Time.After(u.last) {
			u.last = row.Time
		}
	}
	key := modelEffortKey{Model: row.Model, Effort: row.Effort}
	if u.models == nil {
		u.models = make(map[modelEffortKey]*usageAccumulator)
	}
	model := u.models[key]
	if model == nil {
		model = &usageAccumulator{}
		u.models[key] = model
	}
	model.AddRow(row)
}

func (u *threadUsage) modelEffortViews() []ModelEffortUsageView {
	views := make([]ModelEffortUsageView, 0, len(u.models))
	for key, totals := range u.models {
		views = append(views, ModelEffortUsageView{
			Model: key.Model, Effort: key.Effort, Usage: totals.View(),
		})
	}
	slices.SortFunc(views, func(a, b ModelEffortUsageView) int {
		if c := cmp.Compare(a.Model, b.Model); c != 0 {
			return c
		}
		return cmp.Compare(a.Effort, b.Effort)
	})
	return views
}

func (s *Service) loadThreadProjection(ctx context.Context, q ScopeQuery) (*threadProjection, error) {
	sessions, err := s.query.Sessions(ctx, query.SessionQuery{Range: q.Range, Filter: q.Filter})
	if err != nil {
		return nil, fmt.Errorf("projection: sessions: %w", err)
	}
	rows, err := s.query.UsageRows(ctx, query.UsageRowsQuery{Range: q.Range, Filter: q.Filter})
	if err != nil {
		return nil, fmt.Errorf("projection: usage rows: %w", err)
	}

	sessionByKey := make(map[threadSessionKey]query.SessionUsage, len(sessions.Sessions))
	threadIndices := make(map[ThreadKey]int, len(sessions.Sessions))
	projection := &threadProjection{}
	for _, session := range sessions.Sessions {
		sessionKey := threadSessionKey{Agent: session.Agent, SessionID: session.ID}
		sessionByKey[sessionKey] = session
		thread := sessionThreadFromQuery(session)
		if session.Agent == "claude" {
			thread.Type = ThreadMain
			thread.Usage = UsageView{}
			thread.ModelEffort = nil
		}
		threadIndices[thread.Key] = len(projection.Threads)
		projection.Threads = append(projection.Threads, thread)
	}

	threadUsages := make(map[ThreadKey]*threadUsage)
	compactions := make(map[threadSessionKey]map[modelEffortKey]compactionUsage)
	for _, row := range rows.Rows {
		if row.Agent == "codex" && row.ResolvedSession.ID != "" {
			sessionKey := threadSessionKey{Agent: row.ResolvedSession.Agent, SessionID: row.ResolvedSession.ID}
			groupKey := modelEffortKey{Model: row.Model, Effort: row.Effort}
			groups := compactions[sessionKey]
			if groups == nil {
				groups = make(map[modelEffortKey]compactionUsage)
				compactions[sessionKey] = groups
			}
			group := groups[groupKey]
			if row.Origin == "log" {
				group.unavailable = true
			}
			if row.Kind == "compact" {
				group.tokens += row.Input + row.CacheRead + row.CacheWrite + row.Output
			}
			groups[groupKey] = group
		}

		if row.ResolvedSession.Agent != "claude" || row.ResolvedSession.ID == "" {
			continue
		}
		parentKey := threadSessionKey{Agent: row.ResolvedSession.Agent, SessionID: row.ResolvedSession.ID}
		session, ok := sessionByKey[parentKey]
		if !ok {
			continue
		}
		var key ThreadKey
		if row.Subagent == "" {
			key = ThreadKey{Agent: "claude", SessionID: row.ResolvedSession.ID, ThreadID: row.ResolvedSession.ID}
		} else {
			key = ThreadKey{Agent: row.Agent, SessionID: row.ResolvedSession.ID, ThreadID: row.Subagent}
			if _, ok := threadIndices[key]; !ok {
				threadIndices[key] = len(projection.Threads)
				projection.Threads = append(projection.Threads, SessionThreadView{
					Key: key, Type: ThreadSubagent, Project: session.Project,
				})
			}
			thread := &projection.Threads[threadIndices[key]]
			if row.ParentAgent != "" {
				if thread.ParentThreadID != "" && thread.ParentThreadID != row.ParentAgent {
					return nil, ErrConflictingThreadParent
				}
				thread.ParentThreadID = row.ParentAgent
			}
		}

		usage := threadUsages[key]
		if usage == nil {
			usage = &threadUsage{}
			threadUsages[key] = usage
		}
		usage.addRow(row)
	}

	for key, usage := range threadUsages {
		index := threadIndices[key]
		thread := &projection.Threads[index]
		thread.Usage = usage.totals.View()
		thread.ModelEffort = usage.modelEffortViews()
		thread.FirstUsage = usage.first
		thread.LastUsage = usage.last
	}

	for i := range projection.Threads {
		thread := &projection.Threads[i]
		if thread.Key.Agent != "codex" {
			continue
		}
		sessionKey := threadSessionKey{Agent: thread.Key.Agent, SessionID: thread.Key.SessionID}
		for j := range thread.ModelEffort {
			groupKey := modelEffortKey{Model: thread.ModelEffort[j].Model, Effort: thread.ModelEffort[j].Effort}
			compaction := compactions[sessionKey][groupKey]
			if compaction.unavailable {
				continue
			}
			tokens := compaction.tokens
			thread.ModelEffort[j].CompactionTokens = &tokens
		}
	}

	projection.reindex()
	return projection, nil
}

type compactionUsage struct {
	unavailable bool
	tokens      int64
}

func sessionThreadFromQuery(session query.SessionUsage) SessionThreadView {
	thread := SessionThreadView{
		Key: ThreadKey{
			Agent: session.Agent, SessionID: session.ID, ThreadID: session.ID,
		},
		Title:      session.Title,
		Project:    session.Project,
		FirstUsage: session.FirstUsage,
		LastUsage:  session.LastUsage,
		Usage:      usageFromTotals(session.Totals),
	}
	for _, modelEffort := range session.ModelEffort {
		thread.ModelEffort = append(thread.ModelEffort, ModelEffortUsageView{
			Model: modelEffort.Model, Effort: modelEffort.Effort,
			Usage: usageFromTotals(modelEffort.Totals),
		})
	}
	slices.SortFunc(thread.ModelEffort, func(a, b ModelEffortUsageView) int {
		if c := cmp.Compare(a.Model, b.Model); c != 0 {
			return c
		}
		return cmp.Compare(a.Effort, b.Effort)
	})
	return thread
}

func (p *threadProjection) reindex() {
	slices.SortFunc(p.Threads, func(a, b SessionThreadView) int {
		if c := b.LastUsage.Compare(a.LastUsage); c != 0 {
			return c
		}
		if c := cmp.Compare(a.Key.Agent, b.Key.Agent); c != 0 {
			return c
		}
		if c := cmp.Compare(a.Key.SessionID, b.Key.SessionID); c != 0 {
			return c
		}
		return cmp.Compare(a.Key.ThreadID, b.Key.ThreadID)
	})
	p.byKey = make(map[ThreadKey]int, len(p.Threads))
	p.bySession = make(map[threadSessionKey][]int)
	for i, thread := range p.Threads {
		p.byKey[thread.Key] = i
		key := threadSessionKey{Agent: thread.Key.Agent, SessionID: thread.Key.SessionID}
		p.bySession[key] = append(p.bySession[key], i)
	}
}

func (s *Service) Sessions(ctx context.Context, q ScopeQuery) (SessionListView, error) {
	projection, err := s.loadThreadProjection(ctx, q)
	if err != nil {
		return SessionListView{}, err
	}
	return SessionListView{Threads: projection.Threads}, nil
}

func (s *Service) SessionDetail(ctx context.Context, q ScopeQuery, key ThreadKey) (SessionDetailView, error) {
	projection, err := s.loadThreadProjection(ctx, q)
	if err != nil {
		return SessionDetailView{}, err
	}
	index, ok := projection.byKey[key]
	if !ok {
		return SessionDetailView{}, ErrThreadNotFound
	}
	thread := projection.Threads[index]
	detail := SessionDetailView{
		Thread: thread, SelfUsage: thread.Usage, Children: []SessionThreadView{},
	}
	if thread.Key.Agent != "claude" {
		detail.InclusiveUsage = thread.Usage
		return detail, nil
	}

	indices := projection.bySession[threadSessionKey{Agent: key.Agent, SessionID: key.SessionID}]
	if thread.Type == ThreadMain {
		for _, childIndex := range indices {
			child := projection.Threads[childIndex]
			if child.Type == ThreadSubagent {
				detail.Children = append(detail.Children, child)
				detail.SubagentUsage = mergeUsage(detail.SubagentUsage, child.Usage)
			}
		}
	} else if thread.Type == ThreadSubagent {
		descendants := projection.descendants(key, indices)
		for _, child := range descendants {
			detail.Children = append(detail.Children, projection.Threads[child])
			detail.SubagentUsage = mergeUsage(detail.SubagentUsage, projection.Threads[child].Usage)
		}
	}
	detail.InclusiveUsage = mergeUsage(detail.SelfUsage, detail.SubagentUsage)
	return detail, nil
}

func (p *threadProjection) descendants(key ThreadKey, indices []int) []int {
	childrenByParent := make(map[string][]int)
	for _, index := range indices {
		thread := p.Threads[index]
		if thread.Type == ThreadSubagent && thread.ParentThreadID != "" {
			childrenByParent[thread.ParentThreadID] = append(childrenByParent[thread.ParentThreadID], index)
		}
	}
	seen := map[string]struct{}{key.ThreadID: {}}
	queue := []string{key.ThreadID}
	for len(queue) > 0 {
		parent := queue[0]
		queue = queue[1:]
		for _, childIndex := range childrenByParent[parent] {
			childID := p.Threads[childIndex].Key.ThreadID
			if _, ok := seen[childID]; ok {
				continue
			}
			seen[childID] = struct{}{}
			queue = append(queue, childID)
		}
	}

	var result []int
	for _, index := range indices {
		thread := p.Threads[index]
		if thread.Type != ThreadSubagent || thread.Key.ThreadID == key.ThreadID {
			continue
		}
		if _, ok := seen[thread.Key.ThreadID]; ok {
			result = append(result, index)
		}
	}
	return result
}
