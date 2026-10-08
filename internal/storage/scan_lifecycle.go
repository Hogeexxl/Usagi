package storage

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"sort"
	"strings"
	"unicode"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

var followupStartFailedCodes = map[string]struct{}{
	"SCAN_START_FAILED":   {},
	"SCANNER_UNAVAILABLE": {},
	"SOURCE_CHANGED":      {},
}

type sourceManifest []domain.SourceID

func (db *DB) ScanStatusSnapshot(
	ctx context.Context,
	targetScanID *string,
) (domain.ScanStatusSnapshot, error) {
	if targetScanID != nil {
		if err := validateLifecycleScanID(*targetScanID); err != nil {
			return domain.ScanStatusSnapshot{}, err
		}
	}

	var snapshot domain.ScanStatusSnapshot
	err := db.ReadTx(ctx, func(tx *sql.Tx) error {
		appState, err := readAppStateTx(ctx, tx)
		if err != nil {
			return err
		}

		var targetScan *domain.ScanRun
		if targetScanID != nil {
			targetScan, err = readScanRunTx(ctx, tx, *targetScanID)
			if err != nil {
				return err
			}
		}

		sourceScanID := targetScanID
		if sourceScanID == nil {
			sourceScanID = appState.Scan.ActiveScanID
		}
		if sourceScanID == nil {
			sourceScanID = appState.Scan.LastFinishedScanID
		}

		var sources []domain.SourceScanStatus
		if sourceScanID != nil {
			sources, err = readSourceScanStatusesTx(ctx, tx, *sourceScanID)
			if err != nil {
				return err
			}
		}

		snapshot = domain.ScanStatusSnapshot{
			AppState:   appState,
			TargetScan: targetScan,
			Sources:    sources,
		}
		if err := snapshot.Validate(); err != nil {
			return lifecycleInvalidState(err)
		}
		return nil
	})
	if err != nil {
		return domain.ScanStatusSnapshot{}, err
	}
	return snapshot, nil
}

func (db *DB) MarkScanStartedWithSources(
	ctx context.Context,
	event domain.ScanStartEvent,
	sources []domain.SourceID,
) (domain.ScanState, error) {
	if err := event.Validate(); err != nil {
		return domain.ScanState{}, lifecycleInvalidState(err)
	}
	manifest, err := validateSourceManifest(sources)
	if err != nil {
		return domain.ScanState{}, err
	}

	var state domain.ScanState
	err = db.WriteTx(ctx, func(storageTx *Tx) error {
		tx := storageTx.tx
		current, err := readAppStateTx(ctx, tx)
		if err != nil {
			return err
		}
		if current.Scan.ActiveScanID != nil || current.Scan.State == domain.ScanLifecycleRunning {
			return lifecycleInvalidState(errors.New("a scan is already active"))
		}
		if current.Scan.FollowupState != nil {
			if *current.Scan.FollowupState == domain.FollowupQueued {
				return lifecycleInvalidState(errors.New("a follow-up scan is queued"))
			}
			if *current.Scan.FollowupState == domain.FollowupStartFailed {
				if err := ensureStartFailedFollowupProjectionTx(ctx, tx, current.Scan); err != nil {
					return err
				}
			}
		}

		revision, err := storageTx.BumpStatusRevision(ctx)
		if err != nil {
			return err
		}
		_, err = tx.ExecContext(ctx,
			`INSERT INTO scan_runs (
				scan_id, trigger, request_kind, state, requested_at_ms,
				enqueued_status_revision, started_at_ms, started_status_revision
			) VALUES (?, ?, 'direct', 'running', ?, NULL, ?, ?)`,
			event.ScanID, string(event.Trigger), event.RequestedAtMS, event.StartedAtMS, revision,
		)
		if err != nil {
			return mapSQLiteError(err)
		}
		if err := insertSourceScanManifestTx(ctx, tx, event.ScanID, manifest); err != nil {
			return err
		}
		if err := executeLifecycleOne(ctx, tx,
			`UPDATE app_meta SET scan_state = 'running', active_scan_id = ?,
			 last_scan_started_at_ms = ?, followup_scan_id = NULL, followup_state = NULL,
			 followup_trigger = NULL, followup_requested_at_ms = NULL,
			 followup_enqueued_status_revision = NULL, followup_error_code = NULL
			 WHERE id = 1`,
			"update app_meta for direct scan start", event.ScanID, event.StartedAtMS,
		); err != nil {
			return err
		}

		state, err = readScanStateTx(ctx, tx)
		return err
	})
	if err != nil {
		return domain.ScanState{}, err
	}
	return state, nil
}

func (db *DB) ReserveScanFollowup(
	ctx context.Context,
	event domain.ReserveScanFollowupEvent,
) (domain.ScanState, error) {
	if err := event.Validate(); err != nil {
		return domain.ScanState{}, lifecycleInvalidState(err)
	}

	var state domain.ScanState
	err := db.WriteTx(ctx, func(storageTx *Tx) error {
		tx := storageTx.tx
		current, err := readAppStateTx(ctx, tx)
		if err != nil {
			return err
		}
		if current.Scan.State != domain.ScanLifecycleRunning || current.Scan.ActiveScanID == nil {
			return lifecycleInvalidState(errors.New("follow-up reservation requires an active scan"))
		}
		if err := ensureScanRunStateTx(ctx, tx, *current.Scan.ActiveScanID, domain.ScanRunRunning); err != nil {
			return err
		}
		if current.Scan.FollowupState != nil {
			if *current.Scan.FollowupState == domain.FollowupQueued {
				if err := ensureQueuedFollowupProjectionTx(ctx, tx, current.Scan); err != nil {
					return err
				}
				state = current.Scan
				return nil
			}
			return lifecycleInvalidState(errors.New("follow-up slot is not available"))
		}

		revision, err := storageTx.BumpStatusRevision(ctx)
		if err != nil {
			return err
		}
		_, err = tx.ExecContext(ctx,
			`INSERT INTO scan_runs (
				scan_id, trigger, request_kind, state, requested_at_ms,
				enqueued_status_revision
			) VALUES (?, ?, 'followup', 'queued', ?, ?)`,
			event.FollowupScanID, string(event.Trigger), event.RequestedAtMS, revision,
		)
		if err != nil {
			return mapSQLiteError(err)
		}
		if err := executeLifecycleOne(ctx, tx,
			`UPDATE app_meta SET followup_scan_id = ?, followup_state = 'queued',
			 followup_trigger = ?, followup_requested_at_ms = ?,
			 followup_enqueued_status_revision = ?, followup_error_code = NULL
			 WHERE id = 1`,
			"update app_meta for follow-up reservation",
			event.FollowupScanID, string(event.Trigger), event.RequestedAtMS, revision,
		); err != nil {
			return err
		}

		state, err = readScanStateTx(ctx, tx)
		return err
	})
	if err != nil {
		return domain.ScanState{}, err
	}
	return state, nil
}

func (db *DB) MarkFollowupStartedWithSources(
	ctx context.Context,
	event domain.FollowupStartedEvent,
	sources []domain.SourceID,
) (domain.ScanState, error) {
	if err := event.Validate(); err != nil {
		return domain.ScanState{}, lifecycleInvalidState(err)
	}
	manifest, err := validateSourceManifest(sources)
	if err != nil {
		return domain.ScanState{}, err
	}

	var state domain.ScanState
	err = db.WriteTx(ctx, func(storageTx *Tx) error {
		tx := storageTx.tx
		current, err := readAppStateTx(ctx, tx)
		if err != nil {
			return err
		}
		if current.Scan.ActiveScanID != nil || current.Scan.State == domain.ScanLifecycleRunning {
			return lifecycleInvalidState(errors.New("cannot start a follow-up while another scan is active"))
		}
		if current.Scan.FollowupState == nil || *current.Scan.FollowupState != domain.FollowupQueued ||
			current.Scan.FollowupScanID == nil || *current.Scan.FollowupScanID != event.ScanID {
			return lifecycleInvalidState(errors.New("follow-up reservation does not match the requested scan"))
		}
		if err := ensureQueuedFollowupProjectionTx(ctx, tx, current.Scan); err != nil {
			return err
		}

		revision, err := storageTx.BumpStatusRevision(ctx)
		if err != nil {
			return err
		}
		if err := executeLifecycleOne(ctx, tx,
			`UPDATE scan_runs SET state = 'running', started_at_ms = ?, started_status_revision = ?
			 WHERE scan_id = ? AND state = 'queued'`,
			"start queued follow-up", event.StartedAtMS, revision, event.ScanID,
		); err != nil {
			return err
		}
		if err := insertSourceScanManifestTx(ctx, tx, event.ScanID, manifest); err != nil {
			return err
		}
		if err := executeLifecycleOne(ctx, tx,
			`UPDATE app_meta SET scan_state = 'running', active_scan_id = ?,
			 last_scan_started_at_ms = ?, followup_scan_id = NULL, followup_state = NULL,
			 followup_trigger = NULL, followup_requested_at_ms = NULL,
			 followup_enqueued_status_revision = NULL, followup_error_code = NULL
			 WHERE id = 1`,
			"update app_meta for follow-up start", event.ScanID, event.StartedAtMS,
		); err != nil {
			return err
		}

		state, err = readScanStateTx(ctx, tx)
		return err
	})
	if err != nil {
		return domain.ScanState{}, err
	}
	return state, nil
}

func (db *DB) MarkFollowupStartFailed(
	ctx context.Context,
	event domain.FollowupStartFailedEvent,
) (domain.ScanState, error) {
	if err := event.Validate(); err != nil {
		return domain.ScanState{}, lifecycleInvalidState(err)
	}
	if _, ok := followupStartFailedCodes[event.ErrorCode]; !ok {
		return domain.ScanState{}, lifecycleInvalidState(fmt.Errorf("invalid follow-up start failure code: %s", event.ErrorCode))
	}

	var state domain.ScanState
	err := db.WriteTx(ctx, func(storageTx *Tx) error {
		tx := storageTx.tx
		current, err := readAppStateTx(ctx, tx)
		if err != nil {
			return err
		}
		if current.Scan.FollowupState == nil || *current.Scan.FollowupState != domain.FollowupQueued ||
			current.Scan.FollowupScanID == nil || *current.Scan.FollowupScanID != event.ScanID {
			return lifecycleInvalidState(errors.New("follow-up reservation does not match the requested scan"))
		}
		if err := ensureQueuedFollowupProjectionTx(ctx, tx, current.Scan); err != nil {
			return err
		}

		revision, err := storageTx.BumpStatusRevision(ctx)
		if err != nil {
			return err
		}
		if err := executeLifecycleOne(ctx, tx,
			`UPDATE scan_runs SET state = 'start_failed', finished_at_ms = ?,
			 terminal_status_revision = ?, error_code = ?
			 WHERE scan_id = ? AND state = 'queued'`,
			"fail queued follow-up start", event.FailedAtMS, revision, event.ErrorCode, event.ScanID,
		); err != nil {
			return err
		}
		if err := executeLifecycleOne(ctx, tx,
			`UPDATE app_meta SET followup_state = 'start_failed', followup_error_code = ? WHERE id = 1`,
			"update app_meta for failed follow-up start", event.ErrorCode,
		); err != nil {
			return err
		}

		state, err = readScanStateTx(ctx, tx)
		return err
	})
	if err != nil {
		return domain.ScanState{}, err
	}
	return state, nil
}

func (db *DB) MarkScanCompleted(
	ctx context.Context,
	event domain.ScanCompletedEvent,
) (domain.ScanState, error) {
	if err := event.Validate(); err != nil {
		return domain.ScanState{}, lifecycleInvalidState(err)
	}

	var state domain.ScanState
	err := db.WriteTx(ctx, func(storageTx *Tx) error {
		tx := storageTx.tx
		if _, err := requireActiveScanTx(ctx, tx, event.ScanID); err != nil {
			return err
		}

		var failedChildren, unfinishedChildren int64
		if err := tx.QueryRowContext(ctx,
			`SELECT EXISTS(SELECT 1 FROM source_scan_runs WHERE scan_id = ? AND state = 'failed'),
			 (SELECT count(*) FROM source_scan_runs WHERE scan_id = ? AND state IN ('queued', 'running'))`,
			event.ScanID, event.ScanID,
		).Scan(&failedChildren, &unfinishedChildren); err != nil {
			return mapSQLiteError(err)
		}
		if unfinishedChildren != 0 {
			return lifecycleInvalidState(errors.New("cannot complete a scan while source runs remain unfinished"))
		}

		revision, err := storageTx.BumpStatusRevision(ctx)
		if err != nil {
			return err
		}
		if failedChildren != 0 {
			if err := executeLifecycleOne(ctx, tx,
				`UPDATE scan_runs SET state = 'failed', finished_at_ms = ?,
				 terminal_status_revision = ?, error_code = 'SOURCE_RUN_FAILED'
				 WHERE scan_id = ? AND state = 'running'`,
				"aggregate failed source scan", event.CompletedAtMS, revision, event.ScanID,
			); err != nil {
				return err
			}
			if err := executeLifecycleOne(ctx, tx,
				`UPDATE app_meta SET status_revision = ?, scan_state = 'failed', active_scan_id = NULL,
				 last_scan_failed_at_ms = ?, last_scan_error_code = 'SOURCE_RUN_FAILED',
				 last_finished_scan_id = ?, last_finished_scan_result = 'failed'
				 WHERE id = 1`,
				"update app_meta for failed scan completion", revision, event.CompletedAtMS, event.ScanID,
			); err != nil {
				return err
			}
		} else {
			if err := executeLifecycleOne(ctx, tx,
				`UPDATE scan_runs SET state = 'completed', finished_at_ms = ?, terminal_status_revision = ?
				 WHERE scan_id = ? AND state = 'running'`,
				"complete scan", event.CompletedAtMS, revision, event.ScanID,
			); err != nil {
				return err
			}
			if err := executeLifecycleOne(ctx, tx,
				`UPDATE app_meta SET status_revision = ?, scan_state = 'idle', active_scan_id = NULL,
				 last_scan_completed_at_ms = ?, last_scan_error_code = NULL,
				 last_finished_scan_id = ?, last_finished_scan_result = 'completed'
				 WHERE id = 1`,
				"update app_meta for completed scan", revision, event.CompletedAtMS, event.ScanID,
			); err != nil {
				return err
			}
		}

		state, err = readScanStateTx(ctx, tx)
		return err
	})
	if err != nil {
		return domain.ScanState{}, err
	}
	return state, nil
}

func (db *DB) MarkScanFailed(
	ctx context.Context,
	event domain.ScanFailedEvent,
) (domain.ScanState, error) {
	if err := event.Validate(); err != nil {
		return domain.ScanState{}, lifecycleInvalidState(err)
	}

	var state domain.ScanState
	err := db.WriteTx(ctx, func(storageTx *Tx) error {
		tx := storageTx.tx
		if _, err := requireActiveScanTx(ctx, tx, event.ScanID); err != nil {
			return err
		}
		revision, err := storageTx.BumpStatusRevision(ctx)
		if err != nil {
			return err
		}
		if err := executeLifecycleOne(ctx, tx,
			`UPDATE scan_runs SET state = 'failed', finished_at_ms = ?,
			 terminal_status_revision = ?, error_code = ?
			 WHERE scan_id = ? AND state = 'running'`,
			"fail active scan", event.FailedAtMS, revision, event.ErrorCode, event.ScanID,
		); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx,
			`UPDATE source_scan_runs SET state = 'failed', finished_at_ms = ?, error_code = ?
			 WHERE scan_id = ? AND state IN ('queued', 'running')`,
			event.FailedAtMS, event.ErrorCode, event.ScanID,
		); err != nil {
			return mapSQLiteError(err)
		}
		if err := executeLifecycleOne(ctx, tx,
			`UPDATE app_meta SET status_revision = ?, scan_state = 'failed', active_scan_id = NULL,
			 last_scan_failed_at_ms = ?, last_scan_error_code = ?,
			 last_finished_scan_id = ?, last_finished_scan_result = 'failed'
			 WHERE id = 1`,
			"update app_meta for failed scan", revision, event.FailedAtMS, event.ErrorCode, event.ScanID,
		); err != nil {
			return err
		}

		state, err = readScanStateTx(ctx, tx)
		return err
	})
	if err != nil {
		return domain.ScanState{}, err
	}
	return state, nil
}

func (db *DB) MarkSourceScanStarted(
	ctx context.Context,
	scanID string,
	source domain.SourceID,
	startedAtMS int64,
) error {
	return db.transitionSourceScan(ctx, scanID, source, startedAtMS, sourceChildStart, "")
}

func (db *DB) MarkSourceScanCompleted(
	ctx context.Context,
	scanID string,
	source domain.SourceID,
	finishedAtMS int64,
) error {
	return db.transitionSourceScan(ctx, scanID, source, finishedAtMS, sourceChildComplete, "")
}

func (db *DB) MarkSourceScanSkipped(
	ctx context.Context,
	scanID string,
	source domain.SourceID,
	finishedAtMS int64,
) error {
	return db.transitionSourceScan(ctx, scanID, source, finishedAtMS, sourceChildSkip, "")
}

func (db *DB) MarkSourceScanFailed(
	ctx context.Context,
	scanID string,
	source domain.SourceID,
	failedAtMS int64,
	errorCode string,
) error {
	if err := domain.ValidateErrorCode(errorCode); err != nil {
		return lifecycleInvalidState(err)
	}
	return db.transitionSourceScan(ctx, scanID, source, failedAtMS, sourceChildFail, errorCode)
}

type sourceChildTransition uint8

const (
	sourceChildStart sourceChildTransition = iota
	sourceChildComplete
	sourceChildSkip
	sourceChildFail
)

func (db *DB) transitionSourceScan(
	ctx context.Context,
	scanID string,
	source domain.SourceID,
	atMS int64,
	transition sourceChildTransition,
	errorCode string,
) error {
	if err := validateLifecycleScanID(scanID); err != nil {
		return err
	}
	if err := source.Validate(); err != nil {
		return lifecycleInvalidState(err)
	}
	if atMS < 0 {
		return lifecycleInvalidState(errors.New("source scan timestamp must be non-negative"))
	}

	return db.WriteTx(ctx, func(storageTx *Tx) error {
		tx := storageTx.tx
		current, err := readAppStateTx(ctx, tx)
		if err != nil {
			return err
		}
		if current.Scan.State != domain.ScanLifecycleRunning || current.Scan.ActiveScanID == nil ||
			*current.Scan.ActiveScanID != scanID {
			return lifecycleInvalidState(errors.New("source child transition requires the active global scan"))
		}
		if err := ensureScanRunStateTx(ctx, tx, scanID, domain.ScanRunRunning); err != nil {
			return err
		}

		var query string
		var args []any
		switch transition {
		case sourceChildStart:
			query = `UPDATE source_scan_runs SET state = 'running', started_at_ms = ?
				WHERE scan_id = ? AND source = ? AND state = 'queued'`
			args = []any{atMS, scanID, string(source)}
		case sourceChildComplete:
			query = `UPDATE source_scan_runs SET state = 'completed', finished_at_ms = ?
				WHERE scan_id = ? AND source = ? AND state = 'running'`
			args = []any{atMS, scanID, string(source)}
		case sourceChildSkip:
			query = `UPDATE source_scan_runs SET state = 'skipped', finished_at_ms = ?
				WHERE scan_id = ? AND source = ? AND state = 'queued'`
			args = []any{atMS, scanID, string(source)}
		case sourceChildFail:
			query = `UPDATE source_scan_runs SET state = 'failed', finished_at_ms = ?, error_code = ?
				WHERE scan_id = ? AND source = ? AND state IN ('queued', 'running')`
			args = []any{atMS, errorCode, scanID, string(source)}
		default:
			return lifecycleInvalidState(errors.New("unknown source child transition"))
		}
		if err := executeLifecycleOne(ctx, tx, query, "transition source scan", args...); err != nil {
			return err
		}
		_, err = storageTx.BumpStatusRevision(ctx)
		return err
	})
}

func validateSourceManifest(sources []domain.SourceID) (sourceManifest, error) {
	manifest := make(sourceManifest, len(sources))
	for i, source := range sources {
		validated, err := domain.NewSourceID(string(source))
		if err != nil {
			return nil, lifecycleInvalidState(err)
		}
		manifest[i] = validated
	}
	sort.Slice(manifest, func(i, j int) bool { return manifest[i] < manifest[j] })
	for i := 1; i < len(manifest); i++ {
		if manifest[i] == manifest[i-1] {
			return nil, lifecycleInvalidState(fmt.Errorf("duplicate source in manifest: %s", manifest[i]))
		}
	}
	return manifest, nil
}

func insertSourceScanManifestTx(
	ctx context.Context,
	tx *sql.Tx,
	scanID string,
	sources sourceManifest,
) error {
	for _, source := range sources {
		if _, err := tx.ExecContext(ctx,
			`INSERT INTO source_scan_runs (scan_id, source, state, started_at_ms, finished_at_ms, error_code)
			 VALUES (?, ?, 'queued', NULL, NULL, NULL)`,
			scanID, string(source),
		); err != nil {
			return mapSQLiteError(err)
		}
	}
	return nil
}

func ensureScanRunStateTx(
	ctx context.Context,
	tx *sql.Tx,
	scanID string,
	expected domain.ScanRunState,
) error {
	run, err := readScanRunTx(ctx, tx, scanID)
	if err != nil {
		return err
	}
	if run == nil {
		return lifecycleInvalidState(fmt.Errorf("scan row %q does not exist", scanID))
	}
	if run.State != expected {
		return lifecycleInvalidState(fmt.Errorf("scan row %q is %s, expected %s", scanID, run.State, expected))
	}
	return nil
}

func ensureQueuedFollowupProjectionTx(ctx context.Context, tx *sql.Tx, state domain.ScanState) error {
	if state.FollowupScanID == nil {
		return lifecycleInvalidState(errors.New("queued follow-up is missing its scan id"))
	}
	run, err := readScanRunTx(ctx, tx, *state.FollowupScanID)
	if err != nil {
		return err
	}
	if run == nil {
		return lifecycleInvalidState(fmt.Errorf("scan row %q does not exist", *state.FollowupScanID))
	}
	if run.State != domain.ScanRunQueued || run.RequestKind != domain.ScanRequestFollowup ||
		state.FollowupTrigger == nil || *state.FollowupTrigger != run.Trigger ||
		state.FollowupRequestedAtMS == nil || *state.FollowupRequestedAtMS != run.RequestedAtMS ||
		state.FollowupEnqueuedStatusRevision == nil || run.EnqueuedStatusRevision == nil ||
		*state.FollowupEnqueuedStatusRevision != *run.EnqueuedStatusRevision {
		return lifecycleInvalidState(errors.New("queued follow-up projection does not match its scan row"))
	}
	return nil
}

func ensureStartFailedFollowupProjectionTx(ctx context.Context, tx *sql.Tx, state domain.ScanState) error {
	if state.FollowupScanID == nil {
		return lifecycleInvalidState(errors.New("start-failed follow-up is missing its scan id"))
	}
	run, err := readScanRunTx(ctx, tx, *state.FollowupScanID)
	if err != nil {
		return err
	}
	if run == nil {
		return lifecycleInvalidState(fmt.Errorf("scan row %q does not exist", *state.FollowupScanID))
	}
	if run.State != domain.ScanRunStartFailed || run.RequestKind != domain.ScanRequestFollowup ||
		state.FollowupTrigger == nil || *state.FollowupTrigger != run.Trigger ||
		state.FollowupRequestedAtMS == nil || *state.FollowupRequestedAtMS != run.RequestedAtMS ||
		state.FollowupEnqueuedStatusRevision == nil || run.EnqueuedStatusRevision == nil ||
		*state.FollowupEnqueuedStatusRevision != *run.EnqueuedStatusRevision ||
		state.FollowupErrorCode == nil || run.ErrorCode == nil || *state.FollowupErrorCode != *run.ErrorCode {
		return lifecycleInvalidState(errors.New("start-failed follow-up projection does not match its scan row"))
	}
	return nil
}

func requireActiveScanTx(ctx context.Context, tx *sql.Tx, scanID string) (domain.AppState, error) {
	current, err := readAppStateTx(ctx, tx)
	if err != nil {
		return domain.AppState{}, err
	}
	if current.Scan.State != domain.ScanLifecycleRunning || current.Scan.ActiveScanID == nil ||
		*current.Scan.ActiveScanID != scanID {
		return domain.AppState{}, lifecycleInvalidState(errors.New("scan ID is not the current active scan"))
	}
	if err := ensureScanRunStateTx(ctx, tx, scanID, domain.ScanRunRunning); err != nil {
		return domain.AppState{}, err
	}
	return current, nil
}

func readScanStateTx(ctx context.Context, tx *sql.Tx) (domain.ScanState, error) {
	state, err := readAppStateTx(ctx, tx)
	if err != nil {
		return domain.ScanState{}, err
	}
	return state.Scan, nil
}

func readAppStateTx(ctx context.Context, tx *sql.Tx) (domain.AppState, error) {
	var dataRevision, statusRevision int64
	var scanStateRaw string
	var activeScanID, lastFinishedScanID, lastFinishedResultRaw sql.NullString
	var lastScanStartedAtMS, lastScanCompletedAtMS, lastScanFailedAtMS sql.NullInt64
	var lastScanErrorCode sql.NullString
	var followupScanID, followupStateRaw, followupTriggerRaw, followupErrorCode sql.NullString
	var followupRequestedAtMS, followupEnqueuedStatusRevision sql.NullInt64
	err := tx.QueryRowContext(ctx,
		`SELECT data_revision, status_revision, scan_state, active_scan_id,
		 last_finished_scan_id, last_finished_scan_result, last_scan_started_at_ms,
		 last_scan_completed_at_ms, last_scan_failed_at_ms, last_scan_error_code,
		 followup_scan_id, followup_state, followup_trigger, followup_requested_at_ms,
		 followup_enqueued_status_revision, followup_error_code
		 FROM app_meta WHERE id = 1`,
	).Scan(
		&dataRevision, &statusRevision, &scanStateRaw, &activeScanID,
		&lastFinishedScanID, &lastFinishedResultRaw, &lastScanStartedAtMS,
		&lastScanCompletedAtMS, &lastScanFailedAtMS, &lastScanErrorCode,
		&followupScanID, &followupStateRaw, &followupTriggerRaw, &followupRequestedAtMS,
		&followupEnqueuedStatusRevision, &followupErrorCode,
	)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return domain.AppState{}, lifecycleInvalidState(errors.New("app_meta row id=1 is missing"))
		}
		return domain.AppState{}, mapSQLiteError(err)
	}

	scanState, err := domain.ParseScanLifecycleState(scanStateRaw)
	if err != nil {
		return domain.AppState{}, lifecycleInvalidState(err)
	}
	state := domain.ScanState{
		StatusRevision:                 statusRevision,
		State:                          scanState,
		ActiveScanID:                   lifecycleStringPointer(activeScanID),
		LastFinishedScanID:             lifecycleStringPointer(lastFinishedScanID),
		LastScanStartedAtMS:            lifecycleInt64Pointer(lastScanStartedAtMS),
		LastScanCompletedAtMS:          lifecycleInt64Pointer(lastScanCompletedAtMS),
		LastScanFailedAtMS:             lifecycleInt64Pointer(lastScanFailedAtMS),
		LastScanErrorCode:              lifecycleStringPointer(lastScanErrorCode),
		FollowupScanID:                 lifecycleStringPointer(followupScanID),
		FollowupRequestedAtMS:          lifecycleInt64Pointer(followupRequestedAtMS),
		FollowupEnqueuedStatusRevision: lifecycleInt64Pointer(followupEnqueuedStatusRevision),
		FollowupErrorCode:              lifecycleStringPointer(followupErrorCode),
	}
	if lastFinishedResultRaw.Valid {
		value, err := domain.ParseScanResult(lastFinishedResultRaw.String)
		if err != nil {
			return domain.AppState{}, lifecycleInvalidState(err)
		}
		state.LastFinishedScanResult = &value
	}
	if followupStateRaw.Valid {
		value, err := domain.ParseFollowupState(followupStateRaw.String)
		if err != nil {
			return domain.AppState{}, lifecycleInvalidState(err)
		}
		state.FollowupState = &value
	}
	if followupTriggerRaw.Valid {
		value, err := domain.ParseScanTrigger(followupTriggerRaw.String)
		if err != nil {
			return domain.AppState{}, lifecycleInvalidState(err)
		}
		state.FollowupTrigger = &value
	}

	appState := domain.AppState{DataRevision: dataRevision, Scan: state}
	if err := appState.Validate(); err != nil {
		return domain.AppState{}, lifecycleInvalidState(err)
	}
	return appState, nil
}

func readScanRunTx(ctx context.Context, tx *sql.Tx, scanID string) (*domain.ScanRun, error) {
	var run domain.ScanRun
	var triggerRaw, requestKindRaw, stateRaw string
	var enqueuedStatusRevision, startedAtMS, startedStatusRevision sql.NullInt64
	var finishedAtMS, terminalStatusRevision sql.NullInt64
	var errorCode sql.NullString
	err := tx.QueryRowContext(ctx,
		`SELECT scan_id, trigger, request_kind, state, requested_at_ms,
		 enqueued_status_revision, started_at_ms, started_status_revision,
		 finished_at_ms, terminal_status_revision, error_code
		 FROM scan_runs WHERE scan_id = ?`,
		scanID,
	).Scan(
		&run.ScanID, &triggerRaw, &requestKindRaw, &stateRaw, &run.RequestedAtMS,
		&enqueuedStatusRevision, &startedAtMS, &startedStatusRevision,
		&finishedAtMS, &terminalStatusRevision, &errorCode,
	)
	if errors.Is(err, sql.ErrNoRows) {
		return nil, nil
	}
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	run.Trigger, err = domain.ParseScanTrigger(triggerRaw)
	if err != nil {
		return nil, lifecycleInvalidState(err)
	}
	run.RequestKind, err = domain.ParseScanRequestKind(requestKindRaw)
	if err != nil {
		return nil, lifecycleInvalidState(err)
	}
	run.State, err = domain.ParseScanRunState(stateRaw)
	if err != nil {
		return nil, lifecycleInvalidState(err)
	}
	run.EnqueuedStatusRevision = lifecycleInt64Pointer(enqueuedStatusRevision)
	run.StartedAtMS = lifecycleInt64Pointer(startedAtMS)
	run.StartedStatusRevision = lifecycleInt64Pointer(startedStatusRevision)
	run.FinishedAtMS = lifecycleInt64Pointer(finishedAtMS)
	run.TerminalStatusRevision = lifecycleInt64Pointer(terminalStatusRevision)
	run.ErrorCode = lifecycleStringPointer(errorCode)
	if err := run.Validate(); err != nil {
		return nil, lifecycleInvalidState(err)
	}
	return &run, nil
}

func readSourceScanStatusesTx(ctx context.Context, tx *sql.Tx, scanID string) ([]domain.SourceScanStatus, error) {
	rows, err := tx.QueryContext(ctx,
		`SELECT source, state, error_code FROM source_scan_runs
		 WHERE scan_id = ? ORDER BY source ASC`,
		scanID,
	)
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	defer rows.Close()

	var statuses []domain.SourceScanStatus
	for rows.Next() {
		var sourceRaw, stateRaw string
		var errorCode sql.NullString
		if err := rows.Scan(&sourceRaw, &stateRaw, &errorCode); err != nil {
			return nil, mapSQLiteError(err)
		}
		source, err := domain.NewSourceID(sourceRaw)
		if err != nil {
			return nil, lifecycleInvalidState(err)
		}
		state, err := domain.ParseSourceScanState(stateRaw)
		if err != nil {
			return nil, lifecycleInvalidState(err)
		}
		status := domain.SourceScanStatus{
			Source:    source,
			State:     state,
			ErrorCode: lifecycleStringPointer(errorCode),
		}
		if err := status.Validate(); err != nil {
			return nil, lifecycleInvalidState(err)
		}
		statuses = append(statuses, status)
	}
	if err := rows.Err(); err != nil {
		return nil, mapSQLiteError(err)
	}
	return statuses, nil
}

func executeLifecycleOne(
	ctx context.Context,
	tx *sql.Tx,
	query string,
	action string,
	args ...any,
) error {
	result, err := tx.ExecContext(ctx, query, args...)
	if err != nil {
		return mapSQLiteError(err)
	}
	rows, err := result.RowsAffected()
	if err != nil {
		return mapSQLiteError(err)
	}
	if rows != 1 {
		return lifecycleInvalidState(fmt.Errorf("%s affected %d rows, expected one", action, rows))
	}
	return nil
}

func validateLifecycleScanID(scanID string) error {
	if strings.TrimSpace(scanID) == "" || strings.IndexFunc(scanID, unicode.IsControl) >= 0 {
		return lifecycleInvalidState(errors.New("scan id must be non-empty and contain no control characters"))
	}
	return nil
}

func lifecycleStringPointer(value sql.NullString) *string {
	if !value.Valid {
		return nil
	}
	return &value.String
}

func lifecycleInt64Pointer(value sql.NullInt64) *int64 {
	if !value.Valid {
		return nil
	}
	return &value.Int64
}

func lifecycleInvalidState(err error) error {
	return newStorageError(ErrorInvalidState, err)
}
