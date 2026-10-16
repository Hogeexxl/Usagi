package api

import (
	"context"
	"encoding/json"
	"go/parser"
	"go/token"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/api/testsupport"
	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

func TestPublicV1FrozenPathsAndInfoRevision(t *testing.T) {
	server, _, _, _ := newServerFixture(t)
	paths := []string{
		publicV1InfoPath, publicV1RevisionPath, publicV1EventsPath,
		publicV1StatusPath, publicV1QuotaPath, publicV1SummaryPath,
	}
	wants := []string{
		"/api/v1/info", "/api/v1/revision", "/api/v1/events",
		"/api/v1/status", "/api/v1/codex/quota", "/api/v1/usage/summary",
	}
	if !reflect.DeepEqual(paths, wants) {
		t.Fatalf("Public v1 paths = %v, want %v", paths, wants)
	}
	for _, path := range paths {
		recorder := httptest.NewRecorder()
		req := httptest.NewRequest(http.MethodGet, path+"?unsupported=1", nil)
		if !server.servePublicV1Route(recorder, req) || recorder.Code != http.StatusBadRequest {
			t.Fatalf("Public route %s recognized/status = %v/%d", path, recorder.Code, recorder.Code)
		}
	}

	for range 41 {
		server.revisions.SetDataRevision(server.revisions.Current().DataRevision + 1)
	}
	for range 9 {
		server.revisions.BumpStatus()
	}
	infoResponse := request(server, http.MethodGet, publicV1InfoPath)
	if infoResponse.Code != http.StatusOK {
		t.Fatalf("GET %s = %d %s", publicV1InfoPath, infoResponse.Code, infoResponse.Body.String())
	}
	assertJSONKeys(t, infoResponse.Body.Bytes(), "service", "app_version", "api_version", "capabilities", "runtime_mode")
	var info publicV1InfoDTO
	if err := json.Unmarshal(infoResponse.Body.Bytes(), &info); err != nil {
		t.Fatal(err)
	}
	if info.Service != "usagi" || info.AppVersion != "0.2.6" || info.APIVersion != "1" || info.RuntimeMode != "development" ||
		!reflect.DeepEqual(info.Capabilities, []string{"revision", "revision-events", "status", "codex-quota", "usage-summary"}) {
		t.Fatalf("Public info = %+v", info)
	}

	revisionResponse := request(server, http.MethodGet, publicV1RevisionPath)
	if revisionResponse.Code != http.StatusOK {
		t.Fatalf("GET %s = %d %s", publicV1RevisionPath, revisionResponse.Code, revisionResponse.Body.String())
	}
	var revision publicV1RevisionDTO
	if err := json.Unmarshal(revisionResponse.Body.Bytes(), &revision); err != nil {
		t.Fatal(err)
	}
	if revision != (publicV1RevisionDTO{DataRevision: 41, StatusRevision: 9}) {
		t.Fatalf("Public revision = %+v", revision)
	}
}

func TestPublicV1DTOsAreIndependentFromInternalDTOs(t *testing.T) {
	file, err := parser.ParseFile(token.NewFileSet(), "dto_public_v1.go", nil, parser.ImportsOnly)
	if err != nil {
		t.Fatal(err)
	}
	for _, imported := range file.Imports {
		path, err := strconv.Unquote(imported.Path.Value)
		if err != nil {
			t.Fatal(err)
		}
		if strings.HasPrefix(path, "github.com/Hogeexxl/Usagi/internal/") {
			t.Fatalf("Public DTO file imports internal package %q", path)
		}
	}
}

func TestPublicV1StatusCompatibilityUsesOnlyLegacyEnums(t *testing.T) {
	tests := []struct {
		name      string
		state     magpie.RuntimeStatus
		scanState string
		binding   string
		finished  *string
		lastError *string
	}{
		{name: "starting", state: magpie.RuntimeStatusStarting, scanState: "running", binding: "unbound"},
		{name: "ready", state: magpie.RuntimeStatusReady, scanState: "idle", binding: "ready", finished: publicV1String("completed")},
		{name: "unavailable", state: magpie.RuntimeStatusUnavailable, scanState: "failed", binding: "unbound", finished: publicV1String("failed"), lastError: publicV1String(ErrorMagpieUnavailable)},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server, _, runtime, _ := newServerFixture(t)
			runtime.SetStatus(test.state)
			server.revisions.SetDataRevision(server.revisions.Current().DataRevision + 1)
			server.revisions.BumpStatus()
			response := request(server, http.MethodGet, publicV1StatusPath)
			if response.Code != http.StatusOK {
				t.Fatalf("GET %s = %d %s", publicV1StatusPath, response.Code, response.Body.String())
			}
			assertJSONKeys(t, response.Body.Bytes(),
				"data_revision", "status_revision", "scan_state", "source_binding_status", "last_finished_scan_result",
				"last_scan_started_at_ms", "last_scan_completed_at_ms", "last_scan_failed_at_ms", "last_scan_error_code")
			var got publicV1StatusDTO
			if err := json.Unmarshal(response.Body.Bytes(), &got); err != nil {
				t.Fatal(err)
			}
			if got.DataRevision != 1 || got.StatusRevision != 1 || got.ScanState != test.scanState ||
				got.SourceBindingStatus != test.binding || !reflect.DeepEqual(got.LastFinishedScanResult, test.finished) ||
				!reflect.DeepEqual(got.LastScanErrorCode, test.lastError) || got.LastScanStartedAtMS != nil ||
				got.LastScanCompletedAtMS != nil || got.LastScanFailedAtMS != nil {
				t.Fatalf("Public status = %+v", got)
			}
			if strings.Contains(response.Body.String(), `"scan_state":"startup"`) ||
				strings.Contains(response.Body.String(), `"scan_state":"success"`) ||
				strings.Contains(response.Body.String(), `"scan_state":"unavailable"`) {
				t.Fatalf("Public status leaked a Go runtime enum: %s", response.Body.String())
			}
		})
	}
}

func TestPublicV1QuotaUsesCachedSnapshotAndLegacyFourStates(t *testing.T) {
	t.Run("loading", func(t *testing.T) {
		server, projectionService, _, _ := newServerFixture(t)
		assertPublicQuota(t, server, "loading", nil, nil, nil)
		assertPublicQuotaDidNotCallVendor(t, projectionService)
	})

	t.Run("unavailable runtime", func(t *testing.T) {
		server, projectionService, runtime, _ := newServerFixture(t)
		server.quotas.Replace(projection.QuotaView{})
		runtime.SetStatus(magpie.RuntimeStatusUnavailable)
		assertPublicQuota(t, server, "unavailable", nil, nil, nil)
		assertPublicQuotaDidNotCallVendor(t, projectionService)
	})

	t.Run("refresh error", func(t *testing.T) {
		server, projectionService, _, _ := newServerFixture(t)
		server.quotas.RecordError(query.ErrUpstreamUnavailable)
		assertPublicQuota(t, server, "unavailable", nil, nil, nil)
		assertPublicQuotaDidNotCallVendor(t, projectionService)
	})

	t.Run("no Codex account", func(t *testing.T) {
		server, projectionService, _, _ := newServerFixture(t)
		server.quotas.Replace(projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "claude"}}})
		assertPublicQuota(t, server, "auth_required", nil, nil, nil)
		assertPublicQuotaDidNotCallVendor(t, projectionService)
	})

	t.Run("active first Codex account and exact windows", func(t *testing.T) {
		server, projectionService, _, _ := newServerFixture(t)
		readAt := time.UnixMilli(1_737_000_000_111)
		asOf := time.UnixMilli(1_737_000_000_222)
		reset5h := time.UnixMilli(1_737_044_940_000)
		reset7d := time.UnixMilli(1_737_120_660_000)
		server.quotas.Replace(projection.QuotaView{Items: []projection.QuotaProviderView{
			{Provider: "claude", Windows: []projection.QuotaWindowView{{Name: "5 hours", Used: 90}}},
			{Provider: "codex", Name: "active", ReadAt: &readAt, AsOf: &asOf, Windows: []projection.QuotaWindowView{
				{Name: "5h", Used: 98},
				{Name: "7 days", Used: 25, ResetsAt: &reset7d},
				{Name: "5 hours", Used: 12.5, ResetsAt: &reset5h, ResetSecs: 99},
			}},
			{Provider: "codex", Name: "inactive", ReadAt: &asOf, Windows: []projection.QuotaWindowView{{Name: "5 hours", Used: 99}}},
		}})
		fiveHour := &publicV1QuotaWindowDTO{UsedPercent: 12.5, RemainingPercent: 87.5, LimitWindowSeconds: 18_000, ResetAtMS: publicV1Int64(reset5h.UnixMilli())}
		weekly := &publicV1QuotaWindowDTO{UsedPercent: 25, RemainingPercent: 75, LimitWindowSeconds: 604_800, ResetAtMS: publicV1Int64(reset7d.UnixMilli())}
		assertPublicQuota(t, server, "ready", fiveHour, weekly, publicV1Int64(readAt.UnixMilli()))
		assertPublicQuotaDidNotCallVendor(t, projectionService)
	})

	t.Run("AsOf fallback and one missing window", func(t *testing.T) {
		server, projectionService, _, _ := newServerFixture(t)
		asOf := time.UnixMilli(1_737_000_000_222)
		server.quotas.Replace(projection.QuotaView{Items: []projection.QuotaProviderView{{
			Provider: "codex", AsOf: &asOf,
			Windows: []projection.QuotaWindowView{{Name: "5 hours", Used: 50}},
		}}})
		fiveHour := &publicV1QuotaWindowDTO{UsedPercent: 50, RemainingPercent: 50, LimitWindowSeconds: 18_000}
		assertPublicQuota(t, server, "ready", fiveHour, nil, publicV1Int64(asOf.UnixMilli()))
		assertPublicQuotaDidNotCallVendor(t, projectionService)
	})

	t.Run("selected card error", func(t *testing.T) {
		server, projectionService, _, _ := newServerFixture(t)
		server.quotas.Replace(projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "codex", Error: "expired"}}})
		assertPublicQuota(t, server, "unavailable", nil, nil, nil)
		assertPublicQuotaDidNotCallVendor(t, projectionService)
	})
}

func assertPublicQuota(t *testing.T, server *Server, status string, fiveHour, weekly *publicV1QuotaWindowDTO, fetchedAt *int64) {
	t.Helper()
	response := request(server, http.MethodGet, publicV1QuotaPath)
	if response.Code != http.StatusOK {
		t.Fatalf("GET %s = %d %s", publicV1QuotaPath, response.Code, response.Body.String())
	}
	assertJSONKeys(t, response.Body.Bytes(), "status", "five_hour", "weekly", "fetched_at_ms")
	var got publicV1QuotaDTO
	if err := json.Unmarshal(response.Body.Bytes(), &got); err != nil {
		t.Fatal(err)
	}
	if got.Status != status || !reflect.DeepEqual(got.FiveHour, fiveHour) || !reflect.DeepEqual(got.Weekly, weekly) || !reflect.DeepEqual(got.FetchedAtMS, fetchedAt) {
		t.Fatalf("Public quota = %+v, want status=%s five_hour=%+v weekly=%+v fetched_at_ms=%v", got, status, fiveHour, weekly, fetchedAt)
	}
}

func assertPublicQuotaDidNotCallVendor(t *testing.T, service *testsupport.FakeProjectionService) {
	t.Helper()
	if len(service.Quota.Asked) != 0 {
		t.Fatalf("Public quota GET called projection.Quotas with asked=%v", service.Quota.Asked)
	}
}

func TestPublicV1SummaryLegacyMappingsAndSingleCodexProjection(t *testing.T) {
	tests := []struct {
		name string
		view projection.SummaryView
		want publicV1UsageDTO
	}{
		{
			name: "complete token mapping",
			view: projection.SummaryView{
				Usage: projection.UsageView{
					Input: 11, CacheRead: 13, CacheWrite: 17, CacheWrite1h: 7, Output: 29, Reasoning: 7,
					Total: 70, CacheHitRate: publicV1Float64(0.75), Cost: 1.25,
				},
				RootSessionCount: 2,
			},
			want: publicV1UsageDTO{
				InputTokens: 41, CachedTokens: 13, CacheWriteTokens: publicV1Int64(17), UncachedInputTokens: publicV1Int64(11),
				OutputTokens: 29, ReasoningTokens: 7, OtherOutputTokens: 22, TotalTokens: 70, CacheHitRate: publicV1Float64(0.75),
				EstimatedCost: publicV1Float64(1.25), EstimatedCostStatus: "complete", SessionCount: 2,
				SessionHealth: publicV1SessionHealthDTO{TotalSessions: 2, CompleteSessions: 2},
			},
		},
		{
			name: "partial two roots one unpriced",
			view: projection.SummaryView{
				Usage:            projection.UsageView{Input: 100, CacheRead: 50, CacheWrite: 10, CacheWrite1h: 4, Output: 80, Reasoning: 30, Total: 240, Cost: 0.4, Unpriced: 2},
				RootSessionCount: 2, RootUnpricedSessionCount: 1,
			},
			want: publicV1UsageDTO{
				InputTokens: 160, CachedTokens: 50, CacheWriteTokens: publicV1Int64(10), UncachedInputTokens: publicV1Int64(100),
				OutputTokens: 80, ReasoningTokens: 30, OtherOutputTokens: 50, TotalTokens: 240,
				EstimatedCost: publicV1Float64(0.4), EstimatedCostStatus: "partial", SessionCount: 2, CostIncompleteSessionCount: 1,
				SessionHealth: publicV1SessionHealthDTO{TotalSessions: 2, CompleteSessions: 1, IncompleteSessions: 1},
			},
		},
		{
			name: "unknown cost",
			view: projection.SummaryView{
				Usage:            projection.UsageView{Input: 5, Total: 5, Unpriced: 1},
				RootSessionCount: 1, RootUnpricedSessionCount: 1,
			},
			want: publicV1UsageDTO{
				InputTokens: 5, CacheWriteTokens: publicV1Int64(0), UncachedInputTokens: publicV1Int64(5),
				TotalTokens: 5, EstimatedCostStatus: "unknown", SessionCount: 1, CostIncompleteSessionCount: 1,
				SessionHealth: publicV1SessionHealthDTO{TotalSessions: 1, IncompleteSessions: 1},
			},
		},
		{
			name: "empty aggregate",
			want: publicV1UsageDTO{
				CacheWriteTokens: publicV1Int64(0), UncachedInputTokens: publicV1Int64(0),
				EstimatedCost: publicV1Float64(0), EstimatedCostStatus: "complete",
				SessionHealth: publicV1SessionHealthDTO{},
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server, service, runtime, _ := newServerFixture(t)
			runtime.SetStatus(magpie.RuntimeStatusReady)
			service.SummaryResult = test.view
			server.revisions.SetDataRevision(server.revisions.Current().DataRevision + 1)
			response := request(server, http.MethodGet, publicV1SummaryPath+"?range=today")
			if response.Code != http.StatusOK {
				t.Fatalf("GET %s = %d %s", publicV1SummaryPath, response.Code, response.Body.String())
			}
			assertJSONKeys(t, response.Body.Bytes(), "range", "data_revision", "usage")
			var got publicV1SummaryDTO
			if err := json.Unmarshal(response.Body.Bytes(), &got); err != nil {
				t.Fatal(err)
			}
			if got.Range.Key != "today" || got.Range.Timezone != "UTC" || got.Range.EndMS-got.Range.StartMS != int64(24*time.Hour/time.Millisecond) ||
				got.DataRevision != 1 || !reflect.DeepEqual(got.Usage, test.want) {
				t.Fatalf("Public summary = %+v, want usage=%+v", got, test.want)
			}
			assertJSONKeys(t, mustJSON(t, got.Usage),
				"input_tokens", "cached_tokens", "cache_write_tokens", "uncached_input_tokens", "output_tokens", "reasoning_tokens",
				"other_output_tokens", "total_tokens", "cache_hit_rate", "estimated_cost", "estimated_cost_status", "session_count",
				"cost_incomplete_session_count", "complete_session_cost_per_million_tokens", "session_health")
			assertJSONKeys(t, mustJSON(t, got.Usage.SessionHealth), "total_sessions", "complete_sessions", "incomplete_sessions", "error_sessions")
			if len(service.SummaryQueries) != 1 || !reflect.DeepEqual(service.SummaryQueries[0].Range, testPublicInclusiveRange(got.Range)) ||
				!reflect.DeepEqual(service.SummaryQueries[0].Filter, query.UsageFilter{Agents: []string{"codex"}}) {
				t.Fatalf("Public projection queries = %+v", service.SummaryQueries)
			}
			if len(service.SessionQueries) != 0 || len(service.SessionDetailQueries) != 0 {
				t.Fatalf("Public summary made extra session calls: sessions=%d details=%d", len(service.SessionQueries), len(service.SessionDetailQueries))
			}
		})
	}
}

func TestPublicV1SummaryRejectsFiltersAndUsesDatabaseGate(t *testing.T) {
	server, service, runtime, queryService := newServerFixture(t)
	runtime.SetStatus(magpie.RuntimeStatusReady)
	for _, test := range []struct {
		path string
		code string
	}{
		{path: publicV1SummaryPath, code: ErrorInvalidRange},
		{path: publicV1SummaryPath + "?range=today&agent=codex", code: ErrorInvalidFilter},
		{path: publicV1SummaryPath + "?range=today&model_at=codex/gpt-x", code: ErrorInvalidFilter},
		{path: publicV1SummaryPath + "?range=today&project_path=/repo", code: ErrorInvalidFilter},
		{path: publicV1SummaryPath + "?range=today&include_projectless=1", code: ErrorInvalidFilter},
		{path: publicV1SummaryPath + "?range=today&range=year", code: ErrorInvalidRange},
	} {
		response := request(server, http.MethodGet, test.path)
		if response.Code != http.StatusBadRequest || !strings.Contains(response.Body.String(), test.code) {
			t.Fatalf("GET %s = %d %s, want 400 %s", test.path, response.Code, response.Body.String(), test.code)
		}
	}
	if len(service.SummaryQueries) != 0 {
		t.Fatalf("invalid Public summary requests reached Projection: %+v", service.SummaryQueries)
	}

	runtime.SetStatus(magpie.RuntimeStatusStarting)
	response := request(server, http.MethodGet, publicV1SummaryPath+"?range=today")
	if response.Code != http.StatusOK {
		t.Fatalf("summary with completed database and starting runtime = %d %s", response.Code, response.Body.String())
	}
	if len(service.SummaryQueries) != 1 {
		t.Fatalf("completed database summary reached Projection %d times, want once", len(service.SummaryQueries))
	}

	queryService.InitialSyncPending = true
	runtime.SetStatus(magpie.RuntimeStatusReady)
	response = request(server, http.MethodGet, publicV1SummaryPath+"?range=today")
	if response.Code != http.StatusServiceUnavailable || !strings.Contains(response.Body.String(), ErrorDataLoading) {
		t.Fatalf("summary before initial database sync = %d %s", response.Code, response.Body.String())
	}
	if len(service.SummaryQueries) != 1 {
		t.Fatalf("loading Public summary reached Projection: %+v", service.SummaryQueries)
	}
}

func TestPublicV1EventsSendsInitialAndChangedPublicRevisionFrames(t *testing.T) {
	server, _, _, _ := newServerFixture(t)
	for range 41 {
		server.revisions.SetDataRevision(server.revisions.Current().DataRevision + 1)
	}
	for range 9 {
		server.revisions.BumpStatus()
	}
	writer := newStreamWriter()
	ctx, cancel := context.WithCancel(context.Background())
	req := httptest.NewRequest(http.MethodGet, publicV1EventsPath, nil).WithContext(ctx)
	req.Host = "127.0.0.1:37991"
	done := make(chan struct{})
	go func() {
		defer close(done)
		server.ServeHTTP(writer, req)
	}()
	writer.waitFlushes(t, 1)
	initial := string(fixturePublicV1(t, "events_revision.sse"))
	if !strings.HasPrefix(writer.bodyString(), initial) {
		t.Fatalf("initial Public event frame = %q, want prefix %q", writer.bodyString(), initial)
	}
	if writer.Header().Get("Content-Type") != "text/event-stream" || writer.Header().Get("Cache-Control") != "no-store" {
		t.Fatalf("Public event headers = %v", writer.Header())
	}
	server.revisions.SetDataRevision(server.revisions.Current().DataRevision + 1)
	writer.waitFlushes(t, 2)
	server.revisions.BumpStatus()
	writer.waitFlushes(t, 3)
	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("Public event handler did not stop after cancellation")
	}
	body := writer.bodyString()
	for _, want := range []string{
		"id: 9-42\nevent: revision\ndata: {\"data_revision\":42,\"status_revision\":9}\n\n",
		"id: 10-42\nevent: revision\ndata: {\"data_revision\":42,\"status_revision\":10}\n\n",
	} {
		if !strings.Contains(body, want) {
			t.Fatalf("Public event stream missing %q in %q", want, body)
		}
	}
	if strings.Count(body, "event: revision") != 3 {
		t.Fatalf("Public event stream sent %d frames: %q", strings.Count(body, "event: revision"), body)
	}
}

func TestExportPublicV1HTTPResponsesForSwiftDecode(t *testing.T) {
	directory := os.Getenv("USAGI_PUBLIC_V1_EXPORT_DIR")
	if directory == "" {
		t.Skip("USAGI_PUBLIC_V1_EXPORT_DIR is set only for Swift DTO compatibility evidence")
	}
	if err := os.MkdirAll(directory, 0o700); err != nil {
		t.Fatal(err)
	}
	fixtures := make(map[string][]byte)
	addResponse := func(server *Server, name, target string) {
		t.Helper()
		response := request(server, http.MethodGet, target)
		if response.Code != http.StatusOK {
			t.Fatalf("GET %s = %d %s", target, response.Code, response.Body.String())
		}
		fixtures[name] = append([]byte(nil), response.Body.Bytes()...)
	}
	seedRevisions := func(server *Server) {
		t.Helper()
		for range 41 {
			server.revisions.SetDataRevision(server.revisions.Current().DataRevision + 1)
		}
		for range 9 {
			server.revisions.BumpStatus()
		}
	}

	server, _, _, _ := newServerFixture(t)
	seedRevisions(server)
	addResponse(server, "info.json", publicV1InfoPath)
	addResponse(server, "revision.json", publicV1RevisionPath)

	for _, test := range []struct {
		name  string
		state magpie.RuntimeStatus
	}{
		{name: "starting", state: magpie.RuntimeStatusStarting},
		{name: "ready", state: magpie.RuntimeStatusReady},
		{name: "unavailable", state: magpie.RuntimeStatusUnavailable},
	} {
		statusServer, _, runtime, _ := newServerFixture(t)
		runtime.SetStatus(test.state)
		seedRevisions(statusServer)
		addResponse(statusServer, "status_"+test.name+".json", publicV1StatusPath)
	}

	loadingServer, _, _, _ := newServerFixture(t)
	addResponse(loadingServer, "quota_loading.json", publicV1QuotaPath)

	quotaUnavailableServer, _, _, _ := newServerFixture(t)
	quotaUnavailableServer.quotas.RecordError(query.ErrUpstreamUnavailable)
	addResponse(quotaUnavailableServer, "quota_unavailable.json", publicV1QuotaPath)

	quotaAuthServer, _, _, _ := newServerFixture(t)
	quotaAuthServer.quotas.Replace(projection.QuotaView{Items: []projection.QuotaProviderView{{Provider: "claude"}}})
	addResponse(quotaAuthServer, "quota_auth_required.json", publicV1QuotaPath)

	quotaReadyServer, _, _, _ := newServerFixture(t)
	readAt := time.UnixMilli(1_737_000_000_111)
	resetFiveHour := time.UnixMilli(1_737_044_940_000)
	resetWeekly := time.UnixMilli(1_737_120_660_000)
	quotaReadyServer.quotas.Replace(projection.QuotaView{Items: []projection.QuotaProviderView{{
		Provider: "codex", ReadAt: &readAt,
		Windows: []projection.QuotaWindowView{
			{Name: "5 hours", Used: 12.5, ResetsAt: &resetFiveHour},
			{Name: "7 days", Used: 25, ResetsAt: &resetWeekly},
		},
	}}})
	addResponse(quotaReadyServer, "quota_ready.json", publicV1QuotaPath)

	for _, test := range []struct {
		name string
		view projection.SummaryView
	}{
		{
			name: "complete",
			view: projection.SummaryView{
				Usage:            projection.UsageView{Input: 11, CacheRead: 13, CacheWrite: 17, CacheWrite1h: 7, Output: 29, Reasoning: 7, Total: 70, Cost: 1.25},
				RootSessionCount: 2,
			},
		},
		{
			name: "partial",
			view: projection.SummaryView{
				Usage:            projection.UsageView{Input: 100, CacheRead: 50, CacheWrite: 10, CacheWrite1h: 4, Output: 80, Reasoning: 30, Total: 240, Cost: 0.4, Unpriced: 2},
				RootSessionCount: 2, RootUnpricedSessionCount: 1,
			},
		},
		{
			name: "unknown",
			view: projection.SummaryView{
				Usage:            projection.UsageView{Input: 5, Total: 5, Unpriced: 1},
				RootSessionCount: 1, RootUnpricedSessionCount: 1,
			},
		},
		{name: "empty"},
	} {
		summaryServer, service, runtime, _ := newServerFixture(t)
		runtime.SetStatus(magpie.RuntimeStatusReady)
		seedRevisions(summaryServer)
		service.SummaryResult = test.view
		addResponse(summaryServer, "summary_"+test.name+".json", publicV1SummaryPath+"?range=today")
	}

	eventServer, _, _, _ := newServerFixture(t)
	seedRevisions(eventServer)
	writer := newStreamWriter()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	request := httptest.NewRequest(http.MethodGet, publicV1EventsPath, nil).WithContext(ctx)
	request.Host = "127.0.0.1:37991"
	done := make(chan struct{})
	go func() {
		defer close(done)
		eventServer.ServeHTTP(writer, request)
	}()
	writer.waitFlushes(t, 1)
	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("Public event handler did not stop after cancellation")
	}
	for _, line := range strings.Split(writer.bodyString(), "\n") {
		if strings.HasPrefix(line, "data: ") {
			fixtures["sse_revision.json"] = []byte(strings.TrimPrefix(line, "data: "))
			break
		}
	}
	if fixtures["sse_revision.json"] == nil {
		t.Fatal("Public event stream did not contain a data frame")
	}

	for name, body := range fixtures {
		if err := os.WriteFile(filepath.Join(directory, name), body, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	t.Logf("exported %d actual Public handler response bodies to %s", len(fixtures), directory)
}

func fixturePublicV1(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile("testsupport/testdata/public_v1/" + name)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func assertJSONKeys(t *testing.T, data []byte, wants ...string) {
	t.Helper()
	var object map[string]json.RawMessage
	if err := json.Unmarshal(data, &object); err != nil {
		t.Fatalf("decode JSON object keys: %v", err)
	}
	got := make([]string, 0, len(object))
	for key := range object {
		got = append(got, key)
	}
	sort.Strings(got)
	sort.Strings(wants)
	if !reflect.DeepEqual(got, wants) {
		t.Fatalf("JSON keys = %v, want %v in %s", got, wants, data)
	}
}

func mustJSON(t *testing.T, value any) []byte {
	t.Helper()
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func testPublicInclusiveRange(value publicV1RangeDTO) query.DateRange {
	start := time.UnixMilli(value.StartMS).UTC()
	last := time.UnixMilli(value.EndMS).Add(-time.Millisecond).UTC()
	return query.DateRange{From: start.Format(time.DateOnly), To: last.Format(time.DateOnly)}
}

func publicV1Int64(value int64) *int64 {
	return &value
}
