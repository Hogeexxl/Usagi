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
	Threads         []SessionThreadView
	byKey           map[ThreadKey]int
	bySession       map[threadSessionKey][]int
	unknown         map[ThreadKey]UsageView
	legacySubagents map[threadSessionKey]UsageView
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

	projection := &threadProjection{
		byKey:   make(map[ThreadKey]int),
		unknown: make(map[ThreadKey]UsageView), legacySubagents: make(map[threadSessionKey]UsageView),
	}
	sessions := make(map[threadSessionKey]query.SessionUsage, len(dataset.Sessions))
	codexThreadKeys := make(map[string]ThreadKey)
	confirmedRoots := make(map[string]bool)
	for _, session := range dataset.Sessions {
		sessions[threadSessionKey{Agent: session.Agent, SessionID: session.ID}] = session
	}

	addThread := func(thread SessionThreadView) int {
		if index, exists := projection.byKey[thread.Key]; exists {
			current := &projection.Threads[index]
			if current.Type == ThreadUnknown && thread.Type == ThreadMain {
				current.Type = ThreadMain
			}
			if current.Title == "" {
				current.Title = thread.Title
			}
			if current.Nickname == "" {
				current.Nickname = thread.Nickname
			}
			if current.ParentThreadID == "" {
				current.ParentThreadID = thread.ParentThreadID
			}
			if current.Project.Kind == query.ProjectKindUnknown && thread.Project.Kind != query.ProjectKindUnknown {
				current.Project = thread.Project
			}
			return index
		}
		projection.byKey[thread.Key] = len(projection.Threads)
		projection.Threads = append(projection.Threads, thread)
		return len(projection.Threads) - 1
	}

	for _, session := range dataset.Sessions {
		if session.Agent != "codex" {
			addThread(sessionThreadFromQuery(session))
			continue
		}
		thread := SessionThreadView{Title: session.Title, Nickname: session.Nickname, Project: session.Project}
		switch session.Role {
		case "subagent":
			anchor := session.RootSession
			if anchor == "" {
				anchor = session.ID
			} else {
				confirmedRoots[anchor] = true
			}
			thread.Key = ThreadKey{Agent: "codex", SessionID: anchor, ThreadID: session.ID}
			thread.Type = ThreadSubagent
			thread.ParentThreadID = session.ParentSession
		case "unknown":
			thread.Key = ThreadKey{Agent: "codex", SessionID: session.ID, ThreadID: session.ID}
			thread.Type = ThreadUnknown
		default:
			thread.Key = ThreadKey{Agent: "codex", SessionID: session.ID, ThreadID: session.ID}
			thread.Type = ThreadMain
			if session.Role == "main" {
				confirmedRoots[session.ID] = true
			}
		}
		addThread(thread)
		codexThreadKeys[session.ID] = thread.Key
	}

	for _, session := range dataset.Sessions {
		if session.Agent != "codex" || session.Role != "subagent" {
			continue
		}
		if session.RootSession != "" {
			root := ThreadKey{Agent: "codex", SessionID: session.RootSession, ThreadID: session.RootSession}
			addThread(SessionThreadView{Key: root, Type: ThreadMain})
			codexThreadKeys[session.RootSession] = root
		}
	}

	threadUsages := make(map[ThreadKey]*threadUsage)
	unknownUsages := make(map[ThreadKey]*threadUsage)
	legacySubagentUsages := make(map[ThreadKey]*threadUsage)
	subagentIDs := make(map[threadSessionKey]map[string]struct{})
	parents := make(map[ThreadKey]map[string]struct{})
	lastActivities := make(map[threadSessionKey]time.Time, len(dataset.Sessions))
	hasUsageRows := make(map[threadSessionKey]bool, len(dataset.Sessions))
	compactions := make(map[ThreadKey]map[modelEffortKey]compactionUsage)
	addParent := func(key ThreadKey, parent string) {
		if parent == "" {
			return
		}
		if parents[key] == nil {
			parents[key] = make(map[string]struct{})
		}
		parents[key][parent] = struct{}{}
	}
	addOwnRow := func(key ThreadKey, row query.UsageRow) {
		usage := threadUsages[key]
		if usage == nil {
			usage = &threadUsage{}
			threadUsages[key] = usage
		}
		usage.addRow(row)
		if key.Agent != "codex" {
			return
		}
		groupKey := modelEffortKey{Model: query.ModelName(row.Model), Effort: row.Effort}
		groups := compactions[key]
		if groups == nil {
			groups = make(map[modelEffortKey]compactionUsage)
			compactions[key] = groups
		}
		group := groups[groupKey]
		if row.Compaction == nil {
			group.unavailable = true
		} else if *row.Compaction {
			group.tokens += row.Input + row.CacheRead + row.CacheWrite + row.Output
		}
		groups[groupKey] = group
	}
	addUnknownRow := func(key ThreadKey, row query.UsageRow) {
		usage := unknownUsages[key]
		if usage == nil {
			usage = &threadUsage{}
			unknownUsages[key] = usage
		}
		usage.addRow(row)
	}

	for _, row := range dataset.Rows {
		if row.ResolvedSession.ID == "" {
			continue
		}
		sessionKey := threadSessionKey{Agent: row.ResolvedSession.Agent, SessionID: row.ResolvedSession.ID}
		if _, ok := sessions[sessionKey]; !ok {
			continue
		}
		key := ThreadKey{Agent: sessionKey.Agent, SessionID: sessionKey.SessionID, ThreadID: sessionKey.SessionID}
		var activityKey = sessionKey
		switch sessionKey.Agent {
		case "claude":
			if row.Subagent != "" {
				key.ThreadID = row.Subagent
				if subagentIDs[sessionKey] == nil {
					subagentIDs[sessionKey] = make(map[string]struct{})
				}
				subagentIDs[sessionKey][row.Subagent] = struct{}{}
				addParent(key, row.ParentAgent)
				addThread(SessionThreadView{Key: key, Type: ThreadSubagent, Project: sessions[sessionKey].Project})
				addOwnRow(key, row)
			} else {
				addOwnRow(key, row)
			}
		case "codex":
			switch row.SessionRole {
			case "main":
				key.ThreadID = row.Session
				if key.ThreadID == "" {
					key.ThreadID = sessionKey.SessionID
				}
				key.SessionID = key.ThreadID
				confirmedRoots[key.ThreadID] = true
				addThread(SessionThreadView{Key: key, Type: ThreadMain})
				addOwnRow(key, row)
			case "subagent":
				if row.Subagent == "" {
					continue
				}
				actor, found := sessions[threadSessionKey{Agent: "codex", SessionID: row.Subagent}]
				anchor := row.Session
				if anchor == "" {
					anchor = actor.RootSession
				}
				if anchor == "" {
					anchor = row.Subagent
				}
				activityKey = threadSessionKey{Agent: "codex", SessionID: anchor}
				if anchor != row.Subagent {
					confirmedRoots[anchor] = true
					root := ThreadKey{Agent: "codex", SessionID: anchor, ThreadID: anchor}
					addThread(SessionThreadView{Key: root, Type: ThreadMain})
					codexThreadKeys[anchor] = root
				}
				key = ThreadKey{Agent: "codex", SessionID: anchor, ThreadID: row.Subagent}
				thread := SessionThreadView{Key: key, Type: ThreadSubagent, Project: row.Project}
				if found {
					thread.Title, thread.Nickname, thread.Project = actor.Title, actor.Nickname, actor.Project
					if actor.ParentSession != "" {
						addParent(key, actor.ParentSession)
					}
				}
				addParent(key, row.ParentAgent)
				addThread(thread)
				codexThreadKeys[row.Subagent] = key
				addOwnRow(key, row)
			case "unknown":
				id := row.Session
				if id == "" {
					id = row.ResolvedSession.ID
				}
				unknownKey, exists := codexThreadKeys[id]
				if !exists {
					unknownKey = ThreadKey{Agent: "codex", SessionID: id, ThreadID: id}
					addThread(SessionThreadView{Key: unknownKey, Type: ThreadUnknown})
					codexThreadKeys[id] = unknownKey
				}
				addUnknownRow(unknownKey, row)
				if unknownKey.SessionID != "" {
					activityKey = threadSessionKey{Agent: "codex", SessionID: unknownKey.SessionID}
				}
			case "":
				if sessions[sessionKey].Role != "" {
					unknownKey, exists := codexThreadKeys[sessionKey.SessionID]
					if !exists {
						unknownKey = ThreadKey{Agent: "codex", SessionID: sessionKey.SessionID, ThreadID: sessionKey.SessionID}
						addThread(SessionThreadView{Key: unknownKey, Type: ThreadUnknown})
						codexThreadKeys[sessionKey.SessionID] = unknownKey
					}
					addUnknownRow(unknownKey, row)
					activityKey = threadSessionKey{Agent: "codex", SessionID: unknownKey.SessionID}
				} else if isCodexSubagentKind(row.Kind) {
					usage := legacySubagentUsages[key]
					if usage == nil {
						usage = &threadUsage{}
						legacySubagentUsages[key] = usage
					}
					usage.addRow(row)
				} else {
					addOwnRow(key, row)
				}
			default:
				continue
			}
		default:
			addOwnRow(key, row)
		}
		if row.Time.After(lastActivities[activityKey]) {
			lastActivities[activityKey] = row.Time
		}
		hasUsageRows[activityKey] = true
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
	for key, usage := range unknownUsages {
		projection.unknown[key] = usage.totals.View()
	}
	for key, usage := range legacySubagentUsages {
		projection.legacySubagents[threadSessionKey{Agent: key.Agent, SessionID: key.SessionID}] = usage.totals.View()
	}
	for key, index := range projection.byKey {
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

	for key, values := range parents {
		index, exists := projection.byKey[key]
		if !exists {
			continue
		}
		if len(values) == 1 {
			for parent := range values {
				projection.Threads[index].ParentThreadID = parent
			}
		} else if len(values) > 1 {
			slog.Warn("conflicting parent identifiers for session subagent", "agent", key.Agent,
				"session_id", key.SessionID, "thread_id", key.ThreadID)
			projection.Threads[index].ParentThreadID = ""
		}
	}

	projection.reindex()
	items := make([]SessionListItemView, 0, len(dataset.Sessions))
	for index, thread := range projection.Threads {
		if thread.Type == ThreadSubagent {
			if rootIndex, hasRoot := projection.byKey[ThreadKey{Agent: thread.Key.Agent, SessionID: thread.Key.SessionID, ThreadID: thread.Key.SessionID}]; hasRoot && projection.Threads[rootIndex].Type == ThreadMain {
				continue
			}
		}
		if thread.Type != ThreadMain && thread.Type != ThreadSubagent && thread.Type != ThreadUnknown {
			continue
		}
		rollup := projection.threadRollup(index)
		lastActivity := lastActivities[threadSessionKey{Agent: thread.Key.Agent, SessionID: thread.Key.SessionID}]
		if lastActivity.IsZero() && thread.Key.Agent == "codex" && thread.Type == ThreadMain {
			for _, child := range dataset.Sessions {
				if child.Agent == "codex" && child.Role == "subagent" && child.RootSession == thread.Key.ThreadID && child.Last.After(lastActivity) {
					lastActivity = child.Last
				}
			}
		}
		if lastActivity.IsZero() {
			lastActivity = sessions[threadSessionKey{Agent: thread.Key.Agent, SessionID: thread.Key.ThreadID}].Last
		}
		hasRows := hasUsageRows[threadSessionKey{Agent: thread.Key.Agent, SessionID: thread.Key.SessionID}] ||
			sessions[threadSessionKey{Agent: thread.Key.Agent, SessionID: thread.Key.ThreadID}].HasRows
		var count *int64
		if thread.Key.Agent != "codex" {
			value := int64(len(subagentIDs[threadSessionKey{Agent: thread.Key.Agent, SessionID: thread.Key.SessionID}]))
			count = &value
		} else if thread.Type == ThreadMain && confirmedRoots[thread.Key.ThreadID] {
			value := int64(len(rollup.children))
			count = &value
		}
		items = append(items, SessionListItemView{
			Thread: thread, SelfUsage: rollup.selfUsage, SubagentUsage: rollup.subagentUsage,
			UnknownUsage: rollup.unknownUsage, InclusiveUsage: rollup.inclusiveUsage,
			HasUsageRows: hasRows, LastActivity: lastActivity, SubagentCount: count,
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

func (p *threadProjection) threadRollup(index int) sessionRollup {
	thread := p.Threads[index]
	if thread.Type == ThreadUnknown {
		unknown := p.unknown[thread.Key]
		return sessionRollup{unknownUsage: unknown, inclusiveUsage: unknown}
	}
	result := sessionRollup{selfUsage: thread.Usage, inclusiveUsage: thread.Usage, unknownUsage: p.unknown[thread.Key]}
	indices := p.bySession[threadSessionKey{Agent: thread.Key.Agent, SessionID: thread.Key.SessionID}]
	var descendants []int
	if thread.Type == ThreadMain {
		for _, childIndex := range indices {
			if p.Threads[childIndex].Type == ThreadSubagent {
				descendants = append(descendants, childIndex)
			}
		}
	} else if thread.Type == ThreadSubagent {
		descendants = p.descendants(thread.Key, indices)
	}
	for _, childIndex := range descendants {
		child := p.Threads[childIndex]
		result.children = append(result.children, childIndex)
		if child.Type == ThreadSubagent {
			result.subagentUsage = mergeUsage(result.subagentUsage, child.Usage)
		}
		result.unknownUsage = mergeUsage(result.unknownUsage, p.unknown[child.Key])
	}
	if thread.Type == ThreadMain {
		if legacy := p.legacySubagentUsageFor(thread.Key); legacy != (UsageView{}) {
			result.subagentUsage = mergeUsage(result.subagentUsage, legacy)
		}
	}
	result.inclusiveUsage = mergeUsage(result.selfUsage, result.subagentUsage)
	result.inclusiveUsage = mergeUsage(result.inclusiveUsage, result.unknownUsage)
	return result
}

func (p *threadProjection) legacySubagentUsageFor(key ThreadKey) UsageView {
	return p.legacySubagents[threadSessionKey{Agent: key.Agent, SessionID: key.SessionID}]
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
		Type:     ThreadMain,
		Title:    session.Title,
		Nickname: session.Nickname,
		Project:  session.Project,
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
	result.RequestMetrics = result.Usage.requestMetrics.view()
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
	unknownUsage   UsageView
	inclusiveUsage UsageView
	children       []int
}

func (s *Service) SessionDetail(ctx context.Context, q ScopeQuery, key ThreadKey) (SessionDetailView, error) {
	projection, items, _, err := s.loadThreadProjection(ctx, q)
	if err != nil {
		return SessionDetailView{}, err
	}
	index, ok := projection.byKey[key]
	if !ok {
		return SessionDetailView{}, ErrThreadNotFound
	}
	thread := projection.Threads[index]
	lastActivity := thread.LastUsage
	for _, item := range items {
		if item.Thread.Key == key {
			lastActivity = item.LastActivity
			break
		}
	}
	rollup := projection.threadRollup(index)
	detail := SessionDetailView{
		Thread: thread, LastActivity: lastActivity, SelfUsage: rollup.selfUsage, SubagentUsage: rollup.subagentUsage,
		UnknownUsage: rollup.unknownUsage, InclusiveUsage: rollup.inclusiveUsage,
		Children: make([]SessionThreadView, 0, len(rollup.children)),
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
