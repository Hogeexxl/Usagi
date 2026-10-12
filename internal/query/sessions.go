package query

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"sync"

	"github.com/Hogeexxl/Usagi/internal/magpie"
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
