package api

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"testing/fstest"
	"time"

	"github.com/Hogeexxl/Usagi/internal/api/testsupport"
	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

type p1aRuntimeProvider struct {
	*testsupport.FakeRuntime
	mu     sync.RWMutex
	client *magpie.Client
	ready  bool
}

type p1aWatcherQueryProbe struct {
	WatcherQuery
	initialReadDone chan<- struct{}
}

func (q p1aWatcherQueryProbe) UsageSnapshot(ctx context.Context) (magpie.UsageSummary, error) {
	value, err := q.WatcherQuery.UsageSnapshot(ctx)
	q.initialReadDone <- struct{}{}
	return value, err
}

func (q p1aWatcherQueryProbe) RefreshSessions(ctx context.Context) ([]query.SessionRefreshFact, error) {
	value, err := q.WatcherQuery.RefreshSessions(ctx)
	q.initialReadDone <- struct{}{}
	return value, err
}

type p1aWatcherProjectionProbe struct {
	ProjectionService
	initialReadDone chan<- struct{}
}

func (p p1aWatcherProjectionProbe) Quotas(ctx context.Context, asked bool) (projection.QuotaView, error) {
	value, err := p.ProjectionService.Quotas(ctx, asked)
	p.initialReadDone <- struct{}{}
	return value, err
}

func (p *p1aRuntimeProvider) Client() (*magpie.Client, bool) {
	p.mu.RLock()
	defer p.mu.RUnlock()
	return p.client, p.ready
}

func (p *p1aRuntimeProvider) set(status magpie.RuntimeStatus, client *magpie.Client) {
	p.mu.Lock()
	p.client = client
	p.ready = status == magpie.RuntimeStatusReady && client != nil
	p.mu.Unlock()
	p.FakeRuntime.SetStatus(status)
}

func TestRuntimeHTTPTransitionRevisionsAvailabilityAndWatcherBaseline(t *testing.T) {
	t.Setenv("TZ", "UTC")
	now := time.Now().UTC().Truncate(time.Second)
	rows := magpie.UsageRequestsPage{
		Rows: []magpie.UsageRequest{{
			Time: now, Agent: "codex", AgentName: "Codex", Provider: "openai", ProviderName: "OpenAI",
			Model: "gpt-5", Input: 12, Output: 3, Status: http.StatusOK, Session: "session-1",
			NativeSession: "session-1", Cost: 0.25, Priced: true,
		}},
		Total: 1,
	}
	sessions := magpie.SessionsResponse{Sessions: []magpie.Session{{
		Agent: "codex", ID: "session-1", Cwd: "/repo", Title: "Example session", Start: now, Last: now,
		Models: []magpie.SessionModel{}, Name: "Codex", Icon: "codex",
	}}}
	usage := magpie.UsageSummary{
		Period: "all", Bucket: "day", Series: []magpie.UsagePoint{}, Agents: []magpie.UsageGroup{},
		Models: []magpie.UsageGroup{}, Accounts: []magpie.UsageGroup{}, CallerKeys: []magpie.UsageGroup{},
		UsageTotals: magpie.UsageTotals{Calls: 1, Input: 12, Output: 3, Cost: 0.25},
	}
	var requestMu sync.Mutex
	usageRequestCalls := 0
	sessionCalls := 0
	quotaAsked := []string{}
	failUsageRequests := false
	magpieServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/api/usage":
			_ = json.NewEncoder(w).Encode(usage)
		case "/api/usage/requests":
			requestMu.Lock()
			usageRequestCalls++
			fail := failUsageRequests
			requestMu.Unlock()
			if fail {
				http.Error(w, "upstream unavailable", http.StatusServiceUnavailable)
				return
			}
			_ = json.NewEncoder(w).Encode(rows)
		case "/api/sessions":
			requestMu.Lock()
			sessionCalls++
			requestMu.Unlock()
			_ = json.NewEncoder(w).Encode(sessions)
		case "/api/usage/quotas":
			requestMu.Lock()
			quotaAsked = append(quotaAsked, r.URL.Query().Get("asked"))
			requestMu.Unlock()
			_, _ = w.Write([]byte("[]"))
		default:
			http.NotFound(w, r)
		}
	}))
	defer magpieServer.Close()

	runtime := &p1aRuntimeProvider{FakeRuntime: testsupport.NewFakeRuntime(magpie.RuntimeStatusStarting)}
	queryService := query.NewRuntimeService(runtime)
	projectionService := projection.NewService(queryService)
	revisions := NewRevisionService()
	quotaSnapshot := &QuotaSnapshot{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	watcher := newWatchers(queryService, projectionService, revisions, quotaSnapshot, time.Hour, time.Hour, time.Hour)
	watcher.refreshUsage(ctx)
	watcher.refreshSessions(ctx)
	config := DefaultConfig()
	config.StaticFS = fstest.MapFS{}
	initialWatcherReads := make(chan struct{}, 3)
	server, err := NewServer(config, Dependencies{
		Projection: p1aWatcherProjectionProbe{ProjectionService: projectionService, initialReadDone: initialWatcherReads},
		Query:      p1aWatcherQueryProbe{WatcherQuery: queryService, initialReadDone: initialWatcherReads}, Revisions: revisions,
		RuntimeStatus: runtime, QuotaSnapshot: quotaSnapshot, ShutdownRequester: func() {},
	})
	if err != nil {
		t.Fatalf("NewServer() error = %v", err)
	}
	server.Start(ctx)
	for i := 0; i < 3; i++ {
		select {
		case <-initialWatcherReads:
		case <-time.After(time.Second):
			t.Fatal("timed out waiting for initial usage, sessions, and quota watcher reads")
		}
	}

	starting := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if starting.Code != http.StatusServiceUnavailable || !strings.Contains(starting.Body.String(), ErrorMagpieUnavailable) {
		t.Fatalf("starting summary response = %d %s, want 503 %s", starting.Code, starting.Body.String(), ErrorMagpieUnavailable)
	}
	if got := revisions.Current(); got != (RevisionTuple{}) {
		t.Fatalf("starting failed refreshes tuple = %+v, want zero", got)
	}

	streamWriter := newStreamWriter()
	streamCtx, stopStream := context.WithCancel(ctx)
	streamRequest := httptest.NewRequest(http.MethodGet, "/api/events", nil).WithContext(streamCtx)
	streamRequest.Host = "127.0.0.1:37991"
	streamDone := make(chan struct{})
	go func() {
		defer close(streamDone)
		server.ServeHTTP(streamWriter, streamRequest)
	}()
	defer func() {
		stopStream()
		select {
		case <-streamDone:
		case <-time.After(time.Second):
			t.Error("revision event stream did not stop after cancellation")
		}
	}()
	streamWriter.waitFlushes(t, 1)

	runtime.set(magpie.RuntimeStatusReady, magpie.NewClient(magpieServer.URL, magpieServer.Client()))
	waitFor(t, func() bool {
		return revisions.Current() == (RevisionTuple{DataRevision: 1, StatusRevision: 1})
	})
	waitFor(t, func() bool {
		return strings.Contains(streamWriter.bodyString(), `data: {"data_revision":1,"status_revision":1}`)
	})

	watcher.refreshUsage(ctx)
	watcher.refreshSessions(ctx)
	if got := revisions.Current(); got != (RevisionTuple{DataRevision: 1, StatusRevision: 1}) {
		t.Fatalf("first successful watcher baselines tuple = %+v, want ready transition only", got)
	}
	watcher.refreshUsage(ctx)
	watcher.refreshSessions(ctx)
	if got := revisions.Current(); got != (RevisionTuple{DataRevision: 1, StatusRevision: 1}) {
		t.Fatalf("unchanged watcher snapshots tuple = %+v, want ready transition only", got)
	}
	requestMu.Lock()
	if sessionCalls != 2 {
		requestMu.Unlock()
		t.Fatalf("session endpoint calls from two manual watcher ticks = %d, want exactly 2", sessionCalls)
	}
	if len(quotaAsked) != 0 {
		requestMu.Unlock()
		t.Fatalf("quota calls before manual quota tick = %v, want none", quotaAsked)
	}
	requestMu.Unlock()

	ready := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if ready.Code != http.StatusOK || !strings.Contains(ready.Body.String(), `"data_revision":1`) || !strings.Contains(ready.Body.String(), `"input":12`) {
		t.Fatalf("ready summary response = %d %s, want successful revision 1 summary", ready.Code, ready.Body.String())
	}
	requestMu.Lock()
	failUsageRequests = true
	requestMu.Unlock()
	upstreamFailed := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if upstreamFailed.Code != http.StatusBadGateway || !strings.Contains(upstreamFailed.Body.String(), ErrorUpstreamFailed) {
		t.Fatalf("Magpie Web failure response = %d %s, want 502 %s", upstreamFailed.Code, upstreamFailed.Body.String(), ErrorUpstreamFailed)
	}
	requestMu.Lock()
	failUsageRequests = false
	requestsBeforeUnavailable := usageRequestCalls
	requestMu.Unlock()

	runtime.set(magpie.RuntimeStatusUnavailable, nil)
	waitFor(t, func() bool {
		return revisions.Current() == (RevisionTuple{DataRevision: 1, StatusRevision: 2})
	})
	waitFor(t, func() bool {
		return strings.Contains(streamWriter.bodyString(), `data: {"data_revision":1,"status_revision":2}`)
	})
	unavailable := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if unavailable.Code != http.StatusServiceUnavailable || !strings.Contains(unavailable.Body.String(), ErrorMagpieUnavailable) {
		t.Fatalf("unavailable summary response = %d %s, want 503 %s", unavailable.Code, unavailable.Body.String(), ErrorMagpieUnavailable)
	}
	requestMu.Lock()
	defer requestMu.Unlock()
	if usageRequestCalls != requestsBeforeUnavailable {
		t.Fatalf("old Magpie client received %d requests after Runtime became unavailable", usageRequestCalls-requestsBeforeUnavailable)
	}
}
