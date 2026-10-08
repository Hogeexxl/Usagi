package storage

import (
	"context"
	"database/sql"
	_ "embed"
	"fmt"
	"os"
	"path/filepath"
)

//go:embed schema.sql
var currentSchemaSQL string

type SchemaVersion struct {
	Generation int
	Version    int
}
type schemaClass uint8

const (
	schemaFresh schemaClass = iota
	schemaRustLegacy
	schemaCurrent
	schemaTooNew
	schemaInvalid
)

func classifyDatabase(ctx context.Context, path string) (schemaClass, error) {
	db, err := openInspection(path)
	if err != nil {
		return schemaInvalid, err
	}
	defer db.Close()
	var version int
	if err := db.QueryRowContext(ctx, "PRAGMA user_version").Scan(&version); err != nil {
		return schemaInvalid, mapSQLiteError(err)
	}
	var marker int
	if err := db.QueryRowContext(ctx, "SELECT count(*) FROM sqlite_schema WHERE name='schema_meta' AND type='table'").Scan(&marker); err != nil {
		return schemaInvalid, mapSQLiteError(err)
	}
	if marker == 0 {
		switch {
		case version == 0:
			var objects int
			if err := db.QueryRowContext(ctx, "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'").Scan(&objects); err != nil {
				return schemaInvalid, mapSQLiteError(err)
			}
			if objects == 0 {
				return schemaFresh, nil
			}
		case version >= 1 && version <= 14:
			// Release the inspection handle before preflight opens its own connection.
			if err := db.Close(); err != nil {
				return schemaInvalid, mapSQLiteError(err)
			}
			if _, err := preflightLegacySource(ctx, path); err != nil {
				return schemaInvalid, err
			}
			return schemaRustLegacy, nil
		case version >= 15 && version <= 999:
			return schemaTooNew, newStorageError(ErrorSchemaTooNew, fmt.Errorf("Rust schema version %d", version))
		}
		return schemaInvalid, schemaMismatch("database", path, fmt.Errorf("unrecognized schema version %d", version))
	}
	columns, err := inspectColumns(ctx, db, "schema_meta")
	if err != nil {
		return schemaInvalid, err
	}
	required := map[string]bool{"id": false, "generation": false, "version": false}
	for _, column := range columns {
		if _, ok := required[column.name]; ok {
			required[column.name] = true
		}
	}
	for name, present := range required {
		if !present {
			return schemaInvalid, schemaMismatch("table", "schema_meta", fmt.Errorf("missing column %s", name))
		}
	}
	rows, err := db.QueryContext(ctx, "SELECT id,generation,version FROM schema_meta")
	if err != nil {
		return schemaInvalid, mapSQLiteError(err)
	}
	defer rows.Close()
	var id, generation, currentVersion, count int
	for rows.Next() {
		count++
		if err := rows.Scan(&id, &generation, &currentVersion); err != nil {
			return schemaInvalid, schemaMismatch("table", "schema_meta", err)
		}
	}
	if err := rows.Err(); err != nil {
		return schemaInvalid, mapSQLiteError(err)
	}
	if count != 1 || id != 1 {
		return schemaInvalid, schemaMismatch("table", "schema_meta", fmt.Errorf("expected only id=1"))
	}
	if generation > 1 || generation == 1 && currentVersion > 1 {
		return schemaTooNew, newStorageError(ErrorSchemaTooNew, fmt.Errorf("Go schema %d/%d", generation, currentVersion))
	}
	if generation != 1 || currentVersion != 1 || version != 1000 {
		return schemaInvalid, schemaMismatch("table", "schema_meta", fmt.Errorf("marker %d/%d disagrees with user_version=%d", generation, currentVersion, version))
	}
	return schemaCurrent, nil
}

// Test-only injection at the transactional Fresh Create boundary.
var freshCreateTestHook func(*sql.Tx) error

func createFreshCurrent(ctx context.Context, path string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return newStorageError(ErrorIO, err)
	}
	writer, err := openWriter(path)
	if err != nil {
		return err
	}
	defer writer.Close()
	db := &DB{writer: writer}
	return db.WriteTx(ctx, func(tx *Tx) error {
		if _, err := tx.tx.ExecContext(ctx, currentSchemaSQL); err != nil {
			return mapSQLiteError(err)
		}
		if freshCreateTestHook != nil {
			return freshCreateTestHook(tx.tx)
		}
		return nil
	})
}

func buildReferenceSnapshot() (schemaSnapshot, error) {
	db, err := sql.Open("sqlite", ":memory:")
	if err != nil {
		return schemaSnapshot{}, mapSQLiteError(err)
	}
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	defer db.Close()
	if _, err := db.ExecContext(context.Background(), currentSchemaSQL); err != nil {
		return schemaSnapshot{}, mapSQLiteError(err)
	}
	return inspectSchema(context.Background(), db)
}

func quickCheck(ctx context.Context, queryer schemaQueryer) error {
	rows, err := queryer.QueryContext(ctx, "PRAGMA quick_check")
	if err != nil {
		return mapSQLiteError(err)
	}
	defer rows.Close()
	var result string
	count := 0
	for rows.Next() {
		count++
		if err := rows.Scan(&result); err != nil {
			return mapSQLiteError(err)
		}
		if result != "ok" {
			return newStorageError(ErrorDatabaseCorrupt, fmt.Errorf("quick_check: %s", result))
		}
	}
	if err := rows.Err(); err != nil {
		return mapSQLiteError(err)
	}
	if count != 1 {
		return newStorageError(ErrorDatabaseCorrupt, fmt.Errorf("quick_check returned %d rows", count))
	}
	return nil
}

func checkForeignKeys(ctx context.Context, queryer schemaQueryer) error {
	rows, err := queryer.QueryContext(ctx, "PRAGMA foreign_key_check")
	if err != nil {
		return mapSQLiteError(err)
	}
	defer rows.Close()
	if rows.Next() {
		return newStorageError(ErrorDataIntegrity, fmt.Errorf("foreign_key_check returned a violation"))
	}
	return mapSQLiteError(rows.Err())
}

func validateCurrent(ctx context.Context, queryer schemaQueryer) error {
	if err := quickCheck(ctx, queryer); err != nil {
		return err
	}
	if err := checkForeignKeys(ctx, queryer); err != nil {
		return err
	}
	actual, err := inspectSchema(ctx, queryer)
	if err != nil {
		return err
	}
	reference, err := buildReferenceSnapshot()
	if err != nil {
		return err
	}
	if err := compareSchemaSnapshots(reference, actual); err != nil {
		return err
	}
	for _, query := range []string{
		"SELECT count(*) FROM app_meta WHERE id=1",
		"SELECT count(*) FROM codex_adapter_state WHERE id=1",
		"SELECT count(*) FROM source_usage_epochs WHERE source='codex'",
	} {
		var count int
		if err := queryer.QueryRowContext(ctx, query).Scan(&count); err != nil {
			return mapSQLiteError(err)
		}
		if count != 1 {
			return newStorageError(ErrorDataIntegrity, fmt.Errorf("required row count %d for %s", count, query))
		}
	}
	return nil
}

func validateCurrentPath(ctx context.Context, path string) error {
	db, err := openInspection(path)
	if err != nil {
		return err
	}
	defer db.Close()
	return validateCurrent(ctx, db)
}
