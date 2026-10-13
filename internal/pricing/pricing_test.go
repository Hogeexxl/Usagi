package pricing

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/database"
)

type priceRoundTripper func(*http.Request) (*http.Response, error)

func (f priceRoundTripper) RoundTrip(request *http.Request) (*http.Response, error) {
	return f(request)
}

func fixtureClient(t *testing.T, status int) *http.Client {
	t.Helper()
	contents, err := os.ReadFile(filepath.Join("testdata", "catalog.json"))
	if err != nil {
		t.Fatalf("read catalog fixture: %v", err)
	}
	return fixtureClientWithBody(t, status, contents)
}

func fixtureClientWithBody(t *testing.T, status int, contents []byte) *http.Client {
	t.Helper()
	return &http.Client{Transport: priceRoundTripper(func(request *http.Request) (*http.Response, error) {
		body := contents
		if status != http.StatusOK {
			body = nil
		}
		return &http.Response{
			StatusCode: status,
			Header:     make(http.Header),
			Body:       io.NopCloser(bytes.NewReader(body)),
			Request:    request,
		}, nil
	})}
}

func pricingTestDB(t *testing.T) *sql.DB {
	t.Helper()
	_, db := openPricingTestDB(t)
	return db
}

func openPricingTestDB(t *testing.T) (string, *sql.DB) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "pricing.sqlite")
	db, err := database.Open(path)
	if err != nil {
		t.Fatalf("open pricing database %q: %v", path, err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return path, db
}

type ledgerPriceCase struct {
	Provider       string `json:"provider"`
	Model          string `json:"model"`
	Served         string `json:"served"`
	ExpectedPriced bool   `json:"expected_priced"`
	QueryModel     string `json:"query_model"`
	CatalogID      string `json:"catalog_id"`
	ModelKey       string `json:"model_key"`
	Price          *Price `json:"price"`
}

type priceLookupCase struct {
	Name           string `json:"name"`
	Provider       string `json:"provider"`
	Model          string `json:"model"`
	Served         string `json:"served"`
	Requested      string `json:"requested"`
	ExpectedPriced bool   `json:"expected_priced"`
	ExpectedPrice  *Price `json:"expected_price"`
}

type ledgerPriceFixture struct {
	ProviderCatalogs       map[string][]string        `json:"provider_catalogs"`
	LookupProviderCatalogs map[string][]string        `json:"lookup_provider_catalogs"`
	LookupCatalog          map[string]json.RawMessage `json:"lookup_catalog"`
	Cases                  []ledgerPriceCase          `json:"cases"`
	LookupCases            []priceLookupCase          `json:"lookup_cases"`
}

func readLedgerPriceFixture(t *testing.T) ledgerPriceFixture {
	t.Helper()
	contents, err := os.ReadFile(filepath.Join("testdata", "ledger_price_cases.json"))
	if err != nil {
		t.Fatalf("read ledger price fixture: %v", err)
	}
	var fixture ledgerPriceFixture
	if err := json.Unmarshal(contents, &fixture); err != nil {
		t.Fatalf("decode ledger price fixture: %v", err)
	}
	return fixture
}

func lookupFixtureClient(t *testing.T, status int, fixture ledgerPriceFixture) *http.Client {
	t.Helper()
	contents, err := json.Marshal(fixture.LookupCatalog)
	if err != nil {
		t.Fatalf("encode lookup catalog fixture: %v", err)
	}
	return fixtureClientWithBody(t, status, contents)
}

func TestPriceBootstrapFailureLeavesNoVersion(t *testing.T) {
	db := pricingTestDB(t)
	created, err := Bootstrap(context.Background(), db, fixtureClient(t, http.StatusServiceUnavailable), nil, time.Now())
	if err == nil || created {
		t.Fatalf("Bootstrap = (%t, %v), want failure without a version", created, err)
	}
	var versions, revision int
	if err := db.QueryRow("SELECT COUNT(*) FROM price_versions").Scan(&versions); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'data_revision'").Scan(&revision); err != nil {
		t.Fatal(err)
	}
	if versions != 0 || revision != 0 {
		t.Fatalf("after failure versions=%d revision=%d, want 0/0", versions, revision)
	}
}

func TestPriceBootstrapSavesSnapshotAndPrices(t *testing.T) {
	dbPath, db := openPricingTestDB(t)
	now := time.Date(2026, 10, 14, 8, 0, 0, 0, time.UTC)
	providerCatalogs := map[string][]string{"provider-a": {"openai"}, "provider-empty": {}}
	created, err := Bootstrap(context.Background(), db, fixtureClient(t, http.StatusOK), providerCatalogs, now)
	if err != nil || !created {
		t.Fatalf("Bootstrap = (%t, %v), want created version", created, err)
	}
	var source string
	var effective int64
	var providerJSON, makerJSON string
	if err := db.QueryRow(`SELECT source, effective_at_ns, provider_catalogs_json, maker_catalogs_json
		FROM price_versions`).Scan(&source, &effective, &providerJSON, &makerJSON); err != nil {
		t.Fatal(err)
	}
	if source != modelsDevURL || effective != now.UnixNano() {
		t.Fatalf("version source/effective = %q/%d", source, effective)
	}
	var gotProviders map[string][]string
	var gotMakers []string
	if err := json.Unmarshal([]byte(providerJSON), &gotProviders); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal([]byte(makerJSON), &gotMakers); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(gotProviders, providerCatalogs) || !reflect.DeepEqual(gotMakers, makerCatalogs) {
		t.Fatalf("snapshots providers=%v makers=%v", gotProviders, gotMakers)
	}
	var canonical string
	var input, output, cacheRead, cacheWrite, cacheWrite1h sql.NullFloat64
	if err := db.QueryRow(`SELECT canonical_model_id, input, output, cache_read, cache_write, cache_write_1h
		FROM prices WHERE catalog_id = 'openai' AND model_key = 'gpt-6-luna'`).
		Scan(&canonical, &input, &output, &cacheRead, &cacheWrite, &cacheWrite1h); err != nil {
		t.Fatal(err)
	}
	if canonical != "openai/gpt-6-luna" || !input.Valid || input.Float64 != 0.1 ||
		!output.Valid || output.Float64 != 0.5 || !cacheRead.Valid || cacheRead.Float64 != 0.01 ||
		!cacheWrite.Valid || cacheWrite.Float64 != 0.125 || !cacheWrite1h.Valid || cacheWrite1h.Float64 != 0.125 {
		t.Fatalf("models.dev model canonical/five prices = %q/%+v/%+v/%+v/%+v/%+v", canonical, input, output, cacheRead, cacheWrite, cacheWrite1h)
	}
	var noPriceRows int
	if err := db.QueryRow(`SELECT COUNT(*) FROM prices WHERE input IS NULL AND output IS NULL
		AND cache_read IS NULL AND cache_write IS NULL AND cache_write_1h IS NULL`).
		Scan(&noPriceRows); err != nil {
		t.Fatal(err)
	}
	if noPriceRows == 0 {
		t.Fatal("models.dev cost-missing rows were not saved with all prices NULL")
	}
	catalogBytes, err := os.ReadFile(filepath.Join("testdata", "catalog.json"))
	if err != nil {
		t.Fatal(err)
	}
	var catalogSource map[string]struct {
		Models map[string]json.RawMessage `json:"models"`
	}
	if err := json.Unmarshal(catalogBytes, &catalogSource); err != nil {
		t.Fatal(err)
	}
	wantModelRows := 0
	for _, provider := range catalogSource {
		wantModelRows += len(provider.Models)
	}
	var gotModelRows int
	if err := db.QueryRow("SELECT COUNT(*) FROM prices").Scan(&gotModelRows); err != nil {
		t.Fatal(err)
	}
	if gotModelRows != wantModelRows {
		t.Fatalf("saved model rows = %d, fixture contains %d", gotModelRows, wantModelRows)
	}
	var revision string
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'data_revision'").Scan(&revision); err != nil {
		t.Fatal(err)
	}
	if revision != "1" {
		t.Fatalf("data_revision = %q, want 1", revision)
	}

	if err := db.Close(); err != nil {
		t.Fatalf("close database before reopen: %v", err)
	}
	reopened, err := database.Open(dbPath)
	if err != nil {
		t.Fatalf("reopen pricing database: %v", err)
	}
	t.Cleanup(func() { _ = reopened.Close() })
	tx, err := reopened.BeginTx(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	catalog, err := Load(context.Background(), tx)
	if err != nil {
		_ = tx.Rollback()
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	price, priced := catalog.PriceAt(now, "provider-a", "gpt-6-luna", "")
	if !priced || price.Input != 0.1 {
		t.Fatalf("reopened catalog price = (%+v, %t), want openai input 0.1", price, priced)
	}
	called := false
	noDownload := &http.Client{Transport: priceRoundTripper(func(*http.Request) (*http.Response, error) {
		called = true
		return nil, io.EOF
	})}
	created, err = Bootstrap(context.Background(), reopened, noDownload, nil, now.Add(time.Hour))
	if err != nil || created || called {
		t.Fatalf("second Bootstrap = (%t, %v), download called=%t", created, err, called)
	}
}

func TestPriceBootstrapOneHourForClaudeModel(t *testing.T) {
	db := pricingTestDB(t)
	body := []byte(`{"anthropic":{"models":{"claude-haiku-5-5":{"cost":{"input":0.1,"output":0.5,"cache_read":0.01,"cache_write":0.125}}}}}`)
	client := fixtureClientWithBody(t, http.StatusOK, body)
	if created, err := Bootstrap(context.Background(), db, client, nil, time.Now()); err != nil || !created {
		t.Fatalf("Bootstrap = (%t, %v), want created version", created, err)
	}
	var cacheWrite, cacheWrite1h float64
	if err := db.QueryRow(`SELECT cache_write, cache_write_1h FROM prices
		WHERE catalog_id = 'anthropic' AND model_key = 'claude-haiku-5-5'`).Scan(&cacheWrite, &cacheWrite1h); err != nil {
		t.Fatal(err)
	}
	if cacheWrite != 0.125 || cacheWrite1h != 0.2 {
		t.Fatalf("Claude cache write prices = %v/%v, want 0.125/0.2", cacheWrite, cacheWrite1h)
	}
}

func TestPriceLookup(t *testing.T) {
	fixture := readLedgerPriceFixture(t)
	db := pricingTestDB(t)
	now := time.Date(2026, 10, 14, 8, 0, 0, 0, time.UTC)
	_, err := Bootstrap(context.Background(), db, fixtureClient(t, http.StatusOK), fixture.ProviderCatalogs, now)
	if err != nil {
		t.Fatal(err)
	}
	tx, err := db.BeginTx(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	catalog, err := Load(context.Background(), tx)
	if err != nil {
		_ = tx.Rollback()
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	if len(fixture.Cases) == 0 {
		t.Fatal("ledger price fixture has no actual provider/model/served combinations")
	}
	for _, test := range fixture.Cases {
		price, priced := catalog.PriceAt(now, test.Provider, test.Model, test.Served)
		if priced != test.ExpectedPriced {
			t.Errorf("PriceAt(%q, %q, %q) priced=%t, want %t (catalog=%q model_key=%q)",
				test.Provider, test.Model, test.Served, priced, test.ExpectedPriced, test.CatalogID, test.ModelKey)
			continue
		}
		if priced && test.Price != nil && price != *test.Price {
			t.Errorf("PriceAt(%q, %q, %q) = %+v, want %+v from %s/%s (%s)",
				test.Provider, test.Model, test.Served, price, *test.Price, test.CatalogID, test.ModelKey, test.QueryModel)
		}
	}
	guardDB := pricingTestDB(t)
	if _, err := Bootstrap(context.Background(), guardDB, lookupFixtureClient(t, http.StatusOK, fixture), fixture.LookupProviderCatalogs, now); err != nil {
		t.Fatal(err)
	}
	tx, err = guardDB.BeginTx(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	guardCatalog, err := Load(context.Background(), tx)
	if err != nil {
		_ = tx.Rollback()
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	for _, test := range fixture.LookupCases {
		price, priced := guardCatalog.PriceAt(now, test.Provider, test.Model, test.Served)
		if priced != test.ExpectedPriced {
			t.Errorf("lookup case %q priced=%t, want %t", test.Name, priced, test.ExpectedPriced)
			continue
		}
		if priced && test.ExpectedPrice != nil && price != *test.ExpectedPrice {
			t.Errorf("lookup case %q = %+v, want %+v", test.Name, price, *test.ExpectedPrice)
		}
	}
}

func TestPriceVersion(t *testing.T) {
	db := pricingTestDB(t)
	first := time.Date(2026, 10, 1, 0, 0, 0, 0, time.UTC)
	second := first.Add(24 * time.Hour)
	if _, err := Bootstrap(context.Background(), db, fixtureClient(t, http.StatusOK), map[string][]string{"provider-a": {}}, first); err != nil {
		t.Fatal(err)
	}
	tx, err := db.BeginTx(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	result, err := tx.Exec(`INSERT INTO price_versions (source, fetched_at_ns, effective_at_ns, provider_catalogs_json, maker_catalogs_json)
		VALUES (?, ?, ?, '{"provider-a":[]}', '["openai"]')`, modelsDevURL, second.UnixNano(), second.UnixNano())
	if err != nil {
		_ = tx.Rollback()
		t.Fatal(err)
	}
	versionID, err := result.LastInsertId()
	if err != nil {
		_ = tx.Rollback()
		t.Fatal(err)
	}
	if _, err := tx.Exec(`INSERT INTO prices (version_id, catalog_id, model_key, canonical_model_id,
		input, output, cache_read, cache_write, cache_write_1h) VALUES (?, 'openai', 'gpt-6-luna',
		'openai/gpt-6-luna', 10, 20, 30, 40, 40)`, versionID); err != nil {
		_ = tx.Rollback()
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	tx, err = db.BeginTx(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	catalog, err := Load(context.Background(), tx)
	if err != nil {
		_ = tx.Rollback()
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	for _, test := range []struct {
		at   time.Time
		want float64
	}{
		{at: first.Add(-time.Second), want: 0.1},
		{at: first, want: 0.1},
		{at: second.Add(-time.Nanosecond), want: 0.1},
		{at: second, want: 10},
	} {
		price, priced := catalog.PriceAt(test.at, "provider-a", "gpt-6-luna", "")
		if !priced || price.Input != test.want {
			t.Errorf("PriceAt(%s) = (%+v, %t), want input %v", test.at, price, priced, test.want)
		}
	}
}

func TestPriceCost(t *testing.T) {
	price := Price{Input: 1, Output: 2, CacheRead: 3, CacheWrite: 4, CacheWrite1h: 5}
	got := price.Cost(100, 50, 20, 30, 10)
	if got != 0.00039 {
		t.Fatalf("Cost = %.12f, want 0.00039", got)
	}
}

func TestPriceCostIgnoresTiers(t *testing.T) {
	db := pricingTestDB(t)
	body := []byte(`{"openai":{"models":{"tier-fixture":{"cost":{"input":1,"output":2,"cache_read":3,"cache_write":4,"cache_write_1h":5,"tiers":[{"above":1,"input":100,"output":200}]}}}}}`)
	client := &http.Client{Transport: priceRoundTripper(func(request *http.Request) (*http.Response, error) {
		return &http.Response{StatusCode: http.StatusOK, Header: make(http.Header), Body: io.NopCloser(bytes.NewReader(body)), Request: request}, nil
	})}
	now := time.Date(2026, 10, 14, 8, 0, 0, 0, time.UTC)
	if created, err := Bootstrap(context.Background(), db, client, nil, now); err != nil || !created {
		t.Fatalf("Bootstrap with a tier = (%t, %v), want created version", created, err)
	}
	tx, err := db.BeginTx(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	catalog, err := Load(context.Background(), tx)
	if err != nil {
		_ = tx.Rollback()
		t.Fatal(err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
	price, priced := catalog.PriceAt(now, "unknown-provider", "tier-fixture", "")
	if !priced || price != (Price{Input: 1, Output: 2, CacheRead: 3, CacheWrite: 4, CacheWrite1h: 5}) {
		t.Fatalf("PriceAt with tier = (%+v, %t), want base five prices only", price, priced)
	}
}
