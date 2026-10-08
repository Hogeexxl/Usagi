package storage

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"errors"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/platform"
)

func assertErrorKind(t *testing.T, err error, kind ErrorKind) {
	t.Helper()
	var storageErr *Error
	if !errors.As(err, &storageErr) || storageErr.Kind != kind {
		t.Fatalf("error=%v, want %s", err, kind)
	}
}
func openTestSQLite(t *testing.T, path string, params url.Values) *sql.DB {
	t.Helper()
	db, err := openLegacyConnection(path, params)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	})
	return db
}
func execTestSQL(t *testing.T, db *sql.DB, query string, args ...any) {
	t.Helper()
	if _, err := db.ExecContext(context.Background(), query, args...); err != nil {
		t.Fatal(err)
	}
}
func copyFixture(t *testing.T, name string) (string, string) {
	t.Helper()
	directory := t.TempDir()
	source := filepath.Join(directory, "source.sqlite3")
	data, err := os.ReadFile(filepath.Join("..", "..", "tests", "fixtures", "storage", "legacy", name, "input.sqlite3"))
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(source, data, 0600); err != nil {
		t.Fatal(err)
	}
	return source, filepath.Join(directory, "current.sqlite3")
}
func fixtureMutation(t *testing.T, path string, statements ...string) {
	t.Helper()
	db := openTestSQLite(t, path, url.Values{"_foreign_keys": {"0"}})
	for _, statement := range statements {
		execTestSQL(t, db, statement)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
}
func testCurrent(t *testing.T) *DB {
	t.Helper()
	db, err := Open(context.Background(), Config{Path: filepath.Join(t.TempDir(), "current.sqlite3")})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := db.Close(); err != nil {
			t.Error(err)
		}
	})
	return db
}
func testDefaultPaths(t *testing.T) platform.Paths {
	t.Helper()
	root := t.TempDir()
	if runtime.GOOS == "windows" {
		t.Setenv("LOCALAPPDATA", root)
	} else {
		t.Setenv("HOME", root)
	}
	paths, err := platform.ResolvePaths("")
	if err != nil {
		t.Fatal(err)
	}
	return paths
}
func copyFixtureTo(t *testing.T, name, target string) {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("..", "..", "tests", "fixtures", "storage", "legacy", name, "input.sqlite3"))
	if err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Dir(target), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(target, data, 0600); err != nil {
		t.Fatal(err)
	}
}

func TestExplicitPathDoesNotDiscoverDefaultLegacy(t *testing.T) {
	paths := testDefaultPaths(t)
	copyFixtureTo(t, "v14", paths.LegacyCandidates[0])
	before, err := os.ReadFile(paths.LegacyCandidates[0])
	if err != nil {
		t.Fatal(err)
	}
	explicit := filepath.Join(t.TempDir(), "explicit.sqlite3")
	db, err := Open(context.Background(), Config{Path: explicit})
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if db.Path() != explicit {
		t.Fatalf("Path=%s", db.Path())
	}
	after, err := os.ReadFile(paths.LegacyCandidates[0])
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(before, after) {
		t.Fatal("default Legacy changed")
	}
	if _, err := os.Stat(paths.ActivePath); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("default Active created: %v", err)
	}
}
func TestExplicitRustLegacyRequiresConversion(t *testing.T) {
	source, _ := copyFixture(t, "v14")
	_, err := Open(context.Background(), Config{Path: source})
	assertErrorKind(t, err, ErrorLegacyRequiresConversion)
}
func TestDefaultLegacyDiscoveryPriority(t *testing.T) {
	paths := testDefaultPaths(t)
	for _, candidate := range paths.LegacyCandidates {
		copyFixtureTo(t, "v14", candidate)
	}
	fixtureMutation(t, paths.LegacyCandidates[0], "UPDATE app_meta SET data_revision=123")
	fixtureMutation(t, paths.LegacyCandidates[1], "UPDATE app_meta SET data_revision=456")
	db, err := Open(context.Background(), Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	var revision int
	if err := db.readers.QueryRow("SELECT data_revision FROM app_meta").Scan(&revision); err != nil {
		t.Fatal(err)
	}
	if revision != 123 {
		t.Fatalf("selected revision=%d", revision)
	}
	assertPathAbsent(t, paths.LegacyCandidates[0])
	assertPathPresent(t, paths.LegacyCandidates[1])
}
func TestExistingFreshCreateRecovery(t *testing.T) {
	path := filepath.Join(t.TempDir(), "empty.sqlite3")
	if err := os.WriteFile(path, nil, 0600); err != nil {
		t.Fatal(err)
	}
	identity, err := readSourceIdentity(path)
	if err != nil {
		t.Fatal(err)
	}
	db, err := Open(context.Background(), Config{Path: path})
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	current, err := readSourceIdentity(path)
	if err != nil {
		t.Fatal(err)
	}
	if current != identity {
		t.Fatal("Existing Fresh was replaced")
	}
	version, err := db.SchemaVersion(context.Background())
	if err != nil || version != (SchemaVersion{1, 1}) {
		t.Fatalf("version=%v err=%v", version, err)
	}
}
func TestFreshCreateCrashRecovery(t *testing.T) {
	path := filepath.Join(t.TempDir(), "empty.sqlite3")
	if err := os.WriteFile(path, nil, 0600); err != nil {
		t.Fatal(err)
	}
	failure := errors.New("Fresh Transaction interrupted")
	freshCreateTestHook = func(*sql.Tx) error { return failure }
	t.Cleanup(func() { freshCreateTestHook = nil })
	_, err := Open(context.Background(), Config{Path: path})
	if !errors.Is(err, failure) {
		t.Fatalf("err=%v", err)
	}
	freshCreateTestHook = nil
	class, err := classifyDatabase(context.Background(), path)
	if err != nil || class != schemaFresh {
		t.Fatalf("class=%v err=%v", class, err)
	}
	db, err := Open(context.Background(), Config{Path: path})
	if err != nil {
		t.Fatal(err)
	}
	db.Close()
}
func TestInspectionConnectionDoesNotMutateLegacy(t *testing.T) {
	source, _ := copyFixture(t, "v14")
	fixtureMutation(t, source, "PRAGMA journal_mode=DELETE")
	before, err := os.ReadFile(source)
	if err != nil {
		t.Fatal(err)
	}
	if class, err := classifyDatabase(context.Background(), source); err != nil || class != schemaRustLegacy {
		t.Fatalf("class=%v err=%v", class, err)
	}
	if _, err := preflightLegacySource(context.Background(), source); err != nil {
		t.Fatal(err)
	}
	after, err := os.ReadFile(source)
	if err != nil {
		t.Fatal(err)
	}
	if sha256.Sum256(before) != sha256.Sum256(after) {
		t.Fatal("Inspection changed Legacy Main")
	}
	for _, suffix := range []string{"-wal", "-shm", "-journal"} {
		assertPathAbsent(t, source+suffix)
	}
	db := openTestSQLite(t, source, inspectionURIParams())
	var mode string
	if err := db.QueryRow("PRAGMA journal_mode").Scan(&mode); err != nil || mode != "delete" {
		t.Fatalf("mode=%s err=%v", mode, err)
	}
}
func TestSchemaMarkerCrossCheck(t *testing.T) {
	db := testCurrent(t)
	execTestSQL(t, db.writer, "PRAGMA user_version=999")
	_, err := classifyDatabase(context.Background(), db.Path())
	assertErrorKind(t, err, ErrorSchemaMismatch)
}
func TestSchemaTooNewRustRange(t *testing.T) {
	for _, version := range []string{"15", "999"} {
		t.Run(version, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "future.sqlite3")
			fixtureMutation(t, path, "PRAGMA user_version="+version)
			_, err := classifyDatabase(context.Background(), path)
			assertErrorKind(t, err, ErrorSchemaTooNew)
		})
	}
}
func TestMissingGoMarkerAt1000(t *testing.T) {
	path := filepath.Join(t.TempDir(), "broken.sqlite3")
	fixtureMutation(t, path, "PRAGMA user_version=1000")
	_, err := classifyDatabase(context.Background(), path)
	assertErrorKind(t, err, ErrorSchemaMismatch)
}
func TestLegacyProfileRejectsUnknownTable(t *testing.T) {
	for _, name := range []string{"extra", "sqliteXextra"} {
		t.Run(name, func(t *testing.T) {
			source, _ := copyFixture(t, "v14")
			fixtureMutation(t, source, "CREATE TABLE "+quoteIdentifier(name)+"(id INTEGER PRIMARY KEY)")
			_, err := preflightLegacySource(context.Background(), source)
			assertErrorKind(t, err, ErrorSchemaMismatch)
		})
	}
}
func TestLegacyProfileRejectsMissingTableOrColumn(t *testing.T) {
	for _, statement := range []string{"DROP TABLE codex_skill_usage_events", "ALTER TABLE app_meta DROP COLUMN pricing_catalog_version"} {
		t.Run(statement, func(t *testing.T) {
			source, _ := copyFixture(t, "v14")
			fixtureMutation(t, source, statement)
			_, err := preflightLegacySource(context.Background(), source)
			assertErrorKind(t, err, ErrorSchemaMismatch)
		})
	}
}
func TestLegacyProfileRejectsUnknownColumn(t *testing.T) {
	source, _ := copyFixture(t, "v14")
	fixtureMutation(t, source, "ALTER TABLE app_meta ADD COLUMN extra TEXT")
	_, err := preflightLegacySource(context.Background(), source)
	assertErrorKind(t, err, ErrorSchemaMismatch)
}
func TestLegacyProfileAllowsV11AssistVariants(t *testing.T) {
	variants := []struct {
		name    string
		columns []string
	}{
		{"app_meta_without_metadata_parser_version", []string{"metadata_parser_version"}},
		{"app_meta_without_last_full_import_completed_at_ms", []string{"last_full_import_completed_at_ms"}},
		{"app_meta_without_both_v11_assist_columns", []string{"metadata_parser_version", "last_full_import_completed_at_ms"}},
	}
	for version := 1; version <= 10; version++ {
		for _, variant := range variants {
			t.Run(fmtVersion(version)+"/"+variant.name, func(t *testing.T) {
				source, _ := copyFixture(t, fmtVersion(version))
				for _, column := range variant.columns {
					fixtureMutation(t, source, "ALTER TABLE app_meta DROP COLUMN "+quoteIdentifier(column))
				}
				match, err := preflightLegacySource(context.Background(), source)
				if err != nil || match.Version != version || match.Variant != variant.name {
					t.Fatalf("match=%v err=%v", match, err)
				}
				fixtureMutation(t, source, "ALTER TABLE app_meta ADD COLUMN unexpected TEXT")
				_, err = preflightLegacySource(context.Background(), source)
				assertErrorKind(t, err, ErrorSchemaMismatch)
			})
		}
	}
}
func TestSchemaSemanticMismatch(t *testing.T) {
	for _, change := range []struct{ from, to string }{
		{"CHECK (cached_tokens <= input_tokens)", "CHECK (cached_tokens < input_tokens)"},
		{"DEFERRABLE INITIALLY DEFERRED", "NOT DEFERRABLE INITIALLY IMMEDIATE"},
		{"WHERE response_id IS NOT NULL", "WHERE response_id IS NULL"},
	} {
		t.Run(change.to, func(t *testing.T) {
			ddl := strings.Replace(currentSchemaSQL, change.from, change.to, 1)
			if ddl == currentSchemaSQL {
				t.Fatalf("Schema expression missing: %s", change.from)
			}
			db, err := sql.Open("sqlite", ":memory:")
			if err != nil {
				t.Fatal(err)
			}
			defer db.Close()
			db.SetMaxOpenConns(1)
			execTestSQL(t, db, ddl)
			assertErrorKind(t, validateCurrent(context.Background(), db), ErrorSchemaMismatch)
		})
	}
	t.Run("duplicate_fk_association", func(t *testing.T) {
		inspect := func(clauses string) schemaSnapshot {
			db, err := sql.Open("sqlite", ":memory:")
			if err != nil {
				t.Fatal(err)
			}
			defer db.Close()
			db.SetMaxOpenConns(1)
			execTestSQL(t, db, "CREATE TABLE p(id INTEGER PRIMARY KEY); CREATE TABLE c(id INTEGER PRIMARY KEY, ref INTEGER,"+clauses+")")
			snapshot, err := inspectSchema(context.Background(), db)
			if err != nil {
				t.Fatal(err)
			}
			return snapshot
		}
		immediate := "FOREIGN KEY(ref) REFERENCES p(id) ON DELETE CASCADE NOT DEFERRABLE INITIALLY IMMEDIATE"
		deferred := "FOREIGN KEY(ref) REFERENCES p(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED"
		restricted := "FOREIGN KEY(ref) REFERENCES p(id) ON UPDATE CASCADE ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED"
		a := inspect(immediate + "," + deferred + "," + restricted)
		b := inspect(restricted + "," + deferred + "," + immediate)
		if err := compareSchemaSnapshots(a, b); err != nil {
			t.Fatal(err)
		}
		if len(a.foreignKeys) != 3 {
			t.Fatalf("FK count=%d", len(a.foreignKeys))
		}
		deferredCount := 0
		for _, fk := range a.foreignKeys {
			if fk.deferrable && fk.initialMode == "deferred" {
				deferredCount++
			}
		}
		if deferredCount != 2 {
			t.Fatalf("deferred FKs=%d", deferredCount)
		}
	})
}
func TestForeignKeyCheckGate(t *testing.T) {
	db := testCurrent(t)
	execTestSQL(t, db.writer, "PRAGMA foreign_keys=OFF")
	execTestSQL(t, db.writer, "INSERT INTO codex_source_checkpoints(source_file_id,consumer_kind,parser_version,committed_offset,processing_status) VALUES(999,'metadata',0,0,'ready')")
	execTestSQL(t, db.writer, "PRAGMA foreign_keys=ON")
	assertErrorKind(t, validateCurrent(context.Background(), db.writer), ErrorDataIntegrity)
}
func TestRequiredSingletonGate(t *testing.T) {
	for _, query := range []string{"DELETE FROM app_meta WHERE id=1", "DELETE FROM codex_adapter_state WHERE id=1", "DELETE FROM source_usage_epochs WHERE source='codex'"} {
		t.Run(query, func(t *testing.T) {
			db := testCurrent(t)
			execTestSQL(t, db.writer, query)
			assertErrorKind(t, validateCurrent(context.Background(), db.writer), ErrorDataIntegrity)
		})
	}
}
