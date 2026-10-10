package rebuild

import (
	"database/sql"
	"errors"
	"fmt"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

func RecordRebuilt(
	tx *source.WriteTx,
	buildEpoch, sourceFileID int64,
	fullCompressedViewProof bool,
	committedAtMS int64,
) error {
	if tx == nil || buildEpoch < 1 || sourceFileID < 1 || committedAtMS < 0 {
		return ErrInvalidRebuildInput
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return err
	}
	if state.BuildEpoch == nil || *state.BuildEpoch != buildEpoch || state.BuildParserVersion == nil {
		return fmt.Errorf("%w: rebuilt member has no matching target", ErrActivationBlocked)
	}
	member, err := readMember(tx, buildEpoch, sourceFileID)
	if err != nil {
		return err
	}
	var catalog sourceRow
	var checkpointParser, checkpointOffset sql.NullInt64
	var checkpointGuard []byte
	var checkpointStatus sql.NullString
	var sourceGeneration, sourceDevice, sourceInode, sourceParser, resolved, observed sql.NullInt64
	var tailStatus sql.NullString
	var tailStart sql.NullInt64
	err = tx.Private(func(private storage.PrivateTx) error {
		if err := readSourceRow(private, sourceFileID, &catalog); err != nil {
			return err
		}
		if err := private.QueryRow(`SELECT parser_version,committed_offset,guard_hash,processing_status
			FROM codex_source_checkpoints WHERE source_file_id=? AND consumer_kind='usage'`, sourceFileID).
			Scan(&checkpointParser, &checkpointOffset, &checkpointGuard, &checkpointStatus); err != nil {
			return err
		}
		return private.QueryRow(`SELECT file_generation,device_id,inode,usage_parser_version,resolved_through_offset,
			observed_raw_size,raw_tail_status,raw_tail_start_offset FROM codex_usage_source_states
			WHERE ledger_epoch=? AND source_file_id=?`, buildEpoch, sourceFileID).
			Scan(&sourceGeneration, &sourceDevice, &sourceInode, &sourceParser, &resolved, &observed, &tailStatus, &tailStart)
	})
	if err != nil {
		return err
	}
	if catalog.status != "present" || catalog.generation != member.expectedGeneration || catalog.deviceID != member.deviceID ||
		catalog.inode != member.inode || catalog.observed != member.observedRawSize ||
		!sourceGeneration.Valid || sourceGeneration.Int64 != member.expectedGeneration || !sourceDevice.Valid || sourceDevice.Int64 != member.deviceID ||
		!sourceInode.Valid || sourceInode.Int64 != member.inode || !sourceParser.Valid || sourceParser.Int64 != *state.BuildParserVersion ||
		!resolved.Valid || resolved.Int64 < member.requiredOffset || !observed.Valid || observed.Int64 != member.observedRawSize ||
		!checkpointParser.Valid || checkpointParser.Int64 != *state.BuildParserVersion || !checkpointOffset.Valid ||
		checkpointStatus.String != "ready" || resolved.Int64 != checkpointOffset.Int64 ||
		!validCompletedPhysicalBoundary(member, checkpointOffset.Int64, checkpointGuard, catalog.path, observed.Int64,
			tailStatus.String, tailStart) {
		return fmt.Errorf("%w: rebuilt member lacks a matching physical checkpoint/source-state boundary", ErrActivationBlocked)
	}
	if tailStatus.String == "unverified" {
		if !strings.HasSuffix(catalog.path, ".jsonl.zst") || !fullCompressedViewProof ||
			member.requiredOffset != member.observedRawSize || checkpointOffset.Int64 != member.observedRawSize {
			return fmt.Errorf("%w: unverified compressed tail lacks full-view proof", ErrActivationBlocked)
		}
	}
	return tx.Private(func(private storage.PrivateTx) error {
		result, err := private.Exec(`UPDATE codex_usage_build_sources SET raw_tail_status=?,raw_tail_start_offset=?,
			completion_status='rebuilt',completion_error_code=NULL,completed_generation=required_generation,
			completed_through_offset=?,carry_from_epoch=NULL,carry_phase='none',carry_after_start_offset=NULL,
			carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,carry_after_fact_event_id=NULL,
			carry_after_marker_start_offset=NULL,carry_after_window_start_offset=NULL,updated_at_ms=?
			WHERE build_epoch=? AND source_file_id=? AND completion_status IN ('pending','rebuilt')`,
			tailStatus.String, nullableIntValue(tailStart), checkpointOffset.Int64, committedAtMS, buildEpoch, sourceFileID)
		if err != nil {
			return err
		}
		changed, err := result.RowsAffected()
		if err != nil {
			return err
		}
		if changed != 1 {
			return fmt.Errorf("%w: rebuilt member completion CAS failed", ErrActivationBlocked)
		}
		return nil
	})
}

func RecordVerifiedRedundant(
	tx *source.WriteTx,
	buildEpoch int64,
	proof RedundancyActivationProof,
	committedAtMS int64,
) error {
	if tx == nil || buildEpoch < 1 || proof.SourceFileID < 1 || proof.Generation < 1 || proof.WinnerSourceFileID < 1 ||
		proof.WinnerGeneration < 1 || proof.SourceFileID == proof.WinnerSourceFileID || proof.RequiredThroughOffset < 0 || committedAtMS < 0 {
		return ErrInvalidRebuildInput
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return err
	}
	if state.BuildEpoch == nil || *state.BuildEpoch != buildEpoch || state.BuildParserVersion == nil {
		return fmt.Errorf("%w: redundant proof has no matching build target", ErrActivationBlocked)
	}
	member, err := readMember(tx, buildEpoch, proof.SourceFileID)
	if err != nil {
		return err
	}
	if member.expectedGeneration != proof.Generation || member.requiredOffset != proof.RequiredThroughOffset {
		return fmt.Errorf("%w: redundant proof does not match the frozen member boundary", ErrActivationBlocked)
	}
	var row, winner sourceRow
	var checkpointParser, checkpointOffset sql.NullInt64
	var checkpointStatus sql.NullString
	var checkpointGuard []byte
	var contributionCount int64
	err = tx.Private(func(private storage.PrivateTx) error {
		if err := readSourceRow(private, proof.SourceFileID, &row); err != nil {
			return err
		}
		if err := readSourceRow(private, proof.WinnerSourceFileID, &winner); err != nil {
			return err
		}
		if err := private.QueryRow(`SELECT parser_version,committed_offset,guard_hash,processing_status
			FROM codex_source_checkpoints WHERE source_file_id=? AND consumer_kind='usage'`, proof.SourceFileID).
			Scan(&checkpointParser, &checkpointOffset, &checkpointGuard, &checkpointStatus); err != nil {
			return err
		}
		return private.QueryRow(`SELECT
			(SELECT count(*) FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?) +
			(SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=?) +
			(SELECT count(*) FROM codex_skill_usage_events WHERE ledger_epoch=? AND source_file_id=? AND file_generation=?) +
			(SELECT count(*) FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?) +
			(SELECT count(*) FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?) +
			(SELECT count(*) FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?) +
			(SELECT count(*) FROM codex_turns WHERE ledger_epoch=? AND source_file_id=? AND file_generation=?)`,
			buildEpoch, proof.SourceFileID, proof.Generation, buildEpoch, proof.SourceFileID,
			buildEpoch, proof.SourceFileID, proof.Generation, buildEpoch, proof.SourceFileID, proof.Generation,
			buildEpoch, proof.SourceFileID, proof.Generation, buildEpoch, proof.SourceFileID, proof.Generation,
			buildEpoch, proof.SourceFileID, proof.Generation).Scan(&contributionCount)
	})
	if err != nil {
		return err
	}
	if row.status != "present" || row.generation != proof.Generation || row.deviceID != member.deviceID || row.inode != member.inode ||
		row.observed != member.observedRawSize || winner.status != "present" || winner.generation != proof.WinnerGeneration ||
		!checkpointParser.Valid || checkpointParser.Int64 != *state.BuildParserVersion || !checkpointOffset.Valid ||
		checkpointOffset.Int64 < proof.RequiredThroughOffset || checkpointStatus.String != "ready" || contributionCount != 0 ||
		!validPhysicalGuard(checkpointOffset.Int64, checkpointGuard, row.path, row.observed, member.tailStatus) {
		return fmt.Errorf("%w: verified redundant proof lacks a physical boundary or has semantic contribution", ErrActivationBlocked)
	}
	return tx.Private(func(private storage.PrivateTx) error {
		result, err := private.Exec(`UPDATE codex_usage_build_sources SET completion_status='rebuilt',completion_error_code=NULL,
			completed_generation=required_generation,completed_through_offset=?,carry_from_epoch=NULL,carry_phase='none',
			carry_after_start_offset=NULL,carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,carry_after_fact_event_id=NULL,
			carry_after_marker_start_offset=NULL,carry_after_window_start_offset=NULL,updated_at_ms=?
			WHERE build_epoch=? AND source_file_id=? AND completion_status IN ('pending','rebuilt')`,
			checkpointOffset.Int64, committedAtMS, buildEpoch, proof.SourceFileID)
		if err != nil {
			return err
		}
		changed, err := result.RowsAffected()
		if err != nil {
			return err
		}
		if changed != 1 {
			return fmt.Errorf("%w: redundant completion CAS failed", ErrActivationBlocked)
		}
		return nil
	})
}

func RecordQuarantined(tx *source.WriteTx, buildEpoch, sourceFileID int64, errorCode string, committedAtMS int64) error {
	if tx == nil || buildEpoch < 1 || sourceFileID < 1 || errorCode == "" || committedAtMS < 0 {
		return ErrInvalidRebuildInput
	}
	return tx.Private(func(private storage.PrivateTx) error {
		result, err := private.Exec(`UPDATE codex_usage_build_sources SET completion_status='quarantined',completion_error_code=?,
			completed_generation=NULL,completed_through_offset=NULL,carry_from_epoch=NULL,carry_phase='none',
			carry_after_start_offset=NULL,carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,carry_after_fact_event_id=NULL,
			carry_after_marker_start_offset=NULL,carry_after_window_start_offset=NULL,updated_at_ms=?
			WHERE build_epoch=? AND source_file_id=?`, errorCode, committedAtMS, buildEpoch, sourceFileID)
		if err != nil {
			return err
		}
		changed, err := result.RowsAffected()
		if err != nil {
			return err
		}
		if changed != 1 {
			return errors.New("quarantine member is absent from build manifest")
		}
		return nil
	})
}

func validCompletedPhysicalBoundary(
	member manifestMember,
	offset int64,
	guard []byte,
	path string,
	observedSize int64,
	tailStatus string,
	tailStart sql.NullInt64,
) bool {
	if !validRawTailState(tailStatus, tailStart, member.requiredOffset, observedSize) ||
		!validPhysicalGuard(offset, guard, path, observedSize, tailStatus) {
		return false
	}
	switch tailStatus {
	case "none":
		return offset == observedSize
	case "half_line":
		return offset == member.requiredOffset
	case "unverified":
		return strings.HasSuffix(path, ".jsonl.zst") && member.requiredOffset == observedSize && offset == observedSize
	default:
		return false
	}
}
