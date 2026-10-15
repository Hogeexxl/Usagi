package query

import (
	"cmp"
	"context"
	"database/sql"
	"fmt"
	"slices"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/pricing"
)

type sessionMetadata struct {
	cwd, title, nickname             string
	role, parentSession, rootSession string
	start, last                      time.Time
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
		family := agentFamily(request.Agent)
		id, actorID, role := requestSessionIdentity(request, family)
		if id == "" {
			continue
		}
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
		actorKey := sessionKey{Agent: family, ID: actorID}
		meta, matched := metadata[key]
		actorMeta, actorMatched := metadata[actorKey]
		projectMeta, projectMatched := meta, matched
		if !projectMatched && actorMatched {
			projectMeta, projectMatched = actorMeta, true
		}
		project := ProjectRef{Kind: ProjectKindUnknown}
		if projectMatched {
			project = projectOf(magpie.Session{Cwd: projectMeta.cwd}, true)
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
		row.SessionRole = role
		result.Rows = append(result.Rows, row)
		if family == "codex" && role == "unknown" {
			if actorID != "" {
				if _, exists := sessions[actorKey]; !exists {
					value := sessionUsageOf(actorKey, actorMeta, actorMatched)
					if !actorMatched {
						value.Role = "unknown"
						value.Project = project
					}
					value.HasRows = true
					sessions[actorKey] = &value
				} else {
					sessions[actorKey].HasRows = true
				}
			}
			continue
		}
		if actorID == "" {
			continue
		}
		if family == "codex" && role == "subagent" && request.Session != "" {
			if _, exists := sessions[key]; !exists {
				root := sessionUsageOf(key, meta, matched)
				root.Role, root.RootSession = "main", id
				sessions[key] = &root
			} else if sessions[key].Role == "unknown" {
				sessions[key].Role, sessions[key].RootSession = "main", id
			}
		}
		actor := sessions[actorKey]
		if actor == nil {
			value := sessionUsageOf(actorKey, actorMeta, actorMatched)
			if family == "codex" && (actorMeta.role == "" || actorMeta.role == "unknown") {
				value.Role = role
				if role == "main" {
					value.RootSession = actorID
				} else if role == "subagent" {
					value.ParentSession, value.RootSession = request.ParentAgent, request.Session
				}
			}
			if !actorMatched && projectMatched {
				value.Project = project
			}
			actor = &value
			sessions[actorKey] = actor
		} else if family == "codex" && (actor.Role == "" || actor.Role == "unknown") && (role == "main" || role == "subagent") {
			actor.Role = role
			if role == "main" {
				actor.RootSession = actorID
			} else {
				actor.ParentSession, actor.RootSession = request.ParentAgent, request.Session
			}
		}
		actor.HasRows = true
		actor.Totals.add(totalsOf(request))
		if actor.FirstUsage.IsZero() || request.Time.Before(actor.FirstUsage) {
			actor.FirstUsage = request.Time
		}
		if request.Time.After(actor.LastUsage) {
			actor.LastUsage = request.Time
		}
		addSessionModelEffort(actor, request)
	}

	if len(filter.Models) == 0 {
		for key, meta := range metadata {
			if key.Agent == "codex" {
				continue
			}
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
			sessions[key] = &SessionUsage{Agent: key.Agent, ID: key.ID, Title: meta.title, Nickname: meta.nickname, Cwd: meta.cwd, Project: project,
				Role: meta.role, ParentSession: meta.parentSession, RootSession: meta.rootSession,
				Start: meta.start, Last: meta.last, Matched: true}
		}
	}

	var supplementalRoots []*SessionUsage
	supplementalRootIDs := make(map[sessionKey]struct{})
	for _, child := range sessions {
		if child.Agent != "codex" || child.Role != "subagent" || child.RootSession == "" {
			continue
		}
		rootKey := sessionKey{Agent: "codex", ID: child.RootSession}
		if root, exists := sessions[rootKey]; exists {
			if root.Role == "" || root.Role == "unknown" {
				root.Role, root.RootSession = "main", child.RootSession
			}
			continue
		}
		if _, exists := supplementalRootIDs[rootKey]; exists {
			continue
		}
		meta, exists := metadata[rootKey]
		if !exists {
			continue
		}
		root := sessionUsageOf(rootKey, meta, true)
		root.Role, root.RootSession = "main", child.RootSession
		root.Last = time.Time{}
		supplementalRoots = append(supplementalRoots, &root)
		supplementalRootIDs[rootKey] = struct{}{}
	}
	for _, root := range supplementalRoots {
		sessions[sessionKey{Agent: root.Agent, ID: root.ID}] = root
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

func requestSessionIdentity(request magpie.UsageRequest, family string) (resolvedID, actorID, role string) {
	if family != "codex" || request.SessionRole == "" {
		id := request.Session
		if request.NativeSession != "" {
			id = request.NativeSession
		}
		return id, id, ""
	}
	switch request.SessionRole {
	case "main":
		if request.Session == "" {
			return "", "", "unknown"
		}
		return request.Session, request.Session, "main"
	case "subagent":
		if request.Subagent == "" {
			return request.Session, request.Session, "unknown"
		}
		if request.Session == "" {
			return request.Subagent, request.Subagent, "subagent"
		}
		return request.Session, request.Subagent, "subagent"
	default:
		return request.Session, request.Session, "unknown"
	}
}

func addSessionModelEffort(session *SessionUsage, request magpie.UsageRequest) {
	model := ModelEffortUsage{Model: ModelName(request.Model), Effort: request.Effort}
	for i := range session.ModelEffort {
		if session.ModelEffort[i].Model == model.Model && session.ModelEffort[i].Effort == model.Effort {
			session.ModelEffort[i].Totals.add(totalsOf(request))
			return
		}
	}
	model.Totals = totalsOf(request)
	session.ModelEffort = append(session.ModelEffort, model)
}

func readDatasetRequests(ctx context.Context, tx *sql.Tx, r DateRange, location *time.Location) ([]magpie.UsageRequest, error) {
	rows, err := tx.QueryContext(ctx, `SELECT t_ns, agent, agent_name, provider, provider_name,
		model, requested, served, effort, kind, session, session_role, compaction, native_session, subagent, parent_agent,
		source, route_id, rid, response_id, status, rejected, error, tries, input, output,
		cache_read, cache_write, cache_write_1h, reasoning, ms, ttft_ms, first_text_ms,
		first_reasoning_ms, flow_ms, reasoning_hidden, handed_on,
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
		var rejected, priced, reasoningHidden, handedOn int
		var compaction sql.NullInt64
		if err := rows.Scan(&stamp, &request.Agent, &request.AgentName, &request.Provider, &request.ProviderName,
			&request.Model, &request.Requested, &request.Served, &request.Effort, &request.Kind,
			&request.Session, &request.SessionRole, &compaction, &request.NativeSession, &request.Subagent, &request.ParentAgent,
			&request.Source, &request.RouteID, &request.RID, &request.ResponseID, &request.Status,
			&rejected, &request.Error, &request.Tries, &request.Input, &request.Output, &request.CacheRead,
			&request.CacheWrite, &request.CacheWrite1h, &request.Reasoning, &request.Millis, &request.TTFTMs,
			&request.FirstTextMs, &request.FirstReasoningMs, &request.FlowMs, &reasoningHidden, &handedOn,
			&request.Cost, &priced); err != nil {
			return nil, fmt.Errorf("query: scan session dataset request: %w", err)
		}
		request.Time = time.Unix(0, stamp).In(location)
		request.Rejected = rejected != 0
		request.Priced = priced != 0
		request.ReasoningHidden = reasoningHidden != 0
		request.HandedOn = handedOn != 0
		if compaction.Valid {
			value := compaction.Int64 != 0
			request.Compaction = &value
		}
		result = append(result, request)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("query: iterate session dataset requests: %w", err)
	}
	return result, nil
}

func readSessionMetadata(ctx context.Context, tx *sql.Tx) (map[sessionKey]sessionMetadata, error) {
	rows, err := tx.QueryContext(ctx, `SELECT agent, id, cwd, title, nickname, start_ns, last_ns, role, parent_session, root_session FROM sessions`)
	if err != nil {
		return nil, fmt.Errorf("query: read session dataset metadata: %w", err)
	}
	defer rows.Close()
	result := make(map[sessionKey]sessionMetadata)
	for rows.Next() {
		var agent, id, cwd, title, nickname, role, parentSession, rootSession string
		var startNS, lastNS sql.NullInt64
		if err := rows.Scan(&agent, &id, &cwd, &title, &nickname, &startNS, &lastNS, &role, &parentSession, &rootSession); err != nil {
			return nil, fmt.Errorf("query: scan session dataset metadata: %w", err)
		}
		family := agentFamily(agent)
		key := sessionKey{Agent: family, ID: id}
		incoming := sessionMetadata{cwd: cwd, title: title, nickname: nickname, role: role, parentSession: parentSession, rootSession: rootSession}
		if family != "codex" {
			incoming.role, incoming.parentSession, incoming.rootSession = "", "", ""
		}
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
			previous.cwd, previous.title, previous.nickname = incoming.cwd, incoming.title, incoming.nickname
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
	return SessionUsage{Agent: key.Agent, ID: key.ID, Role: metadata.role, ParentSession: metadata.parentSession,
		RootSession: metadata.rootSession, Title: metadata.title, Nickname: metadata.nickname, Cwd: metadata.cwd,
		Project: project, Start: metadata.start, Last: metadata.last, Matched: matched}
}

func usageRowOf(request magpie.UsageRequest, agent, id string, project ProjectRef, matched bool) UsageRow {
	return UsageRow{
		Time: request.Time, Agent: agent, AgentName: request.AgentName, Provider: request.Provider,
		ProviderName: request.ProviderName, Model: request.Model, Effort: request.Effort,
		Session: request.Session, SessionRole: request.SessionRole, Compaction: request.Compaction,
		NativeSession: request.NativeSession, Subagent: request.Subagent,
		ParentAgent: request.ParentAgent, Kind: request.Kind, Origin: request.Source,
		Status: request.Status, Error: request.Error, ResolvedSession: SessionRef{Agent: agent, ID: id, Matched: matched},
		Project: project, Tries: request.Tries, Millis: request.Millis, TTFTMs: request.TTFTMs,
		FirstTextMs: request.FirstTextMs, FirstReasoningMs: request.FirstReasoningMs,
		FlowMs: request.FlowMs, ReasoningHidden: request.ReasoningHidden, HandedOn: request.HandedOn,
		Input:  int64(request.Input),
		Output: int64(request.Output), CacheRead: int64(request.CacheRead), CacheWrite: int64(request.CacheWrite),
		CacheWrite1h: int64(request.CacheWrite1h), Reasoning: int64(request.Reasoning),
		Cost: request.Cost, Priced: request.Priced,
	}
}
