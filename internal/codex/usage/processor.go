package usage

import (
	"fmt"
	"sort"

	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

func ProcessRecords(state CounterState, records []OwnedRecord, committedAtMS int64) (ProcessBatch, error) {
	if committedAtMS < 0 {
		return ProcessBatch{}, fmt.Errorf("commit time must be non-negative")
	}
	if state.Carry.Version == 0 {
		state.Carry = NewReconciliationCarry()
	}
	batch := ProcessBatch{
		SourceState: state.Source,
	}
	if len(records) > 0 {
		batch.LogicalSafeOffset = records[0].Parsed.StartOffset
	}
	openTurn := cloneTurn(state.OpenTurn)
	turns := make(map[string]TurnWrite)
	accounting := candidateAccounting{
		indexes: make(map[string]int), explicitByTurn: make(map[string]sharedusage.NormalizedTokenUsage),
		blockedTurns: make(map[string]bool),
	}
	for _, input := range records {
		record := input.Parsed
		if record.SourceFileID != state.Source.SourceFileID || record.Generation != state.Source.Generation ||
			record.StartOffset < 0 || record.EndOffset <= record.StartOffset || input.PhysicalEndOffset < 0 {
			return ProcessBatch{}, fmt.Errorf("usage record does not match source state")
		}
		if input.Ownership.Kind == rollout.OwnershipReplayedAncestor {
			state.Source.ContinuationState = ContinuationReplayedAncestor
			if err := advanceRecordBoundary(&batch, &state.Source, input); err != nil {
				return ProcessBatch{}, err
			}
			continue
		}
		if input.Ownership.Kind != rollout.OwnershipOwning || input.Ownership.ThreadID != state.Source.OwningThreadID {
			interruptChain(&state.Source, ChainBlockOwnershipGap)
			if openTurn != nil {
				openTurn.Blocks.OwnershipGap = true
				refreshTurnQuality(openTurn)
				openTurn.StateThroughOffset = batch.LogicalSafeOffset
				openTurn.UpdatedAtMS = committedAtMS
				turns[openTurn.TurnKey] = *openTurn
			}
			stop := record.StartOffset
			batch.StopBeforeOffset = &stop
			batch.UnresolvedBoundary = true
			break
		}
		state.Source.ContinuationState = ContinuationOwningLive
		if needsResolvedRoot(record.Kind) && !validCarryIdentity(state.Source.RootSessionID) {
			stop := record.StartOffset
			batch.StopBeforeOffset = &stop
			batch.UnresolvedBoundary = true
			break
		}
		if err := processOwnedRecord(&state.Source, &state.Carry, &openTurn, &batch, turns, &accounting, input, committedAtMS); err != nil {
			return ProcessBatch{}, err
		}
	}
	if openTurn != nil {
		if _, changed := turns[openTurn.TurnKey]; changed {
			turns[openTurn.TurnKey] = *openTurn
		}
	}
	turnKeys := make([]string, 0, len(turns))
	for key := range turns {
		turnKeys = append(turnKeys, key)
	}
	sort.Strings(turnKeys)
	for _, key := range turnKeys {
		batch.TurnUpserts = append(batch.TurnUpserts, turns[key])
	}
	batch.SourceState = state.Source
	batch.SourceState.UpdatedAtMS = committedAtMS
	if err := SetReconciliationCarry(&batch.SourceState, state.Carry); err != nil {
		return ProcessBatch{}, err
	}
	return batch, nil
}

func processOwnedRecord(
	state *SourceState,
	carry *ReconciliationCarry,
	openTurn **TurnWrite,
	batch *ProcessBatch,
	turns map[string]TurnWrite,
	accounting *candidateAccounting,
	input OwnedRecord,
	committedAtMS int64,
) error {
	record := input.Parsed
	switch record.Kind {
	case RawMalformed, RawOversized:
		reason := ChainBlockMalformed
		if record.Kind == RawOversized {
			reason = ChainBlockOversized
		} else if record.GapKind == rollout.GapParser {
			reason = ChainBlockParserGap
		} else if record.GapKind == rollout.GapRequiredInvalid {
			reason = ChainBlockTotalInvalid
		} else if record.GapKind == rollout.GapOwnership {
			reason = ChainBlockOwnershipGap
		}
		interruptChain(state, reason)
		if *openTurn != nil {
			applyChainBlock(&(*openTurn).Blocks, &reason)
			(*openTurn).StateThroughOffset = record.EndOffset
			(*openTurn).UpdatedAtMS = committedAtMS
			refreshTurnQuality(*openTurn)
			turns[(*openTurn).TurnKey] = **openTurn
		}
	case RawTokenCount:
		if err := processTokenCount(state, openTurn, batch, turns, accounting, record, input.PhysicalEndOffset, committedAtMS); err != nil {
			return err
		}
	case RawTurnContext:
		state.ActiveModel = nil
		state.ActiveModelOffset = nil
		if record.Model != "" {
			state.ActiveModel = stringPointer(record.Model)
			state.ActiveModelOffset = int64Pointer(record.StartOffset)
		}
		state.ActiveReasoningEffort = nil
		state.ActiveReasoningEffortOffset = nil
		if record.ReasoningEffort != "" {
			state.ActiveReasoningEffort = stringPointer(record.ReasoningEffort)
			state.ActiveReasoningEffortOffset = int64Pointer(record.StartOffset)
		}
	case RawLifecycle:
		applyLifecycle(state, openTurn, turns, record, committedAtMS)
	case RawResponseUsage:
		if record.Response != nil {
			pending := PendingEvidenceRecord{Kind: PendingResponseUsage,
				TimestampMS: cloneInt64(record.TimestampMS), StartOffset: uint64(record.StartOffset),
				EndOffset: uint64(record.EndOffset), Response: cloneResponseEvidence(record.Response)}
			appendResponseCandidate(state, carry, *openTurn, batch, accounting, record, input.Ownership.ThreadID, OperationResponse, pending)
		}
	case RawCompacted:
		if record.Compaction != nil {
			compaction := cloneCompactionEvidence(record.Compaction)
			pending := PendingUsageEvidence{Record: PendingEvidenceRecord{
				Kind: PendingCompacted, TimestampMS: cloneInt64(record.TimestampMS),
				StartOffset: uint64(record.StartOffset), EndOffset: uint64(record.EndOffset),
				Compaction: compaction,
			}, Model: optionalString(state.ActiveModel), ReasoningEffort: optionalString(state.ActiveReasoningEffort)}
			batch.Compactions = append(batch.Compactions, pending)
			if compaction.Latest != nil && compaction.Latest.Usage.State == UsageValueValid && validCarryIdentity(compaction.Latest.ResponseID) {
				responseRecord := record
				responseRecord.Response = compaction.Latest
				compactedPending := PendingEvidenceRecord{Kind: PendingCompacted, TimestampMS: cloneInt64(record.TimestampMS),
					StartOffset: uint64(record.StartOffset), EndOffset: uint64(record.EndOffset), Compaction: compaction}
				appendResponseCandidate(state, carry, *openTurn, batch, accounting, responseRecord,
					input.Ownership.ThreadID, OperationCompaction, compactedPending)
			}
		}
	case RawUnknown, RawIgnored:
	default:
		return fmt.Errorf("unsupported parsed usage kind %d", record.Kind)
	}
	if err := advanceRecordBoundary(batch, state, input); err != nil {
		return err
	}
	if *openTurn != nil {
		(*openTurn).StateThroughOffset = record.EndOffset
		(*openTurn).UpdatedAtMS = committedAtMS
		refreshTurnQuality(*openTurn)
		turns[(*openTurn).TurnKey] = **openTurn
	}
	return nil
}

func processTokenCount(
	state *SourceState,
	openTurn **TurnWrite,
	batch *ProcessBatch,
	turns map[string]TurnWrite,
	accounting *candidateAccounting,
	record ParsedRecord,
	physicalEnd int64,
	committedAtMS int64,
) error {
	if !record.HasTokenInfo {
		return nil
	}
	if state.OwningThreadID != state.RootSessionID && state.ActiveModel == nil {
		if record.Total.State == UsageValueValid {
			setCounterBaseline(state, *openTurn, record.Total.Value, physicalEnd)
		}
		return nil
	}
	if record.Total.State != UsageValueValid {
		interruptChain(state, ChainBlockTotalInvalid)
		if *openTurn != nil {
			(*openTurn).Blocks.RequiredInvalid = true
			(*openTurn).StateThroughOffset = record.EndOffset
			(*openTurn).UpdatedAtMS = committedAtMS
			refreshTurnQuality(*openTurn)
			turns[(*openTurn).TurnKey] = **openTurn
		}
		return nil
	}
	current := record.Total.Value
	previous := cloneUsage(state.PreviousTotal)
	if record.TimestampMS == nil {
		if *openTurn != nil {
			(*openTurn).Blocks.TimeMissing = true
			(*openTurn).LastTotal = cloneUsage(&current)
			(*openTurn).StateThroughOffset = record.EndOffset
			(*openTurn).UpdatedAtMS = committedAtMS
			refreshTurnQuality(*openTurn)
			turns[(*openTurn).TurnKey] = **openTurn
		}
		setCounterBaseline(state, *openTurn, current, physicalEnd)
		return nil
	}
	if state.ChainState == ChainInterrupted {
		setCounterBaseline(state, *openTurn, current, physicalEnd)
		return nil
	}
	if previous != nil && (requiredCountersDecreased(current, *previous) || cacheWriteDecreased(current, *previous)) {
		if *openTurn != nil {
			(*openTurn).Blocks.Reset = true
		}
		if record.Last.State == UsageValueValid && !usageIsZero(record.Last.Value) {
			appendLegacyCandidate(state, *openTurn, batch, accounting, record, record.Last.Value, previous, current, 0)
		}
		setCounterBaseline(state, *openTurn, current, physicalEnd)
		return nil
	}
	if previous != nil && equalNormalizedUsage(current, *previous) {
		setCounterBaseline(state, *openTurn, current, physicalEnd)
		return nil
	}
	switch record.Last.State {
	case UsageValueValid:
		if !usageIsZero(record.Last.Value) {
			appendLegacyCandidate(state, *openTurn, batch, accounting, record, record.Last.Value, previous, current, 0)
		}
	case UsageValueMissing:
		if previous != nil {
			delta, err := current.CheckedSub(*previous)
			if err != nil {
				return fmt.Errorf("recover usage delta: %w", err)
			}
			if !usageIsZero(delta) {
				appendLegacyCandidate(state, *openTurn, batch, accounting, record, delta, previous, current, 1)
			}
		}
	case UsageValueInvalid:
	}
	setCounterBaseline(state, *openTurn, current, physicalEnd)
	return nil
}

func appendLegacyCandidate(state *SourceState, turn *TurnWrite, batch *ProcessBatch, accounting *candidateAccounting, record ParsedRecord,
	usage sharedusage.NormalizedTokenUsage, previous *sharedusage.NormalizedTokenUsage,
	current sharedusage.NormalizedTokenUsage, eventKind byte) {
	model := "unknown"
	if state.ActiveModel != nil {
		model = *state.ActiveModel
	}
	var effort *string
	if state.ActiveReasoningEffort != nil {
		effort = stringPointer(*state.ActiveReasoningEffort)
	}
	var turnKey *string
	if turn != nil {
		turnKey = stringPointer(turn.TurnKey)
	}
	eventID := LegacyEventID(state.OwningThreadID, turnKey, eventKind, *record.TimestampMS, previous, current, usage, model, effort)
	candidate := UsageCandidate{
		SourceFileID: record.SourceFileID, Generation: record.Generation,
		EventID: eventID, EventKind: eventKind, EvidenceKind: EvidenceLegacy, Operation: OperationResponse,
		OccurredAtMS: *record.TimestampMS, StartOffset: record.StartOffset, EndOffset: record.EndOffset,
		OwningThreadID: state.OwningThreadID, RootSessionID: state.RootSessionID, TurnKey: turnKey,
		Model: model, ReasoningEffort: effort, PreviousTotal: cloneUsage(previous),
		CurrentTotal: cloneUsage(&current), Usage: usage,
	}
	appendCandidate(batch, accounting, turn, candidate)
}

func appendResponseCandidate(state *SourceState, carry *ReconciliationCarry, turn *TurnWrite, batch *ProcessBatch,
	accounting *candidateAccounting, record ParsedRecord, owner string, operation Operation, pendingRecord PendingEvidenceRecord) {
	evidence := cloneResponseEvidence(record.Response)
	model := optionalString(state.ActiveModel)
	effort := optionalString(state.ActiveReasoningEffort)
	if evidence.Usage.State != UsageValueValid {
		if operation == OperationResponse {
			interruptChain(state, ChainBlockTotalInvalid)
		}
		if turn != nil && operation == OperationResponse {
			turn.Blocks.RequiredInvalid = true
		}
		return
	}
	if hasPendingResponse(*carry, evidence.ResponseID) {
		return
	}
	if record.TimestampMS == nil || model == nil {
		carry.PendingEvidence = append(carry.PendingEvidence, PendingUsageEvidence{Record: pendingRecord, Model: model, ReasoningEffort: effort})
		carry.PendingResponseIDs = append(carry.PendingResponseIDs, evidence.ResponseID)
		return
	}
	var turnKey *string
	turnMatches := turn != nil && (evidence.TurnID == "" || (turn.RawTurnID != nil && *turn.RawTurnID == evidence.TurnID))
	if evidence.TurnID != "" {
		turnKey = stringPointer(evidence.TurnID)
	} else if turn != nil {
		turnKey = stringPointer(turn.TurnKey)
	}
	usage := evidence.Usage.Value
	modelValue := "unknown"
	if model != nil {
		modelValue = *model
	}
	candidate := UsageCandidate{
		SourceFileID: record.SourceFileID, Generation: record.Generation,
		EventID: ResponseEventID(owner, evidence.ResponseID), EventKind: 0, EvidenceKind: EvidenceExplicit,
		Operation: operation, OccurredAtMS: *record.TimestampMS, StartOffset: record.StartOffset, EndOffset: record.EndOffset,
		OwningThreadID: owner, RootSessionID: state.RootSessionID, TurnKey: turnKey,
		Model: modelValue, ReasoningEffort: effort, Response: evidence, Usage: usage,
	}
	var accountedTurn *TurnWrite
	if turnMatches {
		accountedTurn = turn
	}
	appendCandidate(batch, accounting, accountedTurn, candidate)
}

type candidateAccounting struct {
	indexes        map[string]int
	explicitByTurn map[string]sharedusage.NormalizedTokenUsage
	blockedTurns   map[string]bool
}

func appendCandidate(batch *ProcessBatch, accounting *candidateAccounting, turn *TurnWrite, candidate UsageCandidate) {
	if index, exists := accounting.indexes[candidate.EventID]; exists {
		existing := &batch.Candidates[index]
		if existing.EvidenceKind == EvidenceExplicit && candidate.EvidenceKind == EvidenceExplicit &&
			!equalNormalizedUsage(existing.Usage, candidate.Usage) {
			setFatal(batch, FatalResponseUsage, candidate)
		}
		if candidate.Operation == OperationCompaction {
			existing.Operation = OperationCompaction
		}
		appendOccurrence(batch, candidate)
		return
	}
	accounting.indexes[candidate.EventID] = len(batch.Candidates)
	batch.Candidates = append(batch.Candidates, candidate)
	appendOccurrence(batch, candidate)
	if turn == nil || candidate.TurnKey == nil || *candidate.TurnKey != turn.TurnKey {
		return
	}
	observeTurnCandidate(turn, candidate.Model, candidate.ReasoningEffort, candidate.CurrentTotal)
	if candidate.EvidenceKind == EvidenceExplicit {
		if total, exists := accounting.explicitByTurn[turn.TurnKey]; exists {
			next, err := total.CheckedAdd(candidate.Usage)
			if err != nil {
				setFatal(batch, FatalArithmeticOverflow, candidate)
				accounting.blockedTurns[turn.TurnKey] = true
				return
			}
			accounting.explicitByTurn[turn.TurnKey] = next
		} else {
			accounting.explicitByTurn[turn.TurnKey] = candidate.Usage
		}
	}
	if accounting.blockedTurns[turn.TurnKey] {
		return
	}
	var accounted sharedusage.NormalizedTokenUsage
	if turn.AccountedCandidateCount == 0 {
		accounted = candidate.Usage
	} else {
		var err error
		accounted, err = turn.Accounted.CheckedAdd(candidate.Usage)
		if err != nil {
			// The turn may contain legacy evidence that E will replace or discard.
			// Keep its last safe aggregate and let E recompute it from the batch.
			accounting.blockedTurns[turn.TurnKey] = true
			return
		}
	}
	if turn.AccountedCandidateCount == int64(^uint64(0)>>1) {
		if candidate.EvidenceKind == EvidenceExplicit {
			setFatal(batch, FatalArithmeticOverflow, candidate)
		}
		accounting.blockedTurns[turn.TurnKey] = true
		return
	}
	turn.Accounted = accounted
	turn.AccountedCandidateCount++
}

func appendOccurrence(batch *ProcessBatch, candidate UsageCandidate) {
	for _, occurrence := range batch.Occurrences {
		if occurrence.SourceFileID == candidate.SourceFileID && occurrence.Generation == candidate.Generation &&
			occurrence.StartOffset == candidate.StartOffset && occurrence.EndOffset == candidate.EndOffset &&
			occurrence.EventID == candidate.EventID {
			return
		}
	}
	batch.Occurrences = append(batch.Occurrences, OccurrenceWrite{
		SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
		StartOffset: candidate.StartOffset, EndOffset: candidate.EndOffset, EventID: candidate.EventID,
	})
}

func setFatal(batch *ProcessBatch, code FatalConflictCode, candidate UsageCandidate) {
	if batch.Fatal != nil {
		return
	}
	batch.Fatal = &FatalConflict{
		Code: code, SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
		Offset: candidate.StartOffset, ThreadID: candidate.OwningThreadID,
	}
}

func hasPendingResponse(carry ReconciliationCarry, responseID string) bool {
	for _, pending := range carry.PendingEvidence {
		if pending.Record.Response != nil && pending.Record.Response.ResponseID == responseID {
			return true
		}
		if pending.Record.Compaction != nil && pending.Record.Compaction.Latest != nil &&
			pending.Record.Compaction.Latest.ResponseID == responseID {
			return true
		}
	}
	return false
}

func applyLifecycle(state *SourceState, openTurn **TurnWrite,
	turns map[string]TurnWrite, record ParsedRecord, committedAtMS int64) {
	var rawTurnID *string
	if record.TurnID != "" {
		rawTurnID = stringPointer(record.TurnID)
	}
	switch record.Lifecycle {
	case LifecycleStarted:
		if *openTurn != nil {
			(*openTurn).Status = TurnAborted
			(*openTurn).EndedAtMS = cloneInt64(record.TimestampMS)
			(*openTurn).EndOffset = int64Pointer(record.StartOffset)
			(*openTurn).Blocks.RequiredInvalid = true
			(*openTurn).StateThroughOffset = record.StartOffset
			(*openTurn).UpdatedAtMS = committedAtMS
			refreshTurnQuality(*openTurn)
			turns[(*openTurn).TurnKey] = **openTurn
		}
		turnKey := TurnKeyFor(state.OwningThreadID, rawTurnID, uint64(record.StartOffset), record.TimestampMS)
		turn := TurnWrite{
			SourceFileID: state.SourceFileID, Generation: state.Generation, TurnKey: turnKey,
			ThreadID: state.OwningThreadID, RawTurnID: rawTurnID, StartedAtMS: cloneInt64(record.TimestampMS),
			StartOffset: record.StartOffset, Status: TurnOpen, Accounted: sharedusage.Zero(),
			ModelState: TurnValueNone, ReasoningEffortState: TurnValueNone,
			QualityStatus: "complete", StateThroughOffset: record.EndOffset, UpdatedAtMS: committedAtMS,
		}
		if state.ChainState == ChainContinuous {
			turn.StartTotal = cloneUsage(state.PreviousTotal)
		}
		if turn.StartTotal == nil {
			turn.Blocks.StartMissing = true
		}
		if record.TimestampMS == nil {
			turn.Blocks.TimeMissing = true
		}
		if state.ChainState == ChainInterrupted {
			applyChainBlock(&turn.Blocks, state.ChainBlockReason)
		}
		refreshTurnQuality(&turn)
		*openTurn = &turn
		state.ActiveTurnKey = stringPointer(turnKey)
		turns[turnKey] = turn
	case LifecycleCompleted, LifecycleAborted, LifecycleFailed:
		if *openTurn == nil || (rawTurnID != nil && ((*openTurn).RawTurnID == nil || *(*openTurn).RawTurnID != *rawTurnID)) {
			return
		}
		turn := *openTurn
		turn.EndedAtMS = cloneInt64(record.TimestampMS)
		turn.EndOffset = int64Pointer(record.EndOffset)
		turn.StateThroughOffset = record.EndOffset
		turn.UpdatedAtMS = committedAtMS
		if record.TimestampMS == nil {
			turn.Blocks.TimeMissing = true
		}
		switch record.Lifecycle {
		case LifecycleCompleted:
			turn.Status = TurnCompleted
		case LifecycleAborted:
			turn.Status = TurnAborted
		case LifecycleFailed:
			turn.Status = TurnFailed
		}
		refreshTurnQuality(turn)
		turns[turn.TurnKey] = *turn
		*openTurn = nil
		state.ActiveTurnKey = nil
	}
}

func observeTurnCandidate(turn *TurnWrite, model string, effort *string, current *sharedusage.NormalizedTokenUsage) {
	if model == "unknown" {
		turn.UnresolvedModelSeen = true
		turn.Blocks.ModelUnresolved = true
	} else {
		switch turn.ModelState {
		case TurnValueNone:
			turn.ModelState = TurnValueSingle
			turn.SingleModel = stringPointer(model)
		case TurnValueSingle:
			if turn.SingleModel == nil || *turn.SingleModel != model {
				turn.ModelState = TurnValueMixed
				turn.SingleModel = nil
			}
		}
	}
	if effort == nil {
		turn.UnresolvedReasoningEffortSeen = true
	} else {
		switch turn.ReasoningEffortState {
		case TurnValueNone:
			turn.ReasoningEffortState = TurnValueSingle
			turn.SingleReasoningEffort = stringPointer(*effort)
		case TurnValueSingle:
			if turn.SingleReasoningEffort == nil || *turn.SingleReasoningEffort != *effort {
				turn.ReasoningEffortState = TurnValueMixed
				turn.SingleReasoningEffort = nil
			}
		}
	}
	if current != nil {
		turn.LastTotal = cloneUsage(current)
	}
	refreshTurnQuality(turn)
}

func advanceRecordBoundary(batch *ProcessBatch, state *SourceState, input OwnedRecord) error {
	if input.Parsed.StartOffset < batch.LogicalSafeOffset || input.Parsed.EndOffset <= input.Parsed.StartOffset {
		return fmt.Errorf("usage records are not in logical offset order")
	}
	batch.LogicalSafeOffset = input.Parsed.EndOffset
	if input.PhysicalEndOffset > state.ResolvedThroughOffset {
		state.ResolvedThroughOffset = input.PhysicalEndOffset
	}
	return nil
}

func setCounterBaseline(state *SourceState, turn *TurnWrite, current sharedusage.NormalizedTokenUsage, physicalOffset int64) {
	state.PreviousTotal = cloneUsage(&current)
	state.PreviousTotalOffset = int64Pointer(physicalOffset)
	state.ChainState = ChainContinuous
	state.ChainBlockReason = nil
	if turn != nil {
		turn.LastTotal = cloneUsage(&current)
	}
}

func needsResolvedRoot(kind RawKind) bool {
	switch kind {
	case RawMalformed, RawOversized, RawResponseUsage, RawCompacted, RawTokenCount, RawTurnContext, RawLifecycle:
		return true
	default:
		return false
	}
}

func interruptChain(state *SourceState, reason ChainBlockReason) {
	state.ChainState = ChainInterrupted
	state.ChainBlockReason = &reason
}

func requiredCountersDecreased(current, previous sharedusage.NormalizedTokenUsage) bool {
	return current.InputTokens < previous.InputTokens || current.CachedTokens < previous.CachedTokens ||
		current.OutputTokens < previous.OutputTokens || current.ReasoningTokens < previous.ReasoningTokens
}

func cacheWriteDecreased(current, previous sharedusage.NormalizedTokenUsage) bool {
	return current.CacheWriteTokens != nil && previous.CacheWriteTokens != nil &&
		*current.CacheWriteTokens < *previous.CacheWriteTokens
}

func equalNormalizedUsage(left, right sharedusage.NormalizedTokenUsage) bool {
	if left.InputTokens != right.InputTokens || left.CachedTokens != right.CachedTokens ||
		left.OutputTokens != right.OutputTokens || left.ReasoningTokens != right.ReasoningTokens ||
		left.TotalTokens != right.TotalTokens || (left.CacheWriteTokens == nil) != (right.CacheWriteTokens == nil) {
		return false
	}
	return left.CacheWriteTokens == nil || *left.CacheWriteTokens == *right.CacheWriteTokens
}

func usageIsZero(value sharedusage.NormalizedTokenUsage) bool {
	return value.InputTokens == 0 && value.CachedTokens == 0 && value.OutputTokens == 0 &&
		value.ReasoningTokens == 0 && value.TotalTokens == 0 &&
		(value.CacheWriteTokens == nil || *value.CacheWriteTokens == 0)
}

func applyChainBlock(blocks *CompensationBlocks, reason *ChainBlockReason) {
	if reason == nil {
		return
	}
	switch *reason {
	case ChainBlockMalformed, ChainBlockOversized, ChainBlockParserGap:
		blocks.ParserGap = true
	case ChainBlockTotalInvalid:
		blocks.RequiredInvalid = true
	case ChainBlockOwnershipGap:
		blocks.OwnershipGap = true
	}
}

func refreshTurnQuality(turn *TurnWrite) {
	if turn.Blocks == (CompensationBlocks{}) {
		turn.QualityStatus = "complete"
	} else {
		turn.QualityStatus = "partial"
	}
}

func cloneTurn(turn *TurnWrite) *TurnWrite {
	if turn == nil {
		return nil
	}
	copy := *turn
	copy.RawTurnID = cloneString(turn.RawTurnID)
	copy.StartedAtMS = cloneInt64(turn.StartedAtMS)
	copy.EndedAtMS = cloneInt64(turn.EndedAtMS)
	copy.EndOffset = cloneInt64(turn.EndOffset)
	copy.StartTotal = cloneUsage(turn.StartTotal)
	copy.LastTotal = cloneUsage(turn.LastTotal)
	copy.Accounted = *cloneUsage(&turn.Accounted)
	copy.SingleModel = cloneString(turn.SingleModel)
	copy.SingleReasoningEffort = cloneString(turn.SingleReasoningEffort)
	return &copy
}

func cloneUsage(value *sharedusage.NormalizedTokenUsage) *sharedusage.NormalizedTokenUsage {
	if value == nil {
		return nil
	}
	copy := *value
	copy.CacheWriteTokens = cloneInt64(value.CacheWriteTokens)
	return &copy
}

func cloneResponseEvidence(value *ResponseEvidence) *ResponseEvidence {
	if value == nil {
		return nil
	}
	copy := *value
	copy.Usage.Value = *cloneUsage(&value.Usage.Value)
	copy.ThreadTokenUsage.Value = *cloneUsage(&value.ThreadTokenUsage.Value)
	return &copy
}

func cloneCompactionEvidence(value *CompactionEvidence) *CompactionEvidence {
	if value == nil {
		return nil
	}
	copy := *value
	if value.Latest != nil {
		copy.Latest = cloneResponseEvidence(value.Latest)
	}
	return &copy
}

func optionalString(value *string) *string { return cloneString(value) }

func cloneString(value *string) *string {
	if value == nil {
		return nil
	}
	copy := *value
	return &copy
}

func stringPointer(value string) *string { return &value }

func int64Pointer(value int64) *int64 { return &value }
