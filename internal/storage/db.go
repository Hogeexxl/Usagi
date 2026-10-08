package storage

import (
	"context"
	"database/sql"
	"errors"
	"os"
	"sync"

	"github.com/Hogeexxl/Usagi/internal/platform"

	_ "modernc.org/sqlite"
)

type Config struct {
	Path string
}

type DB struct {
	writer    *sql.DB
	readers   *sql.DB
	path      string
	closeOnce sync.Once
	closeErr  error
}

func Open(ctx context.Context, cfg Config) (*DB, error) {
	paths, err := platform.ResolvePaths(cfg.Path)
	if err != nil {
		return nil, newStorageError(ErrorIO, err)
	}
	if _, err := inspectCrashArtifacts(ctx, paths.ActivePath); err != nil {
		return nil, err
	}
	_, err = os.Stat(paths.ActivePath)
	if errors.Is(err, os.ErrNotExist) {
		converted := false
		for _, candidate := range paths.LegacyCandidates {
			if _, err := os.Stat(candidate); errors.Is(err, os.ErrNotExist) {
				continue
			} else if err != nil {
				return nil, newStorageError(ErrorIO, err)
			}
			if _, err := preflightLegacySource(ctx, candidate); err != nil {
				return nil, err
			}
			if err := ConvertLegacy(ctx, LegacyConversionConfig{SourcePath: candidate, TargetPath: paths.ActivePath}); err != nil {
				return nil, err
			}
			converted = true
			break
		}
		if !converted {
			if err := createFreshCurrent(ctx, paths.ActivePath); err != nil {
				return nil, err
			}
		}
	} else if err != nil {
		return nil, newStorageError(ErrorIO, err)
	}
	class, err := classifyDatabase(ctx, paths.ActivePath)
	if err != nil {
		return nil, err
	}
	switch class {
	case schemaFresh:
		if err := createFreshCurrent(ctx, paths.ActivePath); err != nil {
			return nil, err
		}
	case schemaRustLegacy:
		return nil, newStorageError(ErrorLegacyRequiresConversion, errors.New("explicit conversion required for Rust Legacy Active"))
	}
	if err := validateCurrentPath(ctx, paths.ActivePath); err != nil {
		return nil, err
	}
	marker, err := readConversionMarker(paths.ActivePath)
	if err != nil {
		return nil, err
	}
	if marker != nil {
		if err := recoverActiveMarker(ctx, marker); err != nil {
			return nil, err
		}
	}
	writer, err := openWriter(paths.ActivePath)
	if err != nil {
		return nil, err
	}
	readers, err := openReaders(paths.ActivePath)
	if err != nil {
		return nil, errors.Join(err, mapSQLiteError(writer.Close()))
	}
	return &DB{writer: writer, readers: readers, path: paths.ActivePath}, nil
}

func (db *DB) Path() string { return db.path }

func (db *DB) Validate(ctx context.Context) error {
	return db.Read(ctx, func(conn *sql.Conn) error {
		return validateCurrent(ctx, conn)
	})
}

func (db *DB) SchemaVersion(ctx context.Context) (SchemaVersion, error) {
	var version SchemaVersion
	err := db.Read(ctx, func(conn *sql.Conn) error {
		return mapSQLiteError(conn.QueryRowContext(ctx, "SELECT generation,version FROM schema_meta WHERE id=1").Scan(&version.Generation, &version.Version))
	})
	return version, err
}

func openWriter(path string) (*sql.DB, error) {
	uri, err := buildSQLiteURI(path, writerURIParams())
	if err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", uri)
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	if err := db.PingContext(context.Background()); err != nil {
		return nil, closeAfterOpenError(db, err)
	}
	return db, nil
}

func openReaders(path string) (*sql.DB, error) {
	uri, err := buildSQLiteURI(path, readerURIParams())
	if err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", uri)
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	db.SetMaxOpenConns(4)
	db.SetMaxIdleConns(4)
	if err := db.PingContext(context.Background()); err != nil {
		return nil, closeAfterOpenError(db, err)
	}
	return db, nil
}

func openInspection(path string) (*sql.DB, error) {
	uri, err := buildSQLiteURI(path, inspectionURIParams())
	if err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", uri)
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	return db, nil
}

func closeAfterOpenError(db *sql.DB, openErr error) error {
	closeErr := db.Close()
	if closeErr != nil {
		return errors.Join(mapSQLiteError(openErr), mapSQLiteError(closeErr))
	}
	return mapSQLiteError(openErr)
}

func (db *DB) Close() error {
	db.closeOnce.Do(func() {
		readerErr := db.readers.Close()
		writerErr := db.writer.Close()
		if readerErr != nil {
			readerErr = mapSQLiteError(readerErr)
		}
		if writerErr != nil {
			writerErr = mapSQLiteError(writerErr)
		}
		db.closeErr = errors.Join(readerErr, writerErr)
	})
	return db.closeErr
}

func (db *DB) Read(ctx context.Context, fn func(*sql.Conn) error) error {
	conn, err := db.readers.Conn(ctx)
	if err != nil {
		return mapSQLiteError(err)
	}
	defer conn.Close()
	return fn(conn)
}

func (db *DB) ReadTx(ctx context.Context, fn func(*sql.Tx) error) error {
	tx, err := db.readers.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return mapSQLiteError(err)
	}
	defer func() { _ = tx.Rollback() }()
	if callbackErr := fn(tx); callbackErr != nil {
		rollbackErr := tx.Rollback()
		if rollbackErr != nil && !errors.Is(rollbackErr, sql.ErrTxDone) {
			return errors.Join(callbackErr, mapSQLiteError(rollbackErr))
		}
		return callbackErr
	}
	return mapSQLiteError(tx.Commit())
}
