package codex

import (
	"bytes"
	"database/sql"
	"encoding/binary"
	"errors"
	"fmt"
	"math"
	"reflect"
	"sort"

	"github.com/Hogeexxl/Usagi/internal/codex/usage"
	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	"github.com/zeebo/blake3"
)

const skillVisibilityReadyParserVersion int64 = 11

var errInvalidActiveSourceStateProof = errors.New("invalid active source state proof")

type compactionQuery interface {
	Query(query string, args ...any) (*sql.Rows, error)
	QueryRow(query string, args ...any) *sql.Row
}

type privateQuery interface {
	Query(query string, args ...any) (*sql.Rows, error)
	QueryRow(query string, args ...any) *sql.Row
}

func MetadataCompactionVisibilityProjection(
	tx *source.WriteTx,
	target source.UsageWriteTarget,
	ownerThreadID string,
) (usage.CompactionVisibilityProjection, error) {
	state, err := tx.UsageEpochState()
	if err != nil {
		return usage.CompactionVisibilityProjection{}, err
	}
	epoch, parserVersion, err := usageTargetEpochParser(state, target)
	if err != nil {
		return usage.CompactionVisibilityProjection{}, err
	}
	return compactionVisibilityProjectionForOwner(tx, target, epoch, parserVersion, &ownerThreadID)
}

func ActiveCompactionVisibilityProjection(tx *source.WriteTx) (usage.CompactionVisibilityProjection, error) {
	state, err := tx.UsageEpochState()
	if err != nil {
		return usage.CompactionVisibilityProjection{}, err
	}
	return compactionVisibilityProjectionForOwner(tx, source.UsageTargetActive, state.ActiveEpoch, state.ActiveParserVersion, nil)
}

func VisiblePrivateEqual(
	tx storage.PrivateTx,
	_ domain.SourceID,
	activeEpoch int64,
	activeParserVersion int64,
	buildEpoch int64,
	buildParserVersion int64,
) (bool, error) {
	activeCompaction, err := compactionVisibilityProjection(tx, source.UsageTargetActive, activeEpoch, activeParserVersion, nil)
	if err != nil {
		return false, err
	}
	buildCompaction, err := compactionVisibilityProjection(tx, source.UsageTargetBuild, buildEpoch, buildParserVersion, nil)
	if err != nil {
		return false, err
	}
	activeSkills, err := skillVisibilityProjection(tx, activeEpoch, activeParserVersion)
	if err != nil {
		return false, err
	}
	buildSkills, err := skillVisibilityProjection(tx, buildEpoch, buildParserVersion)
	if err != nil {
		return false, err
	}
	activeQuarantine, err := quarantineVisibilityProjection(tx, activeEpoch)
	if err != nil {
		return false, err
	}
	buildQuarantine, err := quarantineVisibilityProjection(tx, buildEpoch)
	if err != nil {
		return false, err
	}
	return reflect.DeepEqual(activeCompaction, buildCompaction) &&
		reflect.DeepEqual(activeSkills, buildSkills) &&
		reflect.DeepEqual(activeQuarantine, buildQuarantine), nil
}

func compactionVisibilityProjectionForOwner(
	tx *source.WriteTx,
	target source.UsageWriteTarget,
	epoch int64,
	parserVersion int64,
	ownerThreadID *string,
) (usage.CompactionVisibilityProjection, error) {
	var projection usage.CompactionVisibilityProjection
	err := tx.Private(func(private storage.PrivateTx) error {
		value, err := compactionVisibilityProjection(private, target, epoch, parserVersion, ownerThreadID)
		projection = value
		return err
	})
	return projection, err
}

func usageTargetEpochParser(state domain.SourceUsageEpochState, target source.UsageWriteTarget) (int64, int64, error) {
	switch target {
	case source.UsageTargetActive:
		return state.ActiveEpoch, state.ActiveParserVersion, nil
	case source.UsageTargetBuild:
		if state.BuildEpoch == nil || state.BuildParserVersion == nil {
			return 0, 0, fmt.Errorf("usage build epoch is not active")
		}
		return *state.BuildEpoch, *state.BuildParserVersion, nil
	default:
		return 0, 0, fmt.Errorf("invalid usage write target")
	}
}

func compactionVisibilityProjection(
	q compactionQuery,
	target source.UsageWriteTarget,
	epoch int64,
	parserVersion int64,
	ownerThreadID *string,
) (usage.CompactionVisibilityProjection, error) {
	projection := usage.CompactionVisibilityProjection{
		Events:        make([]usage.CompactionVisibleEvent, 0),
		UnknownScopes: make([]usage.CompactionUnknownScope, 0),
	}
	projection.Ready = epoch > 0 && parserVersion >= usage.CompactionVisibilityReadyParserVersion
	if !projection.Ready {
		return projection, nil
	}
	if target != source.UsageTargetActive && target != source.UsageTargetBuild {
		return projection, fmt.Errorf("invalid usage write target")
	}
	candidates, err := loadCompactionCandidates(q, epoch, ownerThreadID)
	if err != nil {
		return usage.CompactionVisibilityProjection{}, err
	}
	projection.Events, err = loadResolvedCompactionEvents(q, epoch, ownerThreadID)
	if err != nil {
		return usage.CompactionVisibilityProjection{}, err
	}
	unknown, err := loadUnresolvedCompactionScopes(q, epoch, ownerThreadID, candidates)
	if err != nil {
		return usage.CompactionVisibilityProjection{}, err
	}
	incomplete, err := loadIncompleteSourceScopes(q, target, epoch, parserVersion, ownerThreadID, candidates)
	if err != nil {
		return usage.CompactionVisibilityProjection{}, err
	}
	unknown = append(unknown, incomplete...)
	projection.UnknownScopes = mergeCompactionUnknownScopes(unknown)
	return projection, nil
}

type compactionCandidate struct {
	threadID      string
	rootSessionID string
	model         string
	reasoning     *string
}

func loadCompactionCandidates(q compactionQuery, epoch int64, ownerThreadID *string) ([]compactionCandidate, error) {
	rows, err := q.Query(`SELECT DISTINCT thread_id,root_session_id,model,reasoning_effort
		FROM usage_events WHERE source='codex' AND source_epoch=? AND model IS NOT NULL
		AND (? IS NULL OR thread_id=?)
		ORDER BY thread_id,root_session_id,model,reasoning_effort`, epoch, ownerThreadID, ownerThreadID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	candidates := make([]compactionCandidate, 0)
	for rows.Next() {
		var candidate compactionCandidate
		var effort sql.NullString
		if err := rows.Scan(&candidate.threadID, &candidate.rootSessionID, &candidate.model, &effort); err != nil {
			return nil, err
		}
		candidate.reasoning = privateNullableString(effort)
		candidates = append(candidates, candidate)
	}
	return candidates, rows.Err()
}

func loadResolvedCompactionEvents(q compactionQuery, epoch int64, ownerThreadID *string) ([]usage.CompactionVisibleEvent, error) {
	rows, err := q.Query(`SELECT f.event_id,e.thread_id,e.root_session_id,e.model,e.reasoning_effort,e.occurred_at_ms,e.total_tokens
		FROM codex_usage_event_facts f JOIN usage_events e
		ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
		WHERE f.source='codex' AND f.ledger_epoch=? AND f.operation='compaction'
		AND (? IS NULL OR e.thread_id=?) ORDER BY f.event_id`, epoch, ownerThreadID, ownerThreadID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	events := make([]usage.CompactionVisibleEvent, 0)
	for rows.Next() {
		var event usage.CompactionVisibleEvent
		var effort sql.NullString
		if err := rows.Scan(&event.EventID, &event.ThreadID, &event.RootSessionID, &event.Model, &effort, &event.OccurredAtMS, &event.TotalTokens); err != nil {
			return nil, err
		}
		event.ReasoningEffort = privateNullableString(effort)
		events = append(events, event)
	}
	return events, rows.Err()
}

func loadUnresolvedCompactionScopes(
	q compactionQuery,
	epoch int64,
	ownerThreadID *string,
	candidates []compactionCandidate,
) ([]usage.CompactionUnknownScope, error) {
	rows, err := q.Query(`SELECT m.source_file_id,m.file_generation,m.source_start_offset,m.owning_thread_id,
		m.root_session_id,m.occurred_at_ms,m.model,m.reasoning_effort,t.started_at_ms,t.ended_at_ms
		FROM codex_compaction_markers m LEFT JOIN codex_turns t
		ON t.ledger_epoch=m.ledger_epoch AND t.source_file_id=m.source_file_id AND t.file_generation=m.file_generation
		AND t.thread_id=m.owning_thread_id AND t.start_offset<=m.source_start_offset
		AND (t.end_offset IS NULL OR m.source_start_offset<t.end_offset)
		WHERE m.source='codex' AND m.ledger_epoch=? AND m.resolved_event_id IS NULL
		AND (? IS NULL OR m.owning_thread_id=?)
		ORDER BY m.source_file_id,m.file_generation,m.source_start_offset,t.start_offset DESC`, epoch, ownerThreadID, ownerThreadID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	unknown := make([]usage.CompactionUnknownScope, 0)
	seen := make(map[[3]int64]struct{})
	for rows.Next() {
		var sourceFileID, generation, offset int64
		var threadID, rootID string
		var occurred, turnStart, turnEnd sql.NullInt64
		var model, effort sql.NullString
		if err := rows.Scan(&sourceFileID, &generation, &offset, &threadID, &rootID, &occurred, &model, &effort, &turnStart, &turnEnd); err != nil {
			return nil, err
		}
		key := [3]int64{sourceFileID, generation, offset}
		if _, exists := seen[key]; exists {
			continue
		}
		seen[key] = struct{}{}
		var startMS, endMS *int64
		if occurred.Valid {
			start := occurred.Int64
			startMS = &start
			if start != math.MaxInt64 {
				end := start + 1
				endMS = &end
			}
		} else {
			if turnStart.Valid {
				start := turnStart.Int64
				startMS = &start
			}
			if turnEnd.Valid && turnEnd.Int64 != math.MaxInt64 {
				end := turnEnd.Int64 + 1
				endMS = &end
			}
		}
		unknown = addUnknownCandidates(unknown, candidates, threadID, rootID, privateNullableString(model), privateNullableString(effort), startMS, endMS)
	}
	return unknown, rows.Err()
}

func loadIncompleteSourceScopes(
	q compactionQuery,
	target source.UsageWriteTarget,
	epoch int64,
	parserVersion int64,
	ownerThreadID *string,
	candidates []compactionCandidate,
) ([]usage.CompactionUnknownScope, error) {
	var buildEpoch sql.NullInt64
	if target == source.UsageTargetActive {
		if err := q.QueryRow(`SELECT build_epoch FROM source_usage_epochs WHERE source='codex'`).Scan(&buildEpoch); err != nil {
			return nil, err
		}
	}
	activeWithBuild := target == source.UsageTargetActive && buildEpoch.Valid
	checkpointColumns := `cp.parser_version,cp.committed_offset,cp.processing_status`
	checkpointJoin := `LEFT JOIN codex_source_checkpoints cp ON cp.source_file_id=sf.source_file_id AND cp.consumer_kind='usage'`
	if activeWithBuild {
		checkpointColumns = `NULL,NULL,NULL`
		checkpointJoin = ``
	}
	query := `SELECT COALESCE(st.owning_thread_id,sf.thread_id),COALESCE(st.root_session_id,t.root_session_id),
		sf.source_file_id,sf.file_generation,sf.device_id,sf.inode,sf.observed_size,sf.file_status,
		` + checkpointColumns + `,st.raw_tail_status,st.file_generation,st.device_id,
		st.inode,st.usage_parser_version,st.resolved_through_offset,st.observed_raw_size,turn.started_at_ms,
		ue.build_epoch,bm.active_committed_offset,bm.active_guard_hash,bm.active_state_fingerprint
		FROM codex_source_files sf
		LEFT JOIN threads t ON t.thread_id=sf.thread_id AND t.source='codex'
		` + checkpointJoin + `
		LEFT JOIN codex_usage_source_states st ON st.ledger_epoch=? AND st.source_file_id=sf.source_file_id
		LEFT JOIN codex_turns turn ON turn.ledger_epoch=st.ledger_epoch AND turn.source_file_id=sf.source_file_id
		AND turn.file_generation=st.file_generation AND turn.thread_id=st.owning_thread_id
		AND turn.turn_key=st.active_turn_key AND turn.status='open'
		LEFT JOIN source_usage_epochs ue ON ue.source='codex'
		LEFT JOIN codex_usage_build_sources bm ON bm.build_epoch=ue.build_epoch AND bm.source_file_id=sf.source_file_id
		WHERE (sf.file_status='present' OR st.source_file_id IS NOT NULL)
		AND (? IS NULL OR COALESCE(st.owning_thread_id,sf.thread_id)=?)
		ORDER BY sf.source_file_id`
	rows, err := q.Query(query, epoch, ownerThreadID, ownerThreadID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	unknown := make([]usage.CompactionUnknownScope, 0)
	type sourceCompletenessRow struct {
		thread, root                                                                         sql.NullString
		sourceFileID, generation, deviceID, inode, observed                                  int64
		fileStatus                                                                           string
		cpParser, cpOffset                                                                   sql.NullInt64
		cpStatus, rawTail                                                                    sql.NullString
		stateGeneration, stateDevice, stateInode, stateParser, resolvedOffset, stateObserved sql.NullInt64
		turnStart, rowBuildEpoch, frozenOffset                                               sql.NullInt64
		frozenGuard, frozenState                                                             []byte
	}
	var sourceRows []sourceCompletenessRow
	for rows.Next() {
		var row sourceCompletenessRow
		if err := rows.Scan(&row.thread, &row.root, &row.sourceFileID, &row.generation, &row.deviceID, &row.inode, &row.observed, &row.fileStatus,
			&row.cpParser, &row.cpOffset, &row.cpStatus, &row.rawTail, &row.stateGeneration, &row.stateDevice, &row.stateInode, &row.stateParser,
			&row.resolvedOffset, &row.stateObserved, &row.turnStart, &row.rowBuildEpoch, &row.frozenOffset, &row.frozenGuard, &row.frozenState); err != nil {
			return nil, err
		}
		sourceRows = append(sourceRows, row)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	if err := rows.Close(); err != nil {
		return nil, err
	}
	for _, row := range sourceRows {
		if !row.thread.Valid || !row.root.Valid {
			continue
		}
		complete := row.fileStatus == "present" && row.cpParser.Valid && row.cpParser.Int64 == parserVersion &&
			row.cpStatus.Valid && row.cpStatus.String == "ready" && row.cpOffset.Valid && row.cpOffset.Int64 == row.observed &&
			row.rawTail.Valid && row.rawTail.String == "none" && row.stateGeneration.Valid && row.stateGeneration.Int64 == row.generation &&
			row.stateDevice.Valid && row.stateDevice.Int64 == row.deviceID && row.stateInode.Valid && row.stateInode.Int64 == row.inode &&
			row.stateParser.Valid && row.stateParser.Int64 == parserVersion && row.resolvedOffset.Valid && row.resolvedOffset.Int64 == row.observed &&
			row.stateObserved.Valid && row.stateObserved.Int64 == row.observed
		if activeWithBuild {
			complete = false
			validFrozenGuard := row.frozenGuard == nil || len(row.frozenGuard) == 32
			if row.rowBuildEpoch.Valid && row.frozenOffset.Valid && row.frozenOffset.Int64 == row.observed && validFrozenGuard && len(row.frozenState) == 32 {
				activeProof, proofErr := activeSourceStateProofV3(q, epoch, row.sourceFileID)
				if proofErr != nil && !errors.Is(proofErr, errInvalidActiveSourceStateProof) {
					return nil, proofErr
				}
				complete = row.fileStatus == "present" && row.rawTail.Valid && row.rawTail.String == "none" &&
					row.stateGeneration.Valid && row.stateGeneration.Int64 == row.generation && row.stateDevice.Valid && row.stateDevice.Int64 == row.deviceID &&
					row.stateInode.Valid && row.stateInode.Int64 == row.inode && row.stateParser.Valid && row.stateParser.Int64 == parserVersion &&
					row.resolvedOffset.Valid && row.resolvedOffset.Int64 == row.observed && row.stateObserved.Valid && row.stateObserved.Int64 == row.observed &&
					proofErr == nil && bytes.Equal(activeProof, row.frozenState)
			}
		}
		if complete {
			continue
		}
		var startMS *int64
		if row.turnStart.Valid {
			start := row.turnStart.Int64
			startMS = &start
		}
		unknown = addUnknownCandidates(unknown, candidates, row.thread.String, row.root.String, nil, nil, startMS, nil)
	}
	return unknown, nil
}

func addUnknownCandidates(
	unknown []usage.CompactionUnknownScope,
	candidates []compactionCandidate,
	threadID, rootID string,
	model, reasoning *string,
	startMS, endMS *int64,
) []usage.CompactionUnknownScope {
	for _, candidate := range candidates {
		if candidate.threadID != threadID || candidate.rootSessionID != rootID ||
			(model != nil && candidate.model != *model) ||
			(reasoning != nil && !equalOptionalString(candidate.reasoning, reasoning)) {
			continue
		}
		unknown = append(unknown, usage.CompactionUnknownScope{
			ThreadID: threadID, RootSessionID: rootID, Model: candidate.model,
			ReasoningEffort: cloneString(candidate.reasoning), StartMS: cloneInt64(startMS), EndMS: cloneInt64(endMS),
		})
	}
	return unknown
}

func mergeCompactionUnknownScopes(scopes []usage.CompactionUnknownScope) []usage.CompactionUnknownScope {
	sort.Slice(scopes, func(i, j int) bool {
		a, b := scopes[i], scopes[j]
		if a.ThreadID != b.ThreadID {
			return a.ThreadID < b.ThreadID
		}
		if a.RootSessionID != b.RootSessionID {
			return a.RootSessionID < b.RootSessionID
		}
		if a.Model != b.Model {
			return a.Model < b.Model
		}
		if cmp := compareOptionalString(a.ReasoningEffort, b.ReasoningEffort); cmp != 0 {
			return cmp < 0
		}
		if cmp := compareOptionalInt64(a.StartMS, b.StartMS); cmp != 0 {
			return cmp < 0
		}
		return compareOptionalInt64(a.EndMS, b.EndMS) < 0
	})
	merged := make([]usage.CompactionUnknownScope, 0, len(scopes))
	for _, scope := range scopes {
		if len(merged) != 0 {
			previous := &merged[len(merged)-1]
			sameIdentity := previous.ThreadID == scope.ThreadID && previous.RootSessionID == scope.RootSessionID &&
				previous.Model == scope.Model && equalOptionalString(previous.ReasoningEffort, scope.ReasoningEffort)
			overlaps := previous.EndMS == nil || scope.StartMS == nil ||
				(previous.EndMS != nil && scope.StartMS != nil && *scope.StartMS <= *previous.EndMS)
			if sameIdentity && overlaps {
				if previous.EndMS == nil || scope.EndMS == nil {
					previous.EndMS = nil
				} else if *scope.EndMS > *previous.EndMS {
					previous.EndMS = cloneInt64(scope.EndMS)
				}
				continue
			}
		}
		merged = append(merged, scope)
	}
	return merged
}

func skillVisibilityProjection(q privateQuery, epoch, parserVersion int64) ([]skillVisibilityRow, error) {
	rows := make([]skillVisibilityRow, 0)
	if epoch <= 0 || parserVersion < skillVisibilityReadyParserVersion {
		return rows, nil
	}
	result, err := q.Query(`SELECT occurred_at_ms,root_session_id,model,skill_name,COUNT(*)
		FROM codex_skill_usage_events WHERE ledger_epoch=?
		GROUP BY occurred_at_ms,root_session_id,model,skill_name
		ORDER BY occurred_at_ms,root_session_id,model,skill_name`, epoch)
	if err != nil {
		return nil, err
	}
	defer result.Close()
	for result.Next() {
		var row skillVisibilityRow
		var model sql.NullString
		if err := result.Scan(&row.occurredAtMS, &row.rootSessionID, &model, &row.skillName, &row.multiplicity); err != nil {
			return nil, err
		}
		row.model = privateNullableString(model)
		rows = append(rows, row)
	}
	return rows, result.Err()
}

type skillVisibilityRow struct {
	occurredAtMS  int64
	rootSessionID string
	model         *string
	skillName     string
	multiplicity  int64
}

func quarantineVisibilityProjection(q privateQuery, epoch int64) ([]quarantineVisibilityRow, error) {
	rows := make([]quarantineVisibilityRow, 0)
	if epoch <= 0 {
		return rows, nil
	}
	result, err := q.Query(`SELECT root_session_id,primary_error_code,last_activity_at_ms
		FROM codex_usage_session_quarantine WHERE ledger_epoch=? ORDER BY root_session_id`, epoch)
	if err != nil {
		return nil, err
	}
	defer result.Close()
	for result.Next() {
		var row quarantineVisibilityRow
		if err := result.Scan(&row.rootSessionID, &row.primaryErrorCode, &row.lastActivityAtMS); err != nil {
			return nil, err
		}
		rows = append(rows, row)
	}
	return rows, result.Err()
}

type quarantineVisibilityRow struct {
	rootSessionID    string
	primaryErrorCode string
	lastActivityAtMS int64
}

func ActiveSourceStateProofV3(tx *source.WriteTx, activeEpoch, sourceFileID int64) ([]byte, error) {
	var proof []byte
	err := tx.Private(func(private storage.PrivateTx) error {
		value, err := activeSourceStateProofV3(private, activeEpoch, sourceFileID)
		proof = value
		return err
	})
	return proof, err
}

func activeSourceStateProofV3(q privateQuery, activeEpoch, sourceFileID int64) ([]byte, error) {
	if activeEpoch <= 0 {
		return nil, nil
	}
	rows, err := q.Query(`SELECT file_generation,device_id,inode,usage_parser_version,canonical_algorithm_version,
		resolved_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,owning_thread_id,root_session_id,
		continuation_state,previous_total_input_tokens,previous_total_cached_tokens,previous_total_cache_write_tokens,
		previous_total_output_tokens,previous_total_reasoning_tokens,previous_total_total_tokens,previous_total_fingerprint,
		previous_total_offset,chain_state,chain_block_reason,active_turn_key,active_model,active_model_offset,
		active_reasoning_effort,active_reasoning_effort_offset,reconciliation_state_json
		FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=?`, activeEpoch, sourceFileID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	if !rows.Next() {
		return nil, rows.Err()
	}
	values := make([]any, 28)
	destinations := make([]any, len(values))
	for index := range values {
		destinations[index] = &values[index]
	}
	if err := rows.Scan(destinations...); err != nil {
		return nil, fmt.Errorf("%w: read source state: %v", errInvalidActiveSourceStateProof, err)
	}
	if rows.Next() {
		return nil, fmt.Errorf("%w: duplicate source state", errInvalidActiveSourceStateProof)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	durableCarry, ok := values[27].(string)
	if !ok {
		return nil, fmt.Errorf("%w: reconciliation carry is not text", errInvalidActiveSourceStateProof)
	}
	carry, err := usage.DecodeReconciliationCarryJSON([]byte(durableCarry))
	if err != nil {
		return nil, fmt.Errorf("%w: decode reconciliation carry: %v", errInvalidActiveSourceStateProof, err)
	}
	canonicalCarry, err := usage.CanonicalReconciliationCarryJSON(carry)
	if err != nil {
		return nil, fmt.Errorf("%w: encode reconciliation carry: %v", errInvalidActiveSourceStateProof, err)
	}
	if !bytes.Equal(canonicalCarry, []byte(durableCarry)) {
		return nil, fmt.Errorf("%w: reconciliation carry is not canonical", errInvalidActiveSourceStateProof)
	}
	generation, ok := values[0].(int64)
	if !ok {
		return nil, fmt.Errorf("%w: source generation is not an integer", errInvalidActiveSourceStateProof)
	}
	hasher := blake3.New()
	_, _ = hasher.Write([]byte("usage-source-state-proof-v3\x00"))
	for _, value := range values[:27] {
		if err := appendSQLiteValue(hasher, value); err != nil {
			return nil, fmt.Errorf("%w: %v", errInvalidActiveSourceStateProof, err)
		}
	}
	if err := appendSQLiteValue(hasher, string(canonicalCarry)); err != nil {
		return nil, fmt.Errorf("%w: %v", errInvalidActiveSourceStateProof, err)
	}
	if err := appendUsageSourcePrivateProof(q, hasher, activeEpoch, sourceFileID, generation); err != nil {
		return nil, err
	}
	return hasher.Sum(nil), nil
}

func appendUsageSourcePrivateProof(q privateQuery, hasher *blake3.Hasher, epoch, sourceFileID, generation int64) error {
	_, _ = hasher.Write([]byte("usage-source-private-evidence-v1\x00"))
	for _, value := range []int64{epoch, sourceFileID, generation} {
		if err := appendSQLiteValue(hasher, value); err != nil {
			return err
		}
	}
	if err := appendPrivateQueryRows(q, hasher, "codex_usage_event_occurrences", `SELECT source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,event_id
		FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?
		ORDER BY source_file_id,file_generation,source_start_offset`, epoch, sourceFileID, generation); err != nil {
		return err
	}
	if err := appendPrivateQueryRows(q, hasher, "codex_usage_event_facts", `SELECT f.source,f.ledger_epoch,f.event_id,f.owning_thread_id,f.response_id,f.evidence_kind,f.operation
		FROM codex_usage_event_facts f WHERE f.source='codex' AND f.ledger_epoch=? AND (
		EXISTS(SELECT 1 FROM codex_usage_event_occurrences o WHERE o.source=f.source AND o.ledger_epoch=f.ledger_epoch AND o.source_file_id=? AND o.file_generation=? AND o.event_id=f.event_id)
		OR EXISTS(SELECT 1 FROM codex_compaction_markers m WHERE m.source=f.source AND m.ledger_epoch=f.ledger_epoch AND m.source_file_id=? AND m.file_generation=? AND m.resolved_event_id=f.event_id)
		OR EXISTS(SELECT 1 FROM codex_turns t JOIN usage_events e ON e.source='codex' AND e.source_epoch=t.ledger_epoch AND e.thread_id=t.thread_id AND e.turn_key=t.turn_key
		WHERE t.ledger_epoch=f.ledger_epoch AND t.source_file_id=? AND t.file_generation=? AND e.event_kind='turn_compensation' AND e.event_id=f.event_id))
		ORDER BY f.event_id`, epoch, sourceFileID, generation, sourceFileID, generation, sourceFileID, generation); err != nil {
		return err
	}
	if err := verifyCanonicalLegacyWindows(q, epoch, sourceFileID, generation); err != nil {
		return err
	}
	if err := appendPrivateQueryRows(q, hasher, "codex_compaction_markers", `SELECT source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
		owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,response_id,resolved_event_id,unknown_reason
		FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=? AND (
		(source_file_id=? AND file_generation=?) OR resolved_event_id IN (SELECT event_id FROM codex_usage_event_occurrences
		WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?))
		ORDER BY source_file_id,file_generation,source_start_offset`, epoch, sourceFileID, generation, epoch, sourceFileID, generation); err != nil {
		return err
	}
	if err := appendPrivateQueryRows(q, hasher, "codex_usage_reconciliation_windows", `SELECT source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,turn_key,state_json
		FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?
		ORDER BY source_file_id,file_generation,source_start_offset`, epoch, sourceFileID, generation); err != nil {
		return err
	}
	if err := appendPrivateQueryRows(q, hasher, "codex_usage_event_holds", `SELECT source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason
		FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND (
		(source_file_id=? AND file_generation=?) OR event_id IN (SELECT event_id FROM codex_usage_event_occurrences
		WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?))
		ORDER BY source_file_id,file_generation,event_id`, epoch, sourceFileID, generation, epoch, sourceFileID, generation); err != nil {
		return err
	}
	if err := appendPrivateQueryRows(q, hasher, "codex_turns", `SELECT ledger_epoch,source_file_id,file_generation,turn_key,thread_id,raw_turn_id,started_at_ms,ended_at_ms,
		start_offset,end_offset,status,start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,start_total_output_tokens,
		start_total_reasoning_tokens,start_total_total_tokens,start_total_fingerprint,last_total_input_tokens,last_total_cached_tokens,
		last_total_cache_write_tokens,last_total_output_tokens,last_total_reasoning_tokens,last_total_total_tokens,last_total_fingerprint,
		accounted_input_tokens,accounted_cached_tokens,accounted_cache_write_tokens,accounted_output_tokens,accounted_reasoning_tokens,
		accounted_total_tokens,accounted_fingerprint,accounted_candidate_count,model_state,single_model,unresolved_model_seen,
		reasoning_effort_state,single_reasoning_effort,unresolved_reasoning_effort_seen,compensation_allowed,block_start_missing,
		block_time_missing,block_reset,block_ownership_gap,block_parser_gap,block_required_invalid,block_model_unresolved,quality_status,state_through_offset
		FROM codex_turns WHERE ledger_epoch=? AND source_file_id=? AND file_generation=?
		ORDER BY source_file_id,file_generation,turn_key`, epoch, sourceFileID, generation); err != nil {
		return err
	}
	if err := appendPrivateQueryRows(q, hasher, "turn_compensation_events", `SELECT e.source,e.source_epoch,e.event_id,e.event_kind,e.occurred_at_ms,e.thread_id,e.root_session_id,e.turn_key,
		e.model,e.reasoning_effort,e.estimated_cost_nanos_usd,e.input_tokens,e.cached_tokens,e.cache_write_tokens,e.output_tokens,
		e.reasoning_tokens,e.total_tokens,e.quality_status FROM usage_events e
		WHERE e.source='codex' AND e.source_epoch=? AND e.event_kind='turn_compensation'
		AND EXISTS(SELECT 1 FROM codex_turns t WHERE t.ledger_epoch=e.source_epoch AND t.source_file_id=? AND t.file_generation=? AND t.thread_id=e.thread_id AND t.turn_key=e.turn_key)
		ORDER BY e.event_id`, epoch, sourceFileID, generation); err != nil {
		return err
	}
	return appendPrivateQueryRows(q, hasher, "turn_compensation_occurrences", `SELECT o.source,o.ledger_epoch,o.source_file_id,o.file_generation,o.source_start_offset,o.source_end_offset,o.event_id
		FROM codex_usage_event_occurrences o JOIN usage_events e ON e.source=o.source AND e.source_epoch=o.ledger_epoch AND e.event_id=o.event_id
		WHERE o.source='codex' AND o.ledger_epoch=? AND e.event_kind='turn_compensation'
		AND EXISTS(SELECT 1 FROM codex_turns t WHERE t.ledger_epoch=e.source_epoch AND t.source_file_id=? AND t.file_generation=? AND t.thread_id=e.thread_id AND t.turn_key=e.turn_key)
		ORDER BY o.source_file_id,o.file_generation,o.source_start_offset,o.event_id`, epoch, sourceFileID, generation)
}

func verifyCanonicalLegacyWindows(q privateQuery, epoch, sourceFileID, generation int64) error {
	rows, err := q.Query(`SELECT state_json FROM codex_usage_reconciliation_windows
		WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=?
		ORDER BY source_file_id,file_generation,source_start_offset`, epoch, sourceFileID, generation)
	if err != nil {
		return err
	}
	defer rows.Close()
	for rows.Next() {
		var durable string
		if err := rows.Scan(&durable); err != nil {
			return err
		}
		window, err := usage.DecodeLegacyReconciliationWindow([]byte(durable))
		if err != nil {
			return fmt.Errorf("%w: decode reconciliation window: %v", errInvalidActiveSourceStateProof, err)
		}
		canonical, err := usage.CanonicalLegacyReconciliationWindowJSON(window)
		if err != nil {
			return fmt.Errorf("%w: encode reconciliation window: %v", errInvalidActiveSourceStateProof, err)
		}
		if !bytes.Equal(canonical, []byte(durable)) {
			return fmt.Errorf("%w: reconciliation window is not canonical", errInvalidActiveSourceStateProof)
		}
	}
	return rows.Err()
}

func appendPrivateQueryRows(q privateQuery, hasher *blake3.Hasher, tableTag, query string, args ...any) error {
	rows, err := q.Query(query, args...)
	if err != nil {
		return err
	}
	defer rows.Close()
	columns, err := rows.Columns()
	if err != nil {
		return err
	}
	writeU64(hasher, uint64(len(tableTag)))
	_, _ = hasher.Write([]byte(tableTag))
	values := make([]any, len(columns))
	destinations := make([]any, len(columns))
	for index := range values {
		destinations[index] = &values[index]
	}
	for rows.Next() {
		for index := range values {
			values[index] = nil
		}
		if err := rows.Scan(destinations...); err != nil {
			return err
		}
		_, _ = hasher.Write([]byte{0xff})
		writeU64(hasher, uint64(len(columns)))
		for _, value := range values {
			if err := appendSQLiteValue(hasher, value); err != nil {
				return err
			}
		}
	}
	if err := rows.Err(); err != nil {
		return err
	}
	_, _ = hasher.Write([]byte{0xfe})
	return nil
}

func appendSQLiteValue(hasher *blake3.Hasher, value any) error {
	switch value := value.(type) {
	case nil:
		_, _ = hasher.Write([]byte{0})
	case int64:
		_, _ = hasher.Write([]byte{1})
		var encoded [8]byte
		binary.BigEndian.PutUint64(encoded[:], uint64(value))
		_, _ = hasher.Write(encoded[:])
	case float64:
		_, _ = hasher.Write([]byte{2})
		var encoded [8]byte
		binary.BigEndian.PutUint64(encoded[:], math.Float64bits(value))
		_, _ = hasher.Write(encoded[:])
	case string:
		_, _ = hasher.Write([]byte{3})
		writeU64(hasher, uint64(len(value)))
		_, _ = hasher.Write([]byte(value))
	case []byte:
		_, _ = hasher.Write([]byte{4})
		writeU64(hasher, uint64(len(value)))
		_, _ = hasher.Write(value)
	default:
		return fmt.Errorf("unsupported SQLite value type %T", value)
	}
	return nil
}

func writeU64(hasher *blake3.Hasher, value uint64) {
	var encoded [8]byte
	binary.BigEndian.PutUint64(encoded[:], value)
	_, _ = hasher.Write(encoded[:])
}

func privateNullableString(value sql.NullString) *string {
	if !value.Valid {
		return nil
	}
	copy := value.String
	return &copy
}

func cloneString(value *string) *string {
	if value == nil {
		return nil
	}
	copy := *value
	return &copy
}

func cloneInt64(value *int64) *int64 {
	if value == nil {
		return nil
	}
	copy := *value
	return &copy
}

func equalOptionalString(left, right *string) bool {
	if left == nil || right == nil {
		return left == nil && right == nil
	}
	return *left == *right
}

func compareOptionalString(left, right *string) int {
	if left == nil || right == nil {
		if left == nil && right == nil {
			return 0
		}
		if left == nil {
			return -1
		}
		return 1
	}
	if *left < *right {
		return -1
	}
	if *left > *right {
		return 1
	}
	return 0
}

func compareOptionalInt64(left, right *int64) int {
	if left == nil || right == nil {
		if left == nil && right == nil {
			return 0
		}
		if left == nil {
			return -1
		}
		return 1
	}
	if *left < *right {
		return -1
	}
	if *left > *right {
		return 1
	}
	return 0
}
