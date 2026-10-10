package rebuild

import (
	"database/sql"
	"errors"
	"fmt"
	"sort"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

var ErrActivationBlocked = errors.New("usage build is not eligible for activation")

const (
	completionPending     = "pending"
	completionRebuilt     = "rebuilt"
	completionCarried     = "carried"
	completionBlocked     = "blocked"
	completionQuarantined = "quarantined"

	carryNone = "none"
)

type RedundancyActivationProof struct {
	SourceFileID          int64
	Generation            int64
	WinnerSourceFileID    int64
	WinnerGeneration      int64
	RequiredThroughOffset int64
}

type manifestMember struct {
	sourceFileID        int64
	parserVersion       int64
	expectedGeneration  int64
	deviceID            int64
	inode               int64
	owner               sql.NullString
	root                sql.NullString
	activeOffset        int64
	activeGuard         []byte
	activeFingerprint   []byte
	requiredOffset      int64
	observedRawSize     int64
	tailStatus          string
	tailStart           sql.NullInt64
	membershipReason    string
	completion          string
	errorCode           sql.NullString
	completedGeneration sql.NullInt64
	completedOffset     sql.NullInt64
	carryFrom           sql.NullInt64
	carryPhase          string
	afterStartOffset    sql.NullInt64
	afterTurnKey        sql.NullString
	afterAnomalyID      sql.NullString
	afterFactEventID    sql.NullString
	afterMarkerOffset   sql.NullInt64
	afterWindowOffset   sql.NullInt64
}

type sourceRow struct {
	generation int64
	deviceID   int64
	inode      int64
	observed   int64
	status     string
	owner      sql.NullString
	root       sql.NullString
	path       string
}

func validateRequirements(requirements []MemberRequirement) error {
	seen := make(map[int64]struct{}, len(requirements))
	for _, requirement := range requirements {
		if requirement.SourceFileID <= 0 || requirement.Generation <= 0 ||
			requirement.DeviceID < 0 || requirement.Inode < 0 ||
			requirement.RequiredThroughOffset < 0 || requirement.ObservedRawSize < 0 ||
			requirement.RequiredThroughOffset > requirement.ObservedRawSize {
			return fmt.Errorf("%w: invalid member requirement", ErrInvalidRebuildInput)
		}
		if _, ok := seen[requirement.SourceFileID]; ok {
			return fmt.Errorf("%w: duplicate source file requirement", ErrInvalidRebuildInput)
		}
		seen[requirement.SourceFileID] = struct{}{}
		if err := validateOptionalIdentity(requirement.ExpectedOwningThreadID); err != nil {
			return err
		}
		if err := validateOptionalIdentity(requirement.ExpectedRootSessionID); err != nil {
			return err
		}
	}
	return nil
}

func validateOptionalIdentity(value *string) error {
	if value == nil {
		return nil
	}
	if *value == "" || strings.TrimSpace(*value) != *value {
		return fmt.Errorf("%w: empty or untrimmed owner identity", ErrInvalidRebuildInput)
	}
	return nil
}

func normalizeIDs(ids []int64) ([]int64, error) {
	if len(ids) == 0 {
		return nil, nil
	}
	values := append([]int64(nil), ids...)
	sort.Slice(values, func(i, j int) bool { return values[i] < values[j] })
	result := values[:0]
	for _, id := range values {
		if id <= 0 {
			return nil, fmt.Errorf("%w: source file ID must be positive", ErrInvalidRebuildInput)
		}
		if len(result) == 0 || result[len(result)-1] != id {
			result = append(result, id)
		}
	}
	return result, nil
}

func requirementMap(requirements []MemberRequirement) map[int64]MemberRequirement {
	result := make(map[int64]MemberRequirement, len(requirements))
	for _, requirement := range requirements {
		result[requirement.SourceFileID] = requirement
	}
	return result
}

func freezeInitialManifest(
	tx *source.WriteTx,
	state domain.SourceUsageEpochState,
	buildEpoch int64,
	parserVersion int64,
	requirements []MemberRequirement,
	activeStateProof ActiveSourceStateProof,
	committedAtMS int64,
) error {
	if err := validateCurrentRequirements(tx, requirements); err != nil {
		return err
	}
	contributors, err := activeContributors(tx, state.ActiveEpoch)
	if err != nil {
		return err
	}
	byID := requirementMap(requirements)
	ids := make([]int64, 0, len(contributors)+len(byID))
	for id := range contributors {
		ids = append(ids, id)
	}
	for id := range byID {
		ids = append(ids, id)
	}
	ids, err = normalizeIDs(ids)
	if err != nil {
		return err
	}
	for _, id := range ids {
		requirement, present := byID[id]
		if !present {
			requirement, err = durableRequirement(tx, state.ActiveEpoch, id, nil)
			if err != nil {
				return err
			}
		}
		reason := "present_at_build_start"
		_, contributed := contributors[id]
		switch {
		case contributed && present:
			reason = "both"
		case contributed:
			reason = "active_contributor"
		}
		member, err := freezeMember(tx, state, buildEpoch, parserVersion, requirement, reason, activeStateProof)
		if err != nil {
			return err
		}
		if err := insertManifestMember(tx, buildEpoch, parserVersion, member, committedAtMS); err != nil {
			return err
		}
		if err := resetUsageCheckpoint(tx, id, parserVersion); err != nil {
			return err
		}
	}
	return nil
}

func activeContributors(tx *source.WriteTx, activeEpoch int64) (map[int64]struct{}, error) {
	result := make(map[int64]struct{})
	if activeEpoch <= 0 {
		return result, nil
	}
	err := tx.Private(func(private storage.PrivateTx) error {
		rows, err := private.Query(`SELECT source_file_id FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?
			UNION SELECT source_file_id FROM codex_usage_source_states WHERE ledger_epoch=?
			UNION SELECT source_file_id FROM codex_skill_usage_events WHERE ledger_epoch=?
			UNION SELECT source_file_id FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=?
			ORDER BY source_file_id`, activeEpoch, activeEpoch, activeEpoch, activeEpoch)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var id int64
			if err := rows.Scan(&id); err != nil {
				return err
			}
			result[id] = struct{}{}
		}
		return rows.Err()
	})
	return result, err
}

func durableRequirement(
	tx *source.WriteTx,
	activeEpoch int64,
	sourceFileID int64,
	manifest *manifestMember,
) (MemberRequirement, error) {
	var row sourceRow
	var resolved, stateSize sql.NullInt64
	var stateGeneration, stateDevice, stateInode sql.NullInt64
	var stateOwner, stateRoot sql.NullString
	var stateTail sql.NullString
	var tailStart sql.NullInt64
	var stateFound bool
	err := tx.Private(func(private storage.PrivateTx) error {
		if err := readSourceRow(private, sourceFileID, &row); err != nil {
			return err
		}
		if activeEpoch > 0 {
			var err error
			stateFound, err = scanOptionalRow(func() error {
				return private.QueryRow(`SELECT file_generation,device_id,inode,resolved_through_offset,observed_raw_size,
				raw_tail_status,raw_tail_start_offset,owning_thread_id,root_session_id
				FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=?`, activeEpoch, sourceFileID).
					Scan(&stateGeneration, &stateDevice, &stateInode, &resolved, &stateSize, &stateTail, &tailStart, &stateOwner, &stateRoot)
			})
			return err
		}
		return nil
	})
	if err != nil {
		return MemberRequirement{}, err
	}
	if row.status == "present" {
		return MemberRequirement{}, fmt.Errorf("%w: present contributor is missing its current requirement", ErrInvalidRebuildInput)
	}
	if row.status != "missing" {
		return MemberRequirement{}, fmt.Errorf("%w: historical source is not missing", ErrInvalidRebuildInput)
	}
	if manifest != nil {
		if row.generation != manifest.expectedGeneration || row.deviceID != manifest.deviceID || row.inode != manifest.inode || row.observed != manifest.observedRawSize {
			return MemberRequirement{}, fmt.Errorf("%w: missing source no longer matches manifest proof", ErrInvalidRebuildInput)
		}
	}
	if stateFound && stateGeneration.Valid {
		if stateGeneration.Int64 != row.generation || stateDevice.Int64 != row.deviceID || stateInode.Int64 != row.inode || stateSize.Int64 != row.observed {
			return MemberRequirement{}, fmt.Errorf("%w: missing source does not match active source-state proof", ErrInvalidRebuildInput)
		}
	}
	owner := row.owner
	root := row.root
	if stateFound && stateGeneration.Valid {
		owner, root = stateOwner, stateRoot
	}
	required := int64(0)
	if stateFound && resolved.Valid {
		required = resolved.Int64
	} else if manifest != nil {
		required = manifest.requiredOffset
	}
	if required < 0 || required > row.observed {
		return MemberRequirement{}, fmt.Errorf("%w: missing source active boundary is invalid", ErrInvalidRebuildInput)
	}
	return MemberRequirement{
		SourceFileID: sourceFileID, Generation: row.generation, DeviceID: row.deviceID, Inode: row.inode,
		RequiredThroughOffset: required, ObservedRawSize: row.observed,
		ExpectedOwningThreadID: stringPointer(owner), ExpectedRootSessionID: stringPointer(root),
	}, nil
}

func freezeMember(
	tx *source.WriteTx,
	state domain.SourceUsageEpochState,
	buildEpoch int64,
	parserVersion int64,
	requirement MemberRequirement,
	reason string,
	activeStateProof ActiveSourceStateProof,
) (manifestMember, error) {
	var row sourceRow
	var checkpointParser, checkpointOffset sql.NullInt64
	var checkpointGuard []byte
	var checkpointStatus sql.NullString
	var stateGeneration, stateDevice, stateInode, stateParser, stateResolved, stateSize sql.NullInt64
	var stateOwner, stateRoot, stateTail sql.NullString
	var stateTailStart sql.NullInt64
	var checkpointFound, stateFound bool
	err := tx.Private(func(private storage.PrivateTx) error {
		if err := validateMemberRequirement(private, requirement, &row); err != nil {
			return err
		}
		if state.ActiveEpoch <= 0 {
			return nil
		}
		var err error
		checkpointFound, err = scanOptionalRow(func() error {
			return private.QueryRow(`SELECT parser_version,committed_offset,guard_hash,processing_status
			FROM codex_source_checkpoints WHERE source_file_id=? AND consumer_kind='usage'`, requirement.SourceFileID).
				Scan(&checkpointParser, &checkpointOffset, &checkpointGuard, &checkpointStatus)
		})
		if err != nil {
			return err
		}
		stateFound, err = scanOptionalRow(func() error {
			return private.QueryRow(`SELECT file_generation,device_id,inode,usage_parser_version,resolved_through_offset,
			observed_raw_size,raw_tail_status,raw_tail_start_offset,owning_thread_id,root_session_id
			FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=?`, state.ActiveEpoch, requirement.SourceFileID).
				Scan(&stateGeneration, &stateDevice, &stateInode, &stateParser, &stateResolved, &stateSize, &stateTail, &stateTailStart, &stateOwner, &stateRoot)
		})
		if err != nil {
			return err
		}
		return nil
	})
	if err != nil {
		return manifestMember{}, err
	}
	member := manifestMember{
		sourceFileID: requirement.SourceFileID, parserVersion: parserVersion,
		expectedGeneration: requirement.Generation, deviceID: requirement.DeviceID, inode: requirement.Inode,
		owner: nullableString(requirement.ExpectedOwningThreadID), root: nullableString(requirement.ExpectedRootSessionID),
		requiredOffset: requirement.RequiredThroughOffset, observedRawSize: requirement.ObservedRawSize,
		tailStatus: "unverified", membershipReason: reason, completion: completionPending, carryPhase: carryNone,
	}
	if state.ActiveEpoch <= 0 {
		return member, nil
	}
	stateMatches := stateFound && stateGeneration.Valid && stateGeneration.Int64 == requirement.Generation &&
		stateDevice.Valid && stateDevice.Int64 == requirement.DeviceID && stateInode.Valid && stateInode.Int64 == requirement.Inode &&
		stateParser.Valid && stateParser.Int64 == state.ActiveParserVersion && stateSize.Valid && stateSize.Int64 == requirement.ObservedRawSize &&
		stateTail.Valid && validRawTailState(stateTail.String, stateTailStart, requirement.RequiredThroughOffset, requirement.ObservedRawSize) &&
		stateOwner.Valid == (requirement.ExpectedOwningThreadID != nil) &&
		(!stateOwner.Valid || stateOwner.String == *requirement.ExpectedOwningThreadID) &&
		stateRoot.Valid == (requirement.ExpectedRootSessionID != nil) &&
		(!stateRoot.Valid || stateRoot.String == *requirement.ExpectedRootSessionID)
	checkpointReady := checkpointFound && checkpointParser.Valid && checkpointParser.Int64 == state.ActiveParserVersion &&
		checkpointStatus.Valid && checkpointStatus.String == "ready" && checkpointOffset.Valid &&
		stateResolved.Valid && checkpointOffset.Int64 == stateResolved.Int64 &&
		validPhysicalGuard(checkpointOffset.Int64, checkpointGuard, row.path, requirement.ObservedRawSize, stateTail.String)
	if stateMatches {
		member.tailStatus = stateTail.String
		member.tailStart = stateTailStart
		if stateResolved.Valid {
			member.requiredOffset = requirement.RequiredThroughOffset
		}
	}
	if !stateMatches || !checkpointReady {
		return member, nil
	}
	member.activeOffset = checkpointOffset.Int64
	member.activeGuard = cloneBytes(checkpointGuard)
	proof, err := activeStateProof(tx, state.ActiveEpoch, requirement.SourceFileID)
	if err != nil {
		return manifestMember{}, err
	}
	if len(proof) == 0 {
		return member, nil
	}
	member.activeFingerprint = cloneBytes(proof)
	return member, nil
}

func validateCurrentRequirement(private storage.PrivateTx, requirement MemberRequirement, out *sourceRow) error {
	if err := validateMemberRequirement(private, requirement, out); err != nil {
		return err
	}
	if out.status != "present" {
		return fmt.Errorf("%w: present source requirement is not present", ErrInvalidRebuildInput)
	}
	return nil
}

func validateMemberRequirement(private storage.PrivateTx, requirement MemberRequirement, out *sourceRow) error {
	if err := readSourceRow(private, requirement.SourceFileID, out); err != nil {
		return err
	}
	if (out.status != "present" && out.status != "missing") || out.generation != requirement.Generation || out.deviceID != requirement.DeviceID ||
		out.inode != requirement.Inode || out.observed != requirement.ObservedRawSize ||
		!equalNullable(out.owner, requirement.ExpectedOwningThreadID) || !equalNullable(out.root, requirement.ExpectedRootSessionID) {
		return fmt.Errorf("%w: accepted source requirement changed", ErrInvalidRebuildInput)
	}
	return nil
}

func readSourceRow(private storage.PrivateTx, sourceFileID int64, out *sourceRow) error {
	err := private.QueryRow(`SELECT sf.file_generation,sf.device_id,sf.inode,sf.observed_size,sf.file_status,
		sf.thread_id,t.root_session_id,sf.current_path FROM codex_source_files sf
		LEFT JOIN threads t ON t.thread_id=sf.thread_id WHERE sf.source_file_id=?`, sourceFileID).
		Scan(&out.generation, &out.deviceID, &out.inode, &out.observed, &out.status, &out.owner, &out.root, &out.path)
	if errors.Is(err, sql.ErrNoRows) {
		return fmt.Errorf("%w: source file %d does not exist", ErrInvalidRebuildInput, sourceFileID)
	}
	return err
}

func scanOptionalRow(scan func() error) (bool, error) {
	err := scan()
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	return err == nil, err
}

func validPhysicalGuard(offset int64, guard []byte, path string, observedSize int64, tailStatus string) bool {
	if offset < 0 || observedSize < 0 || offset > observedSize {
		return false
	}
	if offset == 0 && guard == nil || offset > 0 && len(guard) > 0 {
		return true
	}
	return offset > 0 && guard == nil && tailStatus == "unverified" &&
		strings.HasSuffix(path, ".jsonl.zst") && offset == observedSize
}

func validRawTailState(status string, start sql.NullInt64, requiredOffset, observedSize int64) bool {
	switch status {
	case "none", "unverified":
		return !start.Valid && requiredOffset == observedSize
	case "half_line":
		return start.Valid && start.Int64 == requiredOffset && requiredOffset < observedSize
	default:
		return false
	}
}

func insertManifestMember(tx *source.WriteTx, buildEpoch, parserVersion int64, member manifestMember, committedAtMS int64) error {
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(`INSERT INTO codex_usage_build_sources(
			build_epoch,source_file_id,target_parser_version,expected_file_generation,expected_device_id,expected_inode,
			expected_owning_thread_id,expected_root_session_id,active_committed_offset,active_guard_hash,active_state_fingerprint,
			required_generation,required_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,membership_reason,
			completion_status,completion_error_code,completed_generation,completed_through_offset,carry_from_epoch,carry_phase,
			carry_after_start_offset,carry_after_turn_key,carry_after_anomaly_id,carry_after_fact_event_id,
			carry_after_marker_start_offset,carry_after_window_start_offset,created_at_ms,updated_at_ms
		) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
			buildEpoch, member.sourceFileID, parserVersion, member.expectedGeneration, member.deviceID, member.inode,
			nullableValue(member.owner), nullableValue(member.root), member.activeOffset, member.activeGuard, member.activeFingerprint,
			member.expectedGeneration, member.requiredOffset, member.observedRawSize, member.tailStatus, nullableIntValue(member.tailStart),
			member.membershipReason, member.completion, nullableValue(member.errorCode), nullableIntValue(member.completedGeneration),
			nullableIntValue(member.completedOffset), nullableIntValue(member.carryFrom), member.carryPhase,
			nullableIntValue(member.afterStartOffset), nullableValue(member.afterTurnKey), nullableValue(member.afterAnomalyID),
			nullableValue(member.afterFactEventID), nullableIntValue(member.afterMarkerOffset), nullableIntValue(member.afterWindowOffset),
			committedAtMS, committedAtMS)
		return err
	})
}

func resetUsageCheckpoint(tx *source.WriteTx, sourceFileID, parserVersion int64) error {
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(`INSERT INTO codex_source_checkpoints(
			source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,processing_status,
			last_successful_scan_at_ms,last_error_code
		) VALUES(?,'usage',?,0,NULL,'rebuild_required',NULL,NULL)
		ON CONFLICT(source_file_id,consumer_kind) DO UPDATE SET parser_version=excluded.parser_version,
			committed_offset=0,guard_hash=NULL,processing_status='rebuild_required',
			last_successful_scan_at_ms=NULL,last_error_code=NULL`, sourceFileID, parserVersion)
		return err
	})
}

func loadManifestMembers(tx *source.WriteTx, buildEpoch int64) ([]manifestMember, error) {
	var members []manifestMember
	err := tx.Private(func(private storage.PrivateTx) error {
		rows, err := private.Query(`SELECT source_file_id,target_parser_version,expected_file_generation,expected_device_id,expected_inode,
			expected_owning_thread_id,expected_root_session_id,active_committed_offset,active_guard_hash,active_state_fingerprint,
			required_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,membership_reason,
			completion_status,completion_error_code,completed_generation,completed_through_offset,carry_from_epoch,carry_phase,
			carry_after_start_offset,carry_after_turn_key,carry_after_anomaly_id,carry_after_fact_event_id,
			carry_after_marker_start_offset,carry_after_window_start_offset
			FROM codex_usage_build_sources WHERE build_epoch=? ORDER BY source_file_id`, buildEpoch)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var member manifestMember
			if err := rows.Scan(&member.sourceFileID, &member.parserVersion, &member.expectedGeneration, &member.deviceID, &member.inode,
				&member.owner, &member.root, &member.activeOffset, &member.activeGuard, &member.activeFingerprint,
				&member.requiredOffset, &member.observedRawSize, &member.tailStatus, &member.tailStart, &member.membershipReason,
				&member.completion, &member.errorCode, &member.completedGeneration, &member.completedOffset, &member.carryFrom,
				&member.carryPhase, &member.afterStartOffset, &member.afterTurnKey, &member.afterAnomalyID, &member.afterFactEventID,
				&member.afterMarkerOffset, &member.afterWindowOffset); err != nil {
				return err
			}
			members = append(members, member)
		}
		return rows.Err()
	})
	return members, err
}

func durableRequirementFromMember(tx *source.WriteTx, activeEpoch int64, member manifestMember) (MemberRequirement, error) {
	return durableRequirement(tx, activeEpoch, member.sourceFileID, &member)
}

func addNewPresentMembers(
	tx *source.WriteTx,
	buildEpoch int64,
	parserVersion int64,
	requirements []MemberRequirement,
	activeStateProof ActiveSourceStateProof,
	committedAtMS int64,
) error {
	state, err := tx.UsageEpochState()
	if err != nil {
		return err
	}
	members, err := loadManifestMembers(tx, buildEpoch)
	if err != nil {
		return err
	}
	existing := make(map[int64]manifestMember, len(members))
	for _, member := range members {
		existing[member.sourceFileID] = member
	}
	contributors, err := activeContributors(tx, state.ActiveEpoch)
	if err != nil {
		return err
	}
	for _, requirement := range requirements {
		old, ok := existing[requirement.SourceFileID]
		if !ok {
			reason := "discovered_during_build"
			member, err := freezeMember(tx, state, buildEpoch, parserVersion, requirement, reason, activeStateProof)
			if err != nil {
				return err
			}
			if err := insertManifestMember(tx, buildEpoch, parserVersion, member, committedAtMS); err != nil {
				return err
			}
			if err := resetUsageCheckpoint(tx, requirement.SourceFileID, parserVersion); err != nil {
				return err
			}
			continue
		}
		var row sourceRow
		if err := tx.Private(func(private storage.PrivateTx) error { return validateCurrentRequirement(private, requirement, &row) }); err != nil {
			return err
		}
		if old.expectedGeneration != requirement.Generation || old.deviceID != requirement.DeviceID || old.inode != requirement.Inode ||
			old.observedRawSize != requirement.ObservedRawSize || !equalNullable(old.owner, requirement.ExpectedOwningThreadID) ||
			!equalNullable(old.root, requirement.ExpectedRootSessionID) {
			return fmt.Errorf("%w: present member changed without reset", ErrInvalidRebuildInput)
		}
		if old.completion == completionBlocked && (old.errorCode.String == "PARSER_CHANGED_RAW_MISSING" || old.errorCode.String == "QUARANTINE_RETRY_RAW_MISSING") {
			if _, ordinary := contributors[requirement.SourceFileID]; ordinary {
				// Reappearance proves the raw member can be replanned; old terminal proof stays cleared.
			}
			if err := resetPresentBlockedMember(tx, buildEpoch, parserVersion, requirement, committedAtMS); err != nil {
				return err
			}
		}
	}
	return nil
}

func resetPresentBlockedMember(tx *source.WriteTx, buildEpoch, parserVersion int64, requirement MemberRequirement, committedAtMS int64) error {
	if err := resetUsageCheckpoint(tx, requirement.SourceFileID, parserVersion); err != nil {
		return err
	}
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(`UPDATE codex_usage_build_sources SET target_parser_version=?,expected_file_generation=?,
			expected_device_id=?,expected_inode=?,expected_owning_thread_id=?,expected_root_session_id=?,required_generation=?,
			required_through_offset=?,observed_raw_size=?,completion_status='pending',completion_error_code=NULL,
			completed_generation=NULL,completed_through_offset=NULL,carry_from_epoch=NULL,carry_phase='none',
			carry_after_start_offset=NULL,carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,carry_after_fact_event_id=NULL,
			carry_after_marker_start_offset=NULL,carry_after_window_start_offset=NULL,updated_at_ms=?
			WHERE build_epoch=? AND source_file_id=? AND completion_status='blocked'`, parserVersion, requirement.Generation,
			requirement.DeviceID, requirement.Inode, nullableStringValue(requirement.ExpectedOwningThreadID),
			nullableStringValue(requirement.ExpectedRootSessionID), requirement.Generation, requirement.RequiredThroughOffset,
			requirement.ObservedRawSize, committedAtMS, buildEpoch, requirement.SourceFileID)
		return err
	})
}

func readMember(tx *source.WriteTx, buildEpoch, sourceFileID int64) (manifestMember, error) {
	var result manifestMember
	err := tx.Private(func(private storage.PrivateTx) error {
		return scanMember(private.QueryRow(`SELECT source_file_id,target_parser_version,expected_file_generation,expected_device_id,expected_inode,
			expected_owning_thread_id,expected_root_session_id,active_committed_offset,active_guard_hash,active_state_fingerprint,
			required_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,membership_reason,
			completion_status,completion_error_code,completed_generation,completed_through_offset,carry_from_epoch,carry_phase,
			carry_after_start_offset,carry_after_turn_key,carry_after_anomaly_id,carry_after_fact_event_id,
			carry_after_marker_start_offset,carry_after_window_start_offset
			FROM codex_usage_build_sources WHERE build_epoch=? AND source_file_id=?`, buildEpoch, sourceFileID), &result)
	})
	return result, err
}

type rowScanner interface{ Scan(...any) error }

func scanMember(row rowScanner, member *manifestMember) error {
	return row.Scan(&member.sourceFileID, &member.parserVersion, &member.expectedGeneration, &member.deviceID, &member.inode,
		&member.owner, &member.root, &member.activeOffset, &member.activeGuard, &member.activeFingerprint,
		&member.requiredOffset, &member.observedRawSize, &member.tailStatus, &member.tailStart, &member.membershipReason,
		&member.completion, &member.errorCode, &member.completedGeneration, &member.completedOffset, &member.carryFrom,
		&member.carryPhase, &member.afterStartOffset, &member.afterTurnKey, &member.afterAnomalyID, &member.afterFactEventID,
		&member.afterMarkerOffset, &member.afterWindowOffset)
}

func stringPointer(value sql.NullString) *string {
	if !value.Valid {
		return nil
	}
	copy := value.String
	return &copy
}

func nullableString(value *string) sql.NullString {
	if value == nil {
		return sql.NullString{}
	}
	return sql.NullString{String: *value, Valid: true}
}

func nullableValue(value sql.NullString) any {
	if !value.Valid {
		return nil
	}
	return value.String
}

func nullableIntValue(value sql.NullInt64) any {
	if !value.Valid {
		return nil
	}
	return value.Int64
}

func equalNullableInt(left, right sql.NullInt64) bool {
	return left.Valid == right.Valid && (!left.Valid || left.Int64 == right.Int64)
}

func nullableStringValue(value *string) any {
	if value == nil {
		return nil
	}
	return *value
}

func equalNullable(left sql.NullString, right *string) bool {
	if left.Valid != (right != nil) {
		return false
	}
	return !left.Valid || left.String == *right
}

func cloneBytes(value []byte) []byte {
	if value == nil {
		return nil
	}
	return append([]byte(nil), value...)
}

func ReplaceBuildTarget(
	tx *source.WriteTx,
	buildEpoch int64,
	parserVersion int64,
	currentRequirements []MemberRequirement,
	activeStateProof ActiveSourceStateProof,
	stripWindows WindowReferenceStripper,
	committedAtMS int64,
) error {
	if tx == nil || tx.Source() != domain.SourceCodex || buildEpoch < 1 || parserVersion < 0 || committedAtMS < 0 || activeStateProof == nil || stripWindows == nil {
		return ErrInvalidRebuildInput
	}
	if err := validateRequirements(currentRequirements); err != nil {
		return err
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return err
	}
	if state.BuildEpoch == nil || *state.BuildEpoch != buildEpoch || state.BuildParserVersion == nil || *state.BuildParserVersion != parserVersion {
		return fmt.Errorf("%w: parser replacement does not match working epoch", ErrInvalidRebuildInput)
	}
	oldMembers, err := loadManifestMembers(tx, buildEpoch)
	if err != nil {
		return err
	}
	contributors, err := activeContributors(tx, state.ActiveEpoch)
	if err != nil {
		return err
	}
	byID := requirementMap(currentRequirements)
	ids := make([]int64, 0, len(oldMembers)+len(contributors)+len(byID))
	oldByID := make(map[int64]manifestMember, len(oldMembers))
	for _, member := range oldMembers {
		ids = append(ids, member.sourceFileID)
		oldByID[member.sourceFileID] = member
	}
	for id := range contributors {
		ids = append(ids, id)
	}
	for id := range byID {
		ids = append(ids, id)
	}
	ids, err = normalizeIDs(ids)
	if err != nil {
		return err
	}
	if _, err := CleanupBuildMembersSemantic(tx, buildEpoch, ids, stripWindows); err != nil {
		return err
	}
	if err := tx.Private(func(private storage.PrivateTx) error {
		if _, err := private.Exec("DELETE FROM codex_usage_session_quarantine WHERE ledger_epoch=?", buildEpoch); err != nil {
			return err
		}
		_, err := private.Exec("DELETE FROM codex_usage_build_sources WHERE build_epoch=?", buildEpoch)
		return err
	}); err != nil {
		return err
	}
	for _, id := range ids {
		requirement, current := byID[id]
		old, hadOld := oldByID[id]
		if !current {
			requirement, err = durableRequirement(tx, state.ActiveEpoch, id, memberPointer(old, hadOld))
			if err != nil {
				return err
			}
		}
		var catalog sourceRow
		if err := tx.Private(func(private storage.PrivateTx) error {
			if err := validateMemberRequirement(private, requirement, &catalog); err != nil {
				return err
			}
			if catalog.status == "missing" && current {
				if hadOld {
					return validateMissingResetRequirement(private, state.ActiveEpoch, old, requirement)
				}
				if _, contributed := contributors[id]; !contributed {
					return fmt.Errorf("%w: missing replacement member has no durable contributor proof", ErrInvalidRebuildInput)
				}
			}
			return nil
		}); err != nil {
			return err
		}
		if catalog.status == "missing" && !hadOld {
			if _, err := durableRequirement(tx, state.ActiveEpoch, id, nil); err != nil {
				return err
			}
		}
		present := catalog.status == "present"
		reason := "present_at_build_start"
		if hadOld {
			reason = old.membershipReason
		} else if _, ok := contributors[id]; ok {
			reason = "active_contributor"
		}
		if present {
			reason = "both"
			if _, ok := contributors[id]; !ok {
				reason = "present_at_build_start"
			}
		}
		member, err := freezeMember(tx, state, buildEpoch, parserVersion, requirement, reason, activeStateProof)
		if err != nil {
			return err
		}
		if hadOld && old.expectedGeneration == requirement.Generation && old.deviceID == requirement.DeviceID &&
			old.inode == requirement.Inode && old.observedRawSize == requirement.ObservedRawSize {
			activeOffset, activeGuard, fingerprint, valid, err := loadFrozenActiveProof(tx, state.ActiveEpoch, old, requirement, activeStateProof)
			if err != nil {
				return err
			}
			if valid {
				member.activeOffset = activeOffset
				member.activeGuard = activeGuard
				member.activeFingerprint = fingerprint
			}
			member.tailStatus = old.tailStatus
			member.tailStart = old.tailStart
		}
		if !present {
			member.completion = completionBlocked
			member.errorCode = sql.NullString{String: "PARSER_CHANGED_RAW_MISSING", Valid: true}
		}
		if err := insertManifestMember(tx, buildEpoch, parserVersion, member, committedAtMS); err != nil {
			return err
		}
		if err := resetUsageCheckpoint(tx, id, parserVersion); err != nil {
			return err
		}
	}
	return nil
}

func memberPointer(member manifestMember, ok bool) *manifestMember {
	if !ok {
		return nil
	}
	return &member
}

func ResetBuildMembersTx(
	tx *source.WriteTx,
	buildEpoch int64,
	trigger Trigger,
	affectedSourceFileIDs []int64,
	affectedRootIDs []string,
	currentRequirements []MemberRequirement,
	activeStateProof ActiveSourceStateProof,
	stripWindows WindowReferenceStripper,
	committedAtMS int64,
) error {
	if tx == nil || tx.Source() != domain.SourceCodex || buildEpoch < 1 || committedAtMS < 0 || activeStateProof == nil || stripWindows == nil {
		return ErrInvalidRebuildInput
	}
	if err := validateTrigger(trigger); err != nil {
		return err
	}
	if trigger == Bootstrap || trigger == ParserChanged {
		return fmt.Errorf("%w: trigger does not reset an existing build", ErrInvalidRebuildInput)
	}
	if err := validateRequirements(currentRequirements); err != nil {
		return err
	}
	ids, err := normalizeIDs(affectedSourceFileIDs)
	if err != nil {
		return err
	}
	roots, err := normalizeStrings(affectedRootIDs)
	if err != nil {
		return err
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return err
	}
	if state.BuildEpoch == nil || *state.BuildEpoch != buildEpoch || state.BuildParserVersion == nil {
		return fmt.Errorf("%w: no matching build to reset", ErrInvalidRebuildInput)
	}
	members, err := loadManifestMembers(tx, buildEpoch)
	if err != nil {
		return err
	}
	byID := make(map[int64]manifestMember, len(members))
	for _, member := range members {
		byID[member.sourceFileID] = member
	}
	rootSet := make(map[string]struct{}, len(roots))
	for _, root := range roots {
		rootSet[root] = struct{}{}
	}
	selected := make(map[int64]struct{}, len(ids))
	for _, id := range ids {
		selected[id] = struct{}{}
	}
	for _, id := range ids {
		if member, ok := byID[id]; ok && member.root.Valid {
			if trigger == FatalIsolation || trigger == QuarantineRetry {
				rootSet[member.root.String] = struct{}{}
			}
		}
	}
	if trigger == QuarantineRetry {
		activeRoots, err := activeQuarantineRootsForSources(tx, state.ActiveEpoch, ids)
		if err != nil {
			return err
		}
		for _, root := range activeRoots {
			rootSet[root] = struct{}{}
		}
	}
	for _, member := range members {
		if !member.root.Valid {
			continue
		}
		if _, ok := rootSet[member.root.String]; ok {
			selected[member.sourceFileID] = struct{}{}
			continue
		}
		if _, ok := selected[member.sourceFileID]; ok {
			quarantined, err := buildRootQuarantined(tx, buildEpoch, member.root.String)
			if err != nil {
				return err
			}
			if quarantined {
				rootSet[member.root.String] = struct{}{}
				for _, sibling := range members {
					if sibling.root.Valid && sibling.root.String == member.root.String {
						selected[sibling.sourceFileID] = struct{}{}
					}
				}
			}
		}
	}
	ids = ids[:0]
	for id := range selected {
		ids = append(ids, id)
	}
	ids, err = normalizeIDs(ids)
	if err != nil {
		return err
	}
	if len(ids) == 0 {
		return nil
	}
	if _, err := CleanupBuildMembersSemantic(tx, buildEpoch, ids, stripWindows); err != nil {
		return err
	}
	current := requirementMap(currentRequirements)
	for _, id := range ids {
		old, ok := byID[id]
		if !ok {
			return fmt.Errorf("%w: reset source is absent from build manifest", ErrInvalidRebuildInput)
		}
		requirement, hasCurrent := current[id]
		var row sourceRow
		if hasCurrent {
			if err := tx.Private(func(private storage.PrivateTx) error {
				if err := validateMemberRequirement(private, requirement, &row); err != nil {
					return err
				}
				if row.status == "missing" {
					return validateMissingResetRequirement(private, state.ActiveEpoch, old, requirement)
				}
				if row.status != "present" {
					return fmt.Errorf("%w: reset requirement has invalid catalog status", ErrInvalidRebuildInput)
				}
				return nil
			}); err != nil {
				return err
			}
		} else {
			requirement, err = durableRequirementFromMember(tx, state.ActiveEpoch, old)
			if err != nil {
				return err
			}
			if err := tx.Private(func(private storage.PrivateTx) error { return readSourceRow(private, id, &row) }); err != nil {
				return err
			}
		}
		samePhysical := old.expectedGeneration == requirement.Generation && old.deviceID == requirement.DeviceID &&
			old.inode == requirement.Inode && old.observedRawSize == requirement.ObservedRawSize
		activeOffset, activeGuard, activeFingerprint := int64(0), []byte(nil), []byte(nil)
		if samePhysical {
			var valid bool
			activeOffset, activeGuard, activeFingerprint, valid, err = loadFrozenActiveProof(tx, state.ActiveEpoch, old, requirement, activeStateProof)
			if err != nil {
				return err
			}
			if !valid {
				activeOffset, activeGuard, activeFingerprint = 0, nil, nil
			}
		}
		status, errorCode := completionPending, sql.NullString{}
		if trigger == QuarantineRetry && row.status == "missing" {
			ordinary, err := hasOrdinaryActiveContributor(tx, state.ActiveEpoch, id)
			if err != nil {
				return err
			}
			if !ordinary {
				status = completionBlocked
				errorCode = sql.NullString{String: "QUARANTINE_RETRY_RAW_MISSING", Valid: true}
			}
		}
		tailStatus, tailStart, err := currentTailProof(tx, state.ActiveEpoch, requirement)
		if err != nil {
			return err
		}
		if err := updateResetMember(tx, buildEpoch, *state.BuildParserVersion, old, requirement, activeOffset, activeGuard,
			activeFingerprint, tailStatus, tailStart, status, errorCode, committedAtMS); err != nil {
			return err
		}
		if err := resetUsageCheckpoint(tx, id, *state.BuildParserVersion); err != nil {
			return err
		}
	}
	for root := range rootSet {
		if err := deleteBuildQuarantine(tx, buildEpoch, root); err != nil {
			return err
		}
	}
	return nil
}

func CleanupBuildMembersSemantic(
	tx *source.WriteTx,
	buildEpoch int64,
	memberIDs []int64,
	stripWindows WindowReferenceStripper,
) ([]string, error) {
	if tx == nil || tx.Source() != domain.SourceCodex || buildEpoch < 1 || stripWindows == nil {
		return nil, ErrInvalidRebuildInput
	}
	ids, err := normalizeIDs(memberIDs)
	if err != nil || len(ids) == 0 {
		return nil, err
	}
	pairs, err := manifestGenerations(tx, buildEpoch, ids)
	if err != nil || len(pairs) == 0 {
		return nil, err
	}
	candidates, err := candidateBuildEventIDs(tx, buildEpoch, pairs)
	if err != nil {
		return nil, err
	}
	if err := deleteBuildSourceRows(tx, buildEpoch, pairs); err != nil {
		return nil, err
	}
	orphans, err := orphanBuildEventIDs(tx, buildEpoch, candidates)
	if err != nil {
		return nil, err
	}
	if len(orphans) > 0 {
		if err := stripWindows(tx, source.UsageTargetBuild, append([]string(nil), orphans...)); err != nil {
			return nil, err
		}
		if err := deleteBuildFacts(tx, buildEpoch, orphans); err != nil {
			return nil, err
		}
		if _, err := tx.DeleteUsageNoRevision(source.UsageTargetBuild, orphans); err != nil {
			return nil, err
		}
	}
	return candidates, nil
}

func manifestGenerations(tx *source.WriteTx, buildEpoch int64, ids []int64) ([]memberGeneration, error) {
	var result []memberGeneration
	err := tx.Private(func(private storage.PrivateTx) error {
		query, args := inQuery(`SELECT source_file_id,expected_file_generation FROM codex_usage_build_sources
			WHERE build_epoch=? AND source_file_id IN (%s) ORDER BY source_file_id`, buildEpoch, ids)
		rows, err := private.Query(query, args...)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var pair memberGeneration
			if err := rows.Scan(&pair.sourceFileID, &pair.generation); err != nil {
				return err
			}
			result = append(result, pair)
		}
		return rows.Err()
	})
	return result, err
}

type memberGeneration struct{ sourceFileID, generation int64 }

func candidateBuildEventIDs(tx *source.WriteTx, buildEpoch int64, pairs []memberGeneration) ([]string, error) {
	var result []string
	err := tx.Private(func(private storage.PrivateTx) error {
		for _, pair := range pairs {
			rows, err := private.Query(`SELECT event_id FROM codex_usage_event_occurrences
				WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?
				UNION SELECT resolved_event_id FROM codex_compaction_markers
				WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=? AND resolved_event_id IS NOT NULL
				UNION SELECT event_id FROM codex_usage_event_holds
				WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=? ORDER BY 1`,
				buildEpoch, pair.sourceFileID, pair.generation, buildEpoch, pair.sourceFileID, pair.generation,
				buildEpoch, pair.sourceFileID, pair.generation)
			if err != nil {
				return err
			}
			for rows.Next() {
				var eventID string
				if err := rows.Scan(&eventID); err != nil {
					rows.Close()
					return err
				}
				result = append(result, eventID)
			}
			if err := rows.Err(); err != nil {
				rows.Close()
				return err
			}
			if err := rows.Close(); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Strings(result)
	return uniqueStrings(result), nil
}

func deleteBuildSourceRows(tx *source.WriteTx, buildEpoch int64, pairs []memberGeneration) error {
	return tx.Private(func(private storage.PrivateTx) error {
		for _, pair := range pairs {
			if _, err := private.Exec(`DELETE FROM codex_compaction_markers
				WHERE ledger_epoch=? AND source_file_id=? AND file_generation=?`, buildEpoch, pair.sourceFileID, pair.generation); err != nil {
				return err
			}
		}
		for _, pair := range pairs {
			args := []any{buildEpoch, pair.sourceFileID, pair.generation}
			for _, table := range []string{"codex_usage_event_occurrences", "codex_usage_reconciliation_windows", "codex_usage_event_holds", "codex_turns", "codex_usage_source_states", "codex_skill_usage_events"} {
				query := fmt.Sprintf("DELETE FROM %s WHERE ledger_epoch=? AND source_file_id=? AND file_generation=?", table)
				if _, err := private.Exec(query, args...); err != nil {
					return err
				}
			}
		}
		return nil
	})
}

func orphanBuildEventIDs(tx *source.WriteTx, buildEpoch int64, candidates []string) ([]string, error) {
	if len(candidates) == 0 {
		return nil, nil
	}
	var result []string
	err := tx.Private(func(private storage.PrivateTx) error {
		for _, candidate := range candidates {
			var refs int64
			err := private.QueryRow(`SELECT
				(SELECT count(*) FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND event_id=?) +
				(SELECT count(*) FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=? AND resolved_event_id=?) +
				(SELECT count(*) FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND event_id=?)`,
				buildEpoch, candidate, buildEpoch, candidate, buildEpoch, candidate).Scan(&refs)
			if err != nil {
				return err
			}
			if refs == 0 {
				result = append(result, candidate)
			}
		}
		return nil
	})
	return result, err
}

func deleteBuildFacts(tx *source.WriteTx, buildEpoch int64, eventIDs []string) error {
	return tx.Private(func(private storage.PrivateTx) error {
		query, args := inQuery(`DELETE FROM codex_usage_event_facts WHERE source='codex' AND ledger_epoch=? AND event_id IN (%s)`, buildEpoch, eventIDs)
		_, err := private.Exec(query, args...)
		return err
	})
}

func inQuery(format string, prefix any, values any) (string, []any) {
	args := []any{prefix}
	var length int
	switch typed := values.(type) {
	case []int64:
		length = len(typed)
		for _, value := range typed {
			args = append(args, value)
		}
	case []string:
		length = len(typed)
		for _, value := range typed {
			args = append(args, value)
		}
	default:
		panic("unsupported SQL IN values")
	}
	placeholders := make([]string, length)
	for i := range placeholders {
		placeholders[i] = "?"
	}
	return fmt.Sprintf(format, strings.Join(placeholders, ",")), args
}

func normalizeStrings(values []string) ([]string, error) {
	result := append([]string(nil), values...)
	sort.Strings(result)
	unique := result[:0]
	for _, value := range result {
		if value == "" || strings.TrimSpace(value) != value {
			return nil, fmt.Errorf("%w: invalid root ID", ErrInvalidRebuildInput)
		}
		if len(unique) == 0 || unique[len(unique)-1] != value {
			unique = append(unique, value)
		}
	}
	return unique, nil
}

func uniqueStrings(values []string) []string {
	if len(values) == 0 {
		return nil
	}
	result := values[:1]
	for _, value := range values[1:] {
		if result[len(result)-1] != value {
			result = append(result, value)
		}
	}
	return result
}

func containsID(ids []int64, needle int64) (int, bool) {
	index := sort.Search(len(ids), func(i int) bool { return ids[i] >= needle })
	return index, index < len(ids) && ids[index] == needle
}

func validateCurrentRequirements(tx *source.WriteTx, requirements []MemberRequirement) error {
	return tx.Private(func(private storage.PrivateTx) error {
		for _, requirement := range requirements {
			var row sourceRow
			if err := validateCurrentRequirement(private, requirement, &row); err != nil {
				return err
			}
		}
		return nil
	})
}

func affectedRoots(tx *source.WriteTx, buildEpoch, activeEpoch int64, trigger Trigger, sourceIDs []int64) ([]string, error) {
	roots := make(map[string]struct{})
	if trigger == QuarantineRetry {
		active, err := activeQuarantineRootsForSources(tx, activeEpoch, sourceIDs)
		if err != nil {
			return nil, err
		}
		for _, root := range active {
			roots[root] = struct{}{}
		}
	}
	if trigger == FatalIsolation {
		for _, sourceID := range sourceIDs {
			var root sql.NullString
			err := tx.Private(func(private storage.PrivateTx) error {
				return private.QueryRow(`SELECT t.root_session_id FROM codex_source_files sf
					LEFT JOIN threads t ON t.thread_id=sf.thread_id WHERE sf.source_file_id=?`, sourceID).Scan(&root)
			})
			if err != nil && !errors.Is(err, sql.ErrNoRows) {
				return nil, err
			}
			if root.Valid {
				roots[root.String] = struct{}{}
			}
		}
	}
	if buildEpoch > 0 && (trigger == FatalIsolation || trigger == QuarantineRetry) {
		members, err := loadManifestMembers(tx, buildEpoch)
		if err != nil {
			return nil, err
		}
		for _, member := range members {
			if member.root.Valid {
				if _, ok := containsID(sourceIDs, member.sourceFileID); ok && trigger == FatalIsolation {
					roots[member.root.String] = struct{}{}
				}
			}
		}
	}
	result := make([]string, 0, len(roots))
	for root := range roots {
		result = append(result, root)
	}
	return normalizeStrings(result)
}

func activeQuarantineRootsForSources(tx *source.WriteTx, activeEpoch int64, sourceIDs []int64) ([]string, error) {
	if activeEpoch <= 0 || len(sourceIDs) == 0 {
		return nil, nil
	}
	var roots []string
	err := tx.Private(func(private storage.PrivateTx) error {
		query, args := inQuery(`SELECT DISTINCT root_session_id FROM codex_usage_session_quarantine_sources
			WHERE ledger_epoch=? AND source_file_id IN (%s) ORDER BY root_session_id`, activeEpoch, sourceIDs)
		rows, err := private.Query(query, args...)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var root string
			if err := rows.Scan(&root); err != nil {
				return err
			}
			roots = append(roots, root)
		}
		return rows.Err()
	})
	return roots, err
}

func buildRootQuarantined(tx *source.WriteTx, buildEpoch int64, root string) (bool, error) {
	var count int64
	err := tx.Private(func(private storage.PrivateTx) error {
		return private.QueryRow(`SELECT
			(SELECT count(*) FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?) +
			(SELECT count(*) FROM codex_usage_session_quarantine_sources WHERE ledger_epoch=? AND root_session_id=?) +
			(SELECT count(*) FROM codex_usage_build_sources WHERE build_epoch=? AND expected_root_session_id=? AND completion_status='quarantined')`,
			buildEpoch, root, buildEpoch, root, buildEpoch, root).Scan(&count)
	})
	return count != 0, err
}

func hasOrdinaryActiveContributor(tx *source.WriteTx, activeEpoch, sourceFileID int64) (bool, error) {
	if activeEpoch <= 0 {
		return false, nil
	}
	var count int64
	err := tx.Private(func(private storage.PrivateTx) error {
		return private.QueryRow(`SELECT
			(SELECT count(*) FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND source_file_id=?) +
			(SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=?) +
			(SELECT count(*) FROM codex_skill_usage_events WHERE ledger_epoch=? AND source_file_id=?)`,
			activeEpoch, sourceFileID, activeEpoch, sourceFileID, activeEpoch, sourceFileID).Scan(&count)
	})
	return count > 0, err
}

func validateMissingResetRequirement(private storage.PrivateTx, activeEpoch int64, old manifestMember, requirement MemberRequirement) error {
	if old.expectedGeneration != requirement.Generation || old.deviceID != requirement.DeviceID || old.inode != requirement.Inode || old.observedRawSize != requirement.ObservedRawSize {
		return fmt.Errorf("%w: missing reset source lacks matching manifest identity proof", ErrInvalidRebuildInput)
	}
	if activeEpoch <= 0 {
		return nil
	}
	var generation, device, inode, size sql.NullInt64
	found, err := scanOptionalRow(func() error {
		return private.QueryRow(`SELECT file_generation,device_id,inode,observed_raw_size FROM codex_usage_source_states
			WHERE ledger_epoch=? AND source_file_id=?`, activeEpoch, requirement.SourceFileID).
			Scan(&generation, &device, &inode, &size)
	})
	if err != nil {
		return err
	}
	if found && (!generation.Valid || generation.Int64 != requirement.Generation || !device.Valid || device.Int64 != requirement.DeviceID ||
		!inode.Valid || inode.Int64 != requirement.Inode || !size.Valid || size.Int64 != requirement.ObservedRawSize) {
		return fmt.Errorf("%w: missing reset source changed from durable active identity", ErrInvalidRebuildInput)
	}
	return nil
}

func loadFrozenActiveProof(
	tx *source.WriteTx,
	activeEpoch int64,
	old manifestMember,
	requirement MemberRequirement,
	activeStateProof ActiveSourceStateProof,
) (int64, []byte, []byte, bool, error) {
	var row sourceRow
	if err := tx.Private(func(private storage.PrivateTx) error { return readSourceRow(private, requirement.SourceFileID, &row) }); err != nil {
		return 0, nil, nil, false, err
	}
	if old.expectedGeneration != requirement.Generation || old.deviceID != requirement.DeviceID || old.inode != requirement.Inode ||
		old.observedRawSize != requirement.ObservedRawSize || len(old.activeFingerprint) == 0 ||
		!validPhysicalGuard(old.activeOffset, old.activeGuard, row.path, old.observedRawSize, old.tailStatus) {
		return 0, nil, nil, false, nil
	}
	if equalNullable(old.owner, requirement.ExpectedOwningThreadID) && equalNullable(old.root, requirement.ExpectedRootSessionID) {
		return old.activeOffset, cloneBytes(old.activeGuard), cloneBytes(old.activeFingerprint), true, nil
	}
	fingerprint, err := activeStateProof(tx, activeEpoch, requirement.SourceFileID)
	if err != nil {
		return 0, nil, nil, false, err
	}
	if len(fingerprint) == 0 {
		return 0, nil, nil, false, nil
	}
	return old.activeOffset, cloneBytes(old.activeGuard), cloneBytes(fingerprint), true, nil
}

func currentTailProof(tx *source.WriteTx, activeEpoch int64, requirement MemberRequirement) (string, sql.NullInt64, error) {
	if activeEpoch <= 0 {
		return "unverified", sql.NullInt64{}, nil
	}
	var generation, device, inode, size, parser, resolved sql.NullInt64
	var status sql.NullString
	var start sql.NullInt64
	var owner, root sql.NullString
	found := false
	err := tx.Private(func(private storage.PrivateTx) error {
		var err error
		found, err = scanOptionalRow(func() error {
			return private.QueryRow(`SELECT file_generation,device_id,inode,observed_raw_size,usage_parser_version,
				resolved_through_offset,raw_tail_status,raw_tail_start_offset,owning_thread_id,root_session_id
				FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=?`, activeEpoch, requirement.SourceFileID).
				Scan(&generation, &device, &inode, &size, &parser, &resolved, &status, &start, &owner, &root)
		})
		return err
	})
	if err != nil {
		return "", sql.NullInt64{}, err
	}
	if !found || !generation.Valid || generation.Int64 != requirement.Generation || !device.Valid || device.Int64 != requirement.DeviceID ||
		!inode.Valid || inode.Int64 != requirement.Inode || !size.Valid || size.Int64 != requirement.ObservedRawSize {
		return "unverified", sql.NullInt64{}, nil
	}
	return status.String, start, nil
}

func updateResetMember(
	tx *source.WriteTx,
	buildEpoch, parserVersion int64,
	old manifestMember,
	requirement MemberRequirement,
	activeOffset int64,
	activeGuard, activeFingerprint []byte,
	tailStatus string,
	tailStart sql.NullInt64,
	completion string,
	errorCode sql.NullString,
	committedAtMS int64,
) error {
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(`UPDATE codex_usage_build_sources SET target_parser_version=?,
			expected_file_generation=?,expected_device_id=?,expected_inode=?,expected_owning_thread_id=?,expected_root_session_id=?,
			active_committed_offset=?,active_guard_hash=?,active_state_fingerprint=?,required_generation=?,required_through_offset=?,
			observed_raw_size=?,raw_tail_status=?,raw_tail_start_offset=?,completion_status=?,completion_error_code=?,
			completed_generation=NULL,completed_through_offset=NULL,carry_from_epoch=NULL,carry_phase='none',
			carry_after_start_offset=NULL,carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,carry_after_fact_event_id=NULL,
			carry_after_marker_start_offset=NULL,carry_after_window_start_offset=NULL,updated_at_ms=?
			WHERE build_epoch=? AND source_file_id=?`, parserVersion, requirement.Generation, requirement.DeviceID, requirement.Inode,
			nullableStringValue(requirement.ExpectedOwningThreadID), nullableStringValue(requirement.ExpectedRootSessionID),
			activeOffset, activeGuard, activeFingerprint, requirement.Generation, requirement.RequiredThroughOffset,
			requirement.ObservedRawSize, tailStatus, nullableIntValue(tailStart), completion, nullableValue(errorCode),
			committedAtMS, buildEpoch, old.sourceFileID)
		return err
	})
}

func deleteBuildQuarantine(tx *source.WriteTx, buildEpoch int64, root string) error {
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(`DELETE FROM codex_usage_session_quarantine WHERE ledger_epoch=? AND root_session_id=?`, buildEpoch, root)
		return err
	})
}
