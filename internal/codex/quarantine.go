package codex

import (
	"database/sql"
	"errors"
	"fmt"
	"sort"

	"github.com/Hogeexxl/Usagi/internal/codex/usage"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

var (
	ErrStaleQuarantineProof = errors.New("stale quarantine source proof")
	ErrInvalidFatalConflict = errors.New("invalid Codex fatal conflict")
)

type quarantineReader interface {
	Query(query string, args ...any) (*sql.Rows, error)
	QueryRow(query string, args ...any) *sql.Row
}

type quarantineMember struct {
	sourceFileID  int64
	generation    int64
	deviceID      int64
	inode         int64
	observedSize  int64
	fileStatus    string
	acceptedGen   int64
	acceptedDev   int64
	acceptedInode int64
	acceptedSize  int64
}

type quarantineProjection struct {
	primaryErrorCode string
	lastActivityAtMS int64
	firstSeenAtMS    int64
}

func QuarantineStillValid(activeEpoch int64, rootID string, currentRootProofs []usage.QuarantineSourceProof, reader storage.PrivateReader) (bool, error) {
	if activeEpoch <= 0 || rootID == "" {
		return false, nil
	}
	if err := validateQuarantineProofList(currentRootProofs); err != nil {
		return false, err
	}
	var exists int
	if err := reader.QueryRow(`SELECT EXISTS(
		SELECT 1 FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?
	)`, activeEpoch, rootID).Scan(&exists); err != nil {
		return false, err
	}
	if exists == 0 {
		return false, nil
	}
	activeProofs, err := loadQuarantineProofs(reader, activeEpoch, rootID)
	if err != nil {
		return false, err
	}
	return equalQuarantineProofs(currentRootProofs, activeProofs), nil
}

func QuarantineRoot(
	tx *source.WriteTx,
	buildEpoch int64,
	rootID string,
	conflict usage.FatalConflict,
	currentPresentProofs []usage.QuarantineSourceProof,
	committedAtMS int64,
) error {
	if rootID == "" || committedAtMS < 0 || !validFatalConflictCode(conflict.Code) {
		return ErrInvalidFatalConflict
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return err
	}
	if state.BuildEpoch == nil || *state.BuildEpoch != buildEpoch || buildEpoch <= 0 {
		return ErrStaleQuarantineProof
	}
	if err := validateQuarantineProofList(currentPresentProofs); err != nil {
		return err
	}

	var eventIDs []string
	err = tx.Private(func(private storage.PrivateTx) error {
		members, err := validateBuildRootProofs(private, buildEpoch, rootID, currentPresentProofs)
		if err != nil {
			return err
		}
		eventIDs, err = collectQuarantineEventIDs(private, buildEpoch, buildEpoch, rootID)
		if err != nil {
			return err
		}
		return deleteQuarantineSemanticRows(private, buildEpoch, buildEpoch, rootID, members, eventIDs)
	})
	if err != nil {
		return err
	}
	if len(eventIDs) != 0 {
		if _, err := tx.DeleteUsageNoRevision(source.UsageTargetBuild, eventIDs); err != nil {
			return err
		}
	}

	return tx.Private(func(private storage.PrivateTx) error {
		lastActivityAtMS, err := rootLastActivity(private, rootID)
		if err != nil {
			return err
		}
		if _, err := private.Exec(`INSERT INTO codex_usage_session_quarantine(
			ledger_epoch,root_session_id,primary_error_code,last_activity_at_ms,first_seen_at_ms,updated_at_ms
		) VALUES(?,?,?,?,?,?) ON CONFLICT(ledger_epoch,root_session_id) DO UPDATE SET
			primary_error_code=excluded.primary_error_code,
			last_activity_at_ms=excluded.last_activity_at_ms,
			updated_at_ms=excluded.updated_at_ms`,
			buildEpoch, rootID, string(conflict.Code), lastActivityAtMS, committedAtMS, committedAtMS); err != nil {
			return err
		}
		if err := replaceQuarantineProofs(private, buildEpoch, rootID, currentPresentProofs, committedAtMS); err != nil {
			return err
		}
		if err := terminalizeQuarantinedMembers(private, buildEpoch, rootID, string(conflict.Code), committedAtMS); err != nil {
			return err
		}
		return verifyQuarantinedRootClean(private, buildEpoch, rootID, eventIDs)
	})
}

func ReuseQuarantineRoot(
	tx *source.WriteTx,
	buildEpoch int64,
	rootID string,
	currentPresentProofs []usage.QuarantineSourceProof,
	committedAtMS int64,
) error {
	if rootID == "" || committedAtMS < 0 {
		return ErrStaleQuarantineProof
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return err
	}
	if state.BuildEpoch == nil || *state.BuildEpoch != buildEpoch || state.ActiveEpoch <= 0 || buildEpoch <= 0 {
		return ErrStaleQuarantineProof
	}
	if err := validateQuarantineProofList(currentPresentProofs); err != nil {
		return err
	}

	var activeProjection quarantineProjection
	var eventIDs []string
	err = tx.Private(func(private storage.PrivateTx) error {
		members, err := validateBuildRootProofs(private, buildEpoch, rootID, currentPresentProofs)
		if err != nil {
			return err
		}
		activeProjection, err = loadQuarantineProjection(private, state.ActiveEpoch, rootID)
		if err != nil {
			return err
		}
		if !validFatalConflictCode(usage.FatalConflictCode(activeProjection.primaryErrorCode)) {
			return ErrStaleQuarantineProof
		}
		activeProofs, err := loadQuarantineProofs(private, state.ActiveEpoch, rootID)
		if err != nil {
			return err
		}
		if !equalQuarantineProofs(currentPresentProofs, activeProofs) {
			return ErrStaleQuarantineProof
		}
		eventIDs, err = collectQuarantineEventIDs(private, buildEpoch, buildEpoch, rootID)
		if err != nil {
			return err
		}
		return deleteQuarantineSemanticRows(private, buildEpoch, buildEpoch, rootID, members, eventIDs)
	})
	if err != nil {
		return err
	}
	if len(eventIDs) != 0 {
		if _, err := tx.DeleteUsageNoRevision(source.UsageTargetBuild, eventIDs); err != nil {
			return err
		}
	}

	return tx.Private(func(private storage.PrivateTx) error {
		if _, err := private.Exec(`INSERT INTO codex_usage_session_quarantine(
			ledger_epoch,root_session_id,primary_error_code,last_activity_at_ms,first_seen_at_ms,updated_at_ms
		) VALUES(?,?,?,?,?,?) ON CONFLICT(ledger_epoch,root_session_id) DO UPDATE SET
			primary_error_code=excluded.primary_error_code,
			last_activity_at_ms=excluded.last_activity_at_ms,
			first_seen_at_ms=excluded.first_seen_at_ms,
			updated_at_ms=excluded.updated_at_ms`,
			buildEpoch, rootID, activeProjection.primaryErrorCode, activeProjection.lastActivityAtMS,
			activeProjection.firstSeenAtMS, committedAtMS); err != nil {
			return err
		}
		if err := replaceQuarantineProofs(private, buildEpoch, rootID, currentPresentProofs, committedAtMS); err != nil {
			return err
		}
		if err := terminalizeQuarantinedMembers(private, buildEpoch, rootID, activeProjection.primaryErrorCode, committedAtMS); err != nil {
			return err
		}
		return verifyQuarantinedRootClean(private, buildEpoch, rootID, eventIDs)
	})
}

func VerifyQuarantinedRootClean(reader storage.PrivateReader, buildEpoch int64, rootID string, eventIDs []string) error {
	return verifyQuarantinedRootClean(reader, buildEpoch, rootID, eventIDs)
}

func verifyQuarantinedRootClean(reader quarantineReader, buildEpoch int64, rootID string, eventIDs []string) error {
	if buildEpoch <= 0 || rootID == "" {
		return ErrStaleQuarantineProof
	}
	if _, err := loadQuarantineMembers(reader, buildEpoch, rootID); err != nil {
		return err
	}
	checks := []struct {
		name  string
		query string
		args  []any
	}{
		{"usage_events", `SELECT COUNT(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND root_session_id=?`, []any{buildEpoch, rootID}},
		{"codex_usage_event_occurrences", `SELECT COUNT(*) FROM codex_usage_event_occurrences x WHERE x.source='codex' AND x.ledger_epoch=? AND EXISTS(SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=? AND m.source_file_id=x.source_file_id AND m.expected_file_generation=x.file_generation)`, []any{buildEpoch, buildEpoch, rootID}},
		{"codex_usage_event_facts", "", nil},
		{"codex_compaction_markers", `SELECT COUNT(*) FROM codex_compaction_markers x WHERE x.source='codex' AND x.ledger_epoch=? AND EXISTS(SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=? AND m.source_file_id=x.source_file_id AND m.expected_file_generation=x.file_generation)`, []any{buildEpoch, buildEpoch, rootID}},
		{"codex_usage_reconciliation_windows", `SELECT COUNT(*) FROM codex_usage_reconciliation_windows x WHERE x.source='codex' AND x.ledger_epoch=? AND EXISTS(SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=? AND m.source_file_id=x.source_file_id AND m.expected_file_generation=x.file_generation)`, []any{buildEpoch, buildEpoch, rootID}},
		{"codex_usage_event_holds", `SELECT COUNT(*) FROM codex_usage_event_holds x WHERE x.source='codex' AND x.ledger_epoch=? AND EXISTS(SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=? AND m.source_file_id=x.source_file_id AND m.expected_file_generation=x.file_generation)`, []any{buildEpoch, buildEpoch, rootID}},
		{"codex_turns", `SELECT COUNT(*) FROM codex_turns x WHERE x.ledger_epoch=? AND (
			EXISTS(SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=? AND m.source_file_id=x.source_file_id AND m.expected_file_generation=x.file_generation)
			OR EXISTS(SELECT 1 FROM threads t WHERE t.thread_id=x.thread_id AND COALESCE(t.root_session_id,t.thread_id)=?))`, []any{buildEpoch, buildEpoch, rootID, rootID}},
		{"codex_usage_source_states", `SELECT COUNT(*) FROM codex_usage_source_states x WHERE x.ledger_epoch=? AND (
			EXISTS(SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=? AND m.source_file_id=x.source_file_id AND m.expected_file_generation=x.file_generation)
			OR x.root_session_id=?)`, []any{buildEpoch, buildEpoch, rootID, rootID}},
		{"codex_skill_usage_events", `SELECT COUNT(*) FROM codex_skill_usage_events x WHERE x.ledger_epoch=? AND (
			EXISTS(SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=? AND m.source_file_id=x.source_file_id AND m.expected_file_generation=x.file_generation)
			OR x.root_session_id=?)`, []any{buildEpoch, buildEpoch, rootID, rootID}},
	}
	for _, check := range checks {
		if check.name == "codex_usage_event_facts" {
			var rootFacts int
			if err := reader.QueryRow(`SELECT COUNT(*) FROM codex_usage_event_facts f JOIN threads t ON t.thread_id=f.owning_thread_id
				WHERE f.source='codex' AND f.ledger_epoch=? AND COALESCE(t.root_session_id,t.thread_id)=?`, buildEpoch, rootID).Scan(&rootFacts); err != nil {
				return err
			}
			if rootFacts != 0 {
				return fmt.Errorf("quarantined root retains %s", check.name)
			}
			for _, eventID := range eventIDs {
				var count int
				if err := reader.QueryRow(`SELECT COUNT(*) FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=? AND event_id=?`, buildEpoch, eventID).Scan(&count); err != nil {
					return err
				}
				if count != 0 {
					return fmt.Errorf("quarantined root retains %s", check.name)
				}
			}
			continue
		}
		if check.name == "usage_events" {
			var rootEvents int
			if err := reader.QueryRow(check.query, check.args...).Scan(&rootEvents); err != nil {
				return err
			}
			if rootEvents != 0 {
				return fmt.Errorf("quarantined root retains %s", check.name)
			}
			for _, eventID := range eventIDs {
				var count int
				if err := reader.QueryRow(`SELECT COUNT(*) FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id=?`, buildEpoch, eventID).Scan(&count); err != nil {
					return err
				}
				if count != 0 {
					return fmt.Errorf("quarantined root retains %s", check.name)
				}
			}
			continue
		}
		var count int
		if err := reader.QueryRow(check.query, check.args...).Scan(&count); err != nil {
			return err
		}
		if count != 0 {
			return fmt.Errorf("quarantined root retains %s", check.name)
		}
	}
	return nil
}

func validateBuildRootProofs(reader quarantineReader, buildEpoch int64, rootID string, proofs []usage.QuarantineSourceProof) ([]quarantineMember, error) {
	members, err := loadQuarantineMembers(reader, buildEpoch, rootID)
	if err != nil {
		return nil, err
	}
	proofByID := make(map[int64]usage.QuarantineSourceProof, len(proofs))
	for _, proof := range proofs {
		proofByID[proof.SourceFileID] = proof
	}
	presentCount := 0
	for _, member := range members {
		switch member.fileStatus {
		case "missing":
			continue
		case "present":
			presentCount++
			proof, ok := proofByID[member.sourceFileID]
			if !ok || proof.Generation != member.acceptedGen || proof.DeviceID != member.acceptedDev || proof.Inode != member.acceptedInode || proof.ObservedSize != member.acceptedSize ||
				proof.Generation != member.generation || proof.DeviceID != member.deviceID || proof.Inode != member.inode || proof.ObservedSize != member.observedSize {
				return nil, ErrStaleQuarantineProof
			}
		default:
			return nil, ErrStaleQuarantineProof
		}
	}
	if presentCount != len(proofs) {
		return nil, ErrStaleQuarantineProof
	}
	return members, nil
}

func loadQuarantineMembers(reader quarantineReader, buildEpoch int64, rootID string) ([]quarantineMember, error) {
	rows, err := reader.Query(`SELECT m.source_file_id,m.expected_file_generation,m.expected_device_id,m.expected_inode,
		m.observed_raw_size,s.file_status,s.file_generation,s.device_id,s.inode,s.observed_size
		FROM codex_usage_build_sources m JOIN codex_source_files s ON s.source_file_id=m.source_file_id
		WHERE m.build_epoch=? AND m.expected_root_session_id=? ORDER BY m.source_file_id`, buildEpoch, rootID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var members []quarantineMember
	for rows.Next() {
		var member quarantineMember
		if err := rows.Scan(&member.sourceFileID, &member.generation, &member.deviceID, &member.inode, &member.observedSize,
			&member.fileStatus, &member.acceptedGen, &member.acceptedDev, &member.acceptedInode, &member.acceptedSize); err != nil {
			return nil, err
		}
		members = append(members, member)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	if len(members) == 0 {
		return nil, ErrStaleQuarantineProof
	}
	return members, nil
}

func collectQuarantineEventIDs(reader quarantineReader, epoch, buildEpoch int64, rootID string) ([]string, error) {
	rows, err := reader.Query(`SELECT event_id FROM usage_events WHERE source='codex' AND source_epoch=? AND root_session_id=?
		UNION SELECT f.event_id FROM codex_usage_event_facts f JOIN threads t ON t.thread_id=f.owning_thread_id
		WHERE f.source='codex' AND f.ledger_epoch=? AND COALESCE(t.root_session_id,t.thread_id)=?
		UNION SELECT o.event_id FROM codex_usage_event_occurrences o JOIN codex_usage_build_sources m
		ON m.build_epoch=? AND m.source_file_id=o.source_file_id AND m.expected_file_generation=o.file_generation
		WHERE o.source='codex' AND o.ledger_epoch=? AND m.expected_root_session_id=?
		UNION SELECT m.resolved_event_id FROM codex_compaction_markers m JOIN codex_usage_build_sources b
		ON b.build_epoch=? AND b.source_file_id=m.source_file_id AND b.expected_file_generation=m.file_generation
		WHERE m.source='codex' AND m.ledger_epoch=? AND b.expected_root_session_id=? AND m.resolved_event_id IS NOT NULL
		UNION SELECT h.event_id FROM codex_usage_event_holds h JOIN codex_usage_build_sources b
		ON b.build_epoch=? AND b.source_file_id=h.source_file_id AND b.expected_file_generation=h.file_generation
		WHERE h.source='codex' AND h.ledger_epoch=? AND b.expected_root_session_id=?
		UNION SELECT e.event_id FROM codex_turns t JOIN codex_usage_build_sources b
		ON b.build_epoch=? AND b.source_file_id=t.source_file_id AND b.expected_file_generation=t.file_generation
		JOIN usage_events e ON e.source='codex' AND e.source_epoch=t.ledger_epoch AND e.thread_id=t.thread_id AND e.turn_key=t.turn_key
		WHERE t.ledger_epoch=? AND b.expected_root_session_id=? AND e.event_kind='turn_compensation'
		ORDER BY event_id`, epoch, rootID,
		epoch, rootID,
		buildEpoch, epoch, rootID,
		buildEpoch, epoch, rootID,
		buildEpoch, epoch, rootID,
		buildEpoch, epoch, rootID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var eventIDs []string
	for rows.Next() {
		var eventID string
		if err := rows.Scan(&eventID); err != nil {
			return nil, err
		}
		eventIDs = append(eventIDs, eventID)
	}
	return eventIDs, rows.Err()
}

func deleteQuarantineSemanticRows(private storage.PrivateTx, epoch, buildEpoch int64, rootID string, members []quarantineMember, eventIDs []string) error {
	if _, err := private.Exec(`DELETE FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=? AND EXISTS(
		SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=?
		AND m.source_file_id=codex_compaction_markers.source_file_id AND m.expected_file_generation=codex_compaction_markers.file_generation)`, epoch, buildEpoch, rootID); err != nil {
		return err
	}
	for _, eventID := range eventIDs {
		if _, err := private.Exec(`DELETE FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=? AND event_id=?`, epoch, eventID); err != nil {
			return err
		}
	}
	deletes := []string{
		`DELETE FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?`,
		`DELETE FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?`,
		`DELETE FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?`,
	}
	for _, query := range deletes {
		for _, member := range members {
			if _, err := private.Exec(query, epoch, member.sourceFileID, member.generation); err != nil {
				return err
			}
		}
	}
	if _, err := private.Exec(`DELETE FROM codex_turns WHERE ledger_epoch=? AND (
		EXISTS(SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=? AND m.source_file_id=codex_turns.source_file_id AND m.expected_file_generation=codex_turns.file_generation)
		OR EXISTS(SELECT 1 FROM threads t WHERE t.thread_id=codex_turns.thread_id AND COALESCE(t.root_session_id,t.thread_id)=?))`, epoch, buildEpoch, rootID, rootID); err != nil {
		return err
	}
	if _, err := private.Exec(`DELETE FROM codex_usage_source_states WHERE ledger_epoch=? AND (
		EXISTS(SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=? AND m.source_file_id=codex_usage_source_states.source_file_id AND m.expected_file_generation=codex_usage_source_states.file_generation)
		OR root_session_id=?)`, epoch, buildEpoch, rootID, rootID); err != nil {
		return err
	}
	if _, err := private.Exec(`DELETE FROM codex_skill_usage_events WHERE ledger_epoch=? AND (
		EXISTS(SELECT 1 FROM codex_usage_build_sources m WHERE m.build_epoch=? AND m.expected_root_session_id=? AND m.source_file_id=codex_skill_usage_events.source_file_id AND m.expected_file_generation=codex_skill_usage_events.file_generation)
		OR root_session_id=?)`, epoch, buildEpoch, rootID, rootID); err != nil {
		return err
	}
	return nil
}

func terminalizeQuarantinedMembers(private storage.PrivateTx, buildEpoch int64, rootID, code string, committedAtMS int64) error {
	result, err := private.Exec(`UPDATE codex_usage_build_sources SET
		completion_status='quarantined',completion_error_code=?,completed_generation=NULL,completed_through_offset=NULL,
		carry_from_epoch=NULL,carry_phase='none',carry_after_start_offset=NULL,carry_after_turn_key=NULL,
		carry_after_anomaly_id=NULL,carry_after_fact_event_id=NULL,carry_after_marker_start_offset=NULL,
		carry_after_window_start_offset=NULL,updated_at_ms=?
		WHERE build_epoch=? AND expected_root_session_id=?`, code, committedAtMS, buildEpoch, rootID)
	if err != nil {
		return err
	}
	rows, err := result.RowsAffected()
	if err != nil {
		return err
	}
	if rows == 0 {
		return ErrStaleQuarantineProof
	}
	return nil
}

func replaceQuarantineProofs(private storage.PrivateTx, epoch int64, rootID string, proofs []usage.QuarantineSourceProof, updatedAtMS int64) error {
	if _, err := private.Exec(`DELETE FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=? AND root_session_id=?`, epoch, rootID); err != nil {
		return err
	}
	ordered := append([]usage.QuarantineSourceProof(nil), proofs...)
	sort.Slice(ordered, func(i, j int) bool { return ordered[i].SourceFileID < ordered[j].SourceFileID })
	for _, proof := range ordered {
		if _, err := private.Exec(`INSERT INTO codex_usage_session_quarantine_sources(
			ledger_epoch,root_session_id,source_file_id,file_generation,device_id,inode,observed_size,updated_at_ms
		) VALUES(?,?,?,?,?,?,?,?)`, epoch, rootID, proof.SourceFileID, proof.Generation, proof.DeviceID, proof.Inode, proof.ObservedSize, updatedAtMS); err != nil {
			return err
		}
	}
	return nil
}

func loadQuarantineProofs(reader quarantineReader, epoch int64, rootID string) ([]usage.QuarantineSourceProof, error) {
	rows, err := reader.Query(`SELECT source_file_id,file_generation,device_id,inode,observed_size
		FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=? AND root_session_id=? ORDER BY source_file_id`, epoch, rootID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var proofs []usage.QuarantineSourceProof
	for rows.Next() {
		var proof usage.QuarantineSourceProof
		if err := rows.Scan(&proof.SourceFileID, &proof.Generation, &proof.DeviceID, &proof.Inode, &proof.ObservedSize); err != nil {
			return nil, err
		}
		proofs = append(proofs, proof)
	}
	return proofs, rows.Err()
}

func validateQuarantineProofList(proofs []usage.QuarantineSourceProof) error {
	seen := make(map[int64]struct{}, len(proofs))
	for _, proof := range proofs {
		if proof.SourceFileID <= 0 || proof.Generation <= 0 || proof.DeviceID < 0 || proof.Inode < 0 || proof.ObservedSize < 0 {
			return ErrStaleQuarantineProof
		}
		if _, exists := seen[proof.SourceFileID]; exists {
			return ErrStaleQuarantineProof
		}
		seen[proof.SourceFileID] = struct{}{}
	}
	return nil
}

func equalQuarantineProofs(left, right []usage.QuarantineSourceProof) bool {
	if len(left) != len(right) {
		return false
	}
	leftByID := make(map[int64]usage.QuarantineSourceProof, len(left))
	for _, proof := range left {
		leftByID[proof.SourceFileID] = proof
	}
	for _, proof := range right {
		if leftByID[proof.SourceFileID] != proof {
			return false
		}
	}
	return true
}

func validFatalConflictCode(code usage.FatalConflictCode) bool {
	switch code {
	case usage.FatalResponseUsage, usage.FatalResponseOwnership, usage.FatalCompactionIdentity, usage.FatalLegacyCoverage, usage.FatalArithmeticOverflow:
		return true
	default:
		return false
	}
}

func rootLastActivity(reader quarantineReader, rootID string) (int64, error) {
	var lastActivityAtMS int64
	err := reader.QueryRow(`SELECT COALESCE(MAX(COALESCE(updated_at_ms,created_at_ms,0)),0)
		FROM threads WHERE thread_id=? OR root_session_id=?`, rootID, rootID).Scan(&lastActivityAtMS)
	return lastActivityAtMS, err
}

func loadQuarantineProjection(reader quarantineReader, epoch int64, rootID string) (quarantineProjection, error) {
	var projection quarantineProjection
	err := reader.QueryRow(`SELECT primary_error_code,last_activity_at_ms,first_seen_at_ms
		FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?`, epoch, rootID).Scan(
		&projection.primaryErrorCode, &projection.lastActivityAtMS, &projection.firstSeenAtMS)
	if errors.Is(err, sql.ErrNoRows) {
		return quarantineProjection{}, ErrStaleQuarantineProof
	}
	return projection, err
}
