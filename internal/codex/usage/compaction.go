package usage

import (
	"errors"
	"fmt"

	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

var errModernCounterArithmeticOverflow = errors.New("modern counter arithmetic overflow")

type compactionCounterEvidence struct {
	Owner      string
	Start      int64
	Domain     *ModernCounterDomain
	Usage      *sharedusage.NormalizedTokenUsage
	Known      bool
	ResponseID string
}

type modernWindowRelation struct {
	Present     bool
	Known       bool
	Total       sharedusage.NormalizedTokenUsage
	Boundary    int64
	WindowStart *int64
}

func (relation modernWindowRelation) currentForWindow(window *durableWindow) (sharedusage.NormalizedTokenUsage, bool, bool) {
	if !relation.Present {
		return sharedusage.NormalizedTokenUsage{}, false, false
	}
	if relation.WindowStart != nil {
		if window.Key.StartOffset != *relation.WindowStart {
			return sharedusage.NormalizedTokenUsage{}, false, false
		}
	} else if window.End != relation.Boundary {
		return sharedusage.NormalizedTokenUsage{}, false, false
	}
	if !relation.Known {
		return sharedusage.NormalizedTokenUsage{}, true, false
	}
	if window.State.CurrentTotal.State == UsageValueValid && usageCoverageEqual(relation.Total, window.State.CurrentTotal.Value) != 1 {
		return sharedusage.NormalizedTokenUsage{}, true, false
	}
	return relation.Total, true, true
}

func compactionCandidates(batches []ProcessBatch) []UsageCandidate {
	var candidates []UsageCandidate
	for _, batch := range batches {
		state := batch.SourceState
		for _, pending := range batch.Compactions {
			evidence := pending.Record.Compaction
			if evidence == nil || evidence.Latest == nil || evidence.Latest.Usage.State != UsageValueValid ||
				pending.Record.TimestampMS == nil || pending.Model == nil || stringsEmpty(*pending.Model) {
				continue
			}
			responseID, ok := compactionResponseID(*evidence)
			if !ok || responseID == "" {
				continue
			}
			latest := *evidence.Latest
			turnKey := (*string)(nil)
			if latest.TurnID != "" {
				turnKey = stringPointer(latest.TurnID)
			} else if state.ActiveTurnKey != nil {
				turnKey = cloneString(state.ActiveTurnKey)
			}
			candidates = append(candidates, UsageCandidate{
				SourceFileID: state.SourceFileID, Generation: state.Generation,
				EventID: ResponseEventID(state.OwningThreadID, responseID), EventKind: 0,
				EvidenceKind: EvidenceExplicit, Operation: OperationCompaction,
				OccurredAtMS: *pending.Record.TimestampMS, StartOffset: int64(pending.Record.StartOffset),
				EndOffset: int64(pending.Record.EndOffset), OwningThreadID: state.OwningThreadID,
				RootSessionID: state.RootSessionID, TurnKey: turnKey, Model: *pending.Model,
				ReasoningEffort: cloneString(pending.ReasoningEffort), Response: &latest, Usage: latest.Usage.Value,
			})
		}
	}
	return candidates
}

func stringsEmpty(value string) bool {
	return value == ""
}

func promoteCompactionBindings(
	batches []ProcessBatch,
	candidates []UsageCandidate,
	bindings map[ResponseKey]durableBinding,
	result *ReconcileResult,
) error {
	for _, batch := range batches {
		for _, pending := range batch.Compactions {
			evidence := pending.Record.Compaction
			if evidence == nil {
				continue
			}
			responseID, ok := compactionResponseID(*evidence)
			if !ok {
				setFatalAt(result, FatalCompactionIdentity, batch.SourceState, int64(pending.Record.StartOffset))
				return nil
			}
			if responseID == "" {
				continue
			}
			key := ResponseKey{OwningThreadID: batch.SourceState.OwningThreadID, ResponseID: responseID}
			if evidence.Latest != nil && evidence.Latest.Usage.State == UsageValueValid {
				if candidate, found := candidateForResponse(candidates, key); found &&
					!equalNormalizedUsage(candidate.Usage, evidence.Latest.Usage.Value) {
					setFatalAt(result, FatalCompactionIdentity, batch.SourceState, int64(pending.Record.StartOffset))
					return nil
				}
				if binding, found := bindings[key]; found &&
					!equalNormalizedUsage(binding.Event.Usage, evidence.Latest.Usage.Value) {
					setFatalAt(result, FatalCompactionIdentity, batch.SourceState, int64(pending.Record.StartOffset))
					return nil
				}
			}
			if index := candidateIndexForResponse(candidates, key); index >= 0 {
				candidates[index].Operation = OperationCompaction
			}
			if binding, found := bindings[key]; found {
				binding.Fact.Operation = OperationCompaction
				bindings[key] = binding
				if factIndex := factIndex(result.Facts, binding.Fact.EventID); factIndex >= 0 {
					result.Facts[factIndex].Operation = OperationCompaction
				} else {
					result.Facts = append(result.Facts, binding.Fact)
				}
			}
		}
	}
	return nil
}

func candidateForResponse(candidates []UsageCandidate, key ResponseKey) (UsageCandidate, bool) {
	index := candidateIndexForResponse(candidates, key)
	if index < 0 {
		return UsageCandidate{}, false
	}
	return candidates[index], true
}

func candidateIndexForResponse(candidates []UsageCandidate, key ResponseKey) int {
	for index := range candidates {
		candidate := &candidates[index]
		if candidate.EvidenceKind == EvidenceExplicit && candidate.Response != nil &&
			candidate.OwningThreadID == key.OwningThreadID && candidate.Response.ResponseID == key.ResponseID {
			return index
		}
	}
	return -1
}

func factIndex(facts []EventFactWrite, eventID string) int {
	for index := range facts {
		if facts[index].EventID == eventID {
			return index
		}
	}
	return -1
}

func ObserveCompaction(
	batch ProcessBatch,
	candidates []UsageCandidate,
	bindings map[ResponseKey]durableBinding,
	result *ReconcileResult,
) {
	if result.Fatal != nil {
		return
	}
	for _, pending := range batch.Compactions {
		evidence := pending.Record.Compaction
		if evidence == nil {
			continue
		}
		responseID, identityOK := compactionResponseID(*evidence)
		if !identityOK {
			setFatalAt(result, FatalCompactionIdentity, batch.SourceState, int64(pending.Record.StartOffset))
			return
		}
		marker := CompactionMarkerWrite{
			SourceFileID: batch.SourceState.SourceFileID, Generation: batch.SourceState.Generation,
			StartOffset: int64(pending.Record.StartOffset), EndOffset: int64(pending.Record.EndOffset),
			OwningThreadID: batch.SourceState.OwningThreadID, RootSessionID: batch.SourceState.RootSessionID,
			OccurredAtMS: cloneInt64(pending.Record.TimestampMS), Model: cloneString(pending.Model),
			ReasoningEffort: cloneString(pending.ReasoningEffort),
		}
		if responseID != "" {
			marker.ResponseID = stringPointer(responseID)
			key := ResponseKey{OwningThreadID: batch.SourceState.OwningThreadID, ResponseID: responseID}
			if index := candidateIndexForResponse(candidates, key); index >= 0 && candidates[index].Operation == OperationCompaction {
				marker.ResolvedEventID = stringPointer(candidates[index].EventID)
			} else if binding, found := bindings[key]; found && binding.Fact.Operation == OperationCompaction {
				marker.ResolvedEventID = stringPointer(binding.Fact.EventID)
			}
			if marker.ResolvedEventID == nil {
				unknown := compactionUnknownReason(*evidence, pending.Record.TimestampMS, pending.Model)
				marker.UnknownReason = &unknown
			}
		} else {
			unknown := MarkerIdentityMissing
			marker.UnknownReason = &unknown
		}
		result.MarkerUpserts = append(result.MarkerUpserts, marker)
		if marker.ResolvedEventID != nil {
			if index := factIndex(result.Facts, *marker.ResolvedEventID); index >= 0 {
				result.Facts[index].Operation = OperationCompaction
			}
		}
	}
}

func compactionResponseID(evidence CompactionEvidence) (string, bool) {
	outer := evidence.ResponseID
	var embedded string
	if evidence.Latest != nil {
		embedded = evidence.Latest.ResponseID
	}
	if outer != "" && embedded != "" && outer != embedded {
		return "", false
	}
	if outer != "" {
		return outer, true
	}
	return embedded, true
}

func compactionUnknownReason(evidence CompactionEvidence, timestamp *int64, model *string) MarkerUnknownReason {
	if evidence.Latest == nil {
		return MarkerUsageMissing
	}
	switch evidence.Latest.Usage.State {
	case UsageValueMissing:
		return MarkerUsageMissing
	case UsageValueInvalid:
		return MarkerUsageInvalid
	case UsageValueValid:
		if timestamp == nil {
			return MarkerTimeMissing
		}
		if model == nil || *model == "" {
			return MarkerModelUnresolved
		}
		return MarkerUsageMissing
	default:
		return MarkerUsageInvalid
	}
}

func compactionEvidenceForBatch(
	batch ProcessBatch,
	candidates []UsageCandidate,
	bindings map[ResponseKey]durableBinding,
) []compactionCounterEvidence {
	var output []compactionCounterEvidence
	for _, pending := range batch.Compactions {
		evidence := pending.Record.Compaction
		if evidence == nil {
			continue
		}
		responseID, ok := compactionResponseID(*evidence)
		if !ok {
			output = append(output, compactionCounterEvidence{Owner: batch.SourceState.OwningThreadID, Start: int64(pending.Record.StartOffset)})
			continue
		}
		value := compactionCounterEvidence{Owner: batch.SourceState.OwningThreadID, Start: int64(pending.Record.StartOffset), ResponseID: responseID}
		if evidence.Latest != nil {
			value.Domain = responseDomain(batch.SourceState.OwningThreadID, *evidence.Latest)
			if evidence.Latest.Usage.State == UsageValueValid {
				usage := evidence.Latest.Usage.Value
				value.Usage = &usage
				value.Known = true
			}
		}
		if responseID != "" {
			key := ResponseKey{OwningThreadID: batch.SourceState.OwningThreadID, ResponseID: responseID}
			if index := candidateIndexForResponse(candidates, key); index >= 0 && candidates[index].Operation == OperationCompaction {
				candidate := candidates[index]
				value.Usage = &candidate.Usage
				value.Domain = responseDomain(candidate.OwningThreadID, *candidate.Response)
				value.Known = true
			} else if binding, found := bindings[key]; found && binding.Fact.Operation == OperationCompaction {
				usage := binding.Event.Usage
				value.Usage = &usage
				value.Known = true
				if value.Domain == nil {
					value.Known = false
				}
			}
		}
		output = append(output, value)
	}
	return output
}

func responseDomain(owner string, response ResponseEvidence) *ModernCounterDomain {
	domain := ModernCounterDomain{ThreadID: owner}
	if response.ThreadID != "" {
		domain.ThreadID = response.ThreadID
	}
	if response.SessionID != "" {
		domain.SessionID = stringPointer(response.SessionID)
	}
	if domain.ThreadID == "" {
		return nil
	}
	return &domain
}

func projectModernCounter(
	modern sharedusage.NormalizedTokenUsage,
	modernDomain ModernCounterDomain,
	legacyDomain ModernCounterDomain,
	compactions []compactionCounterEvidence,
	comparableBoundary int64,
) (sharedusage.NormalizedTokenUsage, bool, error) {
	if !sameModernDomain(modernDomain, legacyDomain) {
		return sharedusage.NormalizedTokenUsage{}, false, nil
	}
	total := sharedusage.Zero()
	for _, marker := range compactions {
		if marker.Owner != modernDomain.ThreadID || marker.Start < comparableBoundary {
			continue
		}
		if !marker.Known || marker.Usage == nil || marker.Domain == nil || !sameModernDomain(*marker.Domain, modernDomain) {
			return sharedusage.NormalizedTokenUsage{}, false, nil
		}
		var err error
		total, err = total.CheckedAdd(*marker.Usage)
		if err != nil {
			return sharedusage.NormalizedTokenUsage{}, false,
				fmt.Errorf("sum compaction contribution: %w: %w", errModernCounterArithmeticOverflow, err)
		}
	}
	projected, err := modern.CheckedSub(total)
	if err != nil {
		return sharedusage.NormalizedTokenUsage{}, false, nil
	}
	return projected, true, nil
}

func modernDomain(candidate UsageCandidate) (ModernCounterDomain, bool) {
	if candidate.Response == nil || candidate.Response.ThreadTokenUsage.State != UsageValueValid {
		return ModernCounterDomain{}, false
	}
	domain := responseDomain(candidate.OwningThreadID, *candidate.Response)
	if domain == nil {
		return ModernCounterDomain{}, false
	}
	return *domain, true
}

func sameModernDomain(left, right ModernCounterDomain) bool {
	if left.ThreadID != right.ThreadID || (left.SessionID == nil) != (right.SessionID == nil) {
		return false
	}
	return left.SessionID == nil || *left.SessionID == *right.SessionID
}

func setFatalAt(result *ReconcileResult, code FatalConflictCode, state SourceState, offset int64) {
	if result.Fatal != nil {
		return
	}
	result.Fatal = &FatalConflict{
		Code: code, SourceFileID: state.SourceFileID, Generation: state.Generation,
		Offset: offset, ThreadID: state.OwningThreadID,
	}
}
