package database

import (
	"context"
	"database/sql"
	"net/url"
	"path/filepath"
	"reflect"
	"testing"
)

func TestSchemaCreatesAndReopens(t *testing.T) {
	path := filepath.Join(t.TempDir(), "usagi.sqlite")
	wantTables := []string{"app_meta", "price_versions", "prices", "requests", "session_links", "sessions", "sync_days"}

	for openNumber := range 2 {
		db, err := Open(path)
		if err != nil {
			t.Fatalf("Open() error = %v", err)
		}

		var version, revision, initialSync string
		if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'schema_version'").Scan(&version); err != nil {
			t.Fatalf("read schema version: %v", err)
		}
		if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'data_revision'").Scan(&revision); err != nil {
			t.Fatalf("read data revision: %v", err)
		}
		if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'initial_sync_complete'").Scan(&initialSync); err != nil {
			t.Fatalf("read initial sync state: %v", err)
		}
		if version != "3" || revision != "0" || initialSync != "0" {
			t.Fatalf("app_meta = schema %q, revision %q, initial sync %q", version, revision, initialSync)
		}
		for table, columns := range map[string][]string{
			"sessions":  {"role", "parent_session", "root_session", "nickname"},
			"requests":  {"session_role", "compaction"},
			"sync_days": {"identity_backfill_pending", "compaction_backfill_pending"},
		} {
			for _, column := range columns {
				if !hasColumn(t, db, table, column) {
					t.Errorf("schema is missing %s.%s", table, column)
				}
			}
		}

		rows, err := db.Query("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
		if err != nil {
			t.Fatalf("list schema tables: %v", err)
		}
		var tables []string
		for rows.Next() {
			var name string
			if err := rows.Scan(&name); err != nil {
				rows.Close()
				t.Fatalf("read schema table: %v", err)
			}
			tables = append(tables, name)
		}
		if err := rows.Close(); err != nil {
			t.Fatalf("close schema rows: %v", err)
		}
		if err := rows.Err(); err != nil {
			t.Fatalf("iterate schema tables: %v", err)
		}
		if !reflect.DeepEqual(tables, wantTables) {
			t.Fatalf("schema tables = %v, want %v", tables, wantTables)
		}
		if openNumber == 0 {
			if _, err := db.Exec("INSERT INTO sync_days (day, row_count) VALUES ('2026-01-02', 3)"); err != nil {
				t.Fatalf("write sync day before reopen: %v", err)
			}
		} else {
			var rowCount int
			if err := db.QueryRow("SELECT row_count FROM sync_days WHERE day = '2026-01-02'").Scan(&rowCount); err != nil {
				t.Fatalf("read sync day after reopen: %v", err)
			}
			if rowCount != 3 {
				t.Fatalf("reopened sync day row_count = %d, want 3", rowCount)
			}
		}

		if err := db.Close(); err != nil {
			t.Fatalf("Close() error = %v", err)
		}
	}
}

func TestMigrationV1ToV3PreservesData(t *testing.T) {
	path := filepath.Join(t.TempDir(), "usagi.sqlite")
	fileURL := url.URL{Scheme: "file", Path: filepath.ToSlash(path)}
	legacy, err := sql.Open("sqlite", fileURL.String())
	if err != nil {
		t.Fatalf("open legacy database: %v", err)
	}
	if _, err := legacy.Exec(schemaSQL); err != nil {
		legacy.Close()
		t.Fatalf("create v1 schema: %v", err)
	}
	for _, statement := range []string{
		`INSERT INTO sessions (agent, id, cwd, title, start_ns, last_ns, gateway, read_only, has_path, agent_name, first_seen_ns, last_seen_ns, disappeared)
		 VALUES ('codex', 'thread-1', '/work', 'Title', 10, 20, 1, 0, 1, 'Codex', 30, 40, 0)`,
		`INSERT INTO requests (t_ns, local_day, model, input, magpie_cost, magpie_priced)
		 VALUES (100, '2026-10-01', 'gpt-5', 7, 0.125, 1)`,
		`INSERT INTO sync_days (day, fingerprint, row_count, last_sync_ns, frozen, difference)
		 VALUES ('2026-10-01', X'0102', 1, 100, 1, 'saved')`,
		`INSERT INTO price_versions (source, fetched_at_ns, effective_at_ns, provider_catalogs_json, maker_catalogs_json)
		 VALUES ('fixture', 1, 2, '{}', '{}')`,
		`INSERT INTO prices (version_id, catalog_id, model_key, canonical_model_id, input, output)
		 VALUES (1, 'openai', 'gpt-5', 'gpt-5', 2, 8)`,
		`INSERT INTO session_links (child_agent, child_id, parent_agent, parent_id, relation_type, source)
		 VALUES ('codex', 'thread-1', 'codex', 'thread-0', 'parent', 'native')`,
	} {
		if _, err := legacy.Exec(statement); err != nil {
			legacy.Close()
			t.Fatalf("seed v1 database: %v", err)
		}
	}
	if err := legacy.Close(); err != nil {
		t.Fatalf("close v1 database: %v", err)
	}

	db, err := Open(path)
	if err != nil {
		t.Fatalf("migrate v1 database: %v", err)
	}
	defer db.Close()
	var version, role, parent, root, sessionRole string
	var cwd, title, dayDifference string
	var cost, priceInput float64
	var priced, input, rowCount, frozen int
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'schema_version'").Scan(&version); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT role, parent_session, root_session, cwd, title FROM sessions WHERE agent = 'codex' AND id = 'thread-1'").Scan(&role, &parent, &root, &cwd, &title); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT session_role, input, magpie_cost, magpie_priced FROM requests WHERE local_day = '2026-10-01'").Scan(&sessionRole, &input, &cost, &priced); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT row_count, frozen, difference FROM sync_days WHERE day = '2026-10-01'").Scan(&rowCount, &frozen, &dayDifference); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT input FROM prices WHERE version_id = 1 AND model_key = 'gpt-5'").Scan(&priceInput); err != nil {
		t.Fatal(err)
	}
	var links int
	if err := db.QueryRow("SELECT COUNT(*) FROM session_links").Scan(&links); err != nil {
		t.Fatal(err)
	}
	var compaction sql.NullInt64
	if err := db.QueryRow("SELECT compaction FROM requests WHERE local_day = '2026-10-01'").Scan(&compaction); err != nil {
		t.Fatal(err)
	}
	if version != "3" || role != "" || parent != "" || root != "" || cwd != "/work" || title != "Title" ||
		sessionRole != "" || input != 7 || cost != 0.125 || priced != 1 || rowCount != 1 || frozen != 1 ||
		dayDifference != "saved" || priceInput != 2 || links != 1 || compaction.Valid {
		t.Fatalf("migrated data: schema=%q session=%q/%q/%q cwd=%q title=%q request=%q/%d/%.3f/%d sync=%d/%d/%q price=%.1f links=%d",
			version, role, parent, root, cwd, title, sessionRole, input, cost, priced, rowCount, frozen, dayDifference, priceInput, links)
	}
	if err := db.Close(); err != nil {
		t.Fatalf("close migrated database: %v", err)
	}
	if reopened, err := Open(path); err != nil {
		t.Fatalf("reopen v3 database: %v", err)
	} else {
		_ = reopened.Close()
	}
}

func TestMigrationV2ToV3QueuesCachedCodexDaysIncludingFrozenDays(t *testing.T) {
	path := filepath.Join(t.TempDir(), "usagi.sqlite")
	fileURL := url.URL{Scheme: "file", Path: filepath.ToSlash(path)}
	legacy, err := sql.Open("sqlite", fileURL.String())
	if err != nil {
		t.Fatalf("open legacy database: %v", err)
	}
	tx, err := legacy.Begin()
	if err != nil {
		legacy.Close()
		t.Fatalf("begin v2 fixture migration: %v", err)
	}
	if _, err := tx.Exec(schemaSQL); err != nil {
		tx.Rollback()
		legacy.Close()
		t.Fatalf("create v1 schema: %v", err)
	}
	if err := migrateV1ToV2(context.Background(), tx); err != nil {
		tx.Rollback()
		legacy.Close()
		t.Fatalf("create v2 schema: %v", err)
	}
	for _, statement := range []string{
		`INSERT INTO sessions (agent, id, cwd, title, start_ns, last_ns, gateway, read_only, has_path, agent_name, first_seen_ns, last_seen_ns, disappeared)
			VALUES ('codex', 'thread-1', '/work', 'Preserved title', 10, 20, 1, 0, 1, 'Codex', 30, 40, 0)`,
		`INSERT INTO requests (t_ns, local_day, agent, model, input, magpie_cost, magpie_priced)
			VALUES (100, '2026-10-01', 'codex', 'gpt-5', 7, 0.125, 1)`,
		`INSERT INTO requests (t_ns, local_day, agent, model, input)
			VALUES (200, '2026-10-02', 'claude', 'sonnet', 3)`,
		`INSERT INTO requests (t_ns, local_day, agent, model, input)
			VALUES (300, '2026-09-30', 'codex', 'gpt-5-mini', 5)`,
		`INSERT INTO sync_days (day, row_count, last_sync_ns, frozen) VALUES ('2026-10-01', 1, 100, 1)`,
		`INSERT INTO sync_days (day, row_count, last_sync_ns, frozen) VALUES ('2026-10-02', 1, 200, 0)`,
	} {
		if _, err := tx.Exec(statement); err != nil {
			tx.Rollback()
			legacy.Close()
			t.Fatalf("seed v2 database: %v", err)
		}
	}
	if err := tx.Commit(); err != nil {
		legacy.Close()
		t.Fatalf("commit v2 fixture: %v", err)
	}
	if err := legacy.Close(); err != nil {
		t.Fatalf("close v2 fixture: %v", err)
	}

	db, err := Open(path)
	if err != nil {
		t.Fatalf("migrate v2 database: %v", err)
	}
	defer db.Close()
	var version, nickname, title string
	var frozen, codexPending, otherPending, insertedPending int
	var model string
	var input int
	var cost float64
	var priced int
	var compaction sql.NullInt64
	if err := db.QueryRow("SELECT value FROM app_meta WHERE key = 'schema_version'").Scan(&version); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT nickname, title FROM sessions WHERE agent = 'codex' AND id = 'thread-1'").Scan(&nickname, &title); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(`SELECT r.model, r.input, r.magpie_cost, r.magpie_priced, r.compaction, d.frozen,
		d.compaction_backfill_pending FROM requests r JOIN sync_days d ON d.day = r.local_day WHERE r.agent = 'codex'`).
		Scan(&model, &input, &cost, &priced, &compaction, &frozen, &codexPending); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT compaction_backfill_pending FROM sync_days WHERE day = '2026-10-02'").Scan(&otherPending); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow("SELECT compaction_backfill_pending FROM sync_days WHERE day = '2026-09-30'").Scan(&insertedPending); err != nil {
		t.Fatal(err)
	}
	if version != "3" || nickname != "" || title != "Preserved title" || model != "gpt-5" || input != 7 || cost != 0.125 || priced != 1 ||
		compaction.Valid || frozen != 1 || codexPending != 1 || otherPending != 0 || insertedPending != 1 {
		t.Fatalf("migrated v2 state: version=%q nickname=%q title=%q request=%q/%d/%.3f/%d compaction=%v frozen=%d pending=%d other=%d inserted=%d",
			version, nickname, title, model, input, cost, priced, compaction, frozen, codexPending, otherPending, insertedPending)
	}
	if err := db.Close(); err != nil {
		t.Fatalf("close migrated v2 database: %v", err)
	}
	reopened, err := Open(path)
	if err != nil {
		t.Fatalf("reopen migrated v2 database: %v", err)
	}
	defer reopened.Close()
	if err := reopened.QueryRow("SELECT compaction_backfill_pending FROM sync_days WHERE day = '2026-10-01'").Scan(&codexPending); err != nil || codexPending != 1 {
		t.Fatalf("pending frozen day after reopen = %d, %v; want 1", codexPending, err)
	}
}

func hasColumn(t *testing.T, db *sql.DB, table, column string) bool {
	t.Helper()
	rows, err := db.Query("PRAGMA table_info(" + table + ")")
	if err != nil {
		t.Fatalf("inspect %s columns: %v", table, err)
	}
	defer rows.Close()
	for rows.Next() {
		var index, notNull, primaryKey int
		var name, columnType string
		var defaultValue sql.NullString
		if err := rows.Scan(&index, &name, &columnType, &notNull, &defaultValue, &primaryKey); err != nil {
			t.Fatalf("read %s column: %v", table, err)
		}
		if name == column {
			return true
		}
	}
	if err := rows.Err(); err != nil {
		t.Fatalf("iterate %s columns: %v", table, err)
	}
	return false
}

func TestMigrationFailureRollsBack(t *testing.T) {
	path := filepath.Join(t.TempDir(), "usagi.sqlite")
	originalSchema := schemaSQL
	schemaSQL += "\nCREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);"
	t.Cleanup(func() { schemaSQL = originalSchema })

	db, err := Open(path)
	if err == nil {
		_ = db.Close()
		t.Fatal("Open() error = nil, want schema migration failure")
	}

	fileURL := url.URL{Scheme: "file", Path: filepath.ToSlash(path)}
	checkDB, err := sql.Open("sqlite", fileURL.String())
	if err != nil {
		t.Fatalf("open database after failed migration: %v", err)
	}
	defer checkDB.Close()

	var tableCount int
	if err := checkDB.QueryRow("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'").Scan(&tableCount); err != nil {
		t.Fatalf("count tables after failed migration: %v", err)
	}
	if tableCount != 0 {
		t.Fatalf("tables after failed migration = %d, want none", tableCount)
	}
}
