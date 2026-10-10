package rebuild

import (
	"database/sql"
	"errors"
	"fmt"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

func verifyActivation(
	tx *source.WriteTx,
	buildEpoch, parserVersion int64,
	redundancyProofs []RedundancyActivationProof,
) error {
	if err := verifyPresentManifest(tx, buildEpoch); err != nil {
		return err
	}
	members, err := loadManifestMembers(tx, buildEpoch)
	if err != nil {
		return err
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return err
	}
	contributors, err := activeContributors(tx, state.ActiveEpoch)
	if err != nil {
		return err
	}
	manifestIDs := make(map[int64]struct{}, len(members))
	for _, member := range members {
		manifestIDs[member.sourceFileID] = struct{}{}
	}
	for sourceFileID := range contributors {
		if _, ok := manifestIDs[sourceFileID]; !ok {
			return fmt.Errorf("%w: active contributor %d is absent from build manifest", ErrActivationBlocked, sourceFileID)
		}
	}
	proofBySource := make(map[int64]RedundancyActivationProof, len(redundancyProofs))
	for _, proof := range redundancyProofs {
		if proof.SourceFileID < 1 {
			return fmt.Errorf("%w: redundancy proof has invalid source", ErrActivationBlocked)
		}
		if _, exists := proofBySource[proof.SourceFileID]; exists {
			return fmt.Errorf("%w: duplicate redundancy proof for source %d", ErrActivationBlocked, proof.SourceFileID)
		}
		proofBySource[proof.SourceFileID] = proof
	}
	var holds int64
	if err := tx.Private(func(private storage.PrivateTx) error {
		return private.QueryRow(`SELECT count(*) FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=?`, buildEpoch).Scan(&holds)
	}); err != nil {
		return err
	}
	if holds != 0 {
		return fmt.Errorf("%w: build has unresolved event holds", ErrActivationBlocked)
	}
	quarantinedRoots := make(map[string][]manifestMember)
	consumedRedundancyProofs := make(map[int64]struct{}, len(redundancyProofs))
	for _, member := range members {
		if member.completion == completionQuarantined {
			if !member.root.Valid {
				return fmt.Errorf("%w: quarantined member has no root", ErrActivationBlocked)
			}
			quarantinedRoots[member.root.String] = append(quarantinedRoots[member.root.String], member)
			continue
		}
		if member.completion != completionRebuilt && member.completion != completionCarried {
			return fmt.Errorf("%w: member %d is not terminal", ErrActivationBlocked, member.sourceFileID)
		}
		if err := verifyCompletedMember(tx, buildEpoch, parserVersion, member, proofBySource, consumedRedundancyProofs); err != nil {
			return err
		}
	}
	if len(consumedRedundancyProofs) != len(proofBySource) {
		return fmt.Errorf("%w: redundancy proofs do not exactly match rebuilt zero-state members", ErrActivationBlocked)
	}
	for root, members := range quarantinedRoots {
		if err := verifyQuarantinedRoot(tx, buildEpoch, root, members); err != nil {
			return err
		}
	}
	return nil
}

func verifyPresentManifest(tx *source.WriteTx, buildEpoch int64) error {
	manifest := make(map[int64]manifestMember)
	members, err := loadManifestMembers(tx, buildEpoch)
	if err != nil {
		return err
	}
	for _, member := range members {
		manifest[member.sourceFileID] = member
	}
	var current []int64
	err = tx.Private(func(private storage.PrivateTx) error {
		rows, err := private.Query("SELECT source_file_id FROM codex_source_files WHERE file_status='present' ORDER BY source_file_id")
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var id int64
			if err := rows.Scan(&id); err != nil {
				return err
			}
			current = append(current, id)
		}
		return rows.Err()
	})
	if err != nil {
		return err
	}
	for _, id := range current {
		member, ok := manifest[id]
		if !ok {
			return fmt.Errorf("%w: current present source %d is absent from build manifest", ErrActivationBlocked, id)
		}
		var row sourceRow
		if err := tx.Private(func(private storage.PrivateTx) error { return readSourceRow(private, id, &row) }); err != nil {
			return err
		}
		if row.status != "present" || row.generation != member.expectedGeneration || row.deviceID != member.deviceID ||
			row.inode != member.inode || row.observed != member.observedRawSize {
			return fmt.Errorf("%w: current source %d no longer matches its manifest proof", ErrActivationBlocked, id)
		}
	}
	for id, member := range manifest {
		var row sourceRow
		if err := tx.Private(func(private storage.PrivateTx) error { return readSourceRow(private, id, &row) }); err != nil {
			return err
		}
		if row.status == "missing" {
			if row.generation != member.expectedGeneration || row.deviceID != member.deviceID || row.inode != member.inode || row.observed != member.observedRawSize {
				return fmt.Errorf("%w: missing member %d lost its durable identity proof", ErrActivationBlocked, id)
			}
			continue
		}
		if row.status != "present" {
			return fmt.Errorf("%w: member %d has invalid catalog status", ErrActivationBlocked, id)
		}
	}
	return nil
}

func verifyCompletedMember(
	tx *source.WriteTx,
	buildEpoch, parserVersion int64,
	member manifestMember,
	proofs map[int64]RedundancyActivationProof,
	consumedProofs map[int64]struct{},
) error {
	if !member.completedGeneration.Valid || member.completedGeneration.Int64 != member.expectedGeneration ||
		!member.completedOffset.Valid || member.completedOffset.Int64 < member.requiredOffset {
		return fmt.Errorf("%w: member %d has invalid completion boundary", ErrActivationBlocked, member.sourceFileID)
	}
	var stateCount int64
	err := tx.Private(func(private storage.PrivateTx) error {
		return private.QueryRow("SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=?", buildEpoch, member.sourceFileID).Scan(&stateCount)
	})
	if err != nil {
		return err
	}
	if stateCount == 0 {
		if member.completion != completionRebuilt {
			return fmt.Errorf("%w: carried member %d has no build source state", ErrActivationBlocked, member.sourceFileID)
		}
		proof, ok := proofs[member.sourceFileID]
		if !ok {
			return fmt.Errorf("%w: rebuilt zero-state member %d lacks a fresh redundancy proof", ErrActivationBlocked, member.sourceFileID)
		}
		if err := verifyRedundantBoundary(tx, buildEpoch, parserVersion, member, proof); err != nil {
			return err
		}
		consumedProofs[member.sourceFileID] = struct{}{}
		return nil
	}
	var catalog sourceRow
	if err := tx.Private(func(private storage.PrivateTx) error { return readSourceRow(private, member.sourceFileID, &catalog) }); err != nil {
		return err
	}
	var stateGeneration, stateDevice, stateInode, stateParser, resolved, observed sql.NullInt64
	var stateOwner, stateRoot, tailStatus sql.NullString
	var tailStart sql.NullInt64
	err = tx.Private(func(private storage.PrivateTx) error {
		return private.QueryRow(`SELECT file_generation,device_id,inode,usage_parser_version,resolved_through_offset,
			observed_raw_size,raw_tail_status,raw_tail_start_offset,owning_thread_id,root_session_id FROM codex_usage_source_states
			WHERE ledger_epoch=? AND source_file_id=?`, buildEpoch, member.sourceFileID).
			Scan(&stateGeneration, &stateDevice, &stateInode, &stateParser, &resolved, &observed, &tailStatus, &tailStart, &stateOwner, &stateRoot)
	})
	if err != nil {
		return err
	}
	var checkpointParser, checkpointOffset sql.NullInt64
	var guard []byte
	var status sql.NullString
	err = tx.Private(func(private storage.PrivateTx) error {
		return private.QueryRow(`SELECT parser_version,committed_offset,guard_hash,processing_status FROM codex_source_checkpoints
			WHERE source_file_id=? AND consumer_kind='usage'`, member.sourceFileID).
			Scan(&checkpointParser, &checkpointOffset, &guard, &status)
	})
	if err != nil {
		return err
	}
	if !stateGeneration.Valid || stateGeneration.Int64 != member.expectedGeneration || !stateDevice.Valid || stateDevice.Int64 != member.deviceID ||
		!stateInode.Valid || stateInode.Int64 != member.inode || !stateParser.Valid || stateParser.Int64 != parserVersion ||
		!resolved.Valid || resolved.Int64 < member.requiredOffset || !observed.Valid || observed.Int64 != member.observedRawSize ||
		!equalNullable(member.owner, stringPointer(stateOwner)) || !equalNullable(member.root, stringPointer(stateRoot)) ||
		!checkpointParser.Valid || checkpointParser.Int64 != parserVersion || !checkpointOffset.Valid ||
		status.String != "ready" || resolved.Int64 != checkpointOffset.Int64 || tailStatus.String != member.tailStatus ||
		!equalNullableInt(tailStart, member.tailStart) ||
		!validCompletedPhysicalBoundary(member, checkpointOffset.Int64, guard, catalog.path, observed.Int64, tailStatus.String, tailStart) {
		return fmt.Errorf("%w: member %d has invalid build source-state/checkpoint proof", ErrActivationBlocked, member.sourceFileID)
	}
	return nil
}

func verifyRedundantBoundary(
	tx *source.WriteTx,
	buildEpoch, parserVersion int64,
	member manifestMember,
	proof RedundancyActivationProof,
) error {
	if proof.SourceFileID != member.sourceFileID || proof.Generation != member.expectedGeneration ||
		proof.WinnerSourceFileID < 1 || proof.WinnerSourceFileID == member.sourceFileID || proof.WinnerGeneration < 1 ||
		proof.RequiredThroughOffset != member.requiredOffset {
		return fmt.Errorf("%w: redundancy proof does not match member %d", ErrActivationBlocked, member.sourceFileID)
	}
	var row, winner sourceRow
	var checkpointParser, checkpointOffset sql.NullInt64
	var checkpointStatus sql.NullString
	var guard []byte
	err := tx.Private(func(private storage.PrivateTx) error {
		if err := readSourceRow(private, member.sourceFileID, &row); err != nil {
			return err
		}
		if err := readSourceRow(private, proof.WinnerSourceFileID, &winner); err != nil {
			return err
		}
		return private.QueryRow(`SELECT parser_version,committed_offset,guard_hash,processing_status FROM codex_source_checkpoints
			WHERE source_file_id=? AND consumer_kind='usage'`, member.sourceFileID).
			Scan(&checkpointParser, &checkpointOffset, &guard, &checkpointStatus)
	})
	if err != nil {
		return err
	}
	winnerMember, err := readMember(tx, buildEpoch, proof.WinnerSourceFileID)
	if err != nil {
		return err
	}
	if row.status != "present" || row.generation != member.expectedGeneration || row.deviceID != member.deviceID || row.inode != member.inode ||
		row.observed != member.observedRawSize || !checkpointParser.Valid || checkpointParser.Int64 != parserVersion ||
		!checkpointOffset.Valid || checkpointOffset.Int64 < member.requiredOffset || checkpointStatus.String != "ready" ||
		winner.status != "present" || winner.generation != proof.WinnerGeneration ||
		winnerMember.expectedGeneration != proof.WinnerGeneration || winnerMember.sourceFileID == member.sourceFileID ||
		!validPhysicalGuard(checkpointOffset.Int64, guard, row.path, row.observed, member.tailStatus) {
		return fmt.Errorf("%w: redundant member %d lacks a matching fresh equality proof and physical completion", ErrActivationBlocked, member.sourceFileID)
	}
	for _, table := range []string{"codex_usage_event_occurrences", "codex_skill_usage_events", "codex_usage_event_holds", "codex_usage_reconciliation_windows", "codex_compaction_markers", "codex_turns"} {
		var count int64
		if err := tx.Private(func(private storage.PrivateTx) error {
			return private.QueryRow(fmt.Sprintf("SELECT count(*) FROM %s WHERE ledger_epoch=? AND source_file_id=?", table), buildEpoch, member.sourceFileID).Scan(&count)
		}); err != nil {
			return err
		}
		if count != 0 {
			return fmt.Errorf("%w: redundant member %d has semantic contribution", ErrActivationBlocked, member.sourceFileID)
		}
	}
	return nil
}

func verifyQuarantinedRoot(tx *source.WriteTx, buildEpoch int64, root string, members []manifestMember) error {
	all, err := loadManifestMembers(tx, buildEpoch)
	if err != nil {
		return err
	}
	rootMembers := make([]manifestMember, 0)
	for _, member := range all {
		if member.root.Valid && member.root.String == root {
			if member.completion != completionQuarantined {
				return fmt.Errorf("%w: root %s is only partially quarantined", ErrActivationBlocked, root)
			}
			rootMembers = append(rootMembers, member)
		}
	}
	if len(rootMembers) != len(members) {
		return fmt.Errorf("%w: root quarantine manifest membership changed", ErrActivationBlocked)
	}
	var code string
	err = tx.Private(func(private storage.PrivateTx) error {
		return private.QueryRow("SELECT primary_error_code FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?", buildEpoch, root).Scan(&code)
	})
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return fmt.Errorf("%w: root %s has no quarantine row", ErrActivationBlocked, root)
		}
		return err
	}
	for _, member := range members {
		if !member.errorCode.Valid || member.errorCode.String != code {
			return fmt.Errorf("%w: root %s quarantine error code differs across members", ErrActivationBlocked, root)
		}
	}
	var proofRows []quarantineProof
	err = tx.Private(func(private storage.PrivateTx) error {
		rows, err := private.Query(`SELECT source_file_id,file_generation,device_id,inode,observed_size
			FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=? AND root_session_id=? ORDER BY source_file_id`, buildEpoch, root)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var proof quarantineProof
			if err := rows.Scan(&proof.sourceFileID, &proof.generation, &proof.deviceID, &proof.inode, &proof.observedSize); err != nil {
				return err
			}
			proofRows = append(proofRows, proof)
		}
		return rows.Err()
	})
	if err != nil {
		return err
	}
	var present []manifestMember
	for _, member := range rootMembers {
		var row sourceRow
		if err := tx.Private(func(private storage.PrivateTx) error { return readSourceRow(private, member.sourceFileID, &row) }); err != nil {
			return err
		}
		if row.status == "present" {
			present = append(present, member)
		}
	}
	if len(proofRows) != len(present) {
		return fmt.Errorf("%w: root %s quarantine proof set is incomplete", ErrActivationBlocked, root)
	}
	for index, member := range present {
		proof := proofRows[index]
		var row sourceRow
		if err := tx.Private(func(private storage.PrivateTx) error { return readSourceRow(private, member.sourceFileID, &row) }); err != nil {
			return err
		}
		if proof.sourceFileID != member.sourceFileID || proof.generation != row.generation || proof.deviceID != row.deviceID ||
			proof.inode != row.inode || proof.observedSize != row.observed {
			return fmt.Errorf("%w: root %s quarantine proof is stale", ErrActivationBlocked, root)
		}
	}
	if err := verifyQuarantineNoLeak(tx, buildEpoch, rootMembers); err != nil {
		return err
	}
	return nil
}

type quarantineProof struct {
	sourceFileID, generation, deviceID, inode, observedSize int64
}

func verifyQuarantineNoLeak(tx *source.WriteTx, buildEpoch int64, members []manifestMember) error {
	if len(members) == 0 || !members[0].root.Valid {
		return fmt.Errorf("%w: quarantined root has no manifest members", ErrActivationBlocked)
	}
	root := members[0].root.String
	sourceIDs := make([]int64, 0, len(members))
	for _, member := range members {
		sourceIDs = append(sourceIDs, member.sourceFileID)
	}
	var count int64
	check := func(description, query string, args ...any) error {
		if err := tx.Private(func(private storage.PrivateTx) error { return private.QueryRow(query, args...).Scan(&count) }); err != nil {
			return err
		}
		if count != 0 {
			return fmt.Errorf("%w: quarantined root retains %s", ErrActivationBlocked, description)
		}
		return nil
	}
	checkSourceMembers := func(table string) error {
		query, args := inQuery(fmt.Sprintf("SELECT count(*) FROM %s WHERE ledger_epoch=? AND source_file_id IN (%%s)", table), buildEpoch, sourceIDs)
		return check(table+" rows for manifest sources", query, args...)
	}
	for _, table := range []string{
		"codex_usage_event_occurrences", "codex_usage_reconciliation_windows", "codex_usage_event_holds",
		"codex_compaction_markers", "codex_turns", "codex_usage_source_states", "codex_skill_usage_events",
	} {
		if err := checkSourceMembers(table); err != nil {
			return err
		}
	}
	rootThreads := "SELECT thread_id FROM threads WHERE thread_id=? OR root_session_id=?"
	if err := check("source-state root attribution", `SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=?
		AND (root_session_id=? OR owning_thread_id IN (`+rootThreads+`))`, buildEpoch, root, root, root); err != nil {
		return err
	}
	if err := check("skill root attribution", `SELECT count(*) FROM codex_skill_usage_events WHERE ledger_epoch=?
		AND (root_session_id=? OR thread_id IN (`+rootThreads+`))`, buildEpoch, root, root, root); err != nil {
		return err
	}
	if err := check("marker root attribution", `SELECT count(*) FROM codex_compaction_markers WHERE ledger_epoch=?
		AND (root_session_id=? OR owning_thread_id IN (`+rootThreads+`))`, buildEpoch, root, root, root); err != nil {
		return err
	}
	if err := check("window root attribution", `SELECT count(*) FROM codex_usage_reconciliation_windows WHERE ledger_epoch=?
		AND owning_thread_id IN (`+rootThreads+`)`, buildEpoch, root, root); err != nil {
		return err
	}
	if err := check("turn root attribution", `SELECT count(*) FROM codex_turns WHERE ledger_epoch=?
		AND thread_id IN (`+rootThreads+`)`, buildEpoch, root, root); err != nil {
		return err
	}
	if err := check("canonical usage", `SELECT count(*) FROM usage_events e WHERE e.source='codex' AND e.source_epoch=?
		AND (e.root_session_id=? OR e.thread_id IN (`+rootThreads+`))`, buildEpoch, root, root, root); err != nil {
		return err
	}
	for _, table := range []string{"codex_usage_event_occurrences", "codex_usage_event_holds"} {
		query := fmt.Sprintf(`SELECT count(*) FROM %s p WHERE p.source='codex' AND p.ledger_epoch=?
			AND p.event_id IN (SELECT e.event_id FROM usage_events e WHERE e.source='codex' AND e.source_epoch=?
			AND (e.root_session_id=? OR e.thread_id IN (`+rootThreads+`)))`, table)
		if err := check(table+" attributed to canonical root", query, buildEpoch, buildEpoch, root, root, root); err != nil {
			return err
		}
	}
	if err := check("facts by owning thread", `SELECT count(*) FROM codex_usage_event_facts f WHERE f.source='codex' AND f.ledger_epoch=?
		AND f.owning_thread_id IN (`+rootThreads+`)`, buildEpoch, root, root); err != nil {
		return err
	}
	if err := check("facts by canonical root event", `SELECT count(*) FROM codex_usage_event_facts f WHERE f.source='codex' AND f.ledger_epoch=?
		AND f.event_id IN (SELECT e.event_id FROM usage_events e WHERE e.source='codex' AND e.source_epoch=?
		AND (e.root_session_id=? OR e.thread_id IN (`+rootThreads+`)))`, buildEpoch, buildEpoch, root, root, root); err != nil {
		return err
	}
	for _, scoped := range []struct{ description, table, eventColumn string }{
		{"facts by member occurrence", "codex_usage_event_occurrences", "event_id"},
		{"facts by member hold", "codex_usage_event_holds", "event_id"},
		{"facts by member resolved marker", "codex_compaction_markers", "resolved_event_id"},
	} {
		query, innerArgs := inQuery(fmt.Sprintf(`SELECT count(*) FROM codex_usage_event_facts f WHERE f.source='codex' AND f.ledger_epoch=?
			AND f.event_id IN (SELECT %s FROM %s WHERE source='codex' AND ledger_epoch=? AND source_file_id IN (%%s))`,
			scoped.eventColumn, scoped.table), buildEpoch, sourceIDs)
		args := append([]any{buildEpoch}, innerArgs...)
		if err := check(scoped.description, query, args...); err != nil {
			return err
		}
	}
	return nil
}
