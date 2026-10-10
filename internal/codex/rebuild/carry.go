package rebuild

import (
	"bytes"
	"database/sql"
	"errors"
	"fmt"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

var ErrCarryIneligible = errors.New("usage member is not eligible for carry")

type CarryOutcome uint8

const (
	CarryProgress CarryOutcome = iota + 1
	CarryFinalized
)

func (c *Coordinator) BeginCarry(tx *source.WriteTx, buildEpoch, sourceFileID int64, trigger Trigger, committedAtMS int64) error {
	if c == nil || c.activeStateProof == nil || c.stripWindows == nil || c.comparator == nil {
		return ErrNilCoordinatorSeam
	}
	if tx == nil || tx.Source() != domain.SourceCodex || buildEpoch < 1 || sourceFileID < 1 || committedAtMS < 0 {
		return ErrInvalidRebuildInput
	}
	if trigger != SourceInvalidated && trigger != FatalIsolation {
		return fmt.Errorf("%w: trigger cannot begin carry", ErrCarryIneligible)
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return err
	}
	if state.BuildEpoch == nil || *state.BuildEpoch != buildEpoch || state.BuildParserVersion == nil ||
		*state.BuildParserVersion != state.ActiveParserVersion {
		return fmt.Errorf("%w: active and build parser pair differs", ErrCarryIneligible)
	}
	member, err := readMember(tx, buildEpoch, sourceFileID)
	if err != nil {
		return err
	}
	if member.carryPhase != carryNone || member.completion != completionPending || member.errorCode.Valid {
		return fmt.Errorf("%w: manifest member is not pending", ErrCarryIneligible)
	}
	row, activeState, err := readCarryProof(tx, state.ActiveEpoch, sourceFileID)
	if err != nil {
		return err
	}
	if err := verifyCarryProof(tx, c.activeStateProof, state, member, row, activeState); err != nil {
		return err
	}
	var replayHolds int64
	if err := tx.Private(func(private storage.PrivateTx) error {
		return private.QueryRow(`SELECT count(*) FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=?
			AND source_file_id=? AND file_generation=? AND hold_reason='replay'`, state.ActiveEpoch, sourceFileID, member.expectedGeneration).Scan(&replayHolds)
	}); err != nil {
		return err
	}
	if replayHolds != 0 {
		return fmt.Errorf("%w: active source has a replay hold", ErrCarryIneligible)
	}
	if _, err := CleanupBuildMembersSemantic(tx, buildEpoch, []int64{sourceFileID}, c.stripWindows); err != nil {
		return err
	}
	if err := resetUsageCheckpoint(tx, sourceFileID, *state.BuildParserVersion); err != nil {
		return err
	}
	return tx.Private(func(private storage.PrivateTx) error {
		result, err := private.Exec(`UPDATE codex_usage_build_sources SET carry_from_epoch=?,carry_phase='occurrences',
			carry_after_start_offset=NULL,carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,carry_after_fact_event_id=NULL,
			carry_after_marker_start_offset=NULL,carry_after_window_start_offset=NULL,completion_status='pending',
			completion_error_code=NULL,completed_generation=NULL,completed_through_offset=NULL,updated_at_ms=?
			WHERE build_epoch=? AND source_file_id=? AND carry_phase='none' AND completion_status='pending'
			AND active_committed_offset=required_through_offset`, state.ActiveEpoch, committedAtMS, buildEpoch, sourceFileID)
		if err != nil {
			return err
		}
		changed, err := result.RowsAffected()
		if err != nil {
			return err
		}
		if changed != 1 {
			return fmt.Errorf("%w: carry begin manifest CAS failed", ErrCarryIneligible)
		}
		return nil
	})
}

func (c *Coordinator) ResumeCarry(tx *source.WriteTx, buildEpoch, sourceFileID int64, trigger Trigger, committedAtMS int64) (CarryOutcome, error) {
	if c == nil || c.activeStateProof == nil || c.stripWindows == nil || c.comparator == nil {
		return 0, ErrNilCoordinatorSeam
	}
	if tx == nil || tx.Source() != domain.SourceCodex || buildEpoch < 1 || sourceFileID < 1 || committedAtMS < 0 {
		return 0, ErrInvalidRebuildInput
	}
	if trigger != SourceInvalidated && trigger != FatalIsolation {
		return 0, fmt.Errorf("%w: trigger cannot resume carry", ErrCarryIneligible)
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return 0, err
	}
	if state.BuildEpoch == nil || *state.BuildEpoch != buildEpoch || state.BuildParserVersion == nil || *state.BuildParserVersion != state.ActiveParserVersion {
		return 0, fmt.Errorf("%w: active and build parser pair differs", ErrCarryIneligible)
	}
	member, err := readMember(tx, buildEpoch, sourceFileID)
	if err != nil {
		return 0, err
	}
	if !member.carryFrom.Valid || member.carryFrom.Int64 != state.ActiveEpoch || member.carryPhase == carryNone || member.completion != completionPending {
		return 0, fmt.Errorf("%w: carry cursor is absent", ErrCarryIneligible)
	}
	row, activeState, err := readCarryProof(tx, state.ActiveEpoch, sourceFileID)
	if err != nil {
		return 0, err
	}
	if err := verifyCarryProof(tx, c.activeStateProof, state, member, row, activeState); err != nil {
		return 0, err
	}
	switch member.carryPhase {
	case "occurrences":
		return CarryProgress, c.carryOccurrenceGroup(tx, state.ActiveEpoch, buildEpoch, member, committedAtMS)
	case "facts":
		return CarryProgress, c.carryFactGroup(tx, state.ActiveEpoch, buildEpoch, member, committedAtMS)
	case "markers":
		return CarryProgress, c.carryOffsetGroup(tx, "codex_compaction_markers", state.ActiveEpoch, buildEpoch, member, member.afterMarkerOffset, "carry_after_marker_start_offset", "markers", "windows", committedAtMS)
	case "windows":
		return CarryProgress, c.carryOffsetGroup(tx, "codex_usage_reconciliation_windows", state.ActiveEpoch, buildEpoch, member, member.afterWindowOffset, "carry_after_window_start_offset", "windows", "turns", committedAtMS)
	case "turns":
		return c.carryTurnGroup(tx, state.ActiveEpoch, buildEpoch, member, committedAtMS)
	case "anomalies":
		if err := setCarryPhase(tx, buildEpoch, sourceFileID, "anomalies", "finalize", "carry_after_anomaly_id", nil, committedAtMS); err != nil {
			return 0, err
		}
		return CarryProgress, nil
	case "finalize":
		if err := c.finalizeCarry(tx, state.ActiveEpoch, buildEpoch, *state.BuildParserVersion, member, row, activeState, committedAtMS); err != nil {
			return 0, err
		}
		return CarryFinalized, nil
	default:
		return 0, fmt.Errorf("%w: unknown v14 carry phase %q", ErrCarryIneligible, member.carryPhase)
	}
}

type carrySourceState struct {
	generation, deviceID, inode, parserVersion, resolvedOffset, observedSize int64
	tailStatus                                                               string
	owner, root                                                              string
}

func readCarryProof(tx *source.WriteTx, activeEpoch, sourceFileID int64) (sourceRow, carrySourceState, error) {
	var row sourceRow
	var active carrySourceState
	err := tx.Private(func(private storage.PrivateTx) error {
		if err := readSourceRow(private, sourceFileID, &row); err != nil {
			return err
		}
		return private.QueryRow(`SELECT file_generation,device_id,inode,usage_parser_version,resolved_through_offset,
			observed_raw_size,raw_tail_status,owning_thread_id,root_session_id FROM codex_usage_source_states
			WHERE ledger_epoch=? AND source_file_id=?`, activeEpoch, sourceFileID).
			Scan(&active.generation, &active.deviceID, &active.inode, &active.parserVersion, &active.resolvedOffset,
				&active.observedSize, &active.tailStatus, &active.owner, &active.root)
	})
	return row, active, err
}

func verifyCarryProof(
	tx *source.WriteTx,
	activeStateProof ActiveSourceStateProof,
	epoch domain.SourceUsageEpochState,
	member manifestMember,
	row sourceRow,
	active carrySourceState,
) error {
	if row.status != "present" && row.status != "missing" || row.generation != member.expectedGeneration || row.deviceID != member.deviceID ||
		row.inode != member.inode || row.observed != member.observedRawSize || active.generation != member.expectedGeneration ||
		active.deviceID != member.deviceID || active.inode != member.inode || active.parserVersion != epoch.ActiveParserVersion ||
		active.resolvedOffset != member.activeOffset || member.activeOffset != member.requiredOffset || active.observedSize != member.observedRawSize ||
		active.owner != member.owner.String || !member.owner.Valid || active.root != member.root.String || !member.root.Valid ||
		!validPhysicalGuard(member.activeOffset, member.activeGuard, row.path, member.observedRawSize, active.tailStatus) ||
		len(member.activeFingerprint) == 0 {
		return fmt.Errorf("%w: frozen active source proof is incomplete or changed", ErrCarryIneligible)
	}
	proof, err := activeStateProof(tx, epoch.ActiveEpoch, member.sourceFileID)
	if err != nil {
		return err
	}
	if !bytes.Equal(proof, member.activeFingerprint) {
		return fmt.Errorf("%w: active source-state fingerprint changed", ErrCarryIneligible)
	}
	return nil
}

func (c *Coordinator) carryOccurrenceGroup(tx *source.WriteTx, activeEpoch, buildEpoch int64, member manifestMember, now int64) error {
	var offset sql.NullInt64
	err := tx.Private(func(private storage.PrivateTx) error {
		return scanOptionalInto(func() error {
			return private.QueryRow(`SELECT source_start_offset FROM (
				SELECT source_start_offset FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?
				UNION SELECT source_start_offset FROM codex_skill_usage_events WHERE ledger_epoch=? AND source_file_id=? AND file_generation=?
			) WHERE (? IS NULL OR source_start_offset>?) ORDER BY source_start_offset LIMIT 1`,
				activeEpoch, member.sourceFileID, member.expectedGeneration, activeEpoch, member.sourceFileID, member.expectedGeneration,
				nullableIntValue(member.afterStartOffset), nullableIntValue(member.afterStartOffset)).Scan(&offset)
		})
	})
	if err != nil {
		return err
	}
	if !offset.Valid {
		return setCarryPhase(tx, buildEpoch, member.sourceFileID, "occurrences", "facts", "carry_after_start_offset", nil, now)
	}
	var eventIDs []string
	err = tx.Private(func(private storage.PrivateTx) error {
		rows, err := private.Query(`SELECT event_id FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?
			AND source_file_id=? AND file_generation=? AND source_start_offset=? ORDER BY event_id`, activeEpoch,
			member.sourceFileID, member.expectedGeneration, offset.Int64)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var eventID string
			if err := rows.Scan(&eventID); err != nil {
				return err
			}
			eventIDs = append(eventIDs, eventID)
		}
		return rows.Err()
	})
	if err != nil {
		return err
	}
	for _, eventID := range eventIDs {
		if _, err := tx.CopyUsageNoRevision(source.UsageTargetActive, source.UsageTargetBuild, eventID); err != nil {
			return err
		}
	}
	key := offset.Int64
	if err := copyEpochRows(tx, "codex_usage_event_occurrences", activeEpoch, buildEpoch, member.sourceFileID, &member.expectedGeneration, "source_start_offset", key); err != nil {
		return err
	}
	if err := copyEpochRows(tx, "codex_skill_usage_events", activeEpoch, buildEpoch, member.sourceFileID, &member.expectedGeneration, "source_start_offset", key); err != nil {
		return err
	}
	return setCarryPhase(tx, buildEpoch, member.sourceFileID, "occurrences", "occurrences", "carry_after_start_offset", &key, now)
}

func (c *Coordinator) carryFactGroup(tx *source.WriteTx, activeEpoch, buildEpoch int64, member manifestMember, now int64) error {
	var eventID sql.NullString
	err := tx.Private(func(private storage.PrivateTx) error {
		return scanOptionalInto(func() error {
			return private.QueryRow(`SELECT event_id FROM (
				SELECT f.event_id FROM codex_usage_event_facts f WHERE f.source='codex' AND f.ledger_epoch=? AND
					(EXISTS(SELECT 1 FROM codex_usage_event_occurrences o WHERE o.source='codex' AND o.ledger_epoch=f.ledger_epoch
					 AND o.source_file_id=? AND o.file_generation=? AND o.event_id=f.event_id) OR
					 EXISTS(SELECT 1 FROM codex_compaction_markers m WHERE m.source='codex' AND m.ledger_epoch=f.ledger_epoch
					 AND m.source_file_id=? AND m.file_generation=? AND m.resolved_event_id=f.event_id))
				UNION SELECT h.event_id FROM codex_usage_event_holds h WHERE h.source='codex' AND h.ledger_epoch=?
					AND h.source_file_id=? AND h.file_generation=?
			) WHERE (? IS NULL OR event_id>?) ORDER BY event_id LIMIT 1`, activeEpoch, member.sourceFileID, member.expectedGeneration,
				member.sourceFileID, member.expectedGeneration, activeEpoch, member.sourceFileID, member.expectedGeneration,
				nullableValue(member.afterFactEventID), nullableValue(member.afterFactEventID)).Scan(&eventID)
		})
	})
	if err != nil {
		return err
	}
	if !eventID.Valid {
		return setCarryPhase(tx, buildEpoch, member.sourceFileID, "facts", "markers", "carry_after_fact_event_id", nil, now)
	}
	if _, err := tx.CopyUsageNoRevision(source.UsageTargetActive, source.UsageTargetBuild, eventID.String); err != nil {
		return err
	}
	if err := copyFactEvent(tx, activeEpoch, buildEpoch, eventID.String); err != nil {
		return err
	}
	if err := copyCarryHold(tx, activeEpoch, buildEpoch, member, eventID.String); err != nil {
		return err
	}
	return setCarryPhase(tx, buildEpoch, member.sourceFileID, "facts", "facts", "carry_after_fact_event_id", &eventID.String, now)
}

func (c *Coordinator) carryOffsetGroup(
	tx *source.WriteTx,
	table string,
	activeEpoch, buildEpoch int64,
	member manifestMember,
	after sql.NullInt64,
	cursorColumn, currentPhase, nextPhase string,
	now int64,
) error {
	var offset sql.NullInt64
	err := tx.Private(func(private storage.PrivateTx) error {
		return scanOptionalInto(func() error {
			return private.QueryRow(fmt.Sprintf(`SELECT source_start_offset FROM %s WHERE ledger_epoch=? AND source_file_id=?
				AND file_generation=? AND (? IS NULL OR source_start_offset>?) ORDER BY source_start_offset LIMIT 1`, table),
				activeEpoch, member.sourceFileID, member.expectedGeneration, nullableIntValue(after), nullableIntValue(after)).Scan(&offset)
		})
	})
	if err != nil {
		return err
	}
	if !offset.Valid {
		return setCarryPhase(tx, buildEpoch, member.sourceFileID, currentPhase, nextPhase, cursorColumn, nil, now)
	}
	key := offset.Int64
	if err := copyEpochRows(tx, table, activeEpoch, buildEpoch, member.sourceFileID, &member.expectedGeneration, "source_start_offset", key); err != nil {
		return err
	}
	return setCarryPhase(tx, buildEpoch, member.sourceFileID, currentPhase, currentPhase, cursorColumn, &key, now)
}

func (c *Coordinator) carryTurnGroup(tx *source.WriteTx, activeEpoch, buildEpoch int64, member manifestMember, now int64) (CarryOutcome, error) {
	var turnKey sql.NullString
	err := tx.Private(func(private storage.PrivateTx) error {
		return scanOptionalInto(func() error {
			return private.QueryRow(`SELECT turn_key FROM codex_turns WHERE ledger_epoch=? AND source_file_id=? AND file_generation=?
				AND (? IS NULL OR turn_key>?) ORDER BY turn_key LIMIT 1`, activeEpoch, member.sourceFileID, member.expectedGeneration,
				nullableValue(member.afterTurnKey), nullableValue(member.afterTurnKey)).Scan(&turnKey)
		})
	})
	if err != nil {
		return 0, err
	}
	if turnKey.Valid {
		if err := copyEpochRows(tx, "codex_turns", activeEpoch, buildEpoch, member.sourceFileID, &member.expectedGeneration, "turn_key", turnKey.String); err != nil {
			return 0, err
		}
		if err := setCarryPhase(tx, buildEpoch, member.sourceFileID, "turns", "turns", "carry_after_turn_key", &turnKey.String, now); err != nil {
			return 0, err
		}
		return CarryProgress, nil
	}
	if err := copyEpochRows(tx, "codex_usage_source_states", activeEpoch, buildEpoch, member.sourceFileID, nil, "", nil); err != nil {
		return 0, err
	}
	if err := setCarryPhase(tx, buildEpoch, member.sourceFileID, "turns", "anomalies", "carry_after_turn_key", nil, now); err != nil {
		return 0, err
	}
	return CarryProgress, nil
}

func (c *Coordinator) finalizeCarry(
	tx *source.WriteTx,
	activeEpoch, buildEpoch, parserVersion int64,
	member manifestMember,
	row sourceRow,
	active carrySourceState,
	now int64,
) error {
	for _, table := range []string{"codex_usage_event_occurrences", "codex_skill_usage_events", "codex_compaction_markers", "codex_usage_reconciliation_windows", "codex_turns", "codex_usage_source_states"} {
		if err := verifyEpochRowsEqual(tx, table, activeEpoch, buildEpoch, member); err != nil {
			return err
		}
	}
	if err := verifyCarryFactsAndHolds(tx, activeEpoch, buildEpoch, member); err != nil {
		return err
	}
	if row.status != "present" && row.status != "missing" || row.generation != member.expectedGeneration || row.deviceID != member.deviceID ||
		row.inode != member.inode || row.observed != member.observedRawSize || active.resolvedOffset != member.activeOffset ||
		active.parserVersion != parserVersion || active.observedSize != member.observedRawSize ||
		!validPhysicalGuard(member.activeOffset, member.activeGuard, row.path, member.observedRawSize, active.tailStatus) {
		return fmt.Errorf("%w: final carry physical proof changed", ErrCarryIneligible)
	}
	completion := completionCarried
	err := tx.Private(func(private storage.PrivateTx) error {
		result, err := private.Exec(`UPDATE codex_source_checkpoints SET parser_version=?,committed_offset=?,guard_hash=?,
			processing_status='ready',last_successful_scan_at_ms=?,last_error_code=NULL
			WHERE source_file_id=? AND consumer_kind='usage' AND parser_version=? AND committed_offset=0
			AND guard_hash IS NULL AND processing_status='rebuild_required'`, parserVersion, member.activeOffset,
			member.activeGuard, now, member.sourceFileID, parserVersion)
		if err != nil {
			return err
		}
		changed, err := result.RowsAffected()
		if err != nil {
			return err
		}
		if changed != 1 {
			return fmt.Errorf("%w: final carry checkpoint CAS failed", ErrCarryIneligible)
		}
		result, err = private.Exec(`UPDATE codex_usage_build_sources SET completion_status=?,completion_error_code=NULL,
			completed_generation=required_generation,completed_through_offset=?,carry_from_epoch=NULL,carry_phase='none',
			carry_after_start_offset=NULL,carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,carry_after_fact_event_id=NULL,
			carry_after_marker_start_offset=NULL,carry_after_window_start_offset=NULL,updated_at_ms=?
			WHERE build_epoch=? AND source_file_id=? AND carry_phase='finalize' AND carry_from_epoch=?`, completion,
			member.activeOffset, now, buildEpoch, member.sourceFileID, activeEpoch)
		if err != nil {
			return err
		}
		changed, err = result.RowsAffected()
		if err != nil {
			return err
		}
		if changed != 1 {
			return fmt.Errorf("%w: final carry manifest CAS failed", ErrCarryIneligible)
		}
		_, err = private.Exec(`DELETE FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND source_file_id=?
			AND file_generation=? AND hold_reason='carry'`, buildEpoch, member.sourceFileID, member.expectedGeneration)
		return err
	})
	return err
}

func setCarryPhase(tx *source.WriteTx, buildEpoch, sourceFileID int64, expected, next, cursor string, value any, now int64) error {
	phaseCursor := map[string]string{
		"occurrences": "carry_after_start_offset", "facts": "carry_after_fact_event_id",
		"markers": "carry_after_marker_start_offset", "windows": "carry_after_window_start_offset",
		"turns": "carry_after_turn_key", "anomalies": "carry_after_anomaly_id",
	}
	if phaseCursor[expected] != cursor || cursor == "carry_after_anomaly_id" && value != nil {
		return ErrInvalidRebuildInput
	}
	cursors := []string{
		"carry_after_start_offset", "carry_after_turn_key", "carry_after_fact_event_id",
		"carry_after_marker_start_offset", "carry_after_window_start_offset", "carry_after_anomaly_id",
	}
	sets := []string{"carry_phase=?"}
	args := []any{next}
	for _, column := range cursors {
		sets = append(sets, column+"=?")
		var current any
		if column == cursor {
			current = value
		}
		args = append(args, current)
	}
	sets = append(sets, "updated_at_ms=?")
	args = append(args, now, buildEpoch, sourceFileID, expected)
	query := fmt.Sprintf("UPDATE codex_usage_build_sources SET %s WHERE build_epoch=? AND source_file_id=? AND carry_phase=?", strings.Join(sets, ","))
	return tx.Private(func(private storage.PrivateTx) error {
		result, err := private.Exec(query, args...)
		if err != nil {
			return err
		}
		changed, err := result.RowsAffected()
		if err != nil {
			return err
		}
		if changed != 1 {
			return fmt.Errorf("%w: carry phase/cursor CAS failed", ErrCarryIneligible)
		}
		return nil
	})
}

func copyEpochRows(
	tx *source.WriteTx,
	table string,
	activeEpoch, buildEpoch, sourceFileID int64,
	generation *int64,
	filterColumn string,
	filterValue any,
) error {
	return tx.Private(func(private storage.PrivateTx) error {
		columns, err := tableColumns(private, table)
		if err != nil {
			return err
		}
		quoted := make([]string, 0, len(columns))
		selectColumns := make([]string, 0, len(columns))
		comparison := make([]string, 0, len(columns)-1)
		for _, column := range columns {
			name := `"` + column + `"`
			quoted = append(quoted, name)
			if column == "ledger_epoch" {
				selectColumns = append(selectColumns, "?")
			} else {
				selectColumns = append(selectColumns, name)
				comparison = append(comparison, name)
			}
		}
		where, sourceArgs := carrySourceWhere(activeEpoch, sourceFileID, generation, filterColumn, filterValue)
		_, err = private.Exec(fmt.Sprintf("INSERT OR IGNORE INTO %s(%s) SELECT %s FROM %s WHERE %s", table,
			strings.Join(quoted, ","), strings.Join(selectColumns, ","), table, where), append([]any{buildEpoch}, sourceArgs...)...)
		if err != nil {
			return err
		}
		targetWhere, targetArgs := carrySourceWhere(buildEpoch, sourceFileID, generation, filterColumn, filterValue)
		columnsSQL := strings.Join(comparison, ",")
		var differences int64
		query := fmt.Sprintf(`SELECT
			(SELECT count(*) FROM (SELECT %s FROM %s WHERE %s EXCEPT SELECT %s FROM %s WHERE %s)) +
			(SELECT count(*) FROM (SELECT %s FROM %s WHERE %s EXCEPT SELECT %s FROM %s WHERE %s))`,
			columnsSQL, table, where, columnsSQL, table, targetWhere,
			columnsSQL, table, targetWhere, columnsSQL, table, where)
		args := append(append(append(append([]any{}, sourceArgs...), targetArgs...), targetArgs...), sourceArgs...)
		if err := private.QueryRow(query, args...).Scan(&differences); err != nil {
			return err
		}
		if differences != 0 {
			return fmt.Errorf("%w: copied %s rows differ", ErrCarryIneligible, table)
		}
		return nil
	})
}

func carrySourceWhere(epoch, sourceFileID int64, generation *int64, filterColumn string, filterValue any) (string, []any) {
	where := "ledger_epoch=? AND source_file_id=?"
	args := []any{epoch, sourceFileID}
	if generation != nil {
		where += " AND file_generation=?"
		args = append(args, *generation)
	}
	if filterColumn != "" {
		where += " AND " + filterColumn + "=?"
		args = append(args, filterValue)
	}
	return where, args
}

func tableColumns(private storage.PrivateTx, table string) ([]string, error) {
	rows, err := private.Query("PRAGMA table_info(" + table + ")")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var columns []string
	for rows.Next() {
		var index, notNull, primaryKey int
		var name, columnType string
		var defaultValue sql.NullString
		if err := rows.Scan(&index, &name, &columnType, &notNull, &defaultValue, &primaryKey); err != nil {
			return nil, err
		}
		columns = append(columns, name)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	hasEpoch := false
	for _, column := range columns {
		if column == "ledger_epoch" {
			hasEpoch = true
			break
		}
	}
	if len(columns) < 2 || !hasEpoch {
		return nil, fmt.Errorf("%w: invalid carry table %s", ErrInvalidRebuildInput, table)
	}
	return columns, nil
}

func copyFactEvent(tx *source.WriteTx, activeEpoch, buildEpoch int64, eventID string) error {
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(`INSERT OR IGNORE INTO codex_usage_event_facts(source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation)
			SELECT source,?,event_id,owning_thread_id,response_id,evidence_kind,operation FROM codex_usage_event_facts
			WHERE source='codex' AND ledger_epoch=? AND event_id=?`, buildEpoch, activeEpoch, eventID)
		if err != nil {
			return err
		}
		var activeCount, buildCount, identical int64
		if err := private.QueryRow("SELECT count(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=? AND event_id=?", activeEpoch, eventID).Scan(&activeCount); err != nil {
			return err
		}
		if err := private.QueryRow("SELECT count(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=? AND event_id=?", buildEpoch, eventID).Scan(&buildCount); err != nil {
			return err
		}
		if err := private.QueryRow(`SELECT count(*) FROM codex_usage_event_facts a JOIN codex_usage_event_facts b
			ON b.source=a.source AND b.ledger_epoch=? AND b.event_id=a.event_id
			WHERE a.source='codex' AND a.ledger_epoch=? AND a.event_id=? AND
			b.owning_thread_id=a.owning_thread_id AND b.response_id IS a.response_id AND
			b.evidence_kind=a.evidence_kind AND b.operation=a.operation`, buildEpoch, activeEpoch, eventID).Scan(&identical); err != nil {
			return err
		}
		if activeCount != buildCount || activeCount != identical {
			return fmt.Errorf("%w: fact %s differs during carry", ErrCarryIneligible, eventID)
		}
		return nil
	})
}

func copyCarryHold(tx *source.WriteTx, activeEpoch, buildEpoch int64, member manifestMember, eventID string) error {
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(`INSERT OR IGNORE INTO codex_usage_event_holds(source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
			SELECT source,?,source_file_id,file_generation,event_id,'carry' FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=?
			AND source_file_id=? AND file_generation=? AND event_id=?`, buildEpoch, activeEpoch, member.sourceFileID, member.expectedGeneration, eventID)
		return err
	})
}

func verifyCarryFactsAndHolds(tx *source.WriteTx, activeEpoch, buildEpoch int64, member manifestMember) error {
	return tx.Private(func(private storage.PrivateTx) error {
		var differences int64
		if err := private.QueryRow(`SELECT
			(SELECT count(*) FROM (SELECT event_id FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?
			 EXCEPT SELECT event_id FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=? AND hold_reason='carry')) +
			(SELECT count(*) FROM (SELECT event_id FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=? AND hold_reason='carry'
			 EXCEPT SELECT event_id FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?))`,
			activeEpoch, member.sourceFileID, member.expectedGeneration, buildEpoch, member.sourceFileID, member.expectedGeneration,
			buildEpoch, member.sourceFileID, member.expectedGeneration, activeEpoch, member.sourceFileID, member.expectedGeneration).Scan(&differences); err != nil {
			return err
		}
		if differences != 0 {
			return fmt.Errorf("%w: carry holds differ", ErrCarryIneligible)
		}
		var eventIDs []string
		rows, err := private.Query(`SELECT event_id FROM (
			SELECT f.event_id FROM codex_usage_event_facts f WHERE f.source='codex' AND f.ledger_epoch=? AND
				(EXISTS(SELECT 1 FROM codex_usage_event_occurrences o WHERE o.source='codex' AND o.ledger_epoch=f.ledger_epoch AND o.source_file_id=? AND o.file_generation=? AND o.event_id=f.event_id)
				 OR EXISTS(SELECT 1 FROM codex_compaction_markers m WHERE m.source='codex' AND m.ledger_epoch=f.ledger_epoch AND m.source_file_id=? AND m.file_generation=? AND m.resolved_event_id=f.event_id)
				 OR EXISTS(SELECT 1 FROM codex_usage_event_holds h WHERE h.source='codex' AND h.ledger_epoch=f.ledger_epoch AND h.source_file_id=? AND h.file_generation=? AND h.event_id=f.event_id))
			UNION SELECT event_id FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?)
			ORDER BY event_id`, activeEpoch, member.sourceFileID, member.expectedGeneration, member.sourceFileID, member.expectedGeneration,
			member.sourceFileID, member.expectedGeneration, activeEpoch, member.sourceFileID, member.expectedGeneration)
		if err != nil {
			return err
		}
		for rows.Next() {
			var eventID string
			if err := rows.Scan(&eventID); err != nil {
				rows.Close()
				return err
			}
			eventIDs = append(eventIDs, eventID)
		}
		if err := rows.Err(); err != nil {
			rows.Close()
			return err
		}
		if err := rows.Close(); err != nil {
			return err
		}
		for _, eventID := range eventIDs {
			var active, build, same int64
			if err := private.QueryRow("SELECT count(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=? AND event_id=?", activeEpoch, eventID).Scan(&active); err != nil {
				return err
			}
			if err := private.QueryRow("SELECT count(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=? AND event_id=?", buildEpoch, eventID).Scan(&build); err != nil {
				return err
			}
			if err := private.QueryRow(`SELECT count(*) FROM codex_usage_event_facts a JOIN codex_usage_event_facts b ON b.source=a.source AND b.ledger_epoch=? AND b.event_id=a.event_id
				WHERE a.source='codex' AND a.ledger_epoch=? AND a.event_id=? AND b.owning_thread_id=a.owning_thread_id AND
				b.response_id IS a.response_id AND b.evidence_kind=a.evidence_kind AND b.operation=a.operation`, buildEpoch, activeEpoch, eventID).Scan(&same); err != nil {
				return err
			}
			if active != build || active != same {
				return fmt.Errorf("%w: source-associated fact %s is missing or differs", ErrCarryIneligible, eventID)
			}
		}
		return nil
	})
}

func verifyEpochRowsEqual(tx *source.WriteTx, table string, activeEpoch, buildEpoch int64, member manifestMember) error {
	return tx.Private(func(private storage.PrivateTx) error {
		columns, err := tableColumns(private, table)
		if err != nil {
			return err
		}
		selectColumns := make([]string, 0, len(columns)-1)
		for _, column := range columns {
			if column != "ledger_epoch" {
				selectColumns = append(selectColumns, `"`+column+`"`)
			}
		}
		whereActive, activeArgs := carrySourceWhere(activeEpoch, member.sourceFileID, &member.expectedGeneration, "", nil)
		whereBuild, buildArgs := carrySourceWhere(buildEpoch, member.sourceFileID, &member.expectedGeneration, "", nil)
		if table == "codex_usage_source_states" {
			whereActive, activeArgs = carrySourceWhere(activeEpoch, member.sourceFileID, nil, "", nil)
			whereBuild, buildArgs = carrySourceWhere(buildEpoch, member.sourceFileID, nil, "", nil)
		}
		projection := strings.Join(selectColumns, ",")
		query := fmt.Sprintf(`SELECT
			(SELECT count(*) FROM (SELECT %s FROM %s WHERE %s EXCEPT SELECT %s FROM %s WHERE %s)) +
			(SELECT count(*) FROM (SELECT %s FROM %s WHERE %s EXCEPT SELECT %s FROM %s WHERE %s))`,
			projection, table, whereActive, projection, table, whereBuild,
			projection, table, whereBuild, projection, table, whereActive)
		args := append(append(append(append([]any{}, activeArgs...), buildArgs...), buildArgs...), activeArgs...)
		var count int64
		if err := private.QueryRow(query, args...).Scan(&count); err != nil {
			return err
		}
		if count != 0 {
			return fmt.Errorf("%w: carried %s set differs", ErrCarryIneligible, table)
		}
		return nil
	})
}

func scanOptionalInto(scan func() error) error {
	_, err := scanOptionalRow(scan)
	return err
}
