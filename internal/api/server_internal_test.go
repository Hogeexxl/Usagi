package api

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"testing/fstest"

	"github.com/Hogeexxl/Usagi/internal/api/testsupport"
	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

func newServerFixture(t *testing.T) (*Server, *testsupport.FakeProjectionService, *testsupport.FakeRuntime, *testsupport.FakeWatcherQuery) {
	return newServerFixtureWithShutdown(t, func() {})
}

func newServerFixtureWithShutdown(t *testing.T, shutdownRequester ShutdownRequester) (*Server, *testsupport.FakeProjectionService, *testsupport.FakeRuntime, *testsupport.FakeWatcherQuery) {
	t.Helper()
	t.Setenv("TZ", "UTC")
	projectionService := &testsupport.FakeProjectionService{}
	runtime := testsupport.NewFakeRuntime(magpie.RuntimeStatusStarting)
	queryService := &testsupport.FakeWatcherQuery{}
	config := DefaultConfig()
	config.StaticFS = fstest.MapFS{
		"index.html":       &fstest.MapFile{Data: []byte("dashboard-index")},
		"assets/app.js":    &fstest.MapFile{Data: []byte("dashboard-script")},
		"assets/theme.css": &fstest.MapFile{Data: []byte("dashboard-style")},
	}
	server, err := NewServer(config, Dependencies{
		Projection: projectionService, Query: queryService, Revisions: NewRevisionService(),
		RuntimeStatus: runtime, QuotaSnapshot: &QuotaSnapshot{}, ShutdownRequester: shutdownRequester,
	})
	if err != nil {
		t.Fatalf("NewServer() error = %v", err)
	}
	return server, projectionService, runtime, queryService
}

func request(server http.Handler, method, target string) *httptest.ResponseRecorder {
	recorder := httptest.NewRecorder()
	req := httptest.NewRequest(method, target, nil)
	req.Host = "127.0.0.1:37991"
	server.ServeHTTP(recorder, req)
	return recorder
}

func TestDefaultConfigAndServerAcceptsStartingRuntime(t *testing.T) {
	config := DefaultConfig()
	if config.ListenAddress != "127.0.0.1:37991" || config.UsageRefreshInterval != 30*time.Second ||
		config.SessionRefreshInterval != 30*time.Second || config.QuotaRefreshInterval != 60*time.Second {
		t.Fatalf("DefaultConfig() = %+v", config)
	}
	server, _, _, _ := newServerFixture(t)
	for _, path := range []string{"/api/health", "/api/status", "/api/revision"} {
		response := request(server, http.MethodGet, path)
		if response.Code != http.StatusNoContent && path == "/api/health" {
			t.Fatalf("GET %s status = %d, want 204", path, response.Code)
		}
		if path != "/api/health" && response.Code != http.StatusOK {
			t.Fatalf("GET %s status = %d, want 200: %s", path, response.Code, response.Body.String())
		}
	}
	status := request(server, http.MethodGet, "/api/status")
	if !strings.Contains(status.Body.String(), `"runtime_state":"starting"`) || !strings.Contains(status.Body.String(), `"last_error_code":null`) {
		t.Fatalf("starting status body = %s", status.Body.String())
	}
}

func TestNewServerRejectsNonLoopbackAndNonpositiveIntervals(t *testing.T) {
	projectionService := &testsupport.FakeProjectionService{}
	runtime := testsupport.NewFakeRuntime(magpie.RuntimeStatusStarting)
	queryService := &testsupport.FakeWatcherQuery{}
	dependencies := Dependencies{
		Projection: projectionService, Query: queryService, Revisions: NewRevisionService(),
		RuntimeStatus: runtime, QuotaSnapshot: &QuotaSnapshot{}, ShutdownRequester: func() {},
	}
	config := DefaultConfig()
	config.ListenAddress = "0.0.0.0:37991"
	if _, err := NewServer(config, dependencies); err == nil {
		t.Fatal("NewServer accepted a non-loopback listen address")
	}
	config = DefaultConfig()
	config.QuotaRefreshInterval = -time.Second
	if _, err := NewServer(config, dependencies); err == nil {
		t.Fatal("NewServer accepted a nonpositive refresh interval")
	}
}

func TestServerStartConnectsRuntimeSubscriptionAndWatchers(t *testing.T) {
	server, _, runtime, queryService := newServerFixture(t)
	beforeReady := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if beforeReady.Code != http.StatusServiceUnavailable || !strings.Contains(beforeReady.Body.String(), ErrorMagpieUnavailable) {
		t.Fatalf("business endpoint before ready = %d %s", beforeReady.Code, beforeReady.Body.String())
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	server.Start(ctx)
	waitFor(t, func() bool {
		usageCalls, sessionCalls := queryService.CallCounts()
		return usageCalls == 1 && sessionCalls == 1
	})
	runtime.SetStatus(magpie.RuntimeStatusReady)
	waitFor(t, func() bool {
		return server.revisions.Current() == (RevisionTuple{DataRevision: 1, StatusRevision: 1})
	})
	afterReady := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if afterReady.Code != http.StatusOK {
		t.Fatalf("business endpoint after ready = %d %s", afterReady.Code, afterReady.Body.String())
	}
	cancel()
}

func TestFakeRuntimeSubscriptionPreservesSnapshotAndEveryTransition(t *testing.T) {
	runtime := testsupport.NewFakeRuntime(magpie.RuntimeStatusStarting)
	statuses, cancel := runtime.Subscribe()
	defer cancel()
	wants := []magpie.RuntimeStatus{magpie.RuntimeStatusStarting}
	for index := 0; index < 128; index++ {
		status := magpie.RuntimeStatusReady
		if index%2 == 0 {
			status = magpie.RuntimeStatusUnavailable
		}
		runtime.SetStatus(status)
		wants = append(wants, status)
	}
	for _, want := range wants {
		select {
		case got := <-statuses:
			if got != want {
				t.Fatalf("runtime event = %q, want %q", got, want)
			}
		case <-time.After(time.Second):
			t.Fatalf("timed out waiting for runtime event %q", want)
		}
	}
}

func TestInternalUsageEndpointsValidateMapAndClassifyErrors(t *testing.T) {
	thread := projection.SessionThreadView{
		Key:  projection.ThreadKey{Agent: "claude", SessionID: "session-1", ThreadID: "thread-1"},
		Type: projection.ThreadMain, Title: "main", Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/repo", Name: "repo"},
		ModelEffort: []projection.ModelEffortUsageView{},
	}
	cases := []struct {
		name        string
		path        string
		invalidPath string
		setError    func(*testsupport.FakeProjectionService, error)
	}{
		{
			name: "summary", path: "/api/usage/summary?range=today", invalidPath: "/api/usage/summary",
			setError: func(p *testsupport.FakeProjectionService, err error) { p.SummaryErr = err },
		},
		{
			name: "model distribution", path: "/api/usage/model-distribution?range=today", invalidPath: "/api/usage/model-distribution?range=today&model=gpt-x",
			setError: func(p *testsupport.FakeProjectionService, err error) { p.ModelDistributionErr = err },
		},
		{
			name: "project distribution", path: "/api/usage/projects?range=today", invalidPath: "/api/usage/projects?range=today&include_unknown_project=true",
			setError: func(p *testsupport.FakeProjectionService, err error) { p.ProjectDistributionErr = err },
		},
		{
			name: "filter options", path: "/api/usage/filter-options", invalidPath: "/api/usage/filter-options?range=today",
			setError: func(p *testsupport.FakeProjectionService, err error) { p.FilterOptionsErr = err },
		},
		{
			name: "sessions", path: "/api/usage/sessions?range=today", invalidPath: "/api/usage/sessions?range=today&model_at=provider",
			setError: func(p *testsupport.FakeProjectionService, err error) { p.SessionsErr = err },
		},
		{
			name: "session detail", path: "/api/usage/session-detail?range=today&agent=claude&session_id=session-1&thread_id=thread-1",
			invalidPath: "/api/usage/session-detail?range=today&agent=claude&session_id=session-1",
			setError:    func(p *testsupport.FakeProjectionService, err error) { p.SessionDetailErr = err },
		},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			server, projectionService, runtime, _ := newServerFixture(t)
			runtime.SetStatus(magpie.RuntimeStatusReady)
			projectionService.SummaryResult = projection.SummaryView{Usage: projection.UsageView{Calls: 3, Input: 7}, RootSessionCount: 2, RootUnpricedSessionCount: 1}
			projectionService.ModelDistributionResult = projection.ModelDistributionView{Items: []projection.ModelDistributionItem{{Model: "gpt-x", Usage: projection.UsageView{Total: 4}}}}
			projectionService.ProjectDistributionResult = projection.ProjectDistributionView{Items: []projection.ProjectDistributionItem{{Project: query.ProjectRef{Kind: query.ProjectKindProject, Path: "/repo", Name: "repo"}}}}
			projectionService.FilterOptionsResult = projection.FilterOptionsView{
				Agents:   []query.AgentOption{{Agent: "claude", DisplayName: "Claude"}},
				Models:   []query.ModelOption{{Model: "gpt-x", ProviderID: "provider-a", ProviderName: "Provider A"}, {Model: "gpt-x", ProviderID: "provider-b", ProviderName: "Provider B"}},
				Projects: []query.ProjectRef{{Kind: query.ProjectKindProject, Path: "/repo", Name: "repo"}},
			}
			projectionService.SessionsResult = projection.SessionListView{Items: []projection.SessionListItemView{{
				Thread: thread, SelfUsage: projection.UsageView{Calls: 2}, InclusiveUsage: projection.UsageView{Calls: 2},
			}, {
				Thread: projection.SessionThreadView{
					Key:  projection.ThreadKey{Agent: "claude", SessionID: "session-2", ThreadID: "main-2"},
					Type: projection.ThreadMain,
				},
				SelfUsage: projection.UsageView{Calls: 3}, SubagentUsage: projection.UsageView{Calls: 5},
				InclusiveUsage: projection.UsageView{Calls: 8}, SubagentCount: 2,
			}}}
			projectionService.SessionDetailResult = projection.SessionDetailView{
				Thread: thread, SelfUsage: projection.UsageView{Calls: 2}, InclusiveUsage: projection.UsageView{Calls: 2},
				Children: []projection.SessionThreadView{},
			}

			response := request(server, http.MethodGet, test.path)
			if response.Code != http.StatusOK {
				t.Fatalf("GET %s status = %d: %s", test.path, response.Code, response.Body.String())
			}
			if response.Header().Get("Content-Type") != "application/json; charset=utf-8" || response.Header().Get("Cache-Control") != "no-store" {
				t.Fatalf("GET %s headers = %v", test.path, response.Header())
			}
			if strings.Contains(response.Body.String(), "estimated_cost_status") {
				t.Fatalf("GET %s returned retired usage field: %s", test.path, response.Body.String())
			}
			if test.name == "sessions" || test.name == "session detail" {
				for _, retired := range []string{"source", "native_session_id", "root_session_id", "data_status", "error_code"} {
					if strings.Contains(response.Body.String(), `"`+retired+`"`) {
						t.Fatalf("GET %s returned retired session field %q: %s", test.path, retired, response.Body.String())
					}
				}
			}
			if test.name == "filter options" && (strings.Count(response.Body.String(), `"model":"gpt-x"`) != 2 || !strings.Contains(response.Body.String(), `"provider_name":"Provider B"`)) {
				t.Fatalf("filter model pair DTO = %s", response.Body.String())
			}
			if test.name == "sessions" && len(projectionService.SessionDetailQueries) != 0 {
				t.Fatalf("Session list handler made %d detail calls", len(projectionService.SessionDetailQueries))
			}
			if test.name == "sessions" {
				var decoded sessionsResponseDTO
				if err := json.Unmarshal(response.Body.Bytes(), &decoded); err != nil {
					t.Fatalf("decode session response: %v", err)
				}
				if len(decoded.Items) != 2 || decoded.Items[0].SubagentCount != 0 || decoded.Items[0].InclusiveUsage.Calls != decoded.Items[0].SelfUsage.Calls ||
					decoded.Items[1].SubagentCount != 2 || decoded.Items[1].InclusiveUsage.Calls != 8 {
					t.Fatalf("session list rollups = %+v", decoded.Items)
				}
			}
			if test.name == "session detail" {
				keys := projectionService.SessionDetailKeys
				if len(keys) != 1 || keys[0] != (projection.ThreadKey{Agent: "claude", SessionID: "session-1", ThreadID: "thread-1"}) {
					t.Fatalf("SessionDetail keys = %+v", keys)
				}
			}

			invalid := request(server, http.MethodGet, test.invalidPath)
			if invalid.Code != http.StatusBadRequest {
				t.Fatalf("GET %s status = %d, want 400: %s", test.invalidPath, invalid.Code, invalid.Body.String())
			}

			test.setError(projectionService, query.ErrUpstreamUnavailable)
			unavailable := request(server, http.MethodGet, test.path)
			if unavailable.Code != http.StatusServiceUnavailable || !strings.Contains(unavailable.Body.String(), ErrorMagpieUnavailable) {
				t.Fatalf("GET %s unavailable response = %d %s", test.path, unavailable.Code, unavailable.Body.String())
			}
		})
	}
}

func TestInternalAPIQuotaCacheAndManualRefresh(t *testing.T) {
	server, projectionService, runtime, _ := newServerFixture(t)
	runtime.SetStatus(magpie.RuntimeStatusReady)
	quotaSnapshot := server.quotas
	initial := request(server, http.MethodGet, "/api/quota")
	if initial.Code != http.StatusOK || !strings.Contains(initial.Body.String(), `"reading":true`) || !strings.Contains(initial.Body.String(), `"items":[]`) {
		t.Fatalf("initial quota response = %d %s", initial.Code, initial.Body.String())
	}
	view := projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "provider-a", Name: "Provider A", Windows: []projection.QuotaWindowView{{
		Name: "weekly", Used: 25, Limit: 100, ResetSecs: 3600,
	}}}}}
	projectionService.Quota.Result = view
	refreshed := request(server, http.MethodPost, "/api/quota/refresh")
	if refreshed.Code != http.StatusOK || !strings.Contains(refreshed.Body.String(), `"resets_at_ms":null`) || !strings.Contains(refreshed.Body.String(), `"reset_secs":3600`) {
		t.Fatalf("manual quota refresh response = %d %s", refreshed.Code, refreshed.Body.String())
	}
	asked := append([]bool(nil), projectionService.Quota.Asked...)
	if len(asked) != 1 || !asked[0] {
		t.Fatalf("manual quota asked values = %v, want [true]", asked)
	}
	cached := request(server, http.MethodGet, "/api/quota")
	if cached.Code != http.StatusOK || !strings.Contains(cached.Body.String(), `"provider":"provider-a"`) {
		t.Fatalf("cached quota response = %d %s", cached.Code, cached.Body.String())
	}
	projectionService.Quota.Err = query.ErrUpstreamUnavailable
	failed := request(server, http.MethodPost, "/api/quota/refresh")
	if failed.Code != http.StatusServiceUnavailable {
		t.Fatalf("failed quota refresh status = %d: %s", failed.Code, failed.Body.String())
	}
	afterFailure := request(server, http.MethodGet, "/api/quota")
	if afterFailure.Code != http.StatusOK || !strings.Contains(afterFailure.Body.String(), `"provider":"provider-a"`) {
		t.Fatalf("cached quota after refresh error = %d %s", afterFailure.Code, afterFailure.Body.String())
	}
	if _, loaded, err := quotaSnapshot.Current(); !loaded || !errors.Is(err, query.ErrUpstreamUnavailable) {
		t.Fatalf("quota snapshot after refresh = loaded %t, error %v", loaded, err)
	}
	if badQuery := request(server, http.MethodGet, "/api/quota?asked=1"); badQuery.Code != http.StatusBadRequest {
		t.Fatalf("quota GET accepted query parameter: %d", badQuery.Code)
	}
	if badRefresh := request(server, http.MethodPost, "/api/quota/refresh?asked=1"); badRefresh.Code != http.StatusBadRequest {
		t.Fatalf("quota refresh accepted query parameter: %d", badRefresh.Code)
	}
}

func TestInternalHandlersParseModelAtByProviderAndModelPair(t *testing.T) {
	cases := []struct {
		name string
		path string
		get  func(*testsupport.FakeProjectionService) projection.ScopeQuery
	}{
		{name: "summary", path: "/api/usage/summary?range=today&model_at=provider-b%2Fgpt-x&model_at=provider-a%2Fgpt-x&model_at=provider-a%2Fgpt-x", get: func(p *testsupport.FakeProjectionService) projection.ScopeQuery { return p.SummaryQueries[0] }},
		{name: "model distribution", path: "/api/usage/model-distribution?range=today&model_at=provider-a%2Fgpt-x", get: func(p *testsupport.FakeProjectionService) projection.ScopeQuery { return p.ModelDistributionQueries[0] }},
		{name: "project distribution", path: "/api/usage/projects?range=today&model_at=provider-a%2Fgpt-x", get: func(p *testsupport.FakeProjectionService) projection.ScopeQuery {
			return p.ProjectDistributionQueries[0]
		}},
		{name: "sessions", path: "/api/usage/sessions?range=today&model_at=provider-a%2Fgpt-x", get: func(p *testsupport.FakeProjectionService) projection.ScopeQuery { return p.SessionQueries[0] }},
		{name: "detail", path: "/api/usage/session-detail?range=today&agent=claude&session_id=s&thread_id=t&model_at=provider-a%2Fgpt-x", get: func(p *testsupport.FakeProjectionService) projection.ScopeQuery { return p.SessionDetailQueries[0] }},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			server, projectionService, runtime, _ := newServerFixture(t)
			runtime.SetStatus(magpie.RuntimeStatusReady)
			response := request(server, http.MethodGet, test.path)
			if response.Code != http.StatusOK {
				t.Fatalf("GET %s = %d %s", test.path, response.Code, response.Body.String())
			}
			want := []query.ModelSelection{{ProviderID: "provider-a", Model: "gpt-x"}}
			if test.name == "summary" {
				want = append(want, query.ModelSelection{ProviderID: "provider-b", Model: "gpt-x"})
			}
			if got := test.get(projectionService).Filter.Models; !reflect.DeepEqual(got, want) {
				t.Fatalf("parsed model_at filter = %+v, want %+v", got, want)
			}
		})
	}
}

func TestInternalAPIBaseRoutesErrorsAndGuards(t *testing.T) {
	server, _, runtime, _ := newServerFixture(t)
	if response := request(server, http.MethodGet, "/api/"); response.Code != http.StatusNotFound || !strings.Contains(response.Body.String(), ErrorNotFound) {
		t.Fatalf("GET /api/ response = %d %s", response.Code, response.Body.String())
	}
	if response := request(server, http.MethodGet, "/api/unknown"); response.Code != http.StatusNotFound || strings.Contains(response.Body.String(), "dashboard-index") {
		t.Fatalf("GET /api/unknown response = %d %s", response.Code, response.Body.String())
	}
	if response := request(server, http.MethodGet, "/api/health?unexpected=1"); response.Code != http.StatusBadRequest {
		t.Fatalf("health accepted a query string: %d", response.Code)
	}
	if response := request(server, http.MethodGet, "/api/revision?unexpected=1"); response.Code != http.StatusBadRequest {
		t.Fatalf("revision accepted a query string: %d", response.Code)
	}
	if response := request(server, http.MethodGet, "/api/status?unexpected=1"); response.Code != http.StatusBadRequest {
		t.Fatalf("status accepted a query string: %d", response.Code)
	}
	if response := request(server, http.MethodGet, "/api/events?unexpected=1"); response.Code != http.StatusBadRequest {
		t.Fatalf("events accepted a query string: %d", response.Code)
	}
	runtime.SetStatus(magpie.RuntimeStatusUnavailable)
	status := request(server, http.MethodGet, "/api/status")
	if !strings.Contains(status.Body.String(), `"last_error_code":"MAGPIE_UNAVAILABLE"`) {
		t.Fatalf("unavailable status body = %s", status.Body.String())
	}

	badHostRequest := httptest.NewRequest(http.MethodGet, "http://example.test/api/health", nil)
	badHost := httptest.NewRecorder()
	server.ServeHTTP(badHost, badHostRequest)
	if badHost.Code != http.StatusForbidden || !strings.Contains(badHost.Body.String(), ErrorForbiddenHost) || badHost.Header().Get("Cache-Control") != "no-store" {
		t.Fatalf("forbidden host response = %d %s headers %v", badHost.Code, badHost.Body.String(), badHost.Header())
	}
	for _, origin := range []string{"https://example.test", "http://127.0.0.1:", "file:///tmp/index.html"} {
		req := httptest.NewRequest(http.MethodGet, "http://127.0.0.1:37991/api/health", nil)
		req.Header.Set("Origin", origin)
		response := httptest.NewRecorder()
		server.ServeHTTP(response, req)
		if response.Code != http.StatusForbidden || !strings.Contains(response.Body.String(), ErrorForbiddenOrigin) {
			t.Fatalf("Origin %q response = %d %s", origin, response.Code, response.Body.String())
		}
	}
}

func TestStaticAssetsAndDashboardFallback(t *testing.T) {
	server, _, _, _ := newServerFixture(t)
	for _, test := range []struct {
		path string
		body string
		mime string
	}{
		{path: "/", body: "dashboard-index", mime: "text/html"},
		{path: "/assets/app.js", body: "dashboard-script", mime: "text/javascript"},
		{path: "/dashboard/sessions", body: "dashboard-index", mime: "text/html"},
	} {
		response := request(server, http.MethodGet, test.path)
		if response.Code != http.StatusOK || response.Body.String() != test.body || !strings.HasPrefix(response.Header().Get("Content-Type"), test.mime) {
			t.Fatalf("GET %s = %d %q %q", test.path, response.Code, response.Body.String(), response.Header().Get("Content-Type"))
		}
	}
}

func TestEventStreamSendsImmediateAndChangedRevisionOnly(t *testing.T) {
	server, _, _, _ := newServerFixture(t)
	writer := newStreamWriter()
	ctx, cancel := context.WithCancel(context.Background())
	request := httptest.NewRequest(http.MethodGet, "/api/events", nil).WithContext(ctx)
	request.Host = "127.0.0.1:37991"
	done := make(chan struct{})
	go func() {
		defer close(done)
		server.ServeHTTP(writer, request)
	}()
	writer.waitFlushes(t, 1)
	if !strings.Contains(writer.bodyString(), "id: 0-0\nevent: revision\ndata: {\"data_revision\":0,\"status_revision\":0}") {
		t.Fatalf("initial event stream = %q", writer.bodyString())
	}
	if writer.Header().Get("Content-Type") != "text/event-stream" || writer.Header().Get("Cache-Control") != "no-store" ||
		writer.Header().Get("Connection") != "keep-alive" || writer.Header().Get("X-Accel-Buffering") != "no" {
		t.Fatalf("event stream headers = %v", writer.Header())
	}
	server.revisions.BumpData()
	writer.waitFlushes(t, 2)
	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("event handler did not stop after request cancellation")
	}
	body := writer.bodyString()
	if strings.Count(body, "event: revision") != 2 || strings.Contains(body, "event: summary") {
		t.Fatalf("event stream body = %q", body)
	}
}

func TestInternalProjectionErrorClassification(t *testing.T) {
	tests := []struct {
		name string
		err  error
		code int
		body string
	}{
		{name: "invalid range", err: query.ErrInvalidRange, code: http.StatusBadRequest, body: ErrorInvalidRange},
		{name: "upstream unavailable", err: query.ErrUpstreamUnavailable, code: http.StatusServiceUnavailable, body: ErrorMagpieUnavailable},
		{name: "Magpie HTTP error", err: &magpie.HTTPError{StatusCode: http.StatusServiceUnavailable}, code: http.StatusBadGateway, body: ErrorUpstreamFailed},
		{name: "Magpie web error", err: &magpie.WebError{Err: fmt.Errorf("decode response: %w", errors.New("bad response"))}, code: http.StatusBadGateway, body: ErrorUpstreamFailed},
		{name: "unknown internal", err: errors.New("projection invariant failed"), code: http.StatusInternalServerError, body: ErrorInternal},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			response := httptest.NewRecorder()
			writeProjectionError(response, test.err)
			if response.Code != test.code || !strings.Contains(response.Body.String(), test.body) {
				t.Fatalf("writeProjectionError(%v) = %d %s", test.err, response.Code, response.Body.String())
			}
		})
	}
}

func TestInternalDTOMapperPreservesNullAndEmptyShapes(t *testing.T) {
	usageJSON, err := json.Marshal(mapUsageDTO(projection.UsageView{}))
	if err != nil {
		t.Fatal(err)
	}
	for _, expected := range []string{`"input":0`, `"cache_read":0`, `"cache_write_1h":0`, `"cache_hit_rate":null`, `"cost":0`, `"unpriced":0`} {
		if !strings.Contains(string(usageJSON), expected) {
			t.Fatalf("usage mapper missing %s: %s", expected, usageJSON)
		}
	}
	threadJSON, err := json.Marshal(mapSessionThreadDTO(projection.SessionThreadView{ModelEffort: nil}))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(threadJSON), `"parent_thread_id":null`) || !strings.Contains(string(threadJSON), `"model_effort":[]`) ||
		!strings.Contains(string(threadJSON), `"first_usage_ms":0`) || !strings.Contains(string(threadJSON), `"session_id":""`) {
		t.Fatalf("session mapper shape = %s", threadJSON)
	}
}

func TestStaticHandlerHandlesHeadAndMissingAsset(t *testing.T) {
	handler := newStaticHandler(fstest.MapFS{"index.html": &fstest.MapFile{Data: []byte("index")}})
	for _, test := range []struct {
		method string
		path   string
		status int
	}{
		{method: http.MethodHead, path: "/", status: http.StatusOK},
		{method: http.MethodPost, path: "/", status: http.StatusMethodNotAllowed},
	} {
		recorder := httptest.NewRecorder()
		req := httptest.NewRequest(test.method, test.path, nil)
		handler.ServeHTTP(recorder, req)
		if recorder.Code != test.status {
			t.Fatalf("%s %s status = %d, want %d", test.method, test.path, recorder.Code, test.status)
		}
	}
}

type streamWriter struct {
	mu      sync.Mutex
	header  http.Header
	status  int
	body    strings.Builder
	flushes int
	flushed chan struct{}
}

func newStreamWriter() *streamWriter {
	return &streamWriter{header: make(http.Header), flushed: make(chan struct{}, 16)}
}

func (w *streamWriter) Header() http.Header { return w.header }

func (w *streamWriter) WriteHeader(status int) {
	w.mu.Lock()
	w.status = status
	w.mu.Unlock()
}

func (w *streamWriter) Write(data []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.status == 0 {
		w.status = http.StatusOK
	}
	return w.body.Write(data)
}

func (w *streamWriter) Flush() {
	w.mu.Lock()
	w.flushes++
	w.mu.Unlock()
	w.flushed <- struct{}{}
}

func (w *streamWriter) waitFlushes(t *testing.T, want int) {
	t.Helper()
	deadline := time.NewTimer(time.Second)
	defer deadline.Stop()
	for {
		w.mu.Lock()
		got := w.flushes
		w.mu.Unlock()
		if got >= want {
			return
		}
		select {
		case <-w.flushed:
		case <-deadline.C:
			t.Fatalf("flush count = %d, want at least %d", got, want)
		}
	}
}

func (w *streamWriter) bodyString() string {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.body.String()
}

func waitFor(t *testing.T, ready func() bool) {
	t.Helper()
	deadline := time.NewTimer(time.Second)
	defer deadline.Stop()
	ticker := time.NewTicker(time.Millisecond)
	defer ticker.Stop()
	for !ready() {
		select {
		case <-ticker.C:
		case <-deadline.C:
			t.Fatal("timed out waiting for server start activity")
		}
	}
}
