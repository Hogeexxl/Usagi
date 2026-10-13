package query

import (
	"cmp"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/pricing"
)

func (s *Service) Sessions(ctx context.Context, q SessionQuery) (SessionResult, error) {
	filter, err := normalizeUsageInput(q.Range, q.Filter)
	if err != nil {
		return SessionResult{}, err
	}
	if err := s.ensureSessions(ctx); err != nil {
		return SessionResult{}, err
	}
	workspace := s.newWorkspace()
	type sessionAggregate struct {
		usage  SessionUsage
		models map[[2]string]UsageTotals
	}
	groups := make(map[sessionKey]*sessionAggregate)
	err = s.runRequests(ctx, q.Range, filter, workspace, func(row magpie.UsageRequest, ref SessionRef, session magpie.Session, project ProjectRef) {
		if ref.ID == "" {
			return
		}
		key := sessionKey{Agent: ref.Agent, ID: ref.ID}
		group, ok := groups[key]
		if !ok {
			group = &sessionAggregate{
				usage: SessionUsage{
					Agent: ref.Agent, ID: ref.ID, Title: session.Title, Cwd: session.Cwd, Project: project,
					Start: session.Start, Last: session.Last, FirstUsage: row.Time, LastUsage: row.Time,
				},
				models: make(map[[2]string]UsageTotals),
			}
			groups[key] = group
		}
		totals := totalsOf(row)
		group.usage.Totals.add(totals)
		if row.Time.Before(group.usage.FirstUsage) {
			group.usage.FirstUsage = row.Time
		}
		if row.Time.After(group.usage.LastUsage) {
			group.usage.LastUsage = row.Time
		}
		modelKey := [2]string{ModelName(row.Model), row.Effort}
		modelTotals := group.models[modelKey]
		modelTotals.add(totals)
		group.models[modelKey] = modelTotals
	})
	if err != nil {
		return SessionResult{}, err
	}
	var result SessionResult
	for _, group := range groups {
		for key, totals := range group.models {
			group.usage.ModelEffort = append(group.usage.ModelEffort, ModelEffortUsage{Model: key[0], Effort: key[1], Totals: totals})
		}
		slices.SortFunc(group.usage.ModelEffort, func(a, b ModelEffortUsage) int {
			if c := cmp.Compare(a.Model, b.Model); c != 0 {
				return c
			}
			return cmp.Compare(a.Effort, b.Effort)
		})
		result.Sessions = append(result.Sessions, group.usage)
	}
	slices.SortFunc(result.Sessions, func(a, b SessionUsage) int {
		if c := b.LastUsage.Compare(a.LastUsage); c != 0 {
			return c
		}
		if c := cmp.Compare(a.Agent, b.Agent); c != 0 {
			return c
		}
		return cmp.Compare(a.ID, b.ID)
	})
	return result, nil
}

type sessionLookup struct {
	session magpie.Session
	matched bool
}

// sessionLookups remembers single-session lookups for rows whose session is
// missing from the snapshot. It is shared across queries and replaced whenever
// the session snapshot changes.
type sessionLookups struct {
	mu   sync.Mutex
	memo map[sessionKey]sessionLookup
}

func newSessionLookups() *sessionLookups {
	return &sessionLookups{memo: make(map[sessionKey]sessionLookup)}
}

func (l *sessionLookups) get(key sessionKey) (sessionLookup, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	result, ok := l.memo[key]
	return result, ok
}

func (l *sessionLookups) put(key sessionKey, result sessionLookup) {
	l.mu.Lock()
	l.memo[key] = result
	l.mu.Unlock()
}

type queryWorkspace struct {
	snapshot *sessionSnapshot
	upstream upstream
	memo     *sessionLookups
}

func (s *Service) newWorkspace() *queryWorkspace {
	s.snapshotMu.RLock()
	snapshot, lookups := s.snapshot, s.lookups
	s.snapshotMu.RUnlock()
	return &queryWorkspace{
		snapshot: snapshot,
		upstream: s.upstream,
		memo:     lookups,
	}
}

func (w *queryWorkspace) resolveSession(ctx context.Context, row magpie.UsageRequest) (SessionRef, magpie.Session, error) {
	var candidates []string
	if row.NativeSession != "" {
		candidates = append(candidates, row.NativeSession)
	}
	if row.Session != "" && row.Session != row.NativeSession {
		candidates = append(candidates, row.Session)
	}
	var ref SessionRef
	for i, id := range candidates {
		if i == 0 {
			ref = SessionRef{Agent: row.Agent, ID: id}
		}
		result, err := w.lookupSession(ctx, sessionKey{Agent: row.Agent, ID: id})
		if err != nil {
			return SessionRef{}, magpie.Session{}, err
		}
		if result.matched {
			return SessionRef{Agent: row.Agent, ID: id, Matched: true}, result.session, nil
		}
	}
	return ref, magpie.Session{}, nil
}

func (w *queryWorkspace) lookupSession(ctx context.Context, key sessionKey) (sessionLookup, error) {
	if session, ok := w.snapshot.sessions[key]; ok {
		return sessionLookup{session: session, matched: true}, nil
	}
	if result, ok := w.memo.get(key); ok {
		return result, nil
	}
	session, err := w.upstream.Session(ctx, key.Agent+":"+key.ID)
	if err != nil {
		var httpErr *magpie.HTTPError
		if !errors.As(err, &httpErr) || httpErr.StatusCode != http.StatusNotFound {
			return sessionLookup{}, fmt.Errorf("query: lookup session %s:%s: %w", key.Agent, key.ID, err)
		}
		w.memo.put(key, sessionLookup{})
		return sessionLookup{}, nil
	}
	result := sessionLookup{session: session, matched: true}
	w.memo.put(key, result)
	return result, nil
}

type sessionMetadata struct {
	cwd, title  string
	start, last time.Time
}

func agentFamily(agent string) string {
	if agent == "claude" || agent == "claude-desktop" {
		return "claude"
	}
	return agent
}

func (s *Service) SessionDataset(ctx context.Context, q SessionQuery) (SessionDatasetResult, error) {
	filter, err := normalizeUsageInput(q.Range, q.Filter)
	if err != nil {
		return SessionDatasetResult{}, err
	}
	tx, err := s.db.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return SessionDatasetResult{}, fmt.Errorf("query: begin session dataset: %w", err)
	}
	defer tx.Rollback()

	requests, err := readDatasetRequests(ctx, tx, q.Range, s.location)
	if err != nil {
		return SessionDatasetResult{}, err
	}
	metadata, err := readSessionMetadata(ctx, tx)
	if err != nil {
		return SessionDatasetResult{}, err
	}
	catalog, err := pricing.Load(ctx, tx)
	if err != nil {
		return SessionDatasetResult{}, err
	}
	if err := tx.Commit(); err != nil {
		return SessionDatasetResult{}, fmt.Errorf("query: finish session dataset: %w", err)
	}

	agents := make(map[string]struct{}, len(filter.Agents))
	for _, agent := range filter.Agents {
		agents[agent] = struct{}{}
	}
	models := make(map[ModelSelection]struct{}, len(filter.Models))
	for _, model := range filter.Models {
		models[model] = struct{}{}
	}

	result := SessionDatasetResult{Sessions: []SessionUsage{}, Rows: []UsageRow{}}
	sessions := make(map[sessionKey]*SessionUsage)
	for _, request := range requests {
		if isRejected(request) {
			continue
		}
		id := request.Session
		if request.NativeSession != "" {
			id = request.NativeSession
		}
		if id == "" {
			continue
		}
		family := agentFamily(request.Agent)
		if len(agents) != 0 {
			if _, ok := agents[family]; !ok {
				continue
			}
		}
		if len(models) != 0 {
			if _, ok := models[ModelSelection{ProviderID: request.Provider, Model: ModelName(request.Model)}]; !ok {
				continue
			}
		}

		key := sessionKey{Agent: family, ID: id}
		meta, matched := metadata[key]
		project := ProjectRef{Kind: ProjectKindUnknown}
		if matched {
			project = projectOf(magpie.Session{Cwd: meta.cwd}, true)
		}
		if !matchesProjects(project, filter.Projects) {
			continue
		}
		request.Cost = 0
		request.Priced = false
		if request.Input+request.Output > 0 {
			if price, ok := catalog.PriceAt(request.Time, request.Provider, request.Model, request.Served); ok {
				request.Cost = price.Cost(int64(request.Input), int64(request.Output), int64(request.CacheRead), int64(request.CacheWrite), int64(request.CacheWrite1h))
				request.Priced = true
			}
		}
		row := usageRowOf(request, family, id, project, matched)
		result.Rows = append(result.Rows, row)

		session := sessions[key]
		if session == nil {
			value := sessionUsageOf(key, meta, matched)
			session = &value
			sessions[key] = session
		}
		session.HasRows = true
		session.Totals.add(totalsOf(request))
		if session.FirstUsage.IsZero() || request.Time.Before(session.FirstUsage) {
			session.FirstUsage = request.Time
		}
		if request.Time.After(session.LastUsage) {
			session.LastUsage = request.Time
		}
		model := ModelEffortUsage{Model: ModelName(request.Model), Effort: request.Effort}
		for i := range session.ModelEffort {
			if session.ModelEffort[i].Model == model.Model && session.ModelEffort[i].Effort == model.Effort {
				session.ModelEffort[i].Totals.add(totalsOf(request))
				model = ModelEffortUsage{}
				break
			}
		}
		if model.Model != "" {
			model.Totals = totalsOf(request)
			session.ModelEffort = append(session.ModelEffort, model)
		}
	}

	if len(filter.Models) == 0 {
		for key, meta := range metadata {
			if _, alreadyAdded := sessions[key]; alreadyAdded || meta.last.IsZero() {
				continue
			}
			day := meta.last.In(s.location).Format(time.DateOnly)
			if day < q.Range.From || day > q.Range.To {
				continue
			}
			if len(agents) != 0 {
				if _, ok := agents[key.Agent]; !ok {
					continue
				}
			}
			project := projectOf(magpie.Session{Cwd: meta.cwd}, true)
			if !matchesProjects(project, filter.Projects) {
				continue
			}
			sessions[key] = &SessionUsage{Agent: key.Agent, ID: key.ID, Title: meta.title, Cwd: meta.cwd, Project: project,
				Start: meta.start, Last: meta.last, Matched: true}
		}
	}

	for _, session := range sessions {
		slices.SortFunc(session.ModelEffort, func(a, b ModelEffortUsage) int {
			if c := cmp.Compare(a.Model, b.Model); c != 0 {
				return c
			}
			return cmp.Compare(a.Effort, b.Effort)
		})
		result.Sessions = append(result.Sessions, *session)
	}
	slices.SortFunc(result.Sessions, func(a, b SessionUsage) int {
		if c := cmp.Compare(a.Agent, b.Agent); c != 0 {
			return c
		}
		return cmp.Compare(a.ID, b.ID)
	})
	slices.SortStableFunc(result.Rows, func(a, b UsageRow) int {
		if c := a.Time.Compare(b.Time); c != 0 {
			return c
		}
		left := [6]string{a.Agent, a.ResolvedSession.ID, a.Subagent, a.Provider, a.Model, a.Effort}
		right := [6]string{b.Agent, b.ResolvedSession.ID, b.Subagent, b.Provider, b.Model, b.Effort}
		for i := range left {
			if c := cmp.Compare(left[i], right[i]); c != 0 {
				return c
			}
		}
		return 0
	})
	return result, nil
}

func readDatasetRequests(ctx context.Context, tx *sql.Tx, r DateRange, location *time.Location) ([]magpie.UsageRequest, error) {
	rows, err := tx.QueryContext(ctx, `SELECT t_ns, agent, agent_name, provider, provider_name,
		model, requested, served, effort, kind, session, native_session, subagent, parent_agent,
		source, route_id, rid, response_id, status, rejected, error, tries, input, output,
		cache_read, cache_write, cache_write_1h, reasoning, ms, ttft_ms, first_text_ms, flow_ms,
		magpie_cost, magpie_priced FROM requests
		WHERE local_day >= ? AND local_day <= ? ORDER BY t_ns, id`, r.From, r.To)
	if err != nil {
		return nil, fmt.Errorf("query: read session dataset requests: %w", err)
	}
	defer rows.Close()
	var result []magpie.UsageRequest
	for rows.Next() {
		var request magpie.UsageRequest
		var stamp int64
		var rejected, priced int
		if err := rows.Scan(&stamp, &request.Agent, &request.AgentName, &request.Provider, &request.ProviderName,
			&request.Model, &request.Requested, &request.Served, &request.Effort, &request.Kind,
			&request.Session, &request.NativeSession, &request.Subagent, &request.ParentAgent,
			&request.Source, &request.RouteID, &request.RID, &request.ResponseID, &request.Status,
			&rejected, &request.Error, &request.Tries, &request.Input, &request.Output, &request.CacheRead,
			&request.CacheWrite, &request.CacheWrite1h, &request.Reasoning, &request.Millis, &request.TTFTMs,
			&request.FirstTextMs, &request.FlowMs, &request.Cost, &priced); err != nil {
			return nil, fmt.Errorf("query: scan session dataset request: %w", err)
		}
		request.Time = time.Unix(0, stamp).In(location)
		request.Rejected = rejected != 0
		request.Priced = priced != 0
		result = append(result, request)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("query: iterate session dataset requests: %w", err)
	}
	return result, nil
}

func readSessionMetadata(ctx context.Context, tx *sql.Tx) (map[sessionKey]sessionMetadata, error) {
	rows, err := tx.QueryContext(ctx, `SELECT agent, id, cwd, title, start_ns, last_ns FROM sessions`)
	if err != nil {
		return nil, fmt.Errorf("query: read session dataset metadata: %w", err)
	}
	defer rows.Close()
	result := make(map[sessionKey]sessionMetadata)
	for rows.Next() {
		var agent, id, cwd, title string
		var startNS, lastNS sql.NullInt64
		if err := rows.Scan(&agent, &id, &cwd, &title, &startNS, &lastNS); err != nil {
			return nil, fmt.Errorf("query: scan session dataset metadata: %w", err)
		}
		family := agentFamily(agent)
		key := sessionKey{Agent: family, ID: id}
		incoming := sessionMetadata{cwd: cwd, title: title}
		if startNS.Valid {
			incoming.start = time.Unix(0, startNS.Int64)
		}
		if lastNS.Valid {
			incoming.last = time.Unix(0, lastNS.Int64)
		}
		previous, exists := result[key]
		if !exists {
			result[key] = incoming
			continue
		}
		if agent == "claude" {
			previous.cwd, previous.title = incoming.cwd, incoming.title
		}
		if previous.start.IsZero() || (!incoming.start.IsZero() && incoming.start.Before(previous.start)) {
			previous.start = incoming.start
		}
		if incoming.last.After(previous.last) {
			previous.last = incoming.last
		}
		result[key] = previous
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("query: iterate session dataset metadata: %w", err)
	}
	return result, nil
}

func sessionUsageOf(key sessionKey, metadata sessionMetadata, matched bool) SessionUsage {
	project := ProjectRef{Kind: ProjectKindUnknown}
	if matched {
		project = projectOf(magpie.Session{Cwd: metadata.cwd}, true)
	}
	return SessionUsage{Agent: key.Agent, ID: key.ID, Title: metadata.title, Cwd: metadata.cwd,
		Project: project, Start: metadata.start, Last: metadata.last, Matched: matched}
}

func usageRowOf(request magpie.UsageRequest, agent, id string, project ProjectRef, matched bool) UsageRow {
	return UsageRow{
		Time: request.Time, Agent: agent, AgentName: request.AgentName, Provider: request.Provider,
		ProviderName: request.ProviderName, Model: request.Model, Effort: request.Effort,
		Session: request.Session, NativeSession: request.NativeSession, Subagent: request.Subagent,
		ParentAgent: request.ParentAgent, Kind: request.Kind, Origin: request.Source,
		Status: request.Status, Error: request.Error, ResolvedSession: SessionRef{Agent: agent, ID: id, Matched: matched},
		Project: project, Tries: request.Tries, Millis: request.Millis, TTFTMs: request.TTFTMs,
		FirstTextMs: request.FirstTextMs, FlowMs: request.FlowMs, Input: int64(request.Input),
		Output: int64(request.Output), CacheRead: int64(request.CacheRead), CacheWrite: int64(request.CacheWrite),
		CacheWrite1h: int64(request.CacheWrite1h), Reasoning: int64(request.Reasoning),
		Cost: request.Cost, Priced: request.Priced,
	}
}
