package projection

import (
	"cmp"
	"context"
	"log/slog"
	"slices"
	"time"

	"github.com/Hogeexxl/Usagi/internal/query"
)

type threadProjection struct {
	Threads   []SessionThreadView
	byKey     map[ThreadKey]int
	bySession map[threadSessionKey][]int
	rollups   map[threadSessionKey]sessionRollup
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
	key := modelEffortKey{Model: query.ModelName(row.Model), Effort: row.Effort}
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
		if c := cmp.Compare(b.Usage.Input+b.Usage.Output, a.Usage.Input+a.Usage.Output); c != 0 {
			return c
		}
		if c := cmp.Compare(a.Model, b.Model); c != 0 {
			return c
		}
		return cmp.Compare(a.Effort, b.Effort)
	})
	return views
}

func (s *Service) loadThreadProjection(ctx context.Context, q ScopeQuery) (*threadProjection, []SessionListItemView, SummaryView, error) {
	dataset, err := s.query.SessionDataset(ctx, query.SessionQuery{Range: q.Range, Filter: q.Filter})
	if err != nil {
		return nil, nil, SummaryView{}, err
	}

	projection := &threadProjection{byKey: make(map[ThreadKey]int), rollups: make(map[threadSessionKey]sessionRollup)}
	sessions := make(map[threadSessionKey]query.SessionUsage, len(dataset.Sessions))
	rootIndices := make(map[threadSessionKey]int, len(dataset.Sessions))
	for _, session := range dataset.Sessions {
		key := threadSessionKey{Agent: session.Agent, SessionID: session.ID}
		sessions[key] = session
		thread := sessionThreadFromQuery(session)
		thread.Usage = UsageView{}
		thread.ModelEffort = nil
		rootIndices[key] = len(projection.Threads)
		projection.byKey[thread.Key] = len(projection.Threads)
		projection.Threads = append(projection.Threads, thread)
	}

	threadUsages := make(map[ThreadKey]*threadUsage)
	inclusiveUsages := make(map[threadSessionKey]*threadUsage, len(dataset.Sessions))
	subagentUsages := make(map[threadSessionKey]*threadUsage, len(dataset.Sessions))
	subagentIDs := make(map[threadSessionKey]map[string]struct{})
	parents := make(map[ThreadKey]map[string]struct{})
	lastActivities := make(map[threadSessionKey]time.Time, len(dataset.Sessions))
	compactions := make(map[threadSessionKey]map[modelEffortKey]compactionUsage)

	for _, row := range dataset.Rows {
		if row.ResolvedSession.ID == "" {
			continue
		}
		sessionKey := threadSessionKey{Agent: row.ResolvedSession.Agent, SessionID: row.ResolvedSession.ID}
		if _, ok := sessions[sessionKey]; !ok {
			continue
		}
		all := inclusiveUsages[sessionKey]
		if all == nil {
			all = &threadUsage{}
			inclusiveUsages[sessionKey] = all
		}
		all.addRow(row)
		if row.Time.After(lastActivities[sessionKey]) {
			lastActivities[sessionKey] = row.Time
		}

		if sessionKey.Agent == "codex" {
			groupKey := modelEffortKey{Model: query.ModelName(row.Model), Effort: row.Effort}
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

		key := ThreadKey{Agent: sessionKey.Agent, SessionID: sessionKey.SessionID, ThreadID: sessionKey.SessionID}
		var role string
		switch sessionKey.Agent {
		case "claude":
			if row.Subagent != "" {
				role = "subagent"
				key.ThreadID = row.Subagent
				if subagentIDs[sessionKey] == nil {
					subagentIDs[sessionKey] = make(map[string]struct{})
				}
				subagentIDs[sessionKey][row.Subagent] = struct{}{}
				if _, exists := parents[key]; !exists {
					parents[key] = make(map[string]struct{})
				}
				if row.ParentAgent != "" {
					parents[key][row.ParentAgent] = struct{}{}
				}
				if _, exists := projection.byKey[key]; !exists {
					projection.byKey[key] = len(projection.Threads)
					projection.Threads = append(projection.Threads, SessionThreadView{
						Key: key, Type: ThreadSubagent, Project: sessions[sessionKey].Project,
					})
				}
			} else {
				role = "self"
			}
		case "codex":
			if isCodexSubagentKind(row.Kind) {
				role = "subagent"
			} else {
				role = "self"
			}
		default:
			role = "self"
		}

		if role == "subagent" {
			usage := subagentUsages[sessionKey]
			if usage == nil {
				usage = &threadUsage{}
				subagentUsages[sessionKey] = usage
			}
			usage.addRow(row)
		} else {
			usage := threadUsages[key]
			if usage == nil {
				usage = &threadUsage{}
				threadUsages[key] = usage
			}
			usage.addRow(row)
		}
		if sessionKey.Agent == "claude" && row.Subagent != "" {
			usage := threadUsages[key]
			if usage == nil {
				usage = &threadUsage{}
				threadUsages[key] = usage
			}
			usage.addRow(row)
		}
	}

	for threadKey, values := range parents {
		index, exists := projection.byKey[threadKey]
		if !exists {
			continue
		}
		if len(values) == 1 {
			for parent := range values {
				projection.Threads[index].ParentThreadID = parent
			}
		} else if len(values) > 1 {
			slog.Warn("conflicting parent_agent values for session subagent", "agent", threadKey.Agent,
				"session_id", threadKey.SessionID, "thread_id", threadKey.ThreadID)
		}
	}
	for key, usage := range threadUsages {
		index, exists := projection.byKey[key]
		if !exists {
			continue
		}
		thread := &projection.Threads[index]
		thread.Usage = usage.totals.View()
		thread.ModelEffort = usage.modelEffortViews()
		thread.FirstUsage, thread.LastUsage = usage.first, usage.last
	}
	for key, index := range rootIndices {
		if key.Agent != "codex" {
			continue
		}
		for j := range projection.Threads[index].ModelEffort {
			groupKey := modelEffortKey{Model: projection.Threads[index].ModelEffort[j].Model, Effort: projection.Threads[index].ModelEffort[j].Effort}
			compaction := compactions[key][groupKey]
			if compaction.unavailable {
				continue
			}
			tokens := compaction.tokens
			projection.Threads[index].ModelEffort[j].CompactionTokens = &tokens
		}
	}

	projection.reindex()
	items := make([]SessionListItemView, 0, len(dataset.Sessions))
	for _, session := range dataset.Sessions {
		key := threadSessionKey{Agent: session.Agent, SessionID: session.ID}
		threadIndex := projection.byKey[ThreadKey{Agent: key.Agent, SessionID: key.SessionID, ThreadID: key.SessionID}]
		thread := projection.Threads[threadIndex]
		self := thread.Usage
		subagent := UsageView{}
		if value := subagentUsages[key]; value != nil {
			subagent = value.totals.View()
		}
		inclusive := UsageView{}
		if value := inclusiveUsages[key]; value != nil {
			inclusive = value.totals.View()
		}
		lastActivity := lastActivities[key]
		if lastActivity.IsZero() {
			lastActivity = session.Last
		}
		var count *int64
		if key.Agent != "codex" {
			value := int64(len(subagentIDs[key]))
			count = &value
		}
		projection.rollups[key] = sessionRollup{selfUsage: self, subagentUsage: subagent, inclusiveUsage: inclusive}
		items = append(items, SessionListItemView{
			Thread: thread, SelfUsage: self, SubagentUsage: subagent, InclusiveUsage: inclusive,
			HasUsageRows: session.HasRows, LastActivity: lastActivity, SubagentCount: count,
		})
	}
	slices.SortFunc(items, func(a, b SessionListItemView) int {
		if c := b.LastActivity.Compare(a.LastActivity); c != 0 {
			return c
		}
		if c := cmp.Compare(a.Thread.Key.Agent, b.Thread.Key.Agent); c != 0 {
			return c
		}
		return cmp.Compare(a.Thread.Key.SessionID, b.Thread.Key.SessionID)
	})
	return projection, items, summaryForItems(items), nil
}

func isCodexSubagentKind(kind string) bool {
	switch kind {
	case "collab_spawn", "thread_spawn", "agent_job":
		return true
	default:
		return false
	}
}

type compactionUsage struct {
	unavailable bool
	tokens      int64
}

func sessionThreadFromQuery(session query.SessionUsage) SessionThreadView {
	return SessionThreadView{
		Key: ThreadKey{
			Agent: session.Agent, SessionID: session.ID, ThreadID: session.ID,
		},
		Type:    ThreadMain,
		Title:   session.Title,
		Project: session.Project,
	}
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
	_, items, summary, err := s.loadThreadProjection(ctx, q)
	if err != nil {
		return SessionListView{}, err
	}
	return SessionListView{Items: items, Summary: summary}, nil
}

func summaryForItems(items []SessionListItemView) SummaryView {
	result := SummaryView{RootSessionCount: int64(len(items))}
	values := make([]int64, 0, len(items))
	for _, item := range items {
		result.Usage = mergeUsage(result.Usage, item.InclusiveUsage)
		if item.InclusiveUsage.Unpriced > 0 {
			result.RootUnpricedSessionCount++
		}
		value := item.InclusiveUsage.Input + item.InclusiveUsage.Output
		if value > 0 {
			values = append(values, value)
		}
	}
	if len(values) == 0 {
		return result
	}
	slices.Sort(values)
	median := values[(len(values)+1)/2-1]
	p90 := values[(9*len(values)+9)/10-1]
	result.SessionMedian, result.SessionP90 = &median, &p90
	return result
}

type sessionRollup struct {
	selfUsage      UsageView
	subagentUsage  UsageView
	inclusiveUsage UsageView
	children       []int
}

func (p *threadProjection) inclusiveRollup(index int) sessionRollup {
	thread := p.Threads[index]
	key := threadSessionKey{Agent: thread.Key.Agent, SessionID: thread.Key.SessionID}
	if thread.Type == ThreadMain {
		indices := p.bySession[key]
		result, ok := p.rollups[key]
		if !ok {
			result = sessionRollup{selfUsage: thread.Usage, inclusiveUsage: thread.Usage}
		}
		if thread.Key.Agent == "claude" {
			for _, childIndex := range indices {
				if p.Threads[childIndex].Type == ThreadSubagent {
					result.children = append(result.children, childIndex)
				}
			}
			return result
		}
		return result
	}
	result := sessionRollup{selfUsage: thread.Usage, inclusiveUsage: thread.Usage}
	if thread.Type == ThreadSubagent {
		indices := p.bySession[key]
		result.children = p.descendants(thread.Key, indices)
		for _, childIndex := range result.children {
			result.subagentUsage = mergeUsage(result.subagentUsage, p.Threads[childIndex].Usage)
		}
		result.inclusiveUsage = mergeUsage(result.selfUsage, result.subagentUsage)
	}
	return result
}

func (s *Service) SessionDetail(ctx context.Context, q ScopeQuery, key ThreadKey) (SessionDetailView, error) {
	projection, _, _, err := s.loadThreadProjection(ctx, q)
	if err != nil {
		return SessionDetailView{}, err
	}
	index, ok := projection.byKey[key]
	if !ok {
		return SessionDetailView{}, ErrThreadNotFound
	}
	thread := projection.Threads[index]
	rollup := projection.inclusiveRollup(index)
	detail := SessionDetailView{
		Thread: thread, SelfUsage: rollup.selfUsage, SubagentUsage: rollup.subagentUsage,
		InclusiveUsage: rollup.inclusiveUsage, Children: make([]SessionThreadView, 0, len(rollup.children)),
	}
	for _, childIndex := range rollup.children {
		detail.Children = append(detail.Children, projection.Threads[childIndex])
	}
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
