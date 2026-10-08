package storage

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/Hogeexxl/Usagi/internal/platform"
)

type SourceFileIdentity struct {
	Platform           string `json:"platform"`
	Device             uint64 `json:"device"`
	Inode              uint64 `json:"inode"`
	VolumeSerialNumber uint64 `json:"volume_serial_number"`
	FileID128          string `json:"file_id_128"`
}
type LegacyConversionPhase string

const (
	LegacyPhasePrepared        LegacyConversionPhase = "prepared"
	LegacyPhaseBackupReady     LegacyConversionPhase = "backup_ready"
	LegacyPhaseActiveCommitted LegacyConversionPhase = "active_committed"
)

type LegacyConversionMarker struct {
	Version        int                   `json:"version"`
	SourcePath     string                `json:"source_path"`
	TargetPath     string                `json:"target_path"`
	BackupID       string                `json:"backup_id"`
	SourceIdentity SourceFileIdentity    `json:"source_identity"`
	Phase          LegacyConversionPhase `json:"phase"`
	CreatedAtMS    int64                 `json:"created_at_ms"`
}
type LegacyConversionConfig struct {
	SourcePath string
	TargetPath string
}

// Package-private, test-only failpoint; there is no runtime configuration.
var conversionTestHook func(string, *LegacyConversionMarker, *sql.DB) error

func conversionBoundary(stage string, marker *LegacyConversionMarker, db *sql.DB) error {
	if conversionTestHook != nil {
		return conversionTestHook(stage, marker, db)
	}
	return nil
}

func markerPath(target string) string { return target + ".legacy-conversion.json" }
func backupPath(marker *LegacyConversionMarker) string {
	return filepath.Join(filepath.Dir(marker.TargetPath), "legacy-backup", marker.BackupID, "legacy.sqlite3")
}

func fileExists(path string) (bool, error) {
	_, err := os.Stat(path)
	if errors.Is(err, os.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, newStorageError(ErrorIO, err)
	}
	return true, nil
}

func readConversionMarker(target string) (*LegacyConversionMarker, error) {
	file, err := os.Open(markerPath(target))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, newStorageError(ErrorIO, err)
	}
	defer file.Close()
	decoder := json.NewDecoder(file)
	decoder.DisallowUnknownFields()
	var marker LegacyConversionMarker
	if err := decoder.Decode(&marker); err != nil {
		return nil, newStorageError(ErrorInvalidState, err)
	}
	var trailing any
	if err := decoder.Decode(&trailing); !errors.Is(err, io.EOF) {
		return nil, newStorageError(ErrorInvalidState, fmt.Errorf("trailing Marker JSON"))
	}
	source, err := platform.NormalizePath(marker.SourcePath)
	if err != nil {
		return nil, newStorageError(ErrorInvalidState, err)
	}
	normalizedTarget, err := platform.NormalizePath(marker.TargetPath)
	if err != nil {
		return nil, newStorageError(ErrorInvalidState, err)
	}
	id, decodeErr := hex.DecodeString(marker.BackupID)
	validPhase := marker.Phase == LegacyPhasePrepared || marker.Phase == LegacyPhaseBackupReady || marker.Phase == LegacyPhaseActiveCommitted
	identity := marker.SourceIdentity
	validIdentity := identity.Platform == "unix" && identity.VolumeSerialNumber == 0 && identity.FileID128 == ""
	if identity.Platform == "windows" {
		fileID, err := hex.DecodeString(identity.FileID128)
		validIdentity = err == nil && len(fileID) == 16 && identity.FileID128 == strings.ToLower(identity.FileID128) && identity.Device == 0 && identity.Inode == 0
	}
	if marker.Version != 1 || !validPhase || !validIdentity || decodeErr != nil || len(id) != 16 || marker.BackupID != strings.ToLower(marker.BackupID) || source != marker.SourcePath || normalizedTarget != marker.TargetPath || normalizedTarget != target || source == target || marker.CreatedAtMS < 0 {
		return nil, newStorageError(ErrorInvalidState, fmt.Errorf("invalid or mismatched Conversion Marker for %s", target))
	}
	return &marker, nil
}

func writeMarkerAtomic(marker *LegacyConversionMarker) (err error) {
	target := markerPath(marker.TargetPath)
	file, err := os.CreateTemp(filepath.Dir(target), ".legacy-marker-*")
	if err != nil {
		return newStorageError(ErrorIO, err)
	}
	temporary := file.Name()
	defer os.Remove(temporary)
	if err := json.NewEncoder(file).Encode(marker); err != nil {
		_ = file.Close()
		return newStorageError(ErrorIO, err)
	}
	if err := conversionBoundary("markerSync", marker, nil); err != nil {
		_ = file.Close()
		return newStorageError(ErrorIO, err)
	}
	if err := file.Sync(); err != nil {
		_ = file.Close()
		return newStorageError(ErrorIO, err)
	}
	if err := file.Close(); err != nil {
		return newStorageError(ErrorIO, err)
	}
	if err := conversionBoundary("markerRename", marker, nil); err != nil {
		return newStorageError(ErrorIO, err)
	}
	if err := os.Rename(temporary, target); err != nil {
		return newStorageError(ErrorIO, err)
	}
	return syncDirectory(filepath.Dir(target))
}

func inspectCrashArtifacts(ctx context.Context, target string) (*LegacyConversionMarker, error) {
	marker, err := readConversionMarker(target)
	if err != nil {
		return nil, err
	}
	active, err := fileExists(target)
	if err != nil {
		return nil, err
	}
	migrating, err := fileExists(target + ".migrating")
	if err != nil {
		return nil, err
	}
	if marker == nil {
		if migrating {
			return nil, newStorageError(ErrorInvalidState, fmt.Errorf("orphan Temp Target %s.migrating", target))
		}
		return nil, nil
	}
	if err := validateConversionVolume(target); err != nil {
		return nil, err
	}
	if active {
		class, err := classifyDatabase(ctx, target)
		if err != nil || class != schemaCurrent {
			return nil, newStorageError(ErrorInvalidState, errors.Join(errors.New("Crash Artifact with non-Current Active"), err))
		}
		return marker, nil
	}
	if marker.Phase == LegacyPhaseActiveCommitted {
		return nil, newStorageError(ErrorInvalidState, fmt.Errorf("committed Active is missing"))
	}
	identity, err := readSourceIdentity(marker.SourcePath)
	if err != nil || identity != marker.SourceIdentity {
		return nil, newStorageError(ErrorInvalidState, errors.Join(errors.New("Pre-commit Source Identity differs"), err))
	}
	if err := removeIfExists(target + ".migrating"); err != nil {
		return nil, err
	}
	if err := os.RemoveAll(filepath.Dir(backupPath(marker))); err != nil {
		return nil, newStorageError(ErrorIO, err)
	}
	if err := removeIfExists(markerPath(target)); err != nil {
		return nil, err
	}
	if err := syncDirectory(filepath.Dir(target)); err != nil {
		return nil, err
	}
	if err := ConvertLegacy(ctx, LegacyConversionConfig{SourcePath: marker.SourcePath, TargetPath: target}); err != nil {
		return nil, err
	}
	return readConversionMarker(target)
}

func recoverActiveMarker(ctx context.Context, marker *LegacyConversionMarker) error {
	if err := validateConversionVolume(marker.TargetPath); err != nil {
		return err
	}
	if err := validateCurrentPath(ctx, marker.TargetPath); err != nil {
		return err
	}
	if err := removeIfExists(marker.TargetPath + ".migrating"); err != nil {
		_, gateErr := validateLegacyRetirement(ctx, marker)
		return gateErr
	}
	if marker.Phase != LegacyPhaseActiveCommitted {
		marker.Phase = LegacyPhaseActiveCommitted
		// Current Validation establishes the earlier commit. Marker repair is
		// best effort even on reentry; the identity and backup gates still run.
		if err := writeMarkerAtomic(marker); err != nil {
			_, gateErr := validateLegacyRetirement(ctx, marker)
			return gateErr
		}
	}
	err := retryLegacyHousekeeping(ctx, marker)
	var storageErr *Error
	if errors.As(err, &storageErr) && storageErr.Kind == ErrorIO {
		return nil
	}
	if err != nil {
		return err
	}
	return nil
}

func openLegacyConnection(path string, params url.Values) (*sql.DB, error) {
	uri, err := buildSQLiteURI(path, params)
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

func ConvertLegacy(ctx context.Context, cfg LegacyConversionConfig) (err error) {
	source, err := platform.NormalizePath(cfg.SourcePath)
	if err != nil {
		return newStorageError(ErrorIO, err)
	}
	target, err := platform.NormalizePath(cfg.TargetPath)
	if err != nil {
		return newStorageError(ErrorIO, err)
	}
	if source == target {
		return newStorageError(ErrorInvalidState, fmt.Errorf("Source and Target must differ"))
	}
	if err := validateConversionVolume(target); err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(target), 0700); err != nil {
		return newStorageError(ErrorIO, err)
	}
	marker, err := readConversionMarker(target)
	if err != nil {
		return err
	}
	active, err := fileExists(target)
	if err != nil {
		return err
	}
	if active {
		if marker == nil || marker.SourcePath != source {
			return newStorageError(ErrorInvalidState, fmt.Errorf("existing Target requires a matching Marker"))
		}
		class, classErr := classifyDatabase(ctx, target)
		if classErr != nil || class != schemaCurrent {
			return newStorageError(ErrorInvalidState, fmt.Errorf("existing Target is not Current"))
		}
		return recoverActiveMarker(ctx, marker)
	}
	if marker != nil {
		if marker.SourcePath != source {
			return newStorageError(ErrorInvalidState, fmt.Errorf("Marker Source differs"))
		}
		_, err := inspectCrashArtifacts(ctx, target)
		return err
	}
	if migrating, err := fileExists(target + ".migrating"); err != nil {
		return err
	} else if migrating {
		return newStorageError(ErrorInvalidState, fmt.Errorf("orphan Temp Target"))
	}
	match, err := preflightLegacySource(ctx, source)
	if err != nil {
		return err
	}
	identity, err := readSourceIdentity(source)
	if err != nil {
		return err
	}
	var randomID [16]byte
	if _, err := rand.Read(randomID[:]); err != nil {
		return newStorageError(ErrorIO, err)
	}
	marker = &LegacyConversionMarker{Version: 1, SourcePath: source, TargetPath: target, BackupID: hex.EncodeToString(randomID[:]), SourceIdentity: identity, Phase: LegacyPhasePrepared, CreatedAtMS: time.Now().UnixMilli()}
	if err := writeMarkerAtomic(marker); err != nil {
		return err
	}
	if err := conversionBoundary("prepared", marker, nil); err != nil {
		return err
	}
	maintenance, err := openLegacyConnection(source, maintenanceURIParams())
	if err != nil {
		return err
	}
	var maintenanceConn *sql.Conn
	var maintenanceTx, importTx *sql.Tx
	var importer *sql.DB
	committed := false
	release := func() error {
		var result []error
		if importTx != nil {
			e := importTx.Rollback()
			if !errors.Is(e, sql.ErrTxDone) {
				result = append(result, mapSQLiteError(e))
			}
			importTx = nil
		}
		if importer != nil {
			result = append(result, mapSQLiteError(importer.Close()))
			importer = nil
		}
		if maintenanceTx != nil {
			e := maintenanceTx.Rollback()
			if !errors.Is(e, sql.ErrTxDone) {
				result = append(result, mapSQLiteError(e))
			}
			maintenanceTx = nil
		}
		if maintenanceConn != nil {
			result = append(result, mapSQLiteError(maintenanceConn.Close()))
			maintenanceConn = nil
		}
		if maintenance != nil {
			result = append(result, mapSQLiteError(maintenance.Close()))
			maintenance = nil
		}
		return errors.Join(result...)
	}
	defer func() {
		releaseErr := release()
		if committed {
			err = nil
		} else {
			err = errors.Join(err, releaseErr)
		}
	}()
	maintenanceConn, err = maintenance.Conn(ctx)
	if err != nil {
		return mapSQLiteError(err)
	}
	var busy, log, checkpointed int
	if err := maintenanceConn.QueryRowContext(ctx, "PRAGMA wal_checkpoint(TRUNCATE)").Scan(&busy, &log, &checkpointed); err != nil {
		return mapSQLiteError(err)
	}
	if busy != 0 {
		return newStorageError(ErrorDatabaseBusy, fmt.Errorf("checkpoint busy=%d log=%d checkpointed=%d", busy, log, checkpointed))
	}
	maintenanceTx, err = maintenanceConn.BeginTx(ctx, nil)
	if err != nil {
		return mapSQLiteError(err)
	}
	if err := conversionBoundary("locked", marker, maintenance); err != nil {
		return err
	}
	identity, err = readSourceIdentity(source)
	if err != nil {
		return err
	}
	if identity != marker.SourceIdentity {
		return newStorageError(ErrorInvalidState, fmt.Errorf("Source Identity changed after locking"))
	}
	importer, err = openLegacyConnection(source, importURIParams())
	if err != nil {
		return err
	}
	importTx, err = importer.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return mapSQLiteError(err)
	}
	var rowCount int
	if err := importTx.QueryRowContext(ctx, "SELECT count(*) FROM app_meta").Scan(&rowCount); err != nil {
		return mapSQLiteError(err)
	}
	if err := conversionBoundary("snapshot", marker, importer); err != nil {
		return err
	}
	if err := buildTempTarget(ctx, marker, match, importTx); err != nil {
		return err
	}
	if err := conversionBoundary("tempFinalized", marker, nil); err != nil {
		return err
	}
	if err := createLegacyBackup(ctx, marker); err != nil {
		return err
	}
	marker.Phase = LegacyPhaseBackupReady
	if err := writeMarkerAtomic(marker); err != nil {
		return err
	}
	if err := conversionBoundary("beforeActiveRename", marker, nil); err != nil {
		return err
	}
	if err := os.Rename(target+".migrating", target); err != nil {
		_ = removeIfExists(target + ".migrating")
		return newStorageError(ErrorIO, err)
	}
	if err := syncDirectory(filepath.Dir(target)); err != nil {
		return err
	}
	committed = true
	if err := conversionBoundary("afterActiveRename", marker, nil); err != nil {
		return nil
	}
	marker.Phase = LegacyPhaseActiveCommitted
	if err := conversionBoundary("beforeCommittedMarker", marker, nil); err != nil {
		return nil
	}
	if err := writeMarkerAtomic(marker); err != nil {
		return nil
	}
	if err := release(); err != nil {
		return nil
	}
	if err := conversionBoundary("sourceReleased", marker, nil); err != nil {
		return nil
	}
	_ = retryLegacyHousekeeping(ctx, marker)
	return nil
}

func buildTempTarget(ctx context.Context, marker *LegacyConversionMarker, match legacyProfileMatch, source *sql.Tx) (err error) {
	path := marker.TargetPath + ".migrating"
	db, err := openLegacyConnection(path, url.Values{"_journal_mode": {"DELETE"}, "_synchronous": {"FULL"}, "_busy_timeout": {"5000"}, "_foreign_keys": {"0"}})
	if err != nil {
		return err
	}
	closed := false
	validationFailed := false
	defer func() {
		if !closed {
			err = errors.Join(err, mapSQLiteError(db.Close()))
		}
		if validationFailed {
			err = errors.Join(err, removeIfExists(path))
		}
	}()
	var foreignKeys int
	if err := db.QueryRowContext(ctx, "PRAGMA foreign_keys").Scan(&foreignKeys); err != nil {
		return mapSQLiteError(err)
	}
	if foreignKeys != 0 {
		return newStorageError(ErrorInvalidState, fmt.Errorf("Temp foreign_keys must be OFF before Transaction"))
	}
	if _, err := db.ExecContext(ctx, currentSchemaSQL); err != nil {
		return mapSQLiteError(err)
	}
	if err := conversionBoundary("tempBeforeImport", marker, db); err != nil {
		return err
	}
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return mapSQLiteError(err)
	}
	defer tx.Rollback()
	if err := importLegacy(ctx, match.Version, match.Variant, source, tx); err != nil {
		return err
	}
	if err := tx.Commit(); err != nil {
		return mapSQLiteError(err)
	}
	if err := conversionBoundary("tempAfterImport", marker, db); err != nil {
		return err
	}
	if err := checkForeignKeys(ctx, db); err != nil {
		return err
	}
	if _, err := db.ExecContext(ctx, "PRAGMA foreign_keys=ON"); err != nil {
		return mapSQLiteError(err)
	}
	if err := db.QueryRowContext(ctx, "PRAGMA foreign_keys").Scan(&foreignKeys); err != nil {
		return mapSQLiteError(err)
	}
	if foreignKeys != 1 {
		return newStorageError(ErrorInvalidState, fmt.Errorf("Temp foreign_keys did not enable"))
	}
	if err := conversionBoundary("tempForeignKeysOn", marker, db); err != nil {
		return err
	}
	if err := validateCurrent(ctx, db); err != nil {
		validationFailed = true
		return err
	}
	if err := db.Close(); err != nil {
		closed = true
		return mapSQLiteError(err)
	}
	closed = true
	if err := syncFile(path); err != nil {
		return err
	}
	if err := syncDirectory(filepath.Dir(path)); err != nil {
		return err
	}
	for _, suffix := range []string{"-wal", "-shm", "-journal"} {
		if exists, err := fileExists(path + suffix); err != nil {
			return err
		} else if exists {
			return newStorageError(ErrorInvalidState, fmt.Errorf("Temp Sidecar remains: %s", suffix))
		}
	}
	return nil
}

func syncFile(path string) error {
	file, err := os.OpenFile(path, os.O_RDWR, 0)
	if err != nil {
		return newStorageError(ErrorIO, err)
	}
	defer file.Close()
	return newStorageError(ErrorIO, file.Sync())
}

func createLegacyBackup(ctx context.Context, marker *LegacyConversionMarker) error {
	path := backupPath(marker)
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return newStorageError(ErrorIO, err)
	}
	source, err := os.Open(marker.SourcePath)
	if err != nil {
		return newStorageError(ErrorIO, err)
	}
	defer source.Close()
	file, err := os.OpenFile(path+".partial", os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return newStorageError(ErrorIO, err)
	}
	if _, err := io.Copy(file, source); err != nil {
		_ = file.Close()
		return newStorageError(ErrorIO, err)
	}
	if err := file.Sync(); err != nil {
		_ = file.Close()
		return newStorageError(ErrorIO, err)
	}
	if err := file.Close(); err != nil {
		return newStorageError(ErrorIO, err)
	}
	if err := conversionBoundary("backupPartial", marker, nil); err != nil {
		return err
	}
	if err := os.Rename(path+".partial", path); err != nil {
		return newStorageError(ErrorIO, err)
	}
	db, err := openInspection(path)
	if err != nil {
		return err
	}
	defer db.Close()
	return quickCheck(ctx, db)
}

func removeIfExists(path string) error {
	err := os.Remove(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	return newStorageError(ErrorIO, err)
}

func validateLegacyRetirement(ctx context.Context, marker *LegacyConversionMarker) (bool, error) {
	path := backupPath(marker)
	if exists, err := fileExists(path); err != nil || !exists {
		return false, newStorageError(ErrorInvalidState, errors.Join(errors.New("verified Backup is required"), err))
	}
	backup, err := openInspection(path)
	if err != nil {
		return false, newStorageError(ErrorInvalidState, err)
	}
	err = quickCheck(ctx, backup)
	closeErr := backup.Close()
	if err != nil || closeErr != nil {
		return false, newStorageError(ErrorInvalidState, errors.Join(err, closeErr))
	}
	present, err := fileExists(marker.SourcePath)
	if err != nil {
		return false, err
	}
	if present {
		identity, err := readSourceIdentity(marker.SourcePath)
		if err != nil || identity != marker.SourceIdentity {
			return false, newStorageError(ErrorInvalidState, errors.Join(errors.New("Housekeeping Source Identity differs"), err))
		}
	} else {
		for _, suffix := range []string{"-wal", "-shm"} {
			if exists, err := fileExists(marker.SourcePath + suffix); err != nil {
				return false, err
			} else if exists {
				return false, newStorageError(ErrorInvalidState, fmt.Errorf("Source Main absent with Sidecar %s", suffix))
			}
		}
	}
	return present, nil
}

func retryLegacyHousekeeping(ctx context.Context, marker *LegacyConversionMarker) error {
	if err := validateConversionVolume(marker.TargetPath); err != nil {
		return err
	}
	present, err := validateLegacyRetirement(ctx, marker)
	if err != nil {
		return err
	}
	if present {
		for _, suffix := range []string{"-wal", "-shm", ""} {
			if err := conversionBoundary("deleteSource"+suffix, marker, nil); err != nil {
				return newStorageError(ErrorIO, err)
			}
			if err := removeIfExists(marker.SourcePath + suffix); err != nil {
				return err
			}
		}
	}
	if err := removeIfExists(markerPath(marker.TargetPath)); err != nil {
		return err
	}
	return syncDirectory(filepath.Dir(marker.TargetPath))
}
