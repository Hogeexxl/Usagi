package query

import (
	"bytes"
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"net/http"
	"net/http/httptest"
	"net/url"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/database"
	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type syncDigestPoint struct {
	Time  time.Time `json:"time"`
	Calls int       `json:"calls"`
	Input int       `json:"input"`
}

type syncRequestCall struct {
	period string
	limit  int
	offset int
	query  url.Values
}

type syncHTTPFixture struct {
	mu                 sync.Mutex
	summary            magpie.UsageSummary
	digests            map[string]syncDigestPoint
	requests           map[string][]magpie.UsageRequest
	pageFailures       map[string]map[int]bool
	totalDeltas        map[string]map[int]int
	verificationDeltas map[string]int
	limitOneCalls      map[string]int
	sessions           []magpie.Session
	sessionLimits      []string
	sessionError       bool
	providersError     bool
	providers          magpie.ProviderCatalogsResponse
	providerCalls      int
	providerQueries    []url.Values
	requestCalls       []syncRequestCall
}

func newSyncHTTPFixture(summary magpie.UsageSummary) *syncHTTPFixture {
	return &syncHTTPFixture{
		summary: summary, digests: make(map[string]syncDigestPoint),
		requests: make(map[string][]magpie.UsageRequest), pageFailures: make(map[string]map[int]bool),
		totalDeltas: make(map[string]map[int]int), verificationDeltas: make(map[string]int),
		limitOneCalls: make(map[string]int),
	}
}

func (f *syncHTTPFixture) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	switch r.URL.Path {
	case "/api/usage":
		f.mu.Lock()
		value := f.summary
		f.mu.Unlock()
		_ = json.NewEncoder(w).Encode(value)
	case "/api/usage/requests":
		f.serveUsageRequests(w, r)
	case "/api/sessions":
		f.mu.Lock()
		f.sessionLimits = append(f.sessionLimits, r.URL.Query().Get("limit"))
		if f.sessionError {
			f.mu.Unlock()
			http.Error(w, "temporary session error", http.StatusServiceUnavailable)
			return
		}
		value := append([]magpie.Session(nil), f.sessions...)
		f.mu.Unlock()
		_ = json.NewEncoder(w).Encode(magpie.SessionsResponse{Sessions: value})
	case "/api/providers":
		f.mu.Lock()
		f.providerCalls++
		f.providerQueries = append(f.providerQueries, r.URL.Query().Clone())
		if f.providersError {
			f.mu.Unlock()
			http.Error(w, "temporary provider read error", http.StatusServiceUnavailable)
			return
		}
		value := f.providers
		f.mu.Unlock()
		_ = json.NewEncoder(w).Encode(value)
	default:
		http.NotFound(w, r)
	}
}

func (f *syncHTTPFixture) serveUsageRequests(w http.ResponseWriter, r *http.Request) {
	query := r.URL.Query()
	period := query.Get("period")
	parts := strings.SplitN(period, "..", 2)
	if len(parts) != 2 {
		http.Error(w, "period required", http.StatusBadRequest)
		return
	}
	day := parts[0]
	limit, _ := strconv.Atoi(query.Get("limit"))
	offset, _ := strconv.Atoi(query.Get("offset"))
	f.mu.Lock()
	f.requestCalls = append(f.requestCalls, syncRequestCall{
		period: period, limit: limit, offset: offset, query: query.Clone(),
	})
	rows := append([]magpie.UsageRequest(nil), f.requests[day]...)
	var points []syncDigestPoint
	for current, point := range f.digests {
		if current >= parts[0] && current <= parts[1] {
			points = append(points, point)
		}
	}
	pageFailure := f.pageFailures[day][offset] && limit == syncPageLimit
	total := len(rows) + f.totalDeltas[day][offset]
	if limit == 1 && parts[0] == parts[1] {
		f.limitOneCalls[day]++
		if f.limitOneCalls[day] >= 2 {
			total += f.verificationDeltas[day]
		}
	}
	f.mu.Unlock()
	if pageFailure {
		http.Error(w, "temporary request failure", http.StatusServiceUnavailable)
		return
	}
	if limit == 1 {
		series := make([]json.RawMessage, 0, len(points))
		for _, point := range points {
			encoded, err := json.Marshal(point)
			if err != nil {
				http.Error(w, err.Error(), http.StatusInternalServerError)
				return
			}
			series = append(series, encoded)
		}
		pageRows := pageRows(rows, offset, limit)
		_ = json.NewEncoder(w).Encode(struct {
			Bucket string                `json:"bucket"`
			Series []json.RawMessage     `json:"series"`
			Rows   []magpie.UsageRequest `json:"rows"`
			Total  int                   `json:"total"`
		}{Bucket: "day", Series: series, Rows: pageRows, Total: total})
		return
	}
	_ = json.NewEncoder(w).Encode(magpie.UsageRequestsPage{
		Rows: pageRows(rows, offset, limit), Total: total, Offset: offset,
	})
}

func pageRows(rows []magpie.UsageRequest, offset, limit int) []magpie.UsageRequest {
	if offset >= len(rows) {
		return []magpie.UsageRequest{}
	}
	end := min(offset+limit, len(rows))
	return rows[offset:end]
}

func (f *syncHTTPFixture) setSessions(sessions ...magpie.Session) {
	f.mu.Lock()
	f.sessions = append([]magpie.Session(nil), sessions...)
	f.mu.Unlock()
}

func (f *syncHTTPFixture) setPageFailure(day string, offset int, fail bool) {
	f.mu.Lock()
	if f.pageFailures[day] == nil {
		f.pageFailures[day] = make(map[int]bool)
	}
	f.pageFailures[day][offset] = fail
	f.mu.Unlock()
}

func (f *syncHTTPFixture) setTotalDelta(day string, offset, delta int) {
	f.mu.Lock()
	if f.totalDeltas[day] == nil {
		f.totalDeltas[day] = make(map[int]int)
	}
	f.totalDeltas[day][offset] = delta
	f.mu.Unlock()
}

func (f *syncHTTPFixture) setVerificationTotalDelta(day string, delta int) {
	f.mu.Lock()
	f.verificationDeltas[day] = delta
	f.mu.Unlock()
}

func (f *syncHTTPFixture) calls() []syncRequestCall {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]syncRequestCall(nil), f.requestCalls...)
}

func (f *syncHTTPFixture) sessionLimitCalls() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.sessionLimits...)
}

func countPageCalls(calls []syncRequestCall) int {
	count := 0
	for _, call := range calls {
		if call.limit == syncPageLimit {
			count++
		}
	}
	return count
}

func (f *syncHTTPFixture) setSessionError(value bool) {
	f.mu.Lock()
	f.sessionError = value
	f.mu.Unlock()
}

func openSyncTestDB(t *testing.T) (*sql.DB, string) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "usagi.sqlite")
	db, err := database.Open(path)
	if err != nil {
		t.Fatalf("open test database: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return db, path
}

func queryTestDB(t *testing.T) *sql.DB {
	t.Helper()
	db, _ := openSyncTestDB(t)
	return db
}

func newSyncTestService(t *testing.T, db *sql.DB, fixture *syncHTTPFixture, location *time.Location, now time.Time) *Service {
	t.Helper()
	server := httptest.NewServer(fixture)
	t.Cleanup(server.Close)
	service := newService(db, magpie.NewClient(server.URL, server.Client()), location)
	service.now = func() time.Time { return now }
	service.pricingHTTPClient = &http.Client{Transport: priceBootstrapFailureTransport{}}
	return service
}

type priceBootstrapFailureTransport struct{}

func (priceBootstrapFailureTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	return &http.Response{
		StatusCode: http.StatusServiceUnavailable,
		Header:     make(http.Header),
		Body:       io.NopCloser(strings.NewReader("")),
		Request:    request,
	}, nil
}

type priceBootstrapFixtureTransport struct {
	body   []byte
	status int
	calls  int
}

func (t *priceBootstrapFixtureTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	t.calls++
	status := t.status
	if status == 0 {
		status = http.StatusOK
	}
	body := t.body
	if status != http.StatusOK {
		body = nil
	}
	return &http.Response{
		StatusCode: status,
		Header:     make(http.Header),
		Body:       io.NopCloser(bytes.NewReader(body)),
		Request:    request,
	}, nil
}

func TestPriceBootstrapUsesHTTPProviderCatalogSnapshot(t *testing.T) {
	db := queryTestDB(t)
	location := time.UTC
	now := time.Date(2026, 10, 14, 8, 0, 0, 0, location)
	fixture := newSyncHTTPFixture(magpie.UsageSummary{})
	fixture.providers = magpie.ProviderCatalogsResponse{Providers: []magpie.ProviderCatalog{
		{ID: "provider-a", Catalog: " OPENAI, DEEPSEEK openai "},
		{ID: "provider-a", Catalog: "google, deepseek"},
		{ID: "provider-empty", Catalog: "   "},
	}}
	service := newSyncTestService(t, db, fixture, location, now)
	transport := &priceBootstrapFixtureTransport{body: []byte(`{"openai":{"models":{"gpt-6-luna":{"canonical_model_id":"openai/gpt-6-luna","cost":{"input":0.1,"output":0.5,"cache_read":0.01,"cache_write":0.125,"cache_write_1h":0.125}}}}}`)}
	service.pricingHTTPClient = &http.Client{Transport: transport}
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("Sync: %v", err)
	}
	var providerJSON string
	if err := db.QueryRow("SELECT provider_catalogs_json FROM price_versions").Scan(&providerJSON); err != nil {
		t.Fatal(err)
	}
	var got map[string][]string
	if err := json.Unmarshal([]byte(providerJSON), &got); err != nil {
		t.Fatal(err)
	}
	want := map[string][]string{
		"provider-a":     {"openai", "deepseek", "google"},
		"provider-empty": {},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("provider snapshot = %v, want %v", got, want)
	}
	if transport.calls != 1 || fixture.providerCalls != 1 || len(fixture.providerQueries) != 1 || len(fixture.providerQueries[0]) != 0 {
		t.Fatalf("bootstrap calls models.dev=%d providers=%d query=%v, want one each and no query", transport.calls, fixture.providerCalls, fixture.providerQueries)
	}
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("second Sync: %v", err)
	}
	if transport.calls != 1 || fixture.providerCalls != 1 {
		t.Fatalf("existing version retried bootstrap: models.dev=%d providers=%d", transport.calls, fixture.providerCalls)
	}
}

func TestPriceBootstrapFailureDoesNotBlockInitialSync(t *testing.T) {
	db := queryTestDB(t)
	location := time.UTC
	now := time.Date(2026, 10, 14, 8, 0, 0, 0, location)
	fixture := newSyncHTTPFixture(magpie.UsageSummary{})
	service := newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("Sync after price bootstrap failure: %v", err)
	}
	complete, err := service.InitialSyncComplete(context.Background())
	if err != nil || !complete {
		t.Fatalf("initial sync complete = (%t, %v), want true", complete, err)
	}
	var versions int
	if err := db.QueryRow("SELECT COUNT(*) FROM price_versions").Scan(&versions); err != nil {
		t.Fatal(err)
	}
	if versions != 0 {
		t.Fatalf("price versions = %d after failed download, want 0", versions)
	}

	day := now.Format(time.DateOnly)
	if _, err := db.Exec(`INSERT INTO requests (t_ns,local_day,agent,provider,model,session,input,magpie_cost,magpie_priced)
		VALUES (? ,?,'codex','provider-a','unlisted-model','session',7,12,1)`, now.UnixNano(), day); err != nil {
		t.Fatalf("insert unavailable-price request: %v", err)
	}
	dataset, err := service.SessionDataset(context.Background(), SessionQuery{Range: DateRange{From: day, To: day}})
	if err != nil {
		t.Fatal(err)
	}
	if len(dataset.Sessions) != 1 || dataset.Sessions[0].Totals.Cost != 0 || dataset.Sessions[0].Totals.Unpriced != 1 ||
		len(dataset.Rows) != 1 || dataset.Rows[0].Priced || dataset.Rows[0].Cost != 0 {
		t.Fatalf("unpriced SessionDataset = %+v, want zero cost and one unpriced token row", dataset)
	}
}

func TestPriceBootstrapProviderFileErrorLeavesNoVersion(t *testing.T) {
	db := queryTestDB(t)
	location := time.UTC
	now := time.Date(2026, 10, 14, 8, 0, 0, 0, location)
	fixture := newSyncHTTPFixture(magpie.UsageSummary{})
	fixture.providers.FileError = "providers file unreadable"
	service := newSyncTestService(t, db, fixture, location, now)
	transport := &priceBootstrapFixtureTransport{body: []byte(`{"openai":{"models":{"gpt-6-luna":{"cost":{"input":0.1}}}}}`)}
	service.pricingHTTPClient = &http.Client{Transport: transport}
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("Sync after providers file error: %v", err)
	}
	var versions int
	if err := db.QueryRow("SELECT COUNT(*) FROM price_versions").Scan(&versions); err != nil {
		t.Fatal(err)
	}
	if versions != 0 || transport.calls != 0 {
		t.Fatalf("provider file error created %d versions and called models.dev %d times", versions, transport.calls)
	}
}

func TestPriceBootstrapProviderRequestFailureLeavesNoVersion(t *testing.T) {
	db := queryTestDB(t)
	location := time.UTC
	now := time.Date(2026, 10, 14, 8, 0, 0, 0, location)
	fixture := newSyncHTTPFixture(magpie.UsageSummary{})
	fixture.providersError = true
	service := newSyncTestService(t, db, fixture, location, now)
	transport := &priceBootstrapFixtureTransport{body: []byte(`{"openai":{"models":{"gpt-6-luna":{"cost":{"input":0.1}}}}}`)}
	service.pricingHTTPClient = &http.Client{Transport: transport}
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("Sync after providers request failure: %v", err)
	}
	complete, err := service.InitialSyncComplete(context.Background())
	if err != nil || !complete {
		t.Fatalf("initial sync complete = (%t, %v), want true", complete, err)
	}
	var versions int
	if err := db.QueryRow("SELECT COUNT(*) FROM price_versions").Scan(&versions); err != nil {
		t.Fatal(err)
	}
	if versions != 0 || transport.calls != 0 {
		t.Fatalf("providers request failure created %d versions and called models.dev %d times", versions, transport.calls)
	}
}

func TestPriceBootstrapRetriesAfterModelsDevFailure(t *testing.T) {
	db := queryTestDB(t)
	location := time.UTC
	now := time.Date(2026, 10, 14, 8, 0, 0, 0, location)
	fixture := newSyncHTTPFixture(magpie.UsageSummary{})
	service := newSyncTestService(t, db, fixture, location, now)
	transport := &priceBootstrapFixtureTransport{
		status: http.StatusServiceUnavailable,
		body:   []byte(`{"openai":{"models":{"gpt-6-luna":{"cost":{"input":0.1}}}}}`),
	}
	service.pricingHTTPClient = &http.Client{Transport: transport}
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("first Sync after models.dev failure: %v", err)
	}
	complete, err := service.InitialSyncComplete(context.Background())
	if err != nil || !complete {
		t.Fatalf("initial sync complete after price failure = (%t, %v), want true", complete, err)
	}
	var versions int
	if err := db.QueryRow("SELECT COUNT(*) FROM price_versions").Scan(&versions); err != nil {
		t.Fatal(err)
	}
	if versions != 0 || transport.calls != 1 {
		t.Fatalf("after first Sync versions=%d downloads=%d, want 0/1", versions, transport.calls)
	}
	transport.status = http.StatusOK
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("second Sync after models.dev recovers: %v", err)
	}
	if err := db.QueryRow("SELECT COUNT(*) FROM price_versions").Scan(&versions); err != nil {
		t.Fatal(err)
	}
	if versions != 1 || transport.calls != 2 || fixture.providerCalls != 2 {
		t.Fatalf("after retry versions=%d downloads=%d provider calls=%d, want 1/2/2", versions, transport.calls, fixture.providerCalls)
	}
}

func TestPriceBootstrapCostGateAndRequestedModelIgnored(t *testing.T) {
	db := queryTestDB(t)
	location := time.UTC
	now := time.Date(2026, 10, 14, 8, 0, 0, 0, location)
	fixture := newSyncHTTPFixture(magpie.UsageSummary{})
	fixture.providers = magpie.ProviderCatalogsResponse{Providers: []magpie.ProviderCatalog{{ID: "provider-a", Catalog: "openai"}}}
	service := newSyncTestService(t, db, fixture, location, now)
	transport := &priceBootstrapFixtureTransport{body: []byte(`{"openai":{"models":{"fixture-model":{"cost":{"input":1,"output":2,"cache_read":3,"cache_write":4,"cache_write_1h":5,"tiers":[{"above":1,"input":100,"output":200}]}},"free-fixture":{"cost":{"input":0,"output":0,"cache_read":0,"cache_write":0,"cache_write_1h":0}}}}}`)}
	service.pricingHTTPClient = &http.Client{Transport: transport}
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("Sync: %v", err)
	}
	day := now.Format(time.DateOnly)
	requests := []magpie.UsageRequest{
		{Time: now, Agent: "codex", Session: "priced", Provider: "provider-a", Model: "fixture-model", Input: 100, Output: 50, CacheRead: 20, CacheWrite: 30, CacheWrite1h: 10},
		{Time: now.Add(time.Second), Agent: "codex", Session: "requested-only", Provider: "provider-a", Model: "unlisted-model", Requested: "fixture-model", Input: 10},
		{Time: now.Add(2 * time.Second), Agent: "codex", Session: "cache-only", Provider: "provider-a", Model: "unlisted-model", CacheRead: 10},
		{Time: now.Add(3 * time.Second), Agent: "codex", Session: "free", Provider: "provider-a", Model: "free-fixture", Input: 10},
	}
	for _, row := range requests {
		if _, err := db.Exec(`INSERT INTO requests (t_ns,local_day,agent,provider,model,requested,session,input,output,cache_read,cache_write,cache_write_1h)
			VALUES (?,?,?,?,?,?,?,?,?,?,?,?)`, row.Time.UnixNano(), day, row.Agent, row.Provider, row.Model, row.Requested,
			row.Session, row.Input, row.Output, row.CacheRead, row.CacheWrite, row.CacheWrite1h); err != nil {
			t.Fatalf("insert cost gate request: %v", err)
		}
	}
	dataset, err := service.SessionDataset(context.Background(), SessionQuery{Range: DateRange{From: day, To: day}})
	if err != nil {
		t.Fatal(err)
	}
	if len(dataset.Sessions) != 4 || len(dataset.Rows) != 4 {
		t.Fatalf("cost-gate dataset size = %d sessions / %d rows, want 4 / 4", len(dataset.Sessions), len(dataset.Rows))
	}
	var formula, requested, cacheOnly, free *UsageRow
	var cost float64
	var unpriced int64
	for i := range dataset.Rows {
		row := &dataset.Rows[i]
		cost += row.Cost
		if row.Input+row.Output > 0 && !row.Priced {
			unpriced++
		}
		switch {
		case row.Model == "fixture-model":
			formula = row
		case row.Model == "free-fixture":
			free = row
		case row.Model == "unlisted-model" && row.Input > 0:
			requested = row
		case row.Model == "unlisted-model" && row.CacheRead > 0:
			cacheOnly = row
		}
	}
	if formula == nil || !formula.Priced || math.Abs(formula.Cost-0.00039) > 1e-9 || math.Abs(cost-0.00039) > 1e-9 || unpriced != 1 {
		t.Fatalf("five-price row = %+v, want priced cost 0.00039", formula)
	}
	if requested == nil || requested.Priced || requested.Cost != 0 {
		t.Fatalf("requested-only row = %+v, want unpriced at zero", requested)
	}
	if cacheOnly == nil || cacheOnly.Priced || cacheOnly.Cost != 0 {
		t.Fatalf("cache-only row = %+v, want zero cost and no price lookup", cacheOnly)
	}
	if free == nil || !free.Priced || free.Cost != 0 {
		t.Fatalf("explicit zero-price row = %+v, want priced at zero", free)
	}
}

func digestPoint(day string, location *time.Location, calls, input int) syncDigestPoint {
	date, err := time.ParseInLocation(time.DateOnly, day, location)
	if err != nil {
		panic(err)
	}
	return syncDigestPoint{Time: date.Add(12 * time.Hour), Calls: calls, Input: input}
}

func digestBytes(t *testing.T, point syncDigestPoint) []byte {
	t.Helper()
	encoded, err := json.Marshal(point)
	if err != nil {
		t.Fatal(err)
	}
	decoder := json.NewDecoder(bytes.NewReader(encoded))
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		t.Fatal(err)
	}
	canonical, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(canonical)
	return digest[:]
}

func seedDay(t *testing.T, db *sql.DB, day string, fingerprint []byte, lastSync int64, frozen bool) {
	t.Helper()
	if _, err := db.Exec(`INSERT INTO sync_days (day, fingerprint, row_count, last_sync_ns, frozen)
		VALUES (?, ?, 0, ?, ?)`, day, fingerprint, lastSync, boolInt(frozen)); err != nil {
		t.Fatalf("seed sync day %s: %v", day, err)
	}
}

func seedRequest(t *testing.T, db *sql.DB, day, model string, input int) {
	t.Helper()
	when, err := time.ParseInLocation(time.DateOnly, day, time.Local)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec("INSERT INTO requests (t_ns, local_day, model, input) VALUES (?, ?, ?, ?)",
		when.UnixNano(), day, model, input); err != nil {
		t.Fatalf("seed request row for %s: %v", day, err)
	}
}

func setInitialSyncComplete(t *testing.T, db *sql.DB) {
	t.Helper()
	if _, err := db.Exec("UPDATE app_meta SET value = '1' WHERE key = 'initial_sync_complete'"); err != nil {
		t.Fatal(err)
	}
}

func dbRevision(t *testing.T, db *sql.DB) int64 {
	t.Helper()
	var value int64
	if err := db.QueryRow("SELECT CAST(value AS INTEGER) FROM app_meta WHERE key = 'data_revision'").Scan(&value); err != nil {
		t.Fatal(err)
	}
	return value
}

func TestSyncDayPaginatesSequentiallyAndUsesInjectedLocalDate(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 0, 30, 0, 0, location)
	pointTime := time.Date(2026, 10, 13, 16, 30, 0, 0, time.UTC)
	const total = 501
	rows := make([]magpie.UsageRequest, total)
	for index := range rows {
		rows[index] = magpie.UsageRequest{
			Time: pointTime.Add(time.Duration(index) * time.Second), Agent: "codex", Model: fmt.Sprintf("m-%03d", index),
			Input: index, Status: http.StatusOK,
			ProviderAccount: "must-not-persist",
		}
	}
	rows[0].RouteID, rows[0].RID, rows[0].ResponseID = 17, "rid-17", "response-17"
	rows[1].Rejected = true
	summaryPoint := magpie.UsagePoint{Label: "2026-10-13", Time: pointTime, UsageTotals: magpie.UsageTotals{Calls: total, Input: total}}
	fixture := newSyncHTTPFixture(magpie.UsageSummary{
		Period: "all", Series: []magpie.UsagePoint{summaryPoint},
		UsageTotals: magpie.UsageTotals{Calls: total, Input: total},
	})
	fixture.digests["2026-10-14"] = syncDigestPoint{Time: pointTime, Calls: total, Input: total}
	fixture.requests["2026-10-14"] = rows
	db, _ := openSyncTestDB(t)
	service := newSyncTestService(t, db, fixture, location, now)
	oldTime := now.Add(-time.Hour).UnixNano()
	if _, err := db.Exec("INSERT INTO requests (t_ns, local_day, model, input) VALUES (?, ?, 'old', 9)", oldTime, "2026-10-14"); err != nil {
		t.Fatal(err)
	}
	revision, err := service.Sync(context.Background())
	if err != nil {
		t.Fatalf("Sync() error = %v", err)
	}
	if revision != 2 || dbRevision(t, db) != 2 {
		t.Fatalf("revision = %d / DB %d, want day replacement plus initial completion (2)", revision, dbRevision(t, db))
	}
	complete, err := service.InitialSyncComplete(context.Background())
	if err != nil || !complete {
		t.Fatalf("InitialSyncComplete() = %t, %v", complete, err)
	}
	var count, rejected int
	if err := db.QueryRow("SELECT COUNT(*), SUM(rejected) FROM requests WHERE local_day = '2026-10-14'").Scan(&count, &rejected); err != nil {
		t.Fatal(err)
	}
	if count != total || rejected != 1 {
		t.Fatalf("stored request count/rejected = %d/%d, want %d/1", count, rejected, total)
	}
	var model string
	var routeID int
	var rid, responseID string
	var accountExists int
	if err := db.QueryRow("SELECT model, route_id, rid, response_id, EXISTS(SELECT 1 FROM pragma_table_info('requests') WHERE name = 'provider_account') FROM requests WHERE local_day = '2026-10-14' ORDER BY id LIMIT 1").Scan(&model, &routeID, &rid, &responseID, &accountExists); err != nil {
		t.Fatal(err)
	}
	if model != "m-000" || routeID != 17 || rid != "rid-17" || responseID != "response-17" || accountExists != 0 {
		t.Fatalf("stored first request = %q route %d rid %q response %q account-column %d", model, routeID, rid, responseID, accountExists)
	}
	var syncedDay string
	if err := db.QueryRow("SELECT day FROM sync_days").Scan(&syncedDay); err != nil || syncedDay != "2026-10-14" {
		t.Fatalf("sync_days local date = %q, %v; want 2026-10-14 across UTC midnight", syncedDay, err)
	}
	var pageCalls []syncRequestCall
	for _, call := range fixture.calls() {
		if call.limit == syncPageLimit {
			pageCalls = append(pageCalls, call)
		}
	}
	if len(pageCalls) != 2 || pageCalls[0].offset != 0 || pageCalls[1].offset != 500 {
		t.Fatalf("sequential page calls = %+v, want offsets 0 then 500", pageCalls)
	}
	for _, call := range pageCalls {
		if call.period != "2026-10-14..2026-10-14" {
			t.Fatalf("day page period = %q, want only the local day", call.period)
		}
		for key := range call.query {
			if key != "period" && key != "limit" && key != "offset" {
				t.Fatalf("day page included unexpected filter %q in %+v", key, call.query)
			}
		}
	}
}

func TestSyncDayFailureOrMovingTotalRetainsPreviousRows(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	day := "2026-10-14"
	point := digestPoint(day, location, 501, 501)
	summary := magpie.UsageSummary{Period: "all", Series: []magpie.UsagePoint{{
		Label: day, Time: point.Time, UsageTotals: magpie.UsageTotals{Calls: 501, Input: 501},
	}}, UsageTotals: magpie.UsageTotals{Calls: 501, Input: 501}}
	for _, test := range []struct {
		name         string
		setup        func(*syncHTTPFixture)
		pages        int
		writeFailure bool
	}{
		{name: "request failure", setup: func(f *syncHTTPFixture) { f.setPageFailure(day, 0, true) }, pages: 1},
		{name: "total changed between pages", setup: func(f *syncHTTPFixture) { f.setTotalDelta(day, 500, 1) }, pages: 2},
		{name: "total changed at final recheck", setup: func(f *syncHTTPFixture) { f.setVerificationTotalDelta(day, 1) }, pages: 2},
		{name: "replacement transaction write failed", setup: func(*syncHTTPFixture) {}, pages: 2, writeFailure: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			fixture := newSyncHTTPFixture(summary)
			fixture.digests[day] = point
			fixture.requests[day] = make([]magpie.UsageRequest, 501)
			test.setup(fixture)
			db, _ := openSyncTestDB(t)
			service := newSyncTestService(t, db, fixture, location, now)
			oldFingerprint := make([]byte, 32)
			oldFingerprint[0] = 99
			seedDay(t, db, day, oldFingerprint, now.Add(-24*time.Hour).UnixNano(), false)
			seedRequest(t, db, day, "old", 7)
			if test.writeFailure {
				if _, err := db.Exec(`CREATE TRIGGER injected_request_write_failure BEFORE INSERT ON requests
					WHEN NEW.model <> 'old' BEGIN SELECT RAISE(FAIL, 'injected write failure'); END`); err != nil {
					t.Fatalf("inject transaction write failure: %v", err)
				}
			}
			revision, err := service.Sync(context.Background())
			if err == nil {
				t.Fatal("Sync() succeeded, want the failed day to be abandoned")
			}
			if revision != 0 || dbRevision(t, db) != 0 {
				t.Fatalf("revision after failed date = %d / DB %d, want 0", revision, dbRevision(t, db))
			}
			var count int
			var model string
			if err := db.QueryRow("SELECT COUNT(*), MIN(model) FROM requests WHERE local_day = ?", day).Scan(&count, &model); err != nil {
				t.Fatal(err)
			}
			var storedFingerprint []byte
			if err := db.QueryRow("SELECT fingerprint FROM sync_days WHERE day = ?", day).Scan(&storedFingerprint); err != nil {
				t.Fatal(err)
			}
			if count != 1 || model != "old" || string(storedFingerprint) != string(oldFingerprint) {
				t.Fatalf("failed replacement changed old day: count=%d model=%q fingerprint=%x", count, model, storedFingerprint)
			}
			pages := 0
			for _, call := range fixture.calls() {
				if call.limit == syncPageLimit {
					pages++
				}
			}
			if pages != test.pages {
				t.Fatalf("page attempts = %d, want %d with no in-cycle retry", pages, test.pages)
			}
		})
	}
}

func TestSyncDayGlobalChangeRetriesRecentDayAfterFailedPull(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	today := now.Format(time.DateOnly)
	yesterday := now.AddDate(0, 0, -1).Format(time.DateOnly)
	todayPoint := digestPoint(today, location, 1, 10)
	yesterdayPoint := digestPoint(yesterday, location, 1, 5)
	todaySummaryPoint := magpie.UsagePoint{Label: today, Time: todayPoint.Time, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 10}}
	yesterdaySummaryPoint := magpie.UsagePoint{Label: yesterday, Time: yesterdayPoint.Time, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 5}}
	fixture := newSyncHTTPFixture(magpie.UsageSummary{
		Period: "all", Series: []magpie.UsagePoint{yesterdaySummaryPoint, todaySummaryPoint},
		UsageTotals: magpie.UsageTotals{Calls: 2, Input: 15},
	})
	fixture.digests[today], fixture.digests[yesterday] = todayPoint, yesterdayPoint
	fixture.requests[today] = []magpie.UsageRequest{{Time: todayPoint.Time, Model: "old-today", Input: 10}}
	fixture.requests[yesterday] = []magpie.UsageRequest{{Time: yesterdayPoint.Time, Model: "old-yesterday", Input: 5}}
	db, _ := openSyncTestDB(t)
	service := newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("initial Sync() error = %v", err)
	}
	initialPages := countPageCalls(fixture.calls())

	fixture.mu.Lock()
	fixture.summary.UsageTotals.Input++
	fixture.requests[today] = []magpie.UsageRequest{{Time: todayPoint.Time, Model: "new-today", Input: 11}}
	fixture.mu.Unlock()
	fixture.setPageFailure(today, 0, true)
	if _, err := service.Sync(context.Background()); err == nil {
		t.Fatal("changed summary with a failed today's request returned no error")
	}
	var todayModel string
	if err := db.QueryRow("SELECT model FROM requests WHERE local_day = ?", today).Scan(&todayModel); err != nil || todayModel != "old-today" {
		t.Fatalf("failed recent refresh replaced today's stored row with %q, error %v", todayModel, err)
	}
	partialPages := countPageCalls(fixture.calls())
	if partialPages != initialPages+2 {
		t.Fatalf("global change fetched %d pages before retry, want both recent days", partialPages-initialPages)
	}

	fixture.setPageFailure(today, 0, false)
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("retry of failed recent day error = %v", err)
	}
	if err := db.QueryRow("SELECT model FROM requests WHERE local_day = ?", today).Scan(&todayModel); err != nil || todayModel != "new-today" {
		t.Fatalf("failed recent day was not retried, stored model %q, error %v", todayModel, err)
	}
	if got := countPageCalls(fixture.calls()); got != partialPages+1 {
		t.Fatalf("retry cycle made %d full-day requests, want only the failed day", got-partialPages)
	}
}

func TestSyncFreezeUsesStrictLocalCutoffAndRecordsDifference(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	const from = "2026-10-04"
	const cutoff = "2026-10-07"
	var series []magpie.UsagePoint
	fixture := newSyncHTTPFixture(magpie.UsageSummary{Period: "all", UsageTotals: magpie.UsageTotals{Calls: 11}})
	for date := 0; date <= 10; date++ {
		day := now.AddDate(0, 0, date-10).In(location).Format(time.DateOnly)
		point := digestPoint(day, location, 1, date+1)
		fixture.digests[day] = point
		series = append(series, magpie.UsagePoint{Label: day, Time: point.Time, UsageTotals: magpie.UsageTotals{Calls: point.Calls, Input: point.Input}})
	}
	fixture.summary.Series = series
	fixture.summary.UsageTotals.Calls = len(series)
	fixture.requests["2026-10-05"] = []magpie.UsageRequest{{
		Time: now.Add(-9 * 24 * time.Hour), Agent: "codex", Model: "new", Input: 90, Status: http.StatusOK,
	}}
	db, _ := openSyncTestDB(t)
	setInitialSyncComplete(t, db)
	for index := 0; index <= 10; index++ {
		day := now.AddDate(0, 0, index-10).In(location).Format(time.DateOnly)
		point := fixture.digests[day]
		end, err := localDayEnd(day, location)
		if err != nil {
			t.Fatal(err)
		}
		lastSync := end.Add(time.Hour).UnixNano()
		if day == "2026-10-05" {
			lastSync = end.UnixNano()
		}
		frozen := day == from
		fingerprint := digestBytes(t, point)
		if day == from {
			fingerprint[0] ^= 0xff
		}
		seedDay(t, db, day, fingerprint, lastSync, frozen)
		seedRequest(t, db, day, "baseline", index)
	}
	service := newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("Sync() error = %v", err)
	}
	var cutoffFrozen, eligibleFrozen, needsCompleteFetchFrozen int
	if err := db.QueryRow("SELECT frozen FROM sync_days WHERE day = ?", cutoff).Scan(&cutoffFrozen); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT frozen FROM sync_days WHERE day = '2026-10-06'").Scan(&eligibleFrozen); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT frozen FROM sync_days WHERE day = '2026-10-05'").Scan(&needsCompleteFetchFrozen); err != nil {
		t.Fatal(err)
	}
	if cutoffFrozen != 0 || eligibleFrozen != 1 || needsCompleteFetchFrozen != 1 {
		t.Fatalf("cutoff/eligible/incomplete frozen flags = %d/%d/%d, want 0/1/1", cutoffFrozen, eligibleFrozen, needsCompleteFetchFrozen)
	}
	var difference string
	if err := db.QueryRow("SELECT difference FROM sync_days WHERE day = ?", from).Scan(&difference); err != nil {
		t.Fatal(err)
	}
	if difference == "" {
		t.Fatal("changed frozen fingerprint did not record a difference")
	}
	var baselineModel string
	if err := db.QueryRow("SELECT model FROM requests WHERE local_day = ?", from).Scan(&baselineModel); err != nil || baselineModel != "baseline" {
		t.Fatalf("frozen requests were overwritten: model=%q error=%v", baselineModel, err)
	}
	var fetchedModel string
	if err := db.QueryRow("SELECT model FROM requests WHERE local_day = '2026-10-05'").Scan(&fetchedModel); err != nil || fetchedModel != "new" {
		t.Fatalf("old day requiring a completed sync retained model %q, error %v", fetchedModel, err)
	}
	if got := dbRevision(t, db); got != 1 {
		t.Fatalf("data revision = %d, want only the mutable day replacement; freeze and difference do not increment", got)
	}
	var fetchedDays []string
	for _, call := range fixture.calls() {
		if call.limit == syncPageLimit {
			fetchedDays = append(fetchedDays, call.period)
		}
	}
	if len(fetchedDays) != 1 || fetchedDays[0] != "2026-10-05..2026-10-05" {
		t.Fatalf("full request fetches for freeze pass = %v, want only 2026-10-05", fetchedDays)
	}
}

func TestSyncFreezeFailureDoesNotFreezeOldDayWithoutSuccessfulPostEndSync(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	const day = "2026-10-05"
	var series []magpie.UsagePoint
	fixture := newSyncHTTPFixture(magpie.UsageSummary{Period: "all"})
	for offset := 0; offset <= 9; offset++ {
		current := now.AddDate(0, 0, offset-9).In(location).Format(time.DateOnly)
		point := digestPoint(current, location, 1, offset+1)
		fixture.digests[current] = point
		series = append(series, magpie.UsagePoint{Label: current, Time: point.Time, UsageTotals: magpie.UsageTotals{Calls: 1, Input: point.Input}})
	}
	fixture.summary.Series = series
	fixture.summary.UsageTotals.Calls = len(series)
	fixture.requests[day] = []magpie.UsageRequest{{Time: now.Add(-9 * 24 * time.Hour), Model: "new", Input: 9}}
	fixture.setPageFailure(day, 0, true)
	db, _ := openSyncTestDB(t)
	setInitialSyncComplete(t, db)
	// Seed the complete date span in this test database, including the one date
	// whose last successful sync ended exactly at local midnight.
	for offset := 0; offset <= 9; offset++ {
		current := now.AddDate(0, 0, offset-9).In(location).Format(time.DateOnly)
		point := fixture.digests[current]
		end, err := localDayEnd(current, location)
		if err != nil {
			t.Fatal(err)
		}
		lastSync := end.Add(time.Hour).UnixNano()
		if current == day {
			lastSync = end.UnixNano()
		}
		seedDay(t, db, current, digestBytes(t, point), lastSync, false)
	}
	seedRequest(t, db, day, "old", 5)
	service := newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err == nil {
		t.Fatal("Sync() succeeded despite the required old-day fetch failing")
	}
	var frozen int
	var lastSync int64
	if err := db.QueryRow("SELECT frozen, last_sync_ns FROM sync_days WHERE day = ?", day).Scan(&frozen, &lastSync); err != nil {
		t.Fatal(err)
	}
	end, err := localDayEnd(day, location)
	if err != nil {
		t.Fatal(err)
	}
	var model string
	if err := db.QueryRow("SELECT model FROM requests WHERE local_day = ?", day).Scan(&model); err != nil {
		t.Fatal(err)
	}
	if frozen != 0 || lastSync != end.UnixNano() || model != "old" || dbRevision(t, db) != 0 {
		t.Fatalf("failed old-day fetch state = frozen %d last_sync %d model %q revision %d", frozen, lastSync, model, dbRevision(t, db))
	}
}

func TestSyncSessionsUpdatesMetadataPreservesHistoryAndIgnoresLastSeenRevision(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	day := now.Format(time.DateOnly)
	point := digestPoint(day, location, 1, 1)
	summary := magpie.UsageSummary{Period: "all", Series: []magpie.UsagePoint{{
		Label: day, Time: point.Time, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 1},
	}}, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 1}}
	fixture := newSyncHTTPFixture(summary)
	fixture.digests[day] = point
	first := magpie.Session{Agent: "codex", ID: "s1", Cwd: "/repo", Title: "Before", Start: now.Add(-time.Hour), Last: now.Add(-time.Minute), Name: "Codex", Path: "/private/session.json"}
	fixture.setSessions(first)
	db, _ := openSyncTestDB(t)
	service := newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("initial Sync() error = %v", err)
	}
	var firstSeen, lastSeen int64
	var hasPath, disappeared int
	var title, agentName string
	if err := db.QueryRow(`SELECT first_seen_ns, last_seen_ns, has_path, disappeared, title, agent_name
		FROM sessions WHERE agent = 'codex' AND id = 's1'`).Scan(&firstSeen, &lastSeen, &hasPath, &disappeared, &title, &agentName); err != nil {
		t.Fatal(err)
	}
	if firstSeen != now.UnixNano() || lastSeen != now.UnixNano() || hasPath != 1 || disappeared != 0 || title != "Before" || agentName != "Codex" {
		t.Fatalf("initial metadata = first %d last %d path %d disappeared %d title %q agent name %q", firstSeen, lastSeen, hasPath, disappeared, title, agentName)
	}
	if limits := fixture.sessionLimitCalls(); len(limits) == 0 || limits[0] != "0" {
		t.Fatalf("full session request limits = %v, want explicit limit=0", limits)
	}
	if _, err := db.Exec("SELECT path FROM sessions WHERE agent = 'codex' AND id = 's1'"); err == nil {
		t.Fatal("session path content was persisted; only has_path should be recorded")
	}
	initialRevision := dbRevision(t, db)

	service.now = func() time.Time { return now.Add(time.Hour) }
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("unchanged session Sync() error = %v", err)
	}
	if err := db.QueryRow("SELECT first_seen_ns, last_seen_ns FROM sessions WHERE agent = 'codex' AND id = 's1'").Scan(&firstSeen, &lastSeen); err != nil {
		t.Fatal(err)
	}
	if firstSeen != now.UnixNano() || lastSeen != now.Add(time.Hour).UnixNano() || dbRevision(t, db) != initialRevision {
		t.Fatalf("pure last_seen refresh changed first/last/revision to %d/%d/%d, initial first/revision %d/%d", firstSeen, lastSeen, dbRevision(t, db), now.UnixNano(), initialRevision)
	}

	fixture.setSessions()
	service.now = func() time.Time { return now.Add(2 * time.Hour) }
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("disappearance Sync() error = %v", err)
	}
	var cwd, retainedTitle string
	if err := db.QueryRow("SELECT disappeared, cwd, title, last_seen_ns FROM sessions WHERE agent = 'codex' AND id = 's1'").Scan(&disappeared, &cwd, &retainedTitle, &lastSeen); err != nil {
		t.Fatal(err)
	}
	if disappeared != 1 || cwd != "/repo" || retainedTitle != "Before" || lastSeen != now.Add(time.Hour).UnixNano() || dbRevision(t, db) != initialRevision+1 {
		t.Fatalf("disappeared metadata = flag %d cwd %q title %q last %d revision %d", disappeared, cwd, retainedTitle, lastSeen, dbRevision(t, db))
	}

	reappeared := first
	reappeared.Title = "After"
	reappeared.Path = ""
	fixture.setSessions(reappeared)
	service.now = func() time.Time { return now.Add(3 * time.Hour) }
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("reappearance Sync() error = %v", err)
	}
	if err := db.QueryRow("SELECT first_seen_ns, last_seen_ns, has_path, disappeared, title FROM sessions WHERE agent = 'codex' AND id = 's1'").Scan(&firstSeen, &lastSeen, &hasPath, &disappeared, &title); err != nil {
		t.Fatal(err)
	}
	if firstSeen != now.UnixNano() || lastSeen != now.Add(3*time.Hour).UnixNano() || hasPath != 0 || disappeared != 0 || title != "After" || dbRevision(t, db) != initialRevision+2 {
		t.Fatalf("reappeared metadata = first %d last %d path %d disappeared %d title %q revision %d", firstSeen, lastSeen, hasPath, disappeared, title, dbRevision(t, db))
	}

	fixture.setSessions()
	fixture.setSessionError(true)
	service.now = func() time.Time { return now.Add(4 * time.Hour) }
	if _, err := service.Sync(context.Background()); err == nil {
		t.Fatal("Sync() succeeded with failed session request")
	}
	if err := db.QueryRow("SELECT disappeared, title FROM sessions WHERE agent = 'codex' AND id = 's1'").Scan(&disappeared, &title); err != nil {
		t.Fatal(err)
	}
	if disappeared != 0 || title != "After" || dbRevision(t, db) != initialRevision+2 {
		t.Fatalf("failed full-session request changed metadata to disappeared=%d title=%q revision=%d", disappeared, title, dbRevision(t, db))
	}
}

func TestSyncSessionNicknameOnlyChangeIncrementsRevisionAndKeepsTitle(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	fixture := newSyncHTTPFixture(magpie.UsageSummary{Period: "all"})
	fixture.setSessions(magpie.Session{
		Agent: "codex", ID: "thread-1", Title: "Saved title", Nickname: "First nickname", Name: "Codex display", Last: now,
	})
	db, _ := openSyncTestDB(t)
	service := newSyncTestService(t, db, fixture, location, now)
	if _, err := service.syncSessions(context.Background()); err != nil {
		t.Fatalf("initial session sync: %v", err)
	}
	// Codex sessions only materialise in the dataset when a request row backs them.
	if _, err := db.Exec(`INSERT INTO requests (t_ns, local_day, agent, provider, model, session, session_role, input, output)
		VALUES (?, ?, 'codex', 'provider-a', 'gpt-5', 'thread-1', 'main', 10, 5)`, now.UnixNano(), now.Format(time.DateOnly)); err != nil {
		t.Fatalf("insert session request: %v", err)
	}
	initialRevision := dbRevision(t, db)

	fixture.setSessions(magpie.Session{
		Agent: "codex", ID: "thread-1", Title: "Saved title", Nickname: "Changed nickname", Name: "Codex display", Last: now,
	})
	if _, err := service.syncSessions(context.Background()); err != nil {
		t.Fatalf("nickname-only session sync: %v", err)
	}
	var title, nickname, agentName string
	if err := db.QueryRow("SELECT title, nickname, agent_name FROM sessions WHERE agent = 'codex' AND id = 'thread-1'").
		Scan(&title, &nickname, &agentName); err != nil {
		t.Fatal(err)
	}
	if title != "Saved title" || nickname != "Changed nickname" || agentName != "Codex display" || dbRevision(t, db) != initialRevision+1 {
		t.Fatalf("nickname-only sync stored title=%q nickname=%q agent=%q revision=%d; want saved title, changed nickname, distinct agent name, revision %d",
			title, nickname, agentName, dbRevision(t, db), initialRevision+1)
	}

	fixture.setSessions(magpie.Session{
		Agent: "codex", ID: "thread-1", Title: "Updated title", Nickname: "Changed nickname", Name: "Codex display", Last: now,
	})
	if _, err := service.syncSessions(context.Background()); err != nil {
		t.Fatalf("title update session sync: %v", err)
	}
	if err := db.QueryRow("SELECT title, nickname FROM sessions WHERE agent = 'codex' AND id = 'thread-1'").
		Scan(&title, &nickname); err != nil {
		t.Fatal(err)
	}
	if title != "Updated title" || nickname != "Changed nickname" || dbRevision(t, db) != initialRevision+2 {
		t.Fatalf("title update stored title=%q nickname=%q revision=%d; want updated title, preserved nickname, revision %d",
			title, nickname, dbRevision(t, db), initialRevision+2)
	}
	dataset, err := service.SessionDataset(context.Background(), SessionQuery{Range: DateRange{From: now.Format(time.DateOnly), To: now.Format(time.DateOnly)}})
	if err != nil {
		t.Fatalf("read session nickname from dataset: %v", err)
	}
	if len(dataset.Sessions) != 1 || dataset.Sessions[0].Title != "Updated title" || dataset.Sessions[0].Nickname != "Changed nickname" {
		t.Fatalf("session dataset title/nickname = %+v, want updated title and separate nickname", dataset.Sessions)
	}
}

func TestSyncCompactionTriStateRoundTrip(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	day := now.Format(time.DateOnly)
	trueValue, falseValue := true, false
	rows := []magpie.UsageRequest{
		{Time: now, Agent: "codex", Model: "gpt-5", Compaction: &trueValue, FirstReasoningMs: 3000, ReasoningHidden: true, HandedOn: true},
		{Time: now.Add(time.Second), Agent: "codex", Model: "gpt-5", Compaction: &falseValue, FirstReasoningMs: 4000},
		{Time: now.Add(2 * time.Second), Agent: "codex", Model: "gpt-5"},
	}
	db, _ := openSyncTestDB(t)
	service := newSyncTestService(t, db, newSyncHTTPFixture(magpie.UsageSummary{Period: "all"}), location, now)
	if err := service.saveSyncDay(context.Background(), day, [32]byte{}, rows, false); err != nil {
		t.Fatalf("save day: %v", err)
	}
	tx, err := db.BeginTx(context.Background(), &sql.TxOptions{ReadOnly: true})
	if err != nil {
		t.Fatal(err)
	}
	got, err := readDatasetRequests(context.Background(), tx, DateRange{From: day, To: day}, location)
	if err != nil {
		tx.Rollback()
		t.Fatalf("read saved day: %v", err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	if len(got) != 3 {
		t.Fatalf("read %d requests, want 3", len(got))
	}
	want := []*bool{&trueValue, &falseValue, nil}
	for index := range want {
		if !sameQueryBoolPointer(got[index].Compaction, want[index]) {
			t.Fatalf("request %d compaction = %v, want %v", index, got[index].Compaction, want[index])
		}
		row := usageRowOf(got[index], "codex", "thread-1", ProjectRef{}, true)
		if !sameQueryBoolPointer(row.Compaction, want[index]) {
			t.Fatalf("usage row %d compaction = %v, want %v", index, row.Compaction, want[index])
		}
		if row.FirstReasoningMs != rows[index].FirstReasoningMs || row.ReasoningHidden != rows[index].ReasoningHidden || row.HandedOn != rows[index].HandedOn {
			t.Fatalf("usage row %d request metric fields = reasoning start %d, hidden %t, handed on %t; want %d/%t/%t",
				index, row.FirstReasoningMs, row.ReasoningHidden, row.HandedOn,
				rows[index].FirstReasoningMs, rows[index].ReasoningHidden, rows[index].HandedOn)
		}
	}
}

func sameQueryBoolPointer(a, b *bool) bool {
	return a == nil && b == nil || a != nil && b != nil && *a == *b
}

func TestSyncBackfillsFrozenCompactionDayRetriesAcrossReopenAndCompletesOnce(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	const oldDay = "2026-10-04"
	today := now.Format(time.DateOnly)
	oldPoint := digestPoint(oldDay, location, 1, 7)
	todayPoint := digestPoint(today, location, 1, 1)
	fixture := newSyncHTTPFixture(magpie.UsageSummary{
		Period: "all", Series: []magpie.UsagePoint{{Label: today, Time: todayPoint.Time, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 1}}},
		UsageTotals: magpie.UsageTotals{Calls: 1, Input: 1},
	})
	fixture.setSessions(magpie.Session{Agent: "codex", ID: "thread-1", Role: "main"})
	fixture.digests[oldDay], fixture.digests[today] = oldPoint, todayPoint
	compaction := true
	upstreamRow := magpie.UsageRequest{
		Time: oldPoint.Time, Agent: "codex", Provider: "openai", Model: "gpt-5", RID: "rid-old", ResponseID: "response-old",
		Status: http.StatusOK, Input: 7, Output: 3, Cost: 0.125, Priced: true, Compaction: &compaction,
		FirstReasoningMs: 2400, ReasoningHidden: true, HandedOn: true,
	}
	fixture.requests[oldDay] = []magpie.UsageRequest{upstreamRow}
	db, path := openSyncTestDB(t)
	setInitialSyncComplete(t, db)
	seedDay(t, db, oldDay, digestBytes(t, oldPoint), now.Add(-time.Hour).UnixNano(), true)
	if _, err := db.Exec("UPDATE sync_days SET row_count = 1, identity_backfill_pending = 1, compaction_backfill_pending = 1, request_metrics_backfill_pending = 1 WHERE day = ?", oldDay); err != nil {
		t.Fatal(err)
	}
	seedDay(t, db, today, digestBytes(t, todayPoint), now.Add(-time.Hour).UnixNano(), false)
	if _, err := db.Exec(`INSERT INTO requests (t_ns, local_day, agent, provider, model, rid, response_id, status, input, output, magpie_cost, magpie_priced)
		VALUES (?, ?, 'codex', 'openai', 'gpt-5', 'rid-old', 'response-old', ?, 7, 3, 0.125, 1)`,
		oldPoint.Time.UnixNano(), oldDay, http.StatusOK); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(`CREATE TRIGGER fail_compaction_replace BEFORE INSERT ON requests
		WHEN NEW.local_day = '2026-10-04'
		BEGIN SELECT RAISE(ABORT, 'fixture replacement failure'); END`); err != nil {
		t.Fatal(err)
	}
	service := newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err == nil {
		t.Fatal("Sync succeeded despite transactional replacement failure")
	}
	var model string
	var input int
	var output int
	var cost float64
	var priced, identityPending, pending, requestMetricsPending, frozen int
	var compactionValue sql.NullInt64
	var handedOn, firstReasoning, reasoningHidden int
	if err := db.QueryRow(`SELECT r.model, r.input, r.output, r.magpie_cost, r.magpie_priced, r.compaction,
		r.handed_on, r.first_reasoning_ms, r.reasoning_hidden,
		d.identity_backfill_pending, d.compaction_backfill_pending, d.request_metrics_backfill_pending, d.frozen
		FROM requests r JOIN sync_days d ON d.day = r.local_day WHERE r.local_day = ?`, oldDay).
		Scan(&model, &input, &output, &cost, &priced, &compactionValue, &handedOn, &firstReasoning, &reasoningHidden,
			&identityPending, &pending, &requestMetricsPending, &frozen); err != nil {
		t.Fatal(err)
	}
	if model != "gpt-5" || input != 7 || output != 3 || cost != 0.125 || priced != 1 || compactionValue.Valid || handedOn != 0 || firstReasoning != 0 || reasoningHidden != 0 || identityPending != 1 || pending != 1 || requestMetricsPending != 1 || frozen != 1 {
		t.Fatalf("failed replacement changed cached day: model=%q input=%d output=%d cost=%.3f priced=%d compaction=%v fields=%d/%d/%d pending=%d/%d/%d frozen=%d",
			model, input, output, cost, priced, compactionValue, handedOn, firstReasoning, reasoningHidden, identityPending, pending, requestMetricsPending, frozen)
	}
	if _, err := db.Exec("DROP TRIGGER fail_compaction_replace"); err != nil {
		t.Fatal(err)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
	db, err := database.Open(path)
	if err != nil {
		t.Fatalf("reopen pending migrated database: %v", err)
	}
	defer db.Close()
	service = newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("retry compaction backfill after reopen: %v", err)
	}
	if err := db.QueryRow(`SELECT r.model, r.input, r.output, r.magpie_cost, r.magpie_priced, r.compaction,
		r.handed_on, r.first_reasoning_ms, r.reasoning_hidden,
		d.identity_backfill_pending, d.compaction_backfill_pending, d.request_metrics_backfill_pending, d.frozen
		FROM requests r JOIN sync_days d ON d.day = r.local_day WHERE r.local_day = ?`, oldDay).
		Scan(&model, &input, &output, &cost, &priced, &compactionValue, &handedOn, &firstReasoning, &reasoningHidden,
			&identityPending, &pending, &requestMetricsPending, &frozen); err != nil {
		t.Fatal(err)
	}
	if model != "gpt-5" || input != 7 || output != 3 || cost != 0.125 || priced != 1 || !compactionValue.Valid || compactionValue.Int64 != 1 || handedOn != 1 || firstReasoning != 2400 || reasoningHidden != 1 || identityPending != 0 || pending != 0 || requestMetricsPending != 0 || frozen != 1 {
		t.Fatalf("successful backfill state: model=%q input=%d output=%d cost=%.3f priced=%d compaction=%v fields=%d/%d/%d pending=%d/%d/%d frozen=%d",
			model, input, output, cost, priced, compactionValue, handedOn, firstReasoning, reasoningHidden, identityPending, pending, requestMetricsPending, frozen)
	}
	var rowCount int
	if err := db.QueryRow("SELECT COUNT(*) FROM requests WHERE local_day = ?", oldDay).Scan(&rowCount); err != nil || rowCount != 1 {
		t.Fatalf("request count after backfill = %d, %v; want one source row", rowCount, err)
	}
	pageCalls := countPageCalls(fixture.calls())
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
	db, err = database.Open(path)
	if err != nil {
		t.Fatalf("reopen completed database: %v", err)
	}
	defer db.Close()
	service = newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("Sync after completed backfill: %v", err)
	}
	if got := countPageCalls(fixture.calls()); got != pageCalls {
		t.Fatalf("completed backfill made %d repeat full-day requests", got-pageCalls)
	}
}

func TestSyncBackfillsFrozenDaysRetriesAndRefreshesChangedIdentity(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	today := now.Format(time.DateOnly)
	days, err := daysBetween("2026-10-06", today)
	if err != nil {
		t.Fatal(err)
	}
	fixture := newSyncHTTPFixture(magpie.UsageSummary{Period: "all"})
	for index, day := range days {
		point := digestPoint(day, location, 1, index+1)
		fixture.digests[day] = point
		fixture.summary.Series = append(fixture.summary.Series, magpie.UsagePoint{
			Label: day, Time: point.Time, UsageTotals: magpie.UsageTotals{Calls: 1, Input: index + 1},
		})
		row := magpie.UsageRequest{
			Time: point.Time, Agent: "codex", Model: "fetched", Input: 2, Status: http.StatusOK,
			Session: fmt.Sprintf("thread-%d", index), SessionRole: "main", Cost: 0.125, Priced: true,
		}
		if index == 0 {
			row.Session, row.NativeSession, row.Subagent, row.ParentAgent = "root-a", "root-a", "child-a", "parent-a"
			row.SessionRole = "subagent"
		} else if index == 2 {
			row.Session, row.NativeSession = "root-a", "root-a"
		}
		fixture.requests[day] = []magpie.UsageRequest{row}
	}
	fixture.summary.UsageTotals.Calls = len(days)
	fixture.setSessions(
		magpie.Session{Agent: "codex", ID: "root-a", Role: "main"},
		magpie.Session{Agent: "codex", ID: "child-a", Role: "subagent", ParentSession: "parent-a", RootSession: "root-a"},
	)
	failedDay := days[1]
	fixture.setPageFailure(failedDay, 0, true)
	db, _ := openSyncTestDB(t)
	setInitialSyncComplete(t, db)
	cutoff := now.AddDate(0, 0, -7).Format(time.DateOnly)
	for _, day := range days {
		point := fixture.digests[day]
		if _, err := db.Exec(`INSERT INTO sync_days (day, fingerprint, row_count, last_sync_ns, frozen)
			VALUES (?, ?, 1, ?, ?)`, day, digestBytes(t, point), now.Add(-time.Hour).UnixNano(), boolInt(day < cutoff)); err != nil {
			t.Fatalf("seed sync day %s: %v", day, err)
		}
		identitySession, identityNative := "thread", ""
		if day == days[0] || day == days[2] {
			identitySession, identityNative = "root-a", "root-a"
		}
		if _, err := db.Exec(`INSERT INTO requests (
			t_ns, local_day, agent, model, input, session, native_session, magpie_cost, magpie_priced
		) VALUES (?, ?, 'codex', 'baseline', 1, ?, ?, 0.75, 1)`, fixture.digests[day].Time.UnixNano(), day,
			identitySession, identityNative); err != nil {
			t.Fatalf("seed request %s: %v", day, err)
		}
	}
	service := newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err == nil {
		t.Fatal("first identity backfill succeeded despite one failed day")
	}
	var savedSessionRole, savedSessionParent, savedSessionRoot string
	if err := db.QueryRow(`SELECT role, parent_session, root_session FROM sessions
		WHERE agent = 'codex' AND id = 'child-a'`).Scan(&savedSessionRole, &savedSessionParent, &savedSessionRoot); err != nil {
		t.Fatal(err)
	}
	if savedSessionRole != "subagent" || savedSessionParent != "parent-a" || savedSessionRoot != "root-a" {
		t.Fatalf("saved session identity = %q/%q/%q", savedSessionRole, savedSessionParent, savedSessionRoot)
	}
	var started, complete string
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'session_identity_backfill_started'").Scan(&started); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'session_identity_backfill_complete'").Scan(&complete); err != nil {
		t.Fatal(err)
	}
	var pending int
	if err := db.QueryRow("SELECT COUNT(*) FROM sync_days WHERE identity_backfill_pending = 1").Scan(&pending); err != nil {
		t.Fatal(err)
	}
	if started != "1" || complete != "0" || pending != 1 {
		t.Fatalf("failed backfill state = started %q complete %q pending %d", started, complete, pending)
	}
	var oldModel string
	var oldCost float64
	var oldPriced int
	if err := db.QueryRow("SELECT model, magpie_cost, magpie_priced FROM requests WHERE local_day = ?", failedDay).Scan(&oldModel, &oldCost, &oldPriced); err != nil {
		t.Fatal(err)
	}
	if oldModel != "baseline" || oldCost != 0.75 || oldPriced != 1 {
		t.Fatalf("failed day lost prior data: model %q cost %.3f priced %d", oldModel, oldCost, oldPriced)
	}
	var frozen, refreshedPending int
	if err := db.QueryRow("SELECT frozen, identity_backfill_pending FROM sync_days WHERE day = ?", days[0]).Scan(&frozen, &refreshedPending); err != nil {
		t.Fatal(err)
	}
	var fetchedModel, fetchedRole string
	if err := db.QueryRow("SELECT model, session_role FROM requests WHERE local_day = ?", days[0]).Scan(&fetchedModel, &fetchedRole); err != nil {
		t.Fatal(err)
	}
	if frozen != 1 || refreshedPending != 0 || fetchedModel != "fetched" || fetchedRole != "subagent" {
		t.Fatalf("frozen history refresh = frozen %d pending %d model %q role %q", frozen, refreshedPending, fetchedModel, fetchedRole)
	}

	firstPages := countPageCalls(fixture.calls())
	fixture.setPageFailure(failedDay, 0, false)
	service = newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("retry identity backfill: %v", err)
	}
	if got := countPageCalls(fixture.calls()); got != firstPages+1 {
		t.Fatalf("retry made %d full-day requests, want only the failed day", got-firstPages)
	}
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'session_identity_backfill_complete'").Scan(&complete); err != nil || complete != "1" {
		t.Fatalf("backfill completion = %q, %v; want 1", complete, err)
	}
	if err := db.QueryRow("SELECT COUNT(*) FROM sync_days WHERE identity_backfill_pending = 1").Scan(&pending); err != nil || pending != 0 {
		t.Fatalf("pending days after retry = %d, %v; want 0", pending, err)
	}

	child := magpie.Session{Agent: "codex", ID: "child-a", Role: "subagent", ParentSession: "parent-b", RootSession: "root-a"}
	fixture.setSessions(magpie.Session{Agent: "codex", ID: "root-a", Role: "main"}, child)
	changed := fixture.requests[days[0]][0]
	changed.ParentAgent = "parent-b"
	fixture.requests[days[0]] = []magpie.UsageRequest{changed}
	identityRevision := dbRevision(t, db)
	identityPages := countPageCalls(fixture.calls())
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("refresh changed identity day: %v", err)
	}
	if got := dbRevision(t, db); got <= identityRevision {
		t.Fatalf("identity change did not refresh data revision: before %d, after %d", identityRevision, got)
	}
	if got := countPageCalls(fixture.calls()); got != identityPages+2 {
		t.Fatalf("identity change made %d full-day requests, want both root-linked days", got-identityPages)
	}
	var savedParent string
	if err := db.QueryRow("SELECT parent_agent FROM requests WHERE local_day = ?", days[0]).Scan(&savedParent); err != nil || savedParent != "parent-b" {
		t.Fatalf("changed parent identity = %q, %v", savedParent, err)
	}
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'session_identity_backfill_complete'").Scan(&complete); err != nil || complete != "1" {
		t.Fatalf("completion after identity change = %q, %v; want 1", complete, err)
	}
	changedPages := countPageCalls(fixture.calls())
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("unchanged identity Sync: %v", err)
	}
	if got := countPageCalls(fixture.calls()); got != changedPages {
		t.Fatalf("unchanged identity caused %d additional historical requests", got-changedPages)
	}

	var oldSession, oldNative, oldSubagent, oldParent string
	if err := db.QueryRow(`SELECT session, native_session, subagent, parent_agent FROM requests WHERE local_day = ?`, days[2]).
		Scan(&oldSession, &oldNative, &oldSubagent, &oldParent); err != nil {
		t.Fatal(err)
	}
	if oldSession != "root-a" || oldNative != "root-a" || oldSubagent != "" || oldParent != "" {
		t.Fatalf("request before new child appears = %q/%q/%q/%q", oldSession, oldNative, oldSubagent, oldParent)
	}
	childB := magpie.Session{Agent: "codex", ID: "child-b", Role: "subagent", ParentSession: "root-a", RootSession: "root-a"}
	fixture.setSessions(magpie.Session{Agent: "codex", ID: "root-a", Role: "main"}, child, childB)
	newChildRequest := fixture.requests[days[2]][0]
	newChildRequest.SessionRole, newChildRequest.Subagent = "subagent", "child-b"
	fixture.requests[days[2]] = []magpie.UsageRequest{newChildRequest}
	newChildPages := countPageCalls(fixture.calls())
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("refresh history for new child: %v", err)
	}
	if got := countPageCalls(fixture.calls()); got != newChildPages+2 {
		t.Fatalf("new child identity made %d full-day requests, want both root-linked days", got-newChildPages)
	}
	var savedChild, savedRole string
	if err := db.QueryRow("SELECT subagent, session_role FROM requests WHERE local_day = ?", days[2]).Scan(&savedChild, &savedRole); err != nil {
		t.Fatal(err)
	}
	if savedChild != "child-b" || savedRole != "subagent" {
		t.Fatalf("new child request identity = %q/%q", savedChild, savedRole)
	}
	newChildSyncedPages := countPageCalls(fixture.calls())
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("unchanged new child identity Sync: %v", err)
	}
	if got := countPageCalls(fixture.calls()); got != newChildSyncedPages {
		t.Fatalf("unchanged new child identity caused %d additional historical requests", got-newChildSyncedPages)
	}
}

func TestSyncOldUpstreamPreservesStoredSessionIdentityAndDoesNotStartBackfill(t *testing.T) {
	location := time.UTC
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	day := now.Format(time.DateOnly)
	point := digestPoint(day, location, 1, 1)
	fixture := newSyncHTTPFixture(magpie.UsageSummary{
		Period: "all", Series: []magpie.UsagePoint{{Label: day, Time: point.Time, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 1}}},
		UsageTotals: magpie.UsageTotals{Calls: 1, Input: 1},
	})
	fixture.digests[day] = point
	fixture.setSessions(magpie.Session{Agent: "codex", ID: "thread-1", Cwd: "/repo"})
	db, _ := openSyncTestDB(t)
	setInitialSyncComplete(t, db)
	if _, err := db.Exec(`INSERT INTO sessions (
		agent, id, role, parent_session, root_session, first_seen_ns, last_seen_ns
	) VALUES ('codex', 'thread-1', 'subagent', 'parent-1', 'root-1', ?, ?)`, now.UnixNano(), now.UnixNano()); err != nil {
		t.Fatal(err)
	}
	service := newSyncTestService(t, db, fixture, location, now)
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("Sync against old upstream: %v", err)
	}
	var role, parent, root, started, complete string
	if err := db.QueryRow("SELECT role, parent_session, root_session FROM sessions WHERE agent = 'codex' AND id = 'thread-1'").Scan(&role, &parent, &root); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'session_identity_backfill_started'").Scan(&started); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'session_identity_backfill_complete'").Scan(&complete); err != nil {
		t.Fatal(err)
	}
	if role != "subagent" || parent != "parent-1" || root != "root-1" || started != "0" || complete != "0" {
		t.Fatalf("old upstream state: identity %q/%q/%q backfill %q/%q", role, parent, root, started, complete)
	}
}

func TestSyncInitialCompletionContinuesAndSurvivesRestart(t *testing.T) {
	location := time.FixedZone("UTC+8", 8*60*60)
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, location)
	firstDay := now.AddDate(0, 0, -1).Format(time.DateOnly)
	today := now.Format(time.DateOnly)
	firstPoint := digestPoint(firstDay, location, 1, 1)
	todayPoint := digestPoint(today, location, 1, 2)
	summary := magpie.UsageSummary{Period: "all", Series: []magpie.UsagePoint{
		{Label: firstDay, Time: firstPoint.Time, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 1}},
		{Label: today, Time: todayPoint.Time, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 2}},
	}, UsageTotals: magpie.UsageTotals{Calls: 2, Input: 3}}
	fixture := newSyncHTTPFixture(summary)
	fixture.digests[firstDay], fixture.digests[today] = firstPoint, todayPoint
	fixture.requests[firstDay] = []magpie.UsageRequest{{Time: firstPoint.Time, Agent: "codex", Model: "old", Input: 1}}
	fixture.requests[today] = []magpie.UsageRequest{{Time: todayPoint.Time, Agent: "codex", Model: "today", Input: 2}}
	fixture.setPageFailure(firstDay, 0, true)
	path := filepath.Join(t.TempDir(), "persistent.sqlite")
	db, err := database.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	httpServer := httptest.NewServer(fixture)
	defer httpServer.Close()
	service := newService(db, magpie.NewClient(httpServer.URL, httpServer.Client()), location)
	service.now = func() time.Time { return now }
	if _, err := service.Sync(context.Background()); err == nil {
		t.Fatal("first incomplete date pass returned no error")
	}
	complete, err := service.InitialSyncComplete(context.Background())
	if err != nil || complete {
		t.Fatalf("completion after a failed day = %t, %v; want false", complete, err)
	}
	var completedToday int
	if err := db.QueryRow("SELECT COUNT(*) FROM sync_days WHERE day = ? AND last_sync_ns IS NOT NULL", today).Scan(&completedToday); err != nil || completedToday != 1 {
		t.Fatalf("independently completed today row = %d, %v; want 1", completedToday, err)
	}
	fixture.setPageFailure(firstDay, 0, false)
	if _, err := service.Sync(context.Background()); err != nil {
		t.Fatalf("continued first pass error = %v", err)
	}
	complete, err = service.InitialSyncComplete(context.Background())
	if err != nil || !complete {
		t.Fatalf("completion after all dates = %t, %v; want true", complete, err)
	}
	callCount := countPageCalls(fixture.calls())
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
	db, err = database.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	restarted := newService(db, magpie.NewClient(httpServer.URL, httpServer.Client()), location)
	restarted.now = func() time.Time { return now }
	complete, err = restarted.InitialSyncComplete(context.Background())
	if err != nil || !complete {
		t.Fatalf("persisted completion after restart = %t, %v", complete, err)
	}
	if _, err := restarted.Sync(context.Background()); err != nil {
		t.Fatalf("restart Sync() error = %v", err)
	}
	if got := countPageCalls(fixture.calls()); got != callCount {
		t.Fatalf("restart made %d additional request-page calls, want no historical re-fetch", got-callCount)
	}
	var count int
	if err := db.QueryRow("SELECT COUNT(*) FROM requests WHERE local_day IN (?, ?)", firstDay, today).Scan(&count); err != nil || count != 2 {
		t.Fatalf("persisted daily requests after restart = %d, %v; want 2", count, err)
	}
}
