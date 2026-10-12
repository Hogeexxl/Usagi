package api

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
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

type phase3RuntimeProvider struct {
	*testsupport.FakeRuntime
	mu     sync.RWMutex
	client *magpie.Client
	ready  bool
}

type phase3ProjectionProbe struct {
	ProjectionService
	mu          sync.Mutex
	detailCalls int
}

func (p *phase3ProjectionProbe) SessionDetail(ctx context.Context, scope projection.ScopeQuery, key projection.ThreadKey) (projection.SessionDetailView, error) {
	p.mu.Lock()
	p.detailCalls++
	p.mu.Unlock()
	return p.ProjectionService.SessionDetail(ctx, scope, key)
}

func (p *phase3ProjectionProbe) sessionDetailCalls() int {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.detailCalls
}

func (p *phase3RuntimeProvider) Client() (*magpie.Client, bool) {
	p.mu.RLock()
	defer p.mu.RUnlock()
	return p.client, p.ready
}

func (p *phase3RuntimeProvider) set(status magpie.RuntimeStatus, client *magpie.Client) {
	p.mu.Lock()
	p.client = client
	p.ready = status == magpie.RuntimeStatusReady && client != nil
	p.mu.Unlock()
	p.FakeRuntime.SetStatus(status)
}

type phase3MagpieFixture struct {
	mu       sync.Mutex
	now      time.Time
	input    int
	sessions magpie.SessionsResponse
	usage    magpie.UsageSummary
	rows     magpie.UsageRequestsPage
	quotas   []magpie.Quota
	counts   map[string]int
	asked    []string
}

func newPhase3MagpieFixture(now time.Time, input int, title, cwd string) *phase3MagpieFixture {
	last := now.Add(-time.Minute)
	fixture := &phase3MagpieFixture{
		now: now, input: input,
		sessions: magpie.SessionsResponse{Sessions: []magpie.Session{{
			Agent: "codex", ID: "session-1", Cwd: cwd, Title: title, Start: now.Add(-time.Hour), Last: last,
			Models: []magpie.SessionModel{}, Name: "Codex", Icon: "codex",
		}}},
		usage: magpie.UsageSummary{
			Period: "all", Bucket: "day", Series: []magpie.UsagePoint{}, Agents: []magpie.UsageGroup{},
			Models: []magpie.UsageGroup{}, Accounts: []magpie.UsageGroup{}, CallerKeys: []magpie.UsageGroup{},
			UsageTotals: magpie.UsageTotals{Calls: 1, Input: input, Output: 3, Cost: 0.25},
		},
		rows: magpie.UsageRequestsPage{Rows: []magpie.UsageRequest{{
			Time: now, Agent: "codex", AgentName: "Codex", Provider: "openai", ProviderName: "OpenAI",
			Model: "gpt-5", Input: input, Output: 3, Status: http.StatusOK, Session: "session-1",
			NativeSession: "session-1", Cost: 0.25, Priced: true,
		}}, Total: 1},
		quotas: []magpie.Quota{{
			Provider: "codex", Name: "Codex", ReadAt: phase3Time(now),
			Windows: []magpie.QuotaWindow{{Name: "5 hours", Used: 12.5, Limit: 100}},
		}},
		counts: make(map[string]int),
	}
	return fixture
}

func (f *phase3MagpieFixture) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	f.mu.Lock()
	f.counts[r.URL.Path]++
	switch r.URL.Path {
	case "/api/usage":
		value := f.usage
		f.mu.Unlock()
		_ = json.NewEncoder(w).Encode(value)
	case "/api/usage/requests":
		value := f.rows
		f.mu.Unlock()
		_ = json.NewEncoder(w).Encode(value)
	case "/api/sessions":
		value := f.sessions
		value.Sessions = append([]magpie.Session(nil), value.Sessions...)
		f.mu.Unlock()
		_ = json.NewEncoder(w).Encode(value)
	case "/api/sessions/one":
		value := f.sessions.Sessions[0]
		f.mu.Unlock()
		_ = json.NewEncoder(w).Encode(value)
	case "/api/usage/quotas":
		f.asked = append(f.asked, r.URL.Query().Get("asked"))
		value := append([]magpie.Quota(nil), f.quotas...)
		f.mu.Unlock()
		_ = json.NewEncoder(w).Encode(value)
	default:
		f.mu.Unlock()
		http.NotFound(w, r)
	}
}

func (f *phase3MagpieFixture) setSessionMetadata(title, cwd string, last time.Time) {
	f.mu.Lock()
	f.sessions.Sessions[0].Title = title
	f.sessions.Sessions[0].Cwd = cwd
	f.sessions.Sessions[0].Last = last
	f.mu.Unlock()
}

func (f *phase3MagpieFixture) setQuotaUsed(value float64) {
	f.mu.Lock()
	f.quotas[0].Windows[0].Used = value
	f.mu.Unlock()
}

func (f *phase3MagpieFixture) count(path string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.counts[path]
}

func (f *phase3MagpieFixture) askedValues() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.asked...)
}

func (f *phase3MagpieFixture) totalCalls() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	total := 0
	for _, count := range f.counts {
		total += count
	}
	return total
}

func phase3Time(value time.Time) *time.Time {
	return &value
}

func phase3NewServer(t *testing.T, projectionService ProjectionService, queryService WatcherQuery, runtime RuntimeStatusSource, revisions RevisionService, quotas *QuotaSnapshot, assets fs.FS) *Server {
	t.Helper()
	config := DefaultConfig()
	config.StaticFS = assets
	config.UsageRefreshInterval = time.Hour
	config.SessionRefreshInterval = 2 * time.Hour
	config.QuotaRefreshInterval = 3 * time.Hour
	server, err := NewServer(config, Dependencies{
		Projection: projectionService, Query: queryService, RuntimeStatus: runtime,
		Revisions: revisions, QuotaSnapshot: quotas, ShutdownRequester: func() {},
	})
	if err != nil {
		t.Fatalf("NewServer() error = %v", err)
	}
	return server
}

func phase3OpenStream(t *testing.T, server http.Handler, target string) (*streamWriter, context.CancelFunc, <-chan struct{}) {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	req := httptest.NewRequest(http.MethodGet, target, nil).WithContext(ctx)
	req.Host = "127.0.0.1:37991"
	writer := newStreamWriter()
	done := make(chan struct{})
	go func() {
		defer close(done)
		server.ServeHTTP(writer, req)
	}()
	writer.waitFlushes(t, 1)
	t.Cleanup(func() {
		cancel()
		select {
		case <-done:
		case <-time.After(time.Second):
			t.Error("SSE handler did not stop after test cleanup")
		}
	})
	return writer, cancel, done
}

func phase3WaitBody(t *testing.T, writer *streamWriter, text string, timeout time.Duration) {
	t.Helper()
	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	ticker := time.NewTicker(10 * time.Millisecond)
	defer ticker.Stop()
	for !strings.Contains(writer.bodyString(), text) {
		select {
		case <-ticker.C:
		case <-deadline.C:
			t.Fatalf("SSE body did not contain %q; got %q", text, writer.bodyString())
		}
	}
}

func phase3AssertSSEHeaders(t *testing.T, writer *streamWriter) {
	t.Helper()
	if writer.Header().Get("Content-Type") != "text/event-stream" ||
		writer.Header().Get("Cache-Control") != "no-store" ||
		writer.Header().Get("Connection") != "keep-alive" ||
		writer.Header().Get("X-Accel-Buffering") != "no" {
		t.Fatalf("SSE headers = %v", writer.Header())
	}
}

func phase3AssertRevisionFrames(t *testing.T, body string, want ...RevisionTuple) {
	t.Helper()
	var frames []string
	for _, block := range strings.Split(body, "\n\n") {
		if strings.Contains(block, "event: revision") {
			frames = append(frames, block)
		}
	}
	if len(frames) != len(want) {
		t.Fatalf("SSE revision frame count = %d, want %d: %q", len(frames), len(want), body)
	}
	for i, tuple := range want {
		data := fmt.Sprintf(`{"data_revision":%d,"status_revision":%d}`, tuple.DataRevision, tuple.StatusRevision)
		frame := fmt.Sprintf("id: %d-%d\nevent: revision\ndata: %s", tuple.StatusRevision, tuple.DataRevision, data)
		if frames[i] != frame {
			t.Fatalf("SSE frame %d = %q, want %q", i, frames[i], frame)
		}
		var fields map[string]json.RawMessage
		if err := json.Unmarshal([]byte(data), &fields); err != nil || len(fields) != 2 {
			t.Fatalf("SSE revision data fields = %v, error %v", fields, err)
		}
	}
}

func TestPhase3SSEContractsFanoutKeepaliveAndCancellation(t *testing.T) {
	server, _, _, _ := newServerFixture(t)
	server.revisions.BumpData()
	server.revisions.BumpStatus()
	internalWriter, stopInternal, internalDone := phase3OpenStream(t, server, "/api/events")
	publicWriter, stopPublic, publicDone := phase3OpenStream(t, server, publicV1EventsPath)
	phase3AssertSSEHeaders(t, internalWriter)
	phase3AssertSSEHeaders(t, publicWriter)
	initial := RevisionTuple{DataRevision: 1, StatusRevision: 1}
	phase3AssertRevisionFrames(t, internalWriter.bodyString(), initial)
	phase3AssertRevisionFrames(t, publicWriter.bodyString(), initial)

	revisionService := server.revisions.(*revisionService)
	revisionService.mu.Lock()
	for _, updates := range revisionService.subscribers {
		updates <- revisionService.current
	}
	revisionService.mu.Unlock()
	time.Sleep(50 * time.Millisecond)
	phase3AssertRevisionFrames(t, internalWriter.bodyString(), initial)
	phase3AssertRevisionFrames(t, publicWriter.bodyString(), initial)

	dataTuple := server.revisions.BumpData()
	internalWriter.waitFlushes(t, 2)
	publicWriter.waitFlushes(t, 2)
	statusTuple := server.revisions.BumpStatus()
	internalWriter.waitFlushes(t, 3)
	publicWriter.waitFlushes(t, 3)

	phase3WaitBody(t, internalWriter, ": keepalive\n\n", 17*time.Second)
	phase3WaitBody(t, publicWriter, ": keepalive\n\n", time.Second)
	phase3AssertRevisionFrames(t, internalWriter.bodyString(), initial, dataTuple, statusTuple)
	phase3AssertRevisionFrames(t, publicWriter.bodyString(), initial, dataTuple, statusTuple)
	if server.revisions.Current() != statusTuple {
		t.Fatalf("keepalive changed revision tuple to %+v, want %+v", server.revisions.Current(), statusTuple)
	}

	stopInternal()
	stopPublic()
	for name, done := range map[string]<-chan struct{}{"internal": internalDone, "public": publicDone} {
		select {
		case <-done:
		case <-time.After(time.Second):
			t.Fatalf("%s SSE handler did not stop after disconnect", name)
		}
	}
	revisionService.mu.Lock()
	subscriberCount := len(revisionService.subscribers)
	revisionService.mu.Unlock()
	if subscriberCount != 0 {
		t.Fatalf("RevisionService retained %d SSE subscribers after disconnect", subscriberCount)
	}
}

func TestPhase3WatcherRevisionSSEHTTPChainAndRuntimeClients(t *testing.T) {
	t.Setenv("TZ", "UTC")
	now := time.Now().UTC().Truncate(time.Second)
	firstFixture := newPhase3MagpieFixture(now, 12, "Before", "/old/project")
	firstMagpie := httptest.NewServer(firstFixture)
	defer firstMagpie.Close()
	secondFixture := newPhase3MagpieFixture(now, 42, "Second client", "/second/project")
	secondMagpie := httptest.NewServer(secondFixture)
	defer secondMagpie.Close()

	runtime := &phase3RuntimeProvider{FakeRuntime: testsupport.NewFakeRuntime(magpie.RuntimeStatusStarting)}
	queryService := query.NewRuntimeService(runtime)
	projectionGraph := projection.NewService(queryService)
	projectionService := &phase3ProjectionProbe{ProjectionService: projectionGraph}
	revisions := NewRevisionService()
	quotas := &QuotaSnapshot{}
	server := phase3NewServer(t, projectionService, queryService, runtime, revisions, quotas, fstest.MapFS{})
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	startRuntimeStatusSubscription(ctx, runtime, revisions)
	stream, stopStream, streamDone := phase3OpenStream(t, server, "/api/events")
	phase3AssertRevisionFrames(t, stream.bodyString(), RevisionTuple{})

	startingSummary := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if startingSummary.Code != http.StatusServiceUnavailable || !strings.Contains(startingSummary.Body.String(), ErrorMagpieUnavailable) {
		t.Fatalf("starting Summary = %d %s, want 503 %s", startingSummary.Code, startingSummary.Body.String(), ErrorMagpieUnavailable)
	}
	phase3AssertPublicStatus(t, server, "running", "unbound", nil, nil)
	runtime.set(magpie.RuntimeStatusReady, magpie.NewClient(firstMagpie.URL, firstMagpie.Client()))
	waitFor(t, func() bool { return revisions.Current() == (RevisionTuple{DataRevision: 1, StatusRevision: 1}) })
	phase3WaitBody(t, stream, `"data_revision":1,"status_revision":1`, time.Second)

	firstSummary := request(server, http.MethodGet, "/api/usage/summary?range=today")
	var before summaryResponseDTO
	if firstSummary.Code != http.StatusOK || json.Unmarshal(firstSummary.Body.Bytes(), &before) != nil || before.DataRevision != 1 || before.Usage.Input != 12 {
		t.Fatalf("ready Summary = %d %s, want revision 1 and input 12", firstSummary.Code, firstSummary.Body.String())
	}

	watcher := newWatchers(queryService, projectionService, revisions, quotas, time.Hour, 2*time.Hour, 3*time.Hour)
	watcher.refreshUsage(ctx)
	watcher.refreshSessions(ctx)
	if got := revisions.Current(); got != (RevisionTuple{DataRevision: 1, StatusRevision: 1}) {
		t.Fatalf("first successful Usage/Sessions baselines tuple = %+v, want only ready transition", got)
	}

	sessionCallsBeforeTick := firstFixture.count("/api/sessions")
	firstFixture.setSessionMetadata("After", "/new/project", now.Add(2*time.Minute))
	watcher.refreshSessions(ctx)
	if got := firstFixture.count("/api/sessions") - sessionCallsBeforeTick; got != 1 {
		t.Fatalf("one Sessions watcher tick made %d /api/sessions calls, want exactly 1", got)
	}
	metadataTuple := revisions.Current()
	if metadataTuple != (RevisionTuple{DataRevision: 2, StatusRevision: 1}) {
		t.Fatalf("metadata-only Sessions change tuple = %+v, want {2 1}", metadataTuple)
	}
	phase3WaitBody(t, stream, `"data_revision":2,"status_revision":1`, time.Second)
	watcher.refreshUsage(ctx)
	if got := revisions.Current(); got != metadataTuple {
		t.Fatalf("unchanged Usage tick changed tuple to %+v, want %+v", got, metadataTuple)
	}

	sessionsResponse := request(server, http.MethodGet, "/api/usage/sessions?range=today")
	var sessions sessionsResponseDTO
	if sessionsResponse.Code != http.StatusOK || json.Unmarshal(sessionsResponse.Body.Bytes(), &sessions) != nil ||
		sessions.DataRevision != metadataTuple.DataRevision || len(sessions.Items) != 1 ||
		sessions.Items[0].Thread.Title != "After" || sessions.Items[0].Thread.Project.Path != "/new/project" {
		t.Fatalf("Dashboard Sessions GET = %d %s, want new metadata at revision %d", sessionsResponse.Code, sessionsResponse.Body.String(), metadataTuple.DataRevision)
	}
	if got := firstFixture.count("/api/sessions/one"); got != 0 {
		t.Fatalf("Sessions GET made %d detail endpoint calls, want none", got)
	}
	if got := projectionService.sessionDetailCalls(); got != 0 {
		t.Fatalf("Sessions GET invoked Projection.SessionDetail %d times, want none", got)
	}

	afterSummaryResponse := request(server, http.MethodGet, "/api/usage/summary?range=today")
	var after summaryResponseDTO
	if afterSummaryResponse.Code != http.StatusOK || json.Unmarshal(afterSummaryResponse.Body.Bytes(), &after) != nil ||
		after.DataRevision != metadataTuple.DataRevision || !reflect.DeepEqual(after.Usage, before.Usage) {
		t.Fatalf("metadata-only Summary = %d %s, want unchanged Usage at revision %d", afterSummaryResponse.Code, afterSummaryResponse.Body.String(), metadataTuple.DataRevision)
	}
	if got := revisions.Current(); got != metadataTuple {
		t.Fatalf("Dashboard GETs changed revision tuple to %+v, want %+v", got, metadataTuple)
	}

	oldClientCalls := firstFixture.totalCalls()
	runtime.set(magpie.RuntimeStatusUnavailable, nil)
	waitFor(t, func() bool { return revisions.Current() == (RevisionTuple{DataRevision: 2, StatusRevision: 2}) })
	phase3AssertPublicStatus(t, server, "failed", "unbound", publicV1String("failed"), publicV1String(ErrorMagpieUnavailable))
	unavailableSummary := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if unavailableSummary.Code != http.StatusServiceUnavailable || !strings.Contains(unavailableSummary.Body.String(), ErrorMagpieUnavailable) {
		t.Fatalf("unavailable Summary = %d %s, want 503 %s", unavailableSummary.Code, unavailableSummary.Body.String(), ErrorMagpieUnavailable)
	}
	if firstFixture.totalCalls() != oldClientCalls {
		t.Fatalf("old Magpie client received a request while Runtime was unavailable")
	}

	runtime.set(magpie.RuntimeStatusReady, magpie.NewClient(secondMagpie.URL, secondMagpie.Client()))
	waitFor(t, func() bool { return revisions.Current() == (RevisionTuple{DataRevision: 3, StatusRevision: 3}) })
	phase3AssertPublicStatus(t, server, "idle", "ready", publicV1String("completed"), nil)
	secondSummary := request(server, http.MethodGet, "/api/usage/summary?range=today")
	var second summaryResponseDTO
	if secondSummary.Code != http.StatusOK || json.Unmarshal(secondSummary.Body.Bytes(), &second) != nil || second.DataRevision != 3 || second.Usage.Input != 42 {
		t.Fatalf("Summary through replacement Runtime client = %d %s, want revision 3 and input 42", secondSummary.Code, secondSummary.Body.String())
	}
	if firstFixture.totalCalls() != oldClientCalls || secondFixture.count("/api/usage/requests") == 0 {
		t.Fatalf("runtime client selection was stale: old calls %d->%d, replacement usage requests %d", oldClientCalls, firstFixture.totalCalls(), secondFixture.count("/api/usage/requests"))
	}

	stopStream()
	select {
	case <-streamDone:
	case <-time.After(time.Second):
		t.Fatal("runtime integration SSE handler did not stop after disconnect")
	}
}

func phase3AssertPublicStatus(t *testing.T, server http.Handler, scanState, binding string, finished, lastError *string) {
	t.Helper()
	response := request(server, http.MethodGet, publicV1StatusPath)
	if response.Code != http.StatusOK {
		t.Fatalf("Public status = %d %s", response.Code, response.Body.String())
	}
	var status publicV1StatusDTO
	if err := json.Unmarshal(response.Body.Bytes(), &status); err != nil {
		t.Fatal(err)
	}
	if status.ScanState != scanState || status.SourceBindingStatus != binding ||
		!reflect.DeepEqual(status.LastFinishedScanResult, finished) || !reflect.DeepEqual(status.LastScanErrorCode, lastError) {
		t.Fatalf("Public status = %+v, want scan=%q binding=%q finished=%v error=%v", status, scanState, binding, finished, lastError)
	}
}

func TestPhase3QuotaRefreshAndCachedInternalPublicReads(t *testing.T) {
	now := time.Now().UTC().Truncate(time.Second)
	fixture := newPhase3MagpieFixture(now, 12, "Session", "/repo")
	magpieServer := httptest.NewServer(fixture)
	defer magpieServer.Close()
	runtime := &phase3RuntimeProvider{FakeRuntime: testsupport.NewFakeRuntime(magpie.RuntimeStatusReady)}
	runtime.set(magpie.RuntimeStatusReady, magpie.NewClient(magpieServer.URL, magpieServer.Client()))
	queryService := query.NewRuntimeService(runtime)
	projectionService := projection.NewService(queryService)
	revisions := NewRevisionService()
	quotas := &QuotaSnapshot{}
	server := phase3NewServer(t, projectionService, queryService, runtime, revisions, quotas, fstest.MapFS{})
	watcher := newWatchers(queryService, projectionService, revisions, quotas, time.Hour, 2*time.Hour, 3*time.Hour)

	watcher.refreshQuota(context.Background())
	if got := fixture.askedValues(); len(got) != 1 || got[0] != "" {
		t.Fatalf("background quota asked values = %v, want one ordinary request", got)
	}
	quotaCalls := fixture.count("/api/usage/quotas")
	for _, test := range []struct{ target, field string }{
		{target: "/api/quota", field: `"used":12.5`},
		{target: publicV1QuotaPath, field: `"used_percent":12.5`},
	} {
		response := request(server, http.MethodGet, test.target)
		if response.Code != http.StatusOK || !strings.Contains(response.Body.String(), test.field) {
			t.Fatalf("cached quota GET %s = %d %s", test.target, response.Code, response.Body.String())
		}
	}
	if got := fixture.count("/api/usage/quotas"); got != quotaCalls {
		t.Fatalf("cached Internal/Public quota GETs made vendor requests: %d->%d", quotaCalls, got)
	}

	fixture.setQuotaUsed(37.5)
	manual := request(server, http.MethodPost, "/api/quota/refresh")
	if manual.Code != http.StatusOK || !strings.Contains(manual.Body.String(), `"used":37.5`) {
		t.Fatalf("manual quota refresh = %d %s", manual.Code, manual.Body.String())
	}
	if got := fixture.askedValues(); len(got) != 2 || got[0] != "" || got[1] != "1" {
		t.Fatalf("background/manual quota asked values = %v, want [empty, 1]", got)
	}
	view, loaded, err := quotas.Current()
	if err != nil || !loaded || len(view.Items) != 1 || view.Items[0].Windows[0].Used != 37.5 {
		t.Fatalf("shared quota snapshot after manual refresh = %+v, loaded %t, error %v", view, loaded, err)
	}
	for _, test := range []struct{ target, field string }{
		{target: "/api/quota", field: `"used":37.5`},
		{target: publicV1QuotaPath, field: `"used_percent":37.5`},
	} {
		response := request(server, http.MethodGet, test.target)
		if response.Code != http.StatusOK || !strings.Contains(response.Body.String(), test.field) {
			t.Fatalf("updated cached quota GET %s = %d %s", test.target, response.Code, response.Body.String())
		}
	}
	if got := revisions.Current(); got != (RevisionTuple{}) {
		t.Fatalf("quota-only changes tuple = %+v, want zero", got)
	}
	if fixture.count("/api/usage") != 0 || fixture.count("/api/usage/requests") != 0 {
		t.Fatalf("quota refresh caused Usage requests: summary=%d rows=%d", fixture.count("/api/usage"), fixture.count("/api/usage/requests"))
	}
}

func TestPhase3ViteAssetsAndAPIFallbackOverLoopbackHTTP(t *testing.T) {
	server, _, _, _ := newServerFixture(t)
	dist := filepath.Join("..", "..", "frontend", "dist")
	if _, err := os.Stat(filepath.Join(dist, "index.html")); err != nil {
		t.Fatalf("current Vite build is unavailable at %s: %v", dist, err)
	}
	server.static = newStaticHandler(os.DirFS(dist))
	loopback := httptest.NewServer(server)
	defer loopback.Close()
	client := loopback.Client()

	indexResponse, err := client.Get(loopback.URL + "/")
	if err != nil {
		t.Fatal(err)
	}
	index, readErr := io.ReadAll(indexResponse.Body)
	_ = indexResponse.Body.Close()
	if readErr != nil || indexResponse.StatusCode != http.StatusOK || !strings.Contains(string(index), "<title>Usagi</title>") {
		t.Fatalf("GET / = %d, read error %v, body prefix %q", indexResponse.StatusCode, readErr, string(index[:min(len(index), 160)]))
	}
	assetPattern := regexp.MustCompile(`src="(/assets/[^"]+\.js)"`)
	match := assetPattern.FindSubmatch(index)
	if len(match) != 2 {
		t.Fatalf("Vite index did not name a JavaScript asset: %s", index)
	}
	assetResponse, err := client.Get(loopback.URL + string(match[1]))
	if err != nil {
		t.Fatal(err)
	}
	asset, assetReadErr := io.ReadAll(assetResponse.Body)
	_ = assetResponse.Body.Close()
	if assetReadErr != nil || assetResponse.StatusCode != http.StatusOK || len(asset) == 0 || !strings.Contains(assetResponse.Header.Get("Content-Type"), "javascript") {
		t.Fatalf("GET %s = %d, read error %v, content-type %q, bytes %d", match[1], assetResponse.StatusCode, assetReadErr, assetResponse.Header.Get("Content-Type"), len(asset))
	}

	deepLink, err := client.Get(loopback.URL + "/dashboard/sessions/deep-link")
	if err != nil {
		t.Fatal(err)
	}
	deepLinkBody, deepLinkErr := io.ReadAll(deepLink.Body)
	_ = deepLink.Body.Close()
	if deepLinkErr != nil || deepLink.StatusCode != http.StatusOK || string(deepLinkBody) != string(index) {
		t.Fatalf("GET dashboard deep link = %d, error %v, body matches index %t", deepLink.StatusCode, deepLinkErr, string(deepLinkBody) == string(index))
	}

	unknownAPI, err := client.Get(loopback.URL + "/api/not-found")
	if err != nil {
		t.Fatal(err)
	}
	unknownBody, unknownErr := io.ReadAll(unknownAPI.Body)
	_ = unknownAPI.Body.Close()
	if unknownErr != nil || unknownAPI.StatusCode != http.StatusNotFound || !strings.Contains(unknownAPI.Header.Get("Content-Type"), "application/json") || strings.Contains(string(unknownBody), "<html") || !strings.Contains(string(unknownBody), ErrorNotFound) {
		t.Fatalf("GET /api/not-found = %d, error %v, content-type %q, body %s", unknownAPI.StatusCode, unknownErr, unknownAPI.Header.Get("Content-Type"), unknownBody)
	}
}
