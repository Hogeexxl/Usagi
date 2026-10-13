package pricing

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"time"
)

const modelsDevURL = "https://models.dev/api.json"

type Price struct {
	Input        float64 `json:"input"`
	Output       float64 `json:"output"`
	CacheRead    float64 `json:"cache_read"`
	CacheWrite   float64 `json:"cache_write"`
	CacheWrite1h float64 `json:"cache_write_1h"`
}

func (p Price) Cost(input, output, cacheRead, cacheWrite, cacheWrite1h int64) float64 {
	return (float64(input)*p.Input + float64(output)*p.Output +
		float64(cacheRead)*p.CacheRead + float64(cacheWrite-cacheWrite1h)*p.CacheWrite +
		float64(cacheWrite1h)*p.CacheWrite1h) / 1_000_000
}

type catalogModel struct {
	canonicalModelID string
	price            *Price
}

type priceVersion struct {
	id               int64
	effectiveAtNS    int64
	providerCatalogs map[string][]string
	makerCatalogs    []string
	models           map[string]map[string]catalogModel
	listed           map[string]bool
	canonical        map[string]bool
}

type Catalog struct {
	versions []priceVersion
	cache    map[lookupKey]lookupResult
}

type modelsDevCatalog map[string]modelsDevProvider

type modelsDevProvider struct {
	Models map[string]modelsDevModel `json:"models"`
}

type modelsDevModel struct {
	Cost             *Price `json:"cost"`
	CanonicalModelID string `json:"canonical_model_id"`
}

func Bootstrap(ctx context.Context, db *sql.DB, client *http.Client, providerCatalogs map[string][]string, now time.Time) (bool, error) {
	var exists bool
	if err := db.QueryRowContext(ctx, "SELECT EXISTS(SELECT 1 FROM price_versions)").Scan(&exists); err != nil {
		return false, fmt.Errorf("pricing: check existing versions: %w", err)
	}
	if exists {
		return false, nil
	}

	downloadCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	request, err := http.NewRequestWithContext(downloadCtx, http.MethodGet, modelsDevURL, nil)
	if err != nil {
		return false, fmt.Errorf("pricing: create models.dev request: %w", err)
	}
	response, err := client.Do(request)
	if err != nil {
		return false, fmt.Errorf("pricing: download models.dev: %w", err)
	}
	defer response.Body.Close()
	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		return false, fmt.Errorf("pricing: models.dev returned HTTP status %d", response.StatusCode)
	}

	var source modelsDevCatalog
	if err := json.NewDecoder(response.Body).Decode(&source); err != nil {
		return false, fmt.Errorf("pricing: decode models.dev: %w", err)
	}
	if len(source) == 0 {
		return false, fmt.Errorf("pricing: models.dev returned an empty catalog")
	}
	modelCount := 0
	for _, provider := range source {
		modelCount += len(provider.Models)
	}
	if modelCount == 0 {
		return false, fmt.Errorf("pricing: models.dev returned no models")
	}

	if providerCatalogs == nil {
		providerCatalogs = map[string][]string{}
	}
	providerJSON, err := json.Marshal(providerCatalogs)
	if err != nil {
		return false, fmt.Errorf("pricing: encode provider catalogs: %w", err)
	}
	makerJSON, err := json.Marshal(makerCatalogs)
	if err != nil {
		return false, fmt.Errorf("pricing: encode maker catalogs: %w", err)
	}

	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return false, fmt.Errorf("pricing: begin bootstrap: %w", err)
	}
	defer tx.Rollback()

	stamp := now.UnixNano()
	result, err := tx.ExecContext(ctx, `INSERT INTO price_versions (
		source, fetched_at_ns, effective_at_ns, provider_catalogs_json, maker_catalogs_json
	) VALUES (?, ?, ?, ?, ?)`, modelsDevURL, stamp, stamp, string(providerJSON), string(makerJSON))
	if err != nil {
		return false, fmt.Errorf("pricing: insert version: %w", err)
	}
	versionID, err := result.LastInsertId()
	if err != nil {
		return false, fmt.Errorf("pricing: read inserted version id: %w", err)
	}
	statement, err := tx.PrepareContext(ctx, `INSERT INTO prices (
		version_id, catalog_id, model_key, canonical_model_id,
		input, output, cache_read, cache_write, cache_write_1h
	) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`)
	if err != nil {
		return false, fmt.Errorf("pricing: prepare prices: %w", err)
	}
	defer statement.Close()
	for catalogID, provider := range source {
		for modelKey, model := range provider.Models {
			var input, output, cacheRead, cacheWrite, cacheWrite1h any
			if model.Cost != nil {
				price := *model.Cost
				oneHourFor(modelKey, &price)
				input, output, cacheRead = price.Input, price.Output, price.CacheRead
				cacheWrite, cacheWrite1h = price.CacheWrite, price.CacheWrite1h
			}
			if _, err := statement.ExecContext(ctx, versionID, catalogID, modelKey, model.CanonicalModelID,
				input, output, cacheRead, cacheWrite, cacheWrite1h); err != nil {
				return false, fmt.Errorf("pricing: insert model price: %w", err)
			}
		}
	}
	if _, err := tx.ExecContext(ctx, "UPDATE app_meta SET value = CAST(value AS INTEGER) + 1 WHERE key = 'data_revision'"); err != nil {
		return false, fmt.Errorf("pricing: increment data revision: %w", err)
	}
	if err := tx.Commit(); err != nil {
		return false, fmt.Errorf("pricing: commit bootstrap: %w", err)
	}
	return true, nil
}

func Load(ctx context.Context, tx *sql.Tx) (*Catalog, error) {
	rows, err := tx.QueryContext(ctx, `SELECT id, effective_at_ns, provider_catalogs_json, maker_catalogs_json
		FROM price_versions ORDER BY effective_at_ns, id`)
	if err != nil {
		return nil, fmt.Errorf("pricing: load versions: %w", err)
	}
	byID := make(map[int64]int)
	catalog := &Catalog{cache: make(map[lookupKey]lookupResult)}
	for rows.Next() {
		var version priceVersion
		var providerJSON, makerJSON string
		if err := rows.Scan(&version.id, &version.effectiveAtNS, &providerJSON, &makerJSON); err != nil {
			rows.Close()
			return nil, fmt.Errorf("pricing: scan version: %w", err)
		}
		if err := json.Unmarshal([]byte(providerJSON), &version.providerCatalogs); err != nil {
			rows.Close()
			return nil, fmt.Errorf("pricing: decode provider catalogs: %w", err)
		}
		if err := json.Unmarshal([]byte(makerJSON), &version.makerCatalogs); err != nil {
			rows.Close()
			return nil, fmt.Errorf("pricing: decode maker catalogs: %w", err)
		}
		version.models = make(map[string]map[string]catalogModel)
		version.listed = make(map[string]bool)
		version.canonical = make(map[string]bool)
		byID[version.id] = len(catalog.versions)
		catalog.versions = append(catalog.versions, version)
	}
	if err := rows.Close(); err != nil {
		return nil, fmt.Errorf("pricing: close versions: %w", err)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("pricing: iterate versions: %w", err)
	}

	priceRows, err := tx.QueryContext(ctx, `SELECT version_id, catalog_id, model_key, canonical_model_id,
		input, output, cache_read, cache_write, cache_write_1h FROM prices ORDER BY version_id, catalog_id, model_key`)
	if err != nil {
		return nil, fmt.Errorf("pricing: load prices: %w", err)
	}
	defer priceRows.Close()
	for priceRows.Next() {
		var versionID int64
		var catalogID, modelKey, canonicalModelID string
		var input, output, cacheRead, cacheWrite, cacheWrite1h sql.NullFloat64
		if err := priceRows.Scan(&versionID, &catalogID, &modelKey, &canonicalModelID,
			&input, &output, &cacheRead, &cacheWrite, &cacheWrite1h); err != nil {
			return nil, fmt.Errorf("pricing: scan model price: %w", err)
		}
		index, ok := byID[versionID]
		if !ok {
			return nil, fmt.Errorf("pricing: price references missing version %d", versionID)
		}
		version := &catalog.versions[index]
		entry := catalogModel{canonicalModelID: canonicalModelID}
		if input.Valid && output.Valid && cacheRead.Valid && cacheWrite.Valid && cacheWrite1h.Valid {
			entry.price = &Price{
				Input: input.Float64, Output: output.Float64, CacheRead: cacheRead.Float64,
				CacheWrite: cacheWrite.Float64, CacheWrite1h: cacheWrite1h.Float64,
			}
		}
		if version.models[catalogID] == nil {
			version.models[catalogID] = make(map[string]catalogModel)
		}
		version.models[catalogID][modelKey] = entry
		version.listed[bareID(modelKey)] = true
		if _, _, ok := strings.Cut(canonicalModelID, "/"); ok {
			version.canonical[bareID(canonicalModelID)] = true
		}
	}
	if err := priceRows.Err(); err != nil {
		return nil, fmt.Errorf("pricing: iterate prices: %w", err)
	}
	return catalog, nil
}

func oneHourFor(id string, price *Price) {
	if price == nil {
		return
	}
	if price.CacheWrite > 0 && price.CacheWrite1h <= 0 && strings.Contains(strings.ToLower(id), "claude") {
		price.CacheWrite1h = 2 * price.Input
		return
	}
	if price.CacheWrite1h <= 0 {
		price.CacheWrite1h = price.CacheWrite
	}
}

var makerCatalogs = []string{
	"anthropic", "openai", "google", "deepseek", "xai", "moonshotai", "zhipuai", "minimax",
	"stepfun-ai-step-plan", "stepfun-ai", "stepfun-step-plan", "stepfun",
	"xiaomi-token-plan-cn", "xiaomi", "alibaba", "mistral",
}
