package storage

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"sort"
	"testing"
	"time"
)

func fmtVersion(version int) string { return fmt.Sprintf("v%02d", version) }
func assertPathPresent(t *testing.T, path string) {
	t.Helper()
	if _, err := os.Stat(path); err != nil {
		t.Fatalf("required file %s: %v", path, err)
	}
}
func assertPathAbsent(t *testing.T, path string) {
	t.Helper()
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("file should be absent %s: %v", path, err)
	}
}
func installConversionHook(t *testing.T, hook func(string, *LegacyConversionMarker, *sql.DB) error) {
	t.Helper()
	conversionTestHook = hook
	t.Cleanup(func() { conversionTestHook = nil })
}
func convertFixture(t *testing.T, name string) (string, string) {
	t.Helper()
	source, target := copyFixture(t, name)
	if err := ConvertLegacy(context.Background(), LegacyConversionConfig{source, target}); err != nil {
		t.Fatal(err)
	}
	return source, target
}
func readParityRows(t *testing.T, db *sql.DB, table string) [][]any {
	t.Helper()
	columns, err := inspectColumns(context.Background(), db, table)
	if err != nil {
		t.Fatal(err)
	}
	var pk []columnSnapshot
	for _, column := range columns {
		if column.primaryKeyOrder > 0 {
			pk = append(pk, column)
		}
	}
	if len(pk) == 0 {
		t.Fatalf("MissingParityKey: %s", table)
	}
	sort.Slice(pk, func(a, b int) bool { return pk[a].primaryKeyOrder < pk[b].primaryKeyOrder })
	order := make([]string, len(pk))
	for n, column := range pk {
		order[n] = quoteIdentifier(column.name)
	}
	selected := currentImportColumns[table]
	quoted := make([]string, len(selected))
	for n, column := range selected {
		quoted[n] = quoteIdentifier(column)
	}
	rows, err := db.Query("SELECT " + joinColumns(quoted) + " FROM " + quoteIdentifier(table) + " ORDER BY " + joinColumns(order))
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	var result [][]any
	for rows.Next() {
		values := make([]any, len(selected))
		pointers := make([]any, len(values))
		for n := range values {
			pointers[n] = &values[n]
		}
		if err := rows.Scan(pointers...); err != nil {
			t.Fatal(err)
		}
		result = append(result, values)
	}
	if err := rows.Err(); err != nil {
		t.Fatal(err)
	}
	return result
}
func joinColumns(columns []string) string {
	result := ""
	for n, column := range columns {
		if n > 0 {
			result += ","
		}
		result += column
	}
	return result
}
func assertLegacyParity(t *testing.T, name, target string) {
	t.Helper()
	actual := openTestSQLite(t, target, inspectionURIParams())
	expected := openTestSQLite(t, filepath.Join("..", "..", "tests", "fixtures", "storage", "legacy", name, "expected-v14.sqlite3"), inspectionURIParams())
	names := make([]string, 0, len(currentImportColumns))
	for table := range currentImportColumns {
		names = append(names, table)
	}
	sort.Strings(names)
	for _, table := range names {
		got := readParityRows(t, actual, table)
		want := readParityRows(t, expected, table)
		if !reflect.DeepEqual(got, want) {
			t.Fatalf("Row Parity %s differs:\nactual=%#v\nexpected=%#v", table, got, want)
		}
	}
}
func legacyParityCase(t *testing.T, name string) {
	t.Helper()
	_, target := convertFixture(t, name)
	assertLegacyParity(t, name, target)
}
func TestLegacyConversionV01ToV14(t *testing.T) { legacyParityCase(t, "v01") }
func TestLegacyConversionV02ToV14(t *testing.T) { legacyParityCase(t, "v02") }
func TestLegacyConversionV03ToV14(t *testing.T) { legacyParityCase(t, "v03") }
func TestLegacyConversionV04ToV14(t *testing.T) { legacyParityCase(t, "v04") }
func TestLegacyConversionV05ToV14(t *testing.T) { legacyParityCase(t, "v05") }
func TestLegacyConversionV06ToV14(t *testing.T) { legacyParityCase(t, "v06") }
func TestLegacyConversionV07ToV14(t *testing.T) { legacyParityCase(t, "v07") }
func TestLegacyConversionV08ToV14(t *testing.T) { legacyParityCase(t, "v08") }
func TestLegacyConversionV09ToV14(t *testing.T) { legacyParityCase(t, "v09") }
func TestLegacyConversionV10ToV14(t *testing.T) { legacyParityCase(t, "v10") }
func TestLegacyConversionV11ToV14(t *testing.T) { legacyParityCase(t, "v11") }
func TestLegacyConversionV12ToV14(t *testing.T) { legacyParityCase(t, "v12") }
func TestLegacyConversionV13ToV14(t *testing.T) { legacyParityCase(t, "v13") }
func TestLegacyConversionV14ToV14(t *testing.T) { legacyParityCase(t, "v14") }

func TestLegacyConversionV11AssistVariantMissingMetadataParserVersion(t *testing.T) {
	legacyParityCase(t, "v10-without-metadata-parser-version")
}
func TestLegacyConversionV11AssistVariantMissingLastFullImport(t *testing.T) {
	legacyParityCase(t, "v10-without-last-full-import")
}
func TestLegacyConversionV11AssistVariantMissingBoth(t *testing.T) {
	legacyParityCase(t, "v10-without-both-metadata-columns")
}

func TestLegacyConversionV01ProjectKindSQLiteCase(t *testing.T) {
	for _, value := range []any{nil, "", "\x00project", []byte{}, []byte{0, 1}, "/project"} {
		t.Run(fmt.Sprintf("%T_%v", value, value), func(t *testing.T) {
			source, target := copyFixture(t, "v01")
			db := openTestSQLite(t, source, nil)
			wantType := "text"
			switch v := value.(type) {
			case nil:
				wantType = "null"
				execTestSQL(t, db, "UPDATE threads SET project_path=?", value)
			case []byte:
				wantType = "blob"
				if len(v) == 0 {
					execTestSQL(t, db, "UPDATE threads SET project_path=zeroblob(?)", int64(0))
				} else {
					execTestSQL(t, db, "UPDATE threads SET project_path=?", value)
				}
			default:
				execTestSQL(t, db, "UPDATE threads SET project_path=?", value)
			}
			var want string
			var sourceType string
			if err := db.QueryRow("SELECT CASE WHEN project_path IS NOT NULL AND length(project_path)>0 THEN 'project' ELSE 'unknown' END,typeof(project_path) FROM threads WHERE thread_id=?", "thread-root").Scan(&want, &sourceType); err != nil {
				t.Fatal(err)
			}
			if sourceType != wantType {
				t.Fatalf("source typeof=%s; want %s", sourceType, wantType)
			}
			if err := db.Close(); err != nil {
				t.Fatal(err)
			}
			if err := ConvertLegacy(context.Background(), LegacyConversionConfig{source, target}); err != nil {
				t.Fatal(err)
			}
			output := openTestSQLite(t, target, inspectionURIParams())
			var got string
			var gotType string
			var path any
			if err := output.QueryRow("SELECT project_kind,project_path,typeof(project_path) FROM threads WHERE thread_id=?", "thread-root").Scan(&got, &path, &gotType); err != nil {
				t.Fatal(err)
			}
			sameValue := reflect.DeepEqual(path, value)
			if blob, ok := value.([]byte); ok {
				actual, isBlob := path.([]byte)
				sameValue = isBlob && bytes.Equal(actual, blob)
			}
			if got != want || gotType != wantType || !sameValue {
				t.Fatalf("got kind=%s typeof=%s path=%#v; want kind=%s typeof=%s path=%#v", got, gotType, path, want, wantType, value)
			}
		})
	}
}
func TestConvertCreatesTargetDirectory(t *testing.T) {
	source, target := copyFixture(t, "v14")
	target = filepath.Join(filepath.Dir(target), "new", "nested", "current.sqlite3")
	if err := ConvertLegacy(context.Background(), LegacyConversionConfig{source, target}); err != nil {
		t.Fatal(err)
	}
	assertPathPresent(t, target)
	assertPathAbsent(t, source)
	if err := validateCurrentPath(context.Background(), target); err != nil {
		t.Fatal(err)
	}
}
func TestLegacyImportReplacesFreshSeeds(t *testing.T) {
	for _, name := range []string{"v01", "v07", "v14"} {
		t.Run(name, func(t *testing.T) {
			_, target := convertFixture(t, name)
			expected := openTestSQLite(t, filepath.Join("..", "..", "tests", "fixtures", "storage", "legacy", name, "expected-v14.sqlite3"), inspectionURIParams())
			output := openTestSQLite(t, target, inspectionURIParams())
			for _, table := range []string{"app_meta", "codex_adapter_state", "source_usage_epochs"} {
				got := readParityRows(t, output, table)
				want := readParityRows(t, expected, table)
				if len(got) != 1 || !reflect.DeepEqual(got, want) {
					t.Fatalf("%s seeds: got=%#v want=%#v", table, got, want)
				}
			}
			var revision int
			if err := output.QueryRow("SELECT data_revision FROM app_meta WHERE id=1").Scan(&revision); err != nil || revision == 0 {
				t.Fatalf("Legacy seed revision=%d err=%v", revision, err)
			}
		})
	}
}
func TestForeignKeyImportBoundary(t *testing.T) {
	seen := map[string]bool{}
	installConversionHook(t, func(stage string, marker *LegacyConversionMarker, db *sql.DB) error {
		if stage != "tempBeforeImport" && stage != "tempAfterImport" && stage != "tempForeignKeysOn" {
			return nil
		}
		var value int
		if err := db.QueryRow("PRAGMA foreign_keys").Scan(&value); err != nil {
			return err
		}
		want := 0
		if stage == "tempForeignKeysOn" {
			want = 1
		}
		if value != want {
			t.Fatalf("%s foreign_keys=%d, want %d", stage, value, want)
		}
		if stage == "tempAfterImport" {
			if err := checkForeignKeys(context.Background(), db); err != nil {
				return err
			}
		}
		seen[stage] = true
		return nil
	})
	convertFixture(t, "v14")
	if len(seen) != 3 {
		t.Fatalf("boundary observations=%v", seen)
	}
}
func TestTempTargetUsesDeleteJournal(t *testing.T) {
	before, finalized := false, false
	installConversionHook(t, func(stage string, marker *LegacyConversionMarker, db *sql.DB) error {
		if stage == "tempBeforeImport" {
			var journal string
			var sync, busy int
			if err := db.QueryRow("PRAGMA journal_mode").Scan(&journal); err != nil {
				return err
			}
			if err := db.QueryRow("PRAGMA synchronous").Scan(&sync); err != nil {
				return err
			}
			if err := db.QueryRow("PRAGMA busy_timeout").Scan(&busy); err != nil {
				return err
			}
			if journal != "delete" || sync != 2 || busy != 5000 {
				t.Fatalf("Temp journal=%s synchronous=%d busy=%d", journal, sync, busy)
			}
			before = true
		}
		if stage == "tempFinalized" {
			for _, suffix := range []string{"-wal", "-shm", "-journal"} {
				assertPathAbsent(t, marker.TargetPath+".migrating"+suffix)
			}
			finalized = true
		}
		return nil
	})
	convertFixture(t, "v14")
	if !before || !finalized {
		t.Fatal("Temp boundaries not reached")
	}
}
func TestBackupExistsBeforeActiveCommit(t *testing.T) {
	checked := false
	path := ""
	installConversionHook(t, func(stage string, marker *LegacyConversionMarker, db *sql.DB) error {
		if stage != "beforeActiveRename" {
			return nil
		}
		assertPathAbsent(t, marker.TargetPath)
		assertPathPresent(t, marker.TargetPath+".migrating")
		path = backupPath(marker)
		assertPathPresent(t, path)
		backup := openTestSQLite(t, path, inspectionURIParams())
		if err := quickCheck(context.Background(), backup); err != nil {
			return err
		}
		backup.Close()
		stored, err := readConversionMarker(marker.TargetPath)
		if err != nil {
			return err
		}
		if stored.Phase != LegacyPhaseBackupReady {
			t.Fatalf("phase=%s", stored.Phase)
		}
		checked = true
		return nil
	})
	convertFixture(t, "v14")
	if !checked {
		t.Fatal("Backup boundary not reached")
	}
	assertPathPresent(t, path)
}
func TestCrashBeforeActiveCommit(t *testing.T) {
	for _, boundary := range []string{"prepared", "snapshot", "tempFinalized", "beforeActiveRename"} {
		t.Run(boundary, func(t *testing.T) {
			source, target := copyFixture(t, "v14")
			failure := errors.New("crash " + boundary)
			installConversionHook(t, func(stage string, _ *LegacyConversionMarker, _ *sql.DB) error {
				if stage == boundary {
					return failure
				}
				return nil
			})
			err := ConvertLegacy(context.Background(), LegacyConversionConfig{source, target})
			if !errors.Is(err, failure) {
				t.Fatalf("err=%v", err)
			}
			assertPathPresent(t, source)
			assertPathAbsent(t, target)
			assertPathPresent(t, markerPath(target))
			conversionTestHook = nil
			db, err := Open(context.Background(), Config{Path: target})
			if err != nil {
				t.Fatal(err)
			}
			db.Close()
			assertPathAbsent(t, source)
			assertPathAbsent(t, markerPath(target))
			assertPathAbsent(t, target+".migrating")
		})
	}
}
func pendingConversion(t *testing.T, boundary string) (string, string, *LegacyConversionMarker) {
	t.Helper()
	source, target := copyFixture(t, "v14")
	installConversionHook(t, func(stage string, _ *LegacyConversionMarker, _ *sql.DB) error {
		if stage == boundary {
			return errors.New("interrupted")
		}
		return nil
	})
	err := ConvertLegacy(context.Background(), LegacyConversionConfig{source, target})
	if boundary == "afterActiveRename" || boundary == "deleteSource-wal" {
		if err != nil {
			t.Fatal(err)
		}
	} else if err == nil {
		t.Fatal("expected pre-commit interruption")
	}
	conversionTestHook = nil
	marker, err := readConversionMarker(target)
	if err != nil || marker == nil {
		t.Fatalf("marker=%v err=%v", marker, err)
	}
	return source, target, marker
}
func TestBackupPartialFileRecovery(t *testing.T) {
	source, target, marker := pendingConversion(t, "backupPartial")
	partial := backupPath(marker) + ".partial"
	assertPathPresent(t, partial)
	assertPathAbsent(t, backupPath(marker))
	assertPathPresent(t, source)
	db, err := Open(context.Background(), Config{Path: target})
	if err != nil {
		t.Fatal(err)
	}
	db.Close()
	assertPathAbsent(t, partial)
	assertPathAbsent(t, source)
	assertPathPresent(t, target)
}
func TestCrashAfterActiveRenameBeforeMarkerUpdate(t *testing.T) {
	source, target, marker := pendingConversion(t, "afterActiveRename")
	if marker.Phase != LegacyPhaseBackupReady {
		t.Fatalf("Marker phase=%s", marker.Phase)
	}
	assertPathPresent(t, source)
	db, err := Open(context.Background(), Config{Path: target})
	if err != nil {
		t.Fatal(err)
	}
	db.Close()
	assertPathAbsent(t, source)
	assertPathAbsent(t, markerPath(target))
}
func TestCrashArtifactPrecedesFreshCreate(t *testing.T) {
	source, target, marker := pendingConversion(t, "prepared")
	freshCalled := false
	freshCreateTestHook = func(*sql.Tx) error { freshCalled = true; return errors.New("unexpected Fresh") }
	t.Cleanup(func() { freshCreateTestHook = nil })
	db, err := Open(context.Background(), Config{Path: target})
	if err != nil {
		t.Fatal(err)
	}
	db.Close()
	if freshCalled {
		t.Fatal("Fresh Create preceded Marker recovery")
	}
	assertPathAbsent(t, source)
	assertPathAbsent(t, markerPath(target))
	assertPathAbsent(t, backupPath(marker))
}
func TestLegacyHousekeepingPartialRetry(t *testing.T) {
	source, target := copyFixture(t, "v14")
	installConversionHook(t, func(stage string, marker *LegacyConversionMarker, _ *sql.DB) error {
		if stage == "sourceReleased" {
			for _, suffix := range []string{"-wal", "-shm"} {
				if err := os.WriteFile(marker.SourcePath+suffix, []byte("sidecar"), 0600); err != nil {
					return err
				}
			}
		}
		if stage == "deleteSource-shm" {
			return errors.New("delete denied")
		}
		return nil
	})
	if err := ConvertLegacy(context.Background(), LegacyConversionConfig{source, target}); err != nil {
		t.Fatal(err)
	}
	assertPathAbsent(t, source+"-wal")
	assertPathPresent(t, source+"-shm")
	assertPathPresent(t, source)
	assertPathPresent(t, markerPath(target))
	conversionTestHook = nil
	db, err := Open(context.Background(), Config{Path: target})
	if err != nil {
		t.Fatal(err)
	}
	db.Close()
	assertPathAbsent(t, source)
	assertPathAbsent(t, source+"-shm")
	assertPathAbsent(t, markerPath(target))
}
func TestExplicitOpenRetriesMatchingMarker(t *testing.T) {
	source, target, _ := pendingConversion(t, "deleteSource-wal")
	db, err := Open(context.Background(), Config{Path: target})
	if err != nil {
		t.Fatal(err)
	}
	db.Close()
	assertPathAbsent(t, source)
	assertPathAbsent(t, markerPath(target))
}
func TestConvertLegacyReentryAfterCommittedPendingHousekeeping(t *testing.T) {
	source, target, _ := pendingConversion(t, "deleteSource-wal")
	if err := ConvertLegacy(context.Background(), LegacyConversionConfig{source, target}); err != nil {
		t.Fatal(err)
	}
	assertPathAbsent(t, source)
	assertPathAbsent(t, markerPath(target))
}
func TestConvertLegacyReentryMarkerFailureStillSucceeds(t *testing.T) {
	for _, boundary := range []string{"markerSync", "markerRename"} {
		t.Run(boundary, func(t *testing.T) {
			source, target, _ := pendingConversion(t, "afterActiveRename")
			installConversionHook(t, func(stage string, marker *LegacyConversionMarker, _ *sql.DB) error {
				if stage == boundary && marker.Phase == LegacyPhaseActiveCommitted {
					return errors.New("Marker repair failed")
				}
				return nil
			})
			if err := ConvertLegacy(context.Background(), LegacyConversionConfig{source, target}); err != nil {
				t.Fatal(err)
			}
			assertPathPresent(t, source)
			assertPathPresent(t, markerPath(target))
			conversionTestHook = nil
			db, err := Open(context.Background(), Config{Path: target})
			if err != nil {
				t.Fatal(err)
			}
			db.Close()
			assertPathAbsent(t, source)
		})
	}
}
func TestConvertLegacyRejectsCurrentWithoutMarker(t *testing.T) {
	source, _ := copyFixture(t, "v14")
	db := testCurrent(t)
	err := ConvertLegacy(context.Background(), LegacyConversionConfig{source, db.Path()})
	assertErrorKind(t, err, ErrorInvalidState)
	assertPathPresent(t, source)
}
func replaceSource(t *testing.T, path string, data []byte) {
	t.Helper()
	replacement := path + ".replacement"
	if err := os.WriteFile(replacement, data, 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(path, path+".original"); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(replacement, path); err != nil {
		t.Fatal(err)
	}
}
func TestSourceIdentityRecheckedAfterLock(t *testing.T) {
	source, target := copyFixture(t, "v14")
	data, err := os.ReadFile(source)
	if err != nil {
		t.Fatal(err)
	}
	snapshot := false
	installConversionHook(t, func(stage string, marker *LegacyConversionMarker, _ *sql.DB) error {
		if stage == "prepared" {
			replaceSource(t, source, data)
		}
		if stage == "snapshot" {
			snapshot = true
		}
		return nil
	})
	err = ConvertLegacy(context.Background(), LegacyConversionConfig{source, target})
	assertErrorKind(t, err, ErrorInvalidState)
	if snapshot {
		t.Fatal("Snapshot opened after Identity replacement")
	}
	assertPathPresent(t, source)
	assertPathAbsent(t, target+".migrating")
}
func TestHousekeepingRejectsReplacedSource(t *testing.T) {
	source, target, marker := pendingConversion(t, "deleteSource-wal")
	newData := []byte("replacement must survive")
	replaceSource(t, source, newData)
	assertErrorKind(t, retryLegacyHousekeeping(context.Background(), marker), ErrorInvalidState)
	_, err := Open(context.Background(), Config{Path: target})
	assertErrorKind(t, err, ErrorInvalidState)
	data, err := os.ReadFile(source)
	if err != nil || !bytes.Equal(data, newData) {
		t.Fatalf("replacement changed err=%v", err)
	}
	assertPathPresent(t, markerPath(target))
}
func TestHousekeepingOpenChecksNewDefaultMarker(t *testing.T) {
	paths := testDefaultPaths(t)
	copyFixtureTo(t, "v14", paths.LegacyCandidates[0])
	replacement := []byte("new Source")
	installConversionHook(t, func(stage string, marker *LegacyConversionMarker, _ *sql.DB) error {
		if stage == "sourceReleased" {
			replaceSource(t, marker.SourcePath, replacement)
		}
		return nil
	})
	_, err := Open(context.Background(), Config{})
	assertErrorKind(t, err, ErrorInvalidState)
	assertPathPresent(t, paths.ActivePath)
	assertPathPresent(t, markerPath(paths.ActivePath))
	data, err := os.ReadFile(paths.LegacyCandidates[0])
	if err != nil || !bytes.Equal(data, replacement) {
		t.Fatalf("Source was retired: %v", err)
	}
}
func TestHousekeepingRequiresVerifiedBackup(t *testing.T) {
	for _, corrupt := range []bool{false, true} {
		t.Run(fmt.Sprint(corrupt), func(t *testing.T) {
			source, target, marker := pendingConversion(t, "afterActiveRename")
			if corrupt {
				if err := os.WriteFile(backupPath(marker), []byte("invalid"), 0600); err != nil {
					t.Fatal(err)
				}
			} else {
				if err := os.Remove(backupPath(marker)); err != nil {
					t.Fatal(err)
				}
			}
			assertErrorKind(t, retryLegacyHousekeeping(context.Background(), marker), ErrorInvalidState)
			_, err := Open(context.Background(), Config{Path: target})
			assertErrorKind(t, err, ErrorInvalidState)
			assertPathPresent(t, source)
		})
	}
}
func TestPostCommitMarkerFailureStillSucceeds(t *testing.T) {
	for _, boundary := range []string{"markerSync", "markerRename"} {
		t.Run(boundary, func(t *testing.T) {
			source, target := copyFixture(t, "v14")
			installConversionHook(t, func(stage string, marker *LegacyConversionMarker, _ *sql.DB) error {
				if stage == boundary && marker.Phase == LegacyPhaseActiveCommitted {
					return errors.New("post-commit Marker fault")
				}
				return nil
			})
			if err := ConvertLegacy(context.Background(), LegacyConversionConfig{source, target}); err != nil {
				t.Fatal(err)
			}
			marker, err := readConversionMarker(target)
			if err != nil || marker == nil || marker.Phase != LegacyPhaseBackupReady {
				t.Fatalf("Marker=%v err=%v", marker, err)
			}
			assertPathPresent(t, source)
			conversionTestHook = nil
			db, err := Open(context.Background(), Config{Path: target})
			if err != nil {
				t.Fatal(err)
			}
			db.Close()
			assertPathAbsent(t, source)
			assertPathAbsent(t, markerPath(target))
		})
	}
}
func TestWindowsSourceHandlesClosedBeforeHousekeeping(t *testing.T) {
	if runtime.GOOS != "windows" {
		t.Skip("Windows source handles")
	}
	checked := false
	installConversionHook(t, func(stage string, marker *LegacyConversionMarker, _ *sql.DB) error {
		if stage == "sourceReleased" {
			assertWindowsExclusiveSourceOpen(t, marker.SourcePath)
			checked = true
		}
		return nil
	})
	source, _ := convertFixture(t, "v14")
	if !checked {
		t.Fatal("release boundary not reached")
	}
	assertPathAbsent(t, source)
}
func TestOrphanMigratingWithoutMarker(t *testing.T) {
	for _, active := range []string{"absent", "fresh", "current", "legacy"} {
		t.Run(active, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "active.sqlite3")
			switch active {
			case "fresh":
				if err := os.WriteFile(path, nil, 0600); err != nil {
					t.Fatal(err)
				}
			case "current":
				db, err := Open(context.Background(), Config{Path: path})
				if err != nil {
					t.Fatal(err)
				}
				db.Close()
			case "legacy":
				copyFixtureTo(t, "v14", path)
			}
			if err := os.WriteFile(path+".migrating", []byte("orphan"), 0600); err != nil {
				t.Fatal(err)
			}
			_, err := Open(context.Background(), Config{Path: path})
			assertErrorKind(t, err, ErrorInvalidState)
			data, err := os.ReadFile(path + ".migrating")
			if err != nil || string(data) != "orphan" {
				t.Fatalf("orphan changed err=%v", err)
			}
		})
	}
}
func TestCrashInvalidArtifactsPreserved(t *testing.T) {
	for _, scenario := range []string{"invalid-json", "version", "phase", "target", "backup-id", "fresh-active", "missing-source", "replaced-source", "committed-missing-active"} {
		t.Run(scenario, func(t *testing.T) {
			source, target := copyFixture(t, "v14")
			identity, err := readSourceIdentity(source)
			if err != nil {
				t.Fatal(err)
			}
			marker := &LegacyConversionMarker{Version: 1, SourcePath: source, TargetPath: target, BackupID: "0123456789abcdef0123456789abcdef", SourceIdentity: identity, Phase: LegacyPhasePrepared, CreatedAtMS: 1}
			switch scenario {
			case "version":
				marker.Version = 2
			case "phase":
				marker.Phase = "invalid"
			case "backup-id":
				marker.BackupID = "../bad"
			case "fresh-active":
				if err := os.WriteFile(target, nil, 0600); err != nil {
					t.Fatal(err)
				}
			case "missing-source":
				os.Remove(source)
			case "replaced-source":
				replaceSource(t, source, []byte("replaced"))
			case "committed-missing-active":
				marker.Phase = LegacyPhaseActiveCommitted
			}
			if err := writeMarkerAtomic(marker); err != nil {
				t.Fatal(err)
			}
			if scenario == "target" {
				marker.TargetPath = target + "other"
				data, err := json.Marshal(marker)
				if err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(markerPath(target), data, 0600); err != nil {
					t.Fatal(err)
				}
			}
			if scenario == "invalid-json" {
				os.WriteFile(markerPath(target), []byte("{"), 0600)
			}
			os.WriteFile(target+".migrating", []byte("preserve"), 0600)
			_, err = Open(context.Background(), Config{Path: target})
			assertErrorKind(t, err, ErrorInvalidState)
			assertPathPresent(t, markerPath(target))
			assertPathPresent(t, target+".migrating")
		})
	}
}
func TestCheckpointBusyReader(t *testing.T) {
	source, target := copyFixture(t, "v14")
	writer := openTestSQLite(t, source, writerURIParams())
	execTestSQL(t, writer, "UPDATE app_meta SET data_revision=data_revision+1")
	reader := openTestSQLite(t, source, readerURIParams())
	tx, err := reader.BeginTx(context.Background(), &sql.TxOptions{ReadOnly: true})
	if err != nil {
		t.Fatal(err)
	}
	defer tx.Rollback()
	var revision int
	if err := tx.QueryRow("SELECT data_revision FROM app_meta").Scan(&revision); err != nil {
		t.Fatal(err)
	}
	execTestSQL(t, writer, "UPDATE app_meta SET data_revision=data_revision+1")
	err = ConvertLegacy(context.Background(), LegacyConversionConfig{source, target})
	assertErrorKind(t, err, ErrorDatabaseBusy)
	assertPathPresent(t, source)
	assertPathAbsent(t, target)
}
func TestCheckpointBusyWriter(t *testing.T) {
	source, target := copyFixture(t, "v14")
	writer := openTestSQLite(t, source, writerURIParams())
	tx, err := writer.BeginTx(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer tx.Rollback()
	err = ConvertLegacy(context.Background(), LegacyConversionConfig{source, target})
	assertErrorKind(t, err, ErrorDatabaseBusy)
	assertPathPresent(t, source)
	assertPathAbsent(t, target)
}
func TestConversionBlocksIndependentSecondWriter(t *testing.T) {
	attempted := false
	installConversionHook(t, func(stage string, marker *LegacyConversionMarker, _ *sql.DB) error {
		if stage != "snapshot" {
			return nil
		}
		params := maintenanceURIParams()
		params.Set("_busy_timeout", "50")
		second := openTestSQLite(t, marker.SourcePath, params)
		defer second.Close()
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		tx, err := second.BeginTx(ctx, nil)
		if err == nil {
			tx.Rollback()
			t.Fatal("independent Writer entered locked Transaction")
		}
		assertErrorKind(t, mapSQLiteError(err), ErrorDatabaseBusy)
		attempted = true
		return nil
	})
	convertFixture(t, "v14")
	if !attempted {
		t.Fatal("independent Writer not attempted")
	}
}
