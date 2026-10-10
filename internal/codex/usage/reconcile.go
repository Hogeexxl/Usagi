package usage

import (
	"bytes"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"sort"
	"strconv"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

var (
	ErrInvalidLegacyReconciliationWindow            = errors.New("invalid legacy reconciliation window")
	ErrUnsupportedLegacyReconciliationWindowVersion = errors.New("unsupported legacy reconciliation window version")
	ErrInvalidReconciliationPatch                   = errors.New("invalid usage reconciliation patch")
)

type ResponseKey struct {
	OwningThreadID string
	ResponseID     string
}

type LegacyWindowChainState struct {
	Kind   string
	Reason *string
}

type LegacyReconciliationWindow struct {
	Version                  uint8
	PreviousTotal            UsageValue
	CurrentTotal             UsageValue
	LastUsage                UsageValue
	ExplicitResponseIDs      []string
	LegacyCoveredResponseIDs []string
	ProposalEventIDs         []string
	TurnAccountedBefore      sharedusage.NormalizedTokenUsage
	ChainState               LegacyWindowChainState
	Closed                   bool
}

type legacyWindowWire struct {
	Version                  uint8           `json:"version"`
	PreviousTotal            json.RawMessage `json:"previous_total"`
	CurrentTotal             json.RawMessage `json:"current_total"`
	LastUsage                json.RawMessage `json:"last_usage"`
	ExplicitResponseIDs      []string        `json:"explicit_response_ids"`
	LegacyCoveredResponseIDs []string        `json:"legacy_covered_response_ids"`
	ProposalEventIDs         []string        `json:"proposal_event_ids"`
	TurnAccountedBefore      json.RawMessage `json:"turn_accounted_before"`
	ChainState               json.RawMessage `json:"chain_state"`
	Closed                   bool            `json:"closed"`
}

func CanonicalLegacyReconciliationWindowJSON(window LegacyReconciliationWindow) ([]byte, error) {
	if err := validateLegacyReconciliationWindow(window); err != nil {
		return nil, err
	}
	window.ExplicitResponseIDs = sortedUniqueStrings(window.ExplicitResponseIDs)
	window.LegacyCoveredResponseIDs = sortedUniqueStrings(window.LegacyCoveredResponseIDs)
	window.ProposalEventIDs = sortedUniqueStrings(window.ProposalEventIDs)
	encoded := make([]byte, 0, 256)
	encoded = append(encoded, `{"version":`...)
	encoded = appendUint8(encoded, window.Version)
	encoded = append(encoded, `,"previous_total":`...)
	encoded = appendUsageValue(encoded, window.PreviousTotal)
	encoded = append(encoded, `,"current_total":`...)
	encoded = appendUsageValue(encoded, window.CurrentTotal)
	encoded = append(encoded, `,"last_usage":`...)
	encoded = appendUsageValue(encoded, window.LastUsage)
	encoded = append(encoded, `,"explicit_response_ids":`...)
	encoded = appendJSONStringList(encoded, window.ExplicitResponseIDs)
	encoded = append(encoded, `,"legacy_covered_response_ids":`...)
	encoded = appendJSONStringList(encoded, window.LegacyCoveredResponseIDs)
	encoded = append(encoded, `,"proposal_event_ids":`...)
	encoded = appendJSONStringList(encoded, window.ProposalEventIDs)
	encoded = append(encoded, `,"turn_accounted_before":`...)
	encoded = appendNormalizedUsage(encoded, window.TurnAccountedBefore)
	encoded = append(encoded, `,"chain_state":`...)
	encoded = appendLegacyChainState(encoded, window.ChainState)
	encoded = append(encoded, `,"closed":`...)
	if window.Closed {
		encoded = append(encoded, "true"...)
	} else {
		encoded = append(encoded, "false"...)
	}
	return append(encoded, '}'), nil
}

func DecodeLegacyReconciliationWindow(data []byte) (LegacyReconciliationWindow, error) {
	var wire legacyWindowWire
	if err := decodeStrictJSON(data, &wire); err != nil {
		return LegacyReconciliationWindow{}, ErrInvalidLegacyReconciliationWindow
	}
	previous, err := decodeLegacyUsageValue(wire.PreviousTotal)
	if err != nil {
		return LegacyReconciliationWindow{}, err
	}
	current, err := decodeLegacyUsageValue(wire.CurrentTotal)
	if err != nil {
		return LegacyReconciliationWindow{}, err
	}
	last, err := decodeLegacyUsageValue(wire.LastUsage)
	if err != nil {
		return LegacyReconciliationWindow{}, err
	}
	accounted, err := decodeWindowUsage(wire.TurnAccountedBefore)
	if err != nil {
		return LegacyReconciliationWindow{}, ErrInvalidLegacyReconciliationWindow
	}
	chain, err := decodeLegacyChainState(wire.ChainState)
	if err != nil {
		return LegacyReconciliationWindow{}, err
	}
	window := LegacyReconciliationWindow{
		Version: wire.Version, PreviousTotal: previous, CurrentTotal: current, LastUsage: last,
		ExplicitResponseIDs:      wire.ExplicitResponseIDs,
		LegacyCoveredResponseIDs: wire.LegacyCoveredResponseIDs,
		ProposalEventIDs:         wire.ProposalEventIDs,
		TurnAccountedBefore:      accounted, ChainState: chain, Closed: wire.Closed,
	}
	canonical, err := CanonicalLegacyReconciliationWindowJSON(window)
	if err != nil {
		return LegacyReconciliationWindow{}, err
	}
	if !bytes.Equal(canonical, data) {
		return LegacyReconciliationWindow{}, ErrInvalidLegacyReconciliationWindow
	}
	return window, nil
}

func decodeLegacyUsageValue(data []byte) (UsageValue, error) {
	value, err := decodeUsageValue(data)
	if err != nil || validateUsageValue(value) != nil {
		return UsageValue{}, ErrInvalidLegacyReconciliationWindow
	}
	return value, nil
}

func decodeWindowUsage(data []byte) (sharedusage.NormalizedTokenUsage, error) {
	usage, err := decodeNormalizedUsage(data)
	if err != nil {
		return sharedusage.NormalizedTokenUsage{}, ErrInvalidLegacyReconciliationWindow
	}
	return usage, nil
}

func decodeLegacyChainState(data []byte) (LegacyWindowChainState, error) {
	fields, err := decodeObject(data)
	if err != nil {
		return LegacyWindowChainState{}, ErrInvalidLegacyReconciliationWindow
	}
	var kind string
	if err := json.Unmarshal(fields["kind"], &kind); err != nil {
		return LegacyWindowChainState{}, ErrInvalidLegacyReconciliationWindow
	}
	state := LegacyWindowChainState{Kind: kind}
	switch kind {
	case "continuous":
		if !hasExactKeys(fields, "kind") {
			return LegacyWindowChainState{}, ErrInvalidLegacyReconciliationWindow
		}
	case "interrupted":
		if !hasExactKeys(fields, "kind", "reason") {
			return LegacyWindowChainState{}, ErrInvalidLegacyReconciliationWindow
		}
		var reason string
		if err := json.Unmarshal(fields["reason"], &reason); err != nil {
			return LegacyWindowChainState{}, ErrInvalidLegacyReconciliationWindow
		}
		state.Reason = &reason
	default:
		return LegacyWindowChainState{}, ErrInvalidLegacyReconciliationWindow
	}
	return state, nil
}

func decodeStrictJSON(data []byte, dst any) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(dst); err != nil {
		return err
	}
	var extra any
	if err := decoder.Decode(&extra); err != io.EOF {
		if err == nil {
			return ErrInvalidLegacyReconciliationWindow
		}
		return err
	}
	return nil
}

func validateLegacyReconciliationWindow(window LegacyReconciliationWindow) error {
	if window.Version != 1 {
		return ErrUnsupportedLegacyReconciliationWindowVersion
	}
	for _, value := range []UsageValue{window.PreviousTotal, window.CurrentTotal, window.LastUsage} {
		if err := validateUsageValue(value); err != nil {
			return ErrInvalidLegacyReconciliationWindow
		}
	}
	if err := window.TurnAccountedBefore.Validate(); err != nil {
		return ErrInvalidLegacyReconciliationWindow
	}
	for _, values := range [][]string{window.ExplicitResponseIDs, window.LegacyCoveredResponseIDs} {
		for _, value := range values {
			if !validCarryIdentity(value) {
				return ErrInvalidLegacyReconciliationWindow
			}
		}
	}
	for _, eventID := range window.ProposalEventIDs {
		if !validWindowEventID(eventID) {
			return ErrInvalidLegacyReconciliationWindow
		}
	}
	if window.ChainState.Kind == "continuous" {
		if window.ChainState.Reason != nil {
			return ErrInvalidLegacyReconciliationWindow
		}
	} else if window.ChainState.Kind == "interrupted" {
		if window.ChainState.Reason == nil || !validLegacyGapReason(*window.ChainState.Reason) {
			return ErrInvalidLegacyReconciliationWindow
		}
	} else {
		return ErrInvalidLegacyReconciliationWindow
	}
	return nil
}

func validLegacyGapReason(reason string) bool {
	switch reason {
	case "malformed", "oversized", "ownership", "parser", "required_invalid":
		return true
	default:
		return false
	}
}

func validWindowEventID(value string) bool {
	if len(value) != 64 {
		return false
	}
	for _, character := range value {
		if !(character >= '0' && character <= '9') && !(character >= 'a' && character <= 'f') {
			return false
		}
	}
	return true
}

func appendLegacyChainState(dst []byte, state LegacyWindowChainState) []byte {
	dst = append(dst, `{"kind":`...)
	dst = appendJSONString(dst, state.Kind)
	if state.Kind == "interrupted" {
		dst = append(dst, `,"reason":`...)
		dst = appendJSONString(dst, *state.Reason)
	}
	return append(dst, '}')
}

func appendJSONStringList(dst []byte, values []string) []byte {
	dst = append(dst, '[')
	for index, value := range values {
		if index > 0 {
			dst = append(dst, ',')
		}
		dst = appendJSONString(dst, value)
	}
	return append(dst, ']')
}

func appendUint8(dst []byte, value uint8) []byte {
	return strconv.AppendUint(dst, uint64(value), 10)
}

func sortedUniqueStrings(values []string) []string {
	result := append([]string(nil), values...)
	sort.Strings(result)
	return compactStrings(result)
}

type durableBinding struct {
	Fact     EventFactWrite
	Event    sharedusage.CanonicalUsageEventWrite
	HasEvent bool
}

type durableWindow struct {
	Key     PrivateRowKey
	End     int64
	Owner   string
	Root    string
	TurnKey *string
	State   LegacyReconciliationWindow
}

func Reconcile(reader storage.PrivateReader, epoch int64, batch ProcessBatch) (ReconcileResult, error) {
	results, err := ReconcileRoot(reader, epoch, []ProcessBatch{batch})
	if err != nil {
		return ReconcileResult{}, err
	}
	return results[0], nil
}

// ReconcileRoot stages one root's source batches against a shared canonical
// binding view. The first result contains the root-wide semantic patch; each
// later result carries only that source's checkpoint state. Callers commit all
// returned results in the same source WriteTx.
func ReconcileRoot(reader storage.PrivateReader, epoch int64, batches []ProcessBatch) ([]ReconcileResult, error) {
	if reader == nil || epoch <= 0 || len(batches) == 0 {
		return nil, fmt.Errorf("root reconciliation requires a private reader, positive epoch, and source batches")
	}
	results := make([]ReconcileResult, len(batches))
	rootID := batches[0].SourceState.RootSessionID
	for index, batch := range batches {
		if batch.SourceState.SourceFileID <= 0 || batch.SourceState.Generation <= 0 ||
			batch.SourceState.RootSessionID == "" || batch.SourceState.RootSessionID != rootID {
			return nil, fmt.Errorf("root reconciliation batches must have valid source identity and one root")
		}
		results[index].SourceState = batch.SourceState
		if batch.Fatal != nil && results[0].Fatal == nil {
			results[0].Fatal = batch.Fatal
		}
	}
	if results[0].Fatal != nil {
		return results, nil
	}
	var candidates []UsageCandidate
	sources := make(map[[2]int64]SourceState, len(batches))
	batchOrder := make(map[[2]int64]int, len(batches))
	for index, batch := range batches {
		key := [2]int64{batch.SourceState.SourceFileID, batch.SourceState.Generation}
		sources[key] = batch.SourceState
		batchOrder[key] = index
		candidates = append(candidates, batch.Candidates...)
		candidates = append(candidates, compactionCandidates([]ProcessBatch{batch})...)
	}
	sort.SliceStable(candidates, func(i, j int) bool {
		leftOrder := batchOrder[[2]int64{candidates[i].SourceFileID, candidates[i].Generation}]
		rightOrder := batchOrder[[2]int64{candidates[j].SourceFileID, candidates[j].Generation}]
		if leftOrder != rightOrder {
			return leftOrder < rightOrder
		}
		return candidates[i].StartOffset < candidates[j].StartOffset
	})
	for _, candidate := range candidates {
		if candidate.RootSessionID != rootID {
			setFatalAt(&results[0], FatalResponseOwnership, batches[0].SourceState, candidate.StartOffset)
			return results, nil
		}
		state, ok := sources[[2]int64{candidate.SourceFileID, candidate.Generation}]
		if !ok {
			return nil, fmt.Errorf("candidate source is outside root reconciliation batch")
		}
		if candidate.OwningThreadID != state.OwningThreadID {
			setFatalAt(&results[0], FatalResponseOwnership, state, candidate.StartOffset)
			return results, nil
		}
	}
	stagedCandidates := append([]UsageCandidate(nil), candidates...)
	shared := ReconcileResult{SourceState: batches[0].SourceState}
	candidates = dedupeCandidates(candidates, &shared)
	if shared.Fatal != nil {
		results[0].Fatal = shared.Fatal
		return results, nil
	}
	durable, err := loadDurableBindings(reader, epoch, batches, candidates, &shared)
	if err != nil {
		return nil, err
	}
	if shared.Fatal != nil {
		results[0].Fatal = shared.Fatal
		return results, nil
	}
	windowsBySource := make([][]durableWindow, len(batches))
	for index, batch := range batches {
		windowsBySource[index], err = loadRelevantWindows(reader, epoch, batch, stagedCandidates)
		if err != nil {
			return nil, err
		}
		if err := loadWindowBindings(reader, epoch, windowsBySource[index], durable); err != nil {
			return nil, err
		}
	}
	windowAssignments := make(map[ResponseKey]PrivateRowKey)
	for _, windows := range windowsBySource {
		for _, window := range windows {
			for _, responseID := range window.State.LegacyCoveredResponseIDs {
				key := ResponseKey{OwningThreadID: window.Owner, ResponseID: responseID}
				if previous, exists := windowAssignments[key]; exists && previous != window.Key {
					setFatalAt(&shared, FatalLegacyCoverage, SourceState{
						SourceFileID: window.Key.SourceFileID, Generation: window.Key.Generation, OwningThreadID: window.Owner,
					}, window.Key.StartOffset)
				} else {
					windowAssignments[key] = window.Key
				}
			}
		}
	}
	if shared.Fatal != nil {
		results[0].Fatal = shared.Fatal
		return results, nil
	}
	if err := promoteCompactionBindings(batches, candidates, durable, &shared); err != nil {
		return nil, err
	}
	if shared.Fatal != nil {
		results[0].Fatal = shared.Fatal
		return results, nil
	}
	for index := range stagedCandidates {
		candidate := &stagedCandidates[index]
		if candidate.Response == nil || candidate.EvidenceKind != EvidenceExplicit {
			continue
		}
		if canonical, found := candidateForResponse(candidates, ResponseKey{
			OwningThreadID: candidate.OwningThreadID, ResponseID: candidate.Response.ResponseID,
		}); found && canonical.Operation == OperationCompaction {
			candidate.Operation = OperationCompaction
		}
	}
	if err := reconcileBindings(reader, epoch, candidates, durable, &shared); err != nil {
		return nil, err
	}
	if shared.Fatal != nil {
		results[0].Fatal = shared.Fatal
		return results, nil
	}
	rootPatch := shared
	rootPatch.SourceState = batches[0].SourceState
	rootPatch.Occurrences = nil
	rootPatch.MarkerUpserts = nil
	rootPatch.MarkerDeletes = nil
	rootPatch.WindowUpserts = nil
	rootPatch.WindowDeletes = nil
	rootPatch.HoldUpserts = nil
	rootPatch.HoldDeletes = nil
	rootPatch.TurnUpserts = nil
	modernRelations := make([]modernWindowRelation, len(batches))
	for index, batch := range batches {
		modernRelations[index], err = projectModernState(batch, stagedCandidates, durable)
		if err != nil {
			if errors.Is(err, errModernCounterArithmeticOverflow) {
				setFatalAt(&results[0], FatalArithmeticOverflow, batch.SourceState, batch.LogicalSafeOffset)
				return results, nil
			}
			return nil, err
		}
	}
	for index, batch := range batches {
		local := ReconcileResult{SourceState: batch.SourceState}
		local.Occurrences = dedupeOccurrences(batch.Occurrences, candidatesForSource(stagedCandidates, batch.SourceState))
		if err := reconcileLegacyWindows(reader, epoch, batch, stagedCandidates, durable, windowsBySource[index], modernRelations[index], windowAssignments, &local); err != nil {
			return nil, err
		}
		if local.Fatal != nil {
			results[0].Fatal = local.Fatal
			return results, nil
		}
		ObserveCompaction(batch, candidates, durable, &local)
		if local.Fatal != nil {
			results[0].Fatal = local.Fatal
			return results, nil
		}
		if err := reconcileTurns(reader, epoch, batch, stagedCandidates, &local); err != nil {
			return nil, err
		}
		if local.Fatal != nil {
			results[0].Fatal = local.Fatal
			return results, nil
		}
		rootPatch.Occurrences = append(rootPatch.Occurrences, local.Occurrences...)
		rootPatch.Events = append(rootPatch.Events, local.Events...)
		rootPatch.Facts = append(rootPatch.Facts, local.Facts...)
		rootPatch.DeleteEventIDs = append(rootPatch.DeleteEventIDs, local.DeleteEventIDs...)
		rootPatch.MarkerUpserts = append(rootPatch.MarkerUpserts, local.MarkerUpserts...)
		rootPatch.MarkerDeletes = append(rootPatch.MarkerDeletes, local.MarkerDeletes...)
		rootPatch.WindowUpserts = append(rootPatch.WindowUpserts, local.WindowUpserts...)
		rootPatch.WindowDeletes = append(rootPatch.WindowDeletes, local.WindowDeletes...)
		rootPatch.HoldUpserts = append(rootPatch.HoldUpserts, local.HoldUpserts...)
		rootPatch.HoldDeletes = append(rootPatch.HoldDeletes, local.HoldDeletes...)
		rootPatch.TurnUpserts = append(rootPatch.TurnUpserts, local.TurnUpserts...)
		results[index].SourceState = local.SourceState
	}
	rootPatch.DeleteEventIDs = sortedUniqueStrings(rootPatch.DeleteEventIDs)
	deleted := stringSet(rootPatch.DeleteEventIDs)
	rootPatch.Events = removeDeletedEvents(rootPatch.Events, deleted)
	rootPatch.Facts = removeDeletedFacts(rootPatch.Facts, deleted)
	rootPatch.Occurrences = removeDeletedOccurrences(rootPatch.Occurrences, deleted)
	rootPatch.Occurrences = dedupeOccurrenceWrites(rootPatch.Occurrences)
	rootPatch.WindowUpserts = dedupeWindowWrites(rootPatch.WindowUpserts)
	rootPatch.MarkerUpserts = dedupeMarkerWrites(rootPatch.MarkerUpserts)
	rootPatch.TurnUpserts = dedupeTurnWrites(rootPatch.TurnUpserts)
	rootPatch.Events = dedupeCanonicalEvents(rootPatch.Events)
	rootPatch.Facts = dedupeFactWrites(rootPatch.Facts)
	results[0] = rootPatch
	for index := 1; index < len(results); index++ {
		results[index] = ReconcileResult{SourceState: results[index].SourceState}
	}
	return results, nil
}

func removeDeletedEvents(events []sharedusage.CanonicalUsageEventWrite, deleted map[string]bool) []sharedusage.CanonicalUsageEventWrite {
	kept := events[:0]
	for _, event := range events {
		if !deleted[event.EventID] {
			kept = append(kept, event)
		}
	}
	return kept
}

func removeDeletedFacts(facts []EventFactWrite, deleted map[string]bool) []EventFactWrite {
	kept := facts[:0]
	for _, fact := range facts {
		if !deleted[fact.EventID] {
			kept = append(kept, fact)
		}
	}
	return kept
}

func removeDeletedOccurrences(occurrences []OccurrenceWrite, deleted map[string]bool) []OccurrenceWrite {
	kept := occurrences[:0]
	for _, occurrence := range occurrences {
		if !deleted[occurrence.EventID] {
			kept = append(kept, occurrence)
		}
	}
	return kept
}

func dedupeCandidates(candidates []UsageCandidate, result *ReconcileResult) []UsageCandidate {
	byEvent := make(map[string]int, len(candidates))
	byResponse := make(map[ResponseKey]int)
	rootOwners := make(map[string]string)
	unique := make([]UsageCandidate, 0, len(candidates))
	for _, candidate := range candidates {
		if candidate.SourceFileID <= 0 || candidate.Generation <= 0 || candidate.StartOffset < 0 ||
			candidate.EndOffset <= candidate.StartOffset || candidate.OwningThreadID == "" || candidate.RootSessionID == "" {
			setFatalAt(result, FatalLegacyCoverage, SourceState{
				SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
				OwningThreadID: candidate.OwningThreadID,
			}, candidate.StartOffset)
			return nil
		}
		if candidate.EvidenceKind == EvidenceExplicit {
			if candidate.Response == nil || candidate.Response.ResponseID == "" {
				setFatalAt(result, FatalResponseOwnership, SourceState{
					SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
					OwningThreadID: candidate.OwningThreadID,
				}, candidate.StartOffset)
				return nil
			}
			if candidate.Response.ThreadID != "" && candidate.Response.ThreadID != candidate.OwningThreadID {
				setFatalAt(result, FatalResponseOwnership, SourceState{
					SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
					OwningThreadID: candidate.OwningThreadID,
				}, candidate.StartOffset)
				return nil
			}
			rootKey := candidate.RootSessionID + "\x00" + candidate.Response.ResponseID
			if owner, exists := rootOwners[rootKey]; exists && owner != candidate.OwningThreadID {
				setFatalAt(result, FatalResponseOwnership, SourceState{
					SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
					OwningThreadID: candidate.OwningThreadID,
				}, candidate.StartOffset)
				return nil
			}
			rootOwners[rootKey] = candidate.OwningThreadID
			key := ResponseKey{OwningThreadID: candidate.OwningThreadID, ResponseID: candidate.Response.ResponseID}
			if index, exists := byResponse[key]; exists {
				prior := unique[index]
				if prior.RootSessionID != candidate.RootSessionID {
					setFatalAt(result, FatalResponseOwnership, SourceState{
						SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
						OwningThreadID: candidate.OwningThreadID,
					}, candidate.StartOffset)
					return nil
				}
				if prior.EventID != candidate.EventID {
					setFatalAt(result, FatalResponseOwnership, SourceState{
						SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
						OwningThreadID: candidate.OwningThreadID,
					}, candidate.StartOffset)
					return nil
				}
				if !equalNormalizedUsage(prior.Usage, candidate.Usage) {
					code := FatalResponseUsage
					if prior.Operation == OperationCompaction || candidate.Operation == OperationCompaction {
						code = FatalCompactionIdentity
					}
					setFatalAt(result, code, SourceState{
						SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
						OwningThreadID: candidate.OwningThreadID,
					}, candidate.StartOffset)
					return nil
				}
				if candidate.Operation == OperationCompaction {
					unique[index].Operation = OperationCompaction
				}
				continue
			}
			byResponse[key] = len(unique)
		}
		if index, exists := byEvent[candidate.EventID]; exists {
			prior := unique[index]
			if !sameCanonicalEvent(candidateEvent(prior), candidateEvent(candidate)) {
				code := FatalLegacyCoverage
				if prior.EvidenceKind == EvidenceExplicit || candidate.EvidenceKind == EvidenceExplicit {
					code = FatalResponseUsage
				}
				setFatalAt(result, code, SourceState{
					SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
					OwningThreadID: candidate.OwningThreadID,
				}, candidate.StartOffset)
				return nil
			}
			if candidate.Operation == OperationCompaction {
				unique[index].Operation = OperationCompaction
			}
			continue
		}
		byEvent[candidate.EventID] = len(unique)
		unique = append(unique, candidate)
	}
	return unique
}

func candidateEvent(candidate UsageCandidate) sharedusage.CanonicalUsageEventWrite {
	kind := sharedusage.EventKindNormal
	switch candidate.EventKind {
	case 1:
		kind = sharedusage.EventKindRecovered
	case 2:
		kind = sharedusage.EventKindTurnCompensation
	}
	return sharedusage.CanonicalUsageEventWrite{
		EventID: candidate.EventID, Kind: kind, OccurredAtMS: candidate.OccurredAtMS,
		ThreadID: candidate.OwningThreadID, RootSessionID: candidate.RootSessionID,
		TurnKey: cloneString(candidate.TurnKey), Model: candidate.Model,
		ReasoningEffort: cloneString(candidate.ReasoningEffort), Usage: candidate.Usage,
	}
}

func sameCanonicalEvent(left, right sharedusage.CanonicalUsageEventWrite) bool {
	return left.EventID == right.EventID && left.Kind == right.Kind && left.OccurredAtMS == right.OccurredAtMS &&
		left.ThreadID == right.ThreadID && left.RootSessionID == right.RootSessionID &&
		equalStringPointer(left.TurnKey, right.TurnKey) && left.Model == right.Model &&
		equalStringPointer(left.ReasoningEffort, right.ReasoningEffort) && equalNormalizedUsage(left.Usage, right.Usage)
}

func equalStringPointer(left, right *string) bool {
	return left == nil && right == nil || left != nil && right != nil && *left == *right
}

func dedupeOccurrences(input []OccurrenceWrite, candidates []UsageCandidate) []OccurrenceWrite {
	occurrences := input
	if len(occurrences) == 0 {
		for _, candidate := range candidates {
			occurrences = append(occurrences, OccurrenceWrite{
				SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
				StartOffset: candidate.StartOffset, EndOffset: candidate.EndOffset, EventID: candidate.EventID,
			})
		}
	}
	indices := make(map[string]int, len(occurrences))
	result := make([]OccurrenceWrite, 0, len(occurrences))
	for _, occurrence := range occurrences {
		key := fmt.Sprintf("%d/%d/%d", occurrence.SourceFileID, occurrence.Generation, occurrence.StartOffset)
		if index, exists := indices[key]; exists {
			result[index] = occurrence
			continue
		}
		indices[key] = len(result)
		result = append(result, occurrence)
	}
	return result
}

func loadDurableBindings(reader storage.PrivateReader, epoch int64, batches []ProcessBatch, candidates []UsageCandidate, result *ReconcileResult) (map[ResponseKey]durableBinding, error) {
	rootID := batches[0].SourceState.RootSessionID
	responseIDs := make(map[string]struct{})
	for _, candidate := range candidates {
		if candidate.Response != nil && candidate.EvidenceKind == EvidenceExplicit {
			responseIDs[candidate.Response.ResponseID] = struct{}{}
		}
	}
	for _, batch := range batches {
		for _, pending := range batch.Compactions {
			if pending.Record.Compaction == nil {
				continue
			}
			if id, ok := compactionResponseID(*pending.Record.Compaction); ok && id != "" {
				responseIDs[id] = struct{}{}
			}
		}
	}
	ids := make([]string, 0, len(responseIDs))
	for id := range responseIDs {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	bindings := make(map[ResponseKey]durableBinding)
	for _, responseID := range ids {
		rows, err := reader.Query(`
			SELECT f.event_id,f.owning_thread_id,f.response_id,f.evidence_kind,f.operation,
			       e.event_kind,e.occurred_at_ms,e.thread_id,e.root_session_id,e.turn_key,e.model,e.reasoning_effort,
			       e.input_tokens,e.cached_tokens,e.cache_write_tokens,e.output_tokens,e.reasoning_tokens,e.total_tokens
			FROM codex_usage_event_facts f
		JOIN usage_events e ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
			WHERE f.source='codex' AND f.ledger_epoch=? AND f.evidence_kind='explicit' AND f.response_id=? AND e.root_session_id=?
			ORDER BY f.owning_thread_id,f.event_id`, epoch, responseID, rootID)
		if err != nil {
			return nil, err
		}
		for rows.Next() {
			binding, err := scanDurableBinding(rows)
			if err != nil {
				_ = rows.Close()
				return nil, err
			}
			key := ResponseKey{OwningThreadID: binding.Fact.OwningThreadID, ResponseID: responseID}
			if old, exists := bindings[key]; exists {
				code := FatalResponseUsage
				if old.Event.RootSessionID != binding.Event.RootSessionID {
					code = FatalResponseOwnership
				}
				setFatalAt(result, code, batches[0].SourceState, firstCandidateOffset(candidates, key))
			} else {
				bindings[key] = binding
			}
		}
		if err := rows.Err(); err != nil {
			_ = rows.Close()
			return nil, err
		}
		if err := rows.Close(); err != nil {
			return nil, err
		}
	}
	for _, candidate := range candidates {
		if candidate.Response == nil || candidate.EvidenceKind != EvidenceExplicit {
			continue
		}
		for key, binding := range bindings {
			if key.ResponseID == candidate.Response.ResponseID && binding.Event.RootSessionID == candidate.RootSessionID &&
				key.OwningThreadID != candidate.OwningThreadID {
				setFatalAt(result, FatalResponseOwnership, batches[0].SourceState, candidate.StartOffset)
			}
		}
	}
	return bindings, nil
}

type rowScanner interface {
	Scan(dest ...any) error
}

func scanDurableBinding(row rowScanner) (durableBinding, error) {
	var binding durableBinding
	var responseID, evidenceKind, operation, eventKind sql.NullString
	var turnKey, reasoningEffort sql.NullString
	var cacheWrite sql.NullInt64
	err := row.Scan(
		&binding.Fact.EventID, &binding.Fact.OwningThreadID, &responseID, &evidenceKind, &operation,
		&eventKind, &binding.Event.OccurredAtMS, &binding.Event.ThreadID, &binding.Event.RootSessionID,
		&turnKey, &binding.Event.Model, &reasoningEffort,
		&binding.Event.Usage.InputTokens, &binding.Event.Usage.CachedTokens, &cacheWrite,
		&binding.Event.Usage.OutputTokens, &binding.Event.Usage.ReasoningTokens, &binding.Event.Usage.TotalTokens,
	)
	if err != nil {
		return durableBinding{}, err
	}
	binding.Fact.ResponseID = nullableStringPointer(responseID)
	binding.Fact.EvidenceKind = EvidenceExplicit
	if evidenceKind.String == "legacy" {
		binding.Fact.EvidenceKind = EvidenceLegacy
	}
	if operation.String == "compaction" {
		binding.Fact.Operation = OperationCompaction
	} else {
		binding.Fact.Operation = OperationResponse
	}
	binding.Event.EventID = binding.Fact.EventID
	binding.Event.Kind = sharedusage.EventKind(eventKind.String)
	binding.Event.TurnKey = nullableStringPointer(turnKey)
	binding.Event.ReasoningEffort = nullableStringPointer(reasoningEffort)
	binding.Event.Usage.CacheWriteTokens = nullableIntPointer(cacheWrite)
	binding.HasEvent = true
	if err := binding.Event.Validate(); err != nil {
		return durableBinding{}, fmt.Errorf("invalid durable canonical usage: %w", err)
	}
	return binding, nil
}

func firstCandidateOffset(candidates []UsageCandidate, key ResponseKey) int64 {
	for _, candidate := range candidates {
		if candidate.Response != nil && candidate.OwningThreadID == key.OwningThreadID && candidate.Response.ResponseID == key.ResponseID {
			return candidate.StartOffset
		}
	}
	return 0
}

func reconcileBindings(reader storage.PrivateReader, epoch int64, candidates []UsageCandidate, durable map[ResponseKey]durableBinding, result *ReconcileResult) error {
	eventIndexes := make(map[string]int)
	factIndexes := make(map[string]int)
	for _, candidate := range candidates {
		event := candidateEvent(candidate)
		if candidate.Usage.Validate() != nil {
			setFatalAt(result, candidateFatalCode(candidate), SourceState{
				SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
				OwningThreadID: candidate.OwningThreadID,
			}, candidate.StartOffset)
			continue
		}
		if candidate.EvidenceKind == EvidenceExplicit {
			key := ResponseKey{OwningThreadID: candidate.OwningThreadID, ResponseID: candidate.Response.ResponseID}
			if existing, exists := durable[key]; exists {
				if existing.Event.RootSessionID != candidate.RootSessionID || existing.Fact.ResponseID == nil ||
					*existing.Fact.ResponseID != key.ResponseID || existing.Fact.OwningThreadID != key.OwningThreadID ||
					existing.Event.EventID != candidate.EventID {
					setFatalAt(result, FatalResponseOwnership, SourceState{
						SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
						OwningThreadID: candidate.OwningThreadID,
					}, candidate.StartOffset)
					continue
				}
				if !equalNormalizedUsage(existing.Event.Usage, event.Usage) {
					setFatalAt(result, FatalResponseUsage, SourceState{
						SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
						OwningThreadID: candidate.OwningThreadID,
					}, candidate.StartOffset)
					continue
				}
				// The first durable binding owns canonical attribution and time.
				event = existing.Event
			}
		}
		if event.OccurredAtMS < 0 || event.Model == "" {
			setFatalAt(result, candidateFatalCode(candidate), SourceState{
				SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
				OwningThreadID: candidate.OwningThreadID,
			}, candidate.StartOffset)
			continue
		}
		if index, exists := eventIndexes[event.EventID]; exists {
			if !sameCanonicalEvent(result.Events[index], event) {
				setFatalAt(result, candidateFatalCode(candidate), SourceState{
					SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
					OwningThreadID: candidate.OwningThreadID,
				}, candidate.StartOffset)
			}
		} else {
			eventIndexes[event.EventID] = len(result.Events)
			result.Events = append(result.Events, event)
		}
		fact := EventFactWrite{
			EventID: event.EventID, OwningThreadID: candidate.OwningThreadID,
			EvidenceKind: candidate.EvidenceKind, Operation: candidate.Operation,
		}
		if candidate.EvidenceKind == EvidenceExplicit {
			fact.ResponseID = stringPointer(candidate.Response.ResponseID)
		}
		if candidate.EvidenceKind == EvidenceExplicit {
			key := ResponseKey{OwningThreadID: candidate.OwningThreadID, ResponseID: candidate.Response.ResponseID}
			if existing, ok := durable[key]; ok && existing.Fact.Operation == OperationCompaction {
				fact.Operation = OperationCompaction
			}
		}
		if old, found, err := loadDurableFactByEventID(reader, epoch, event.EventID); err != nil {
			return err
		} else if found {
			if old.Fact.OwningThreadID != fact.OwningThreadID || old.Fact.EvidenceKind != fact.EvidenceKind ||
				!equalStringPointer(old.Fact.ResponseID, fact.ResponseID) {
				setFatalAt(result, candidateFatalCode(candidate), SourceState{
					SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
					OwningThreadID: candidate.OwningThreadID,
				}, candidate.StartOffset)
				continue
			}
			if old.Fact.Operation == OperationCompaction {
				fact.Operation = OperationCompaction
			}
		}
		if index, exists := factIndexes[fact.EventID]; exists {
			if result.Facts[index].OwningThreadID != fact.OwningThreadID || result.Facts[index].EvidenceKind != fact.EvidenceKind ||
				!equalStringPointer(result.Facts[index].ResponseID, fact.ResponseID) {
				setFatalAt(result, candidateFatalCode(candidate), SourceState{
					SourceFileID: candidate.SourceFileID, Generation: candidate.Generation,
					OwningThreadID: candidate.OwningThreadID,
				}, candidate.StartOffset)
			} else if fact.Operation == OperationCompaction {
				result.Facts[index].Operation = OperationCompaction
			}
		} else {
			factIndexes[fact.EventID] = len(result.Facts)
			result.Facts = append(result.Facts, fact)
		}
	}
	return nil
}

func candidateFatalCode(candidate UsageCandidate) FatalConflictCode {
	if candidate.EvidenceKind == EvidenceExplicit {
		return FatalResponseUsage
	}
	return FatalLegacyCoverage
}

func loadDurableFactByEventID(reader storage.PrivateReader, epoch int64, eventID string) (durableBinding, bool, error) {
	row := reader.QueryRow(`
		SELECT f.event_id,f.owning_thread_id,f.response_id,f.evidence_kind,f.operation,
		       e.event_kind,e.occurred_at_ms,e.thread_id,e.root_session_id,e.turn_key,e.model,e.reasoning_effort,
		       e.input_tokens,e.cached_tokens,e.cache_write_tokens,e.output_tokens,e.reasoning_tokens,e.total_tokens
	FROM codex_usage_event_facts f JOIN usage_events e
	  ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
	WHERE f.source='codex' AND f.ledger_epoch=? AND f.event_id=?`, epoch, eventID)
	binding, err := scanDurableBinding(row)
	if errors.Is(err, sql.ErrNoRows) {
		return durableBinding{}, false, nil
	}
	if err != nil {
		return durableBinding{}, false, err
	}
	return binding, true, nil
}

func loadRelevantWindows(reader storage.PrivateReader, epoch int64, batch ProcessBatch, candidates []UsageCandidate) ([]durableWindow, error) {
	needsWindows := false
	turnKeys := make(map[string]struct{})
	includeNilTurn := false
	for _, turn := range batch.TurnUpserts {
		if turn.SourceFileID == batch.SourceState.SourceFileID && turn.Generation == batch.SourceState.Generation &&
			turn.ThreadID == batch.SourceState.OwningThreadID {
			turnKeys[turn.TurnKey] = struct{}{}
			needsWindows = true
		}
	}
	for _, candidate := range candidates {
		if candidate.EvidenceKind == EvidenceLegacy {
			needsWindows = true
		}
		if candidate.TurnKey == nil {
			includeNilTurn = true
		} else {
			turnKeys[*candidate.TurnKey] = struct{}{}
		}
	}
	if !needsWindows && len(turnKeys) == 0 && !includeNilTurn {
		return nil, nil
	}
	query := `SELECT source_file_id,file_generation,source_start_offset,source_end_offset,owning_thread_id,turn_key,state_json
		FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=? AND owning_thread_id=?`
	args := []any{epoch, batch.SourceState.SourceFileID, batch.SourceState.Generation, batch.SourceState.OwningThreadID}
	if len(turnKeys) > 0 || includeNilTurn {
		query += " AND ("
		parts := make([]string, 0, len(turnKeys)+1)
		if includeNilTurn {
			parts = append(parts, "turn_key IS NULL")
		}
		keys := make([]string, 0, len(turnKeys))
		for key := range turnKeys {
			keys = append(keys, key)
		}
		sort.Strings(keys)
		if len(keys) > 0 {
			placeholders := strings.TrimSuffix(strings.Repeat("?,", len(keys)), ",")
			parts = append(parts, "turn_key IN ("+placeholders+")")
			for _, key := range keys {
				args = append(args, key)
			}
		}
		query += strings.Join(parts, " OR ") + ")"
	}
	query += " ORDER BY source_start_offset"
	rows, err := reader.Query(query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var windows []durableWindow
	for rows.Next() {
		var window durableWindow
		var rawTurn, rawJSON sql.NullString
		if err := rows.Scan(&window.Key.SourceFileID, &window.Key.Generation, &window.Key.StartOffset, &window.End,
			&window.Owner, &rawTurn, &rawJSON); err != nil {
			return nil, err
		}
		if rawTurn.Valid {
			window.TurnKey = stringPointer(rawTurn.String)
		}
		if !rawJSON.Valid {
			return nil, ErrInvalidLegacyReconciliationWindow
		}
		window.State, err = DecodeLegacyReconciliationWindow([]byte(rawJSON.String))
		if err != nil {
			return nil, fmt.Errorf("decode durable reconciliation window: %w", err)
		}
		window.Root = batch.SourceState.RootSessionID
		if window.Key.SourceFileID != batch.SourceState.SourceFileID || window.Key.Generation != batch.SourceState.Generation ||
			window.Owner != batch.SourceState.OwningThreadID {
			return nil, ErrInvalidLegacyReconciliationWindow
		}
		if err := validateDurableWindowReferences(reader, epoch, window); err != nil {
			return nil, err
		}
		windows = append(windows, window)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	return windows, rows.Close()
}

func candidatesForSource(candidates []UsageCandidate, state SourceState) []UsageCandidate {
	result := make([]UsageCandidate, 0, len(candidates))
	for _, candidate := range candidates {
		if candidate.SourceFileID == state.SourceFileID && candidate.Generation == state.Generation {
			result = append(result, candidate)
		}
	}
	return result
}

func dedupeOccurrenceWrites(values []OccurrenceWrite) []OccurrenceWrite {
	seen := make(map[PrivateRowKey]int, len(values))
	result := make([]OccurrenceWrite, 0, len(values))
	for _, occurrence := range values {
		key := PrivateRowKey{SourceFileID: occurrence.SourceFileID, Generation: occurrence.Generation, StartOffset: occurrence.StartOffset}
		if index, ok := seen[key]; ok {
			if result[index] != occurrence {
				result[index] = occurrence
			}
			continue
		}
		seen[key] = len(result)
		result = append(result, occurrence)
	}
	return result
}

func dedupeWindowWrites(values []WindowWrite) []WindowWrite {
	seen := make(map[PrivateRowKey]int, len(values))
	result := make([]WindowWrite, 0, len(values))
	for _, window := range values {
		key := PrivateRowKey{SourceFileID: window.SourceFileID, Generation: window.Generation, StartOffset: window.StartOffset}
		if index, ok := seen[key]; ok {
			result[index] = window
			continue
		}
		seen[key] = len(result)
		result = append(result, window)
	}
	return result
}

func dedupeMarkerWrites(values []CompactionMarkerWrite) []CompactionMarkerWrite {
	seen := make(map[PrivateRowKey]int, len(values))
	result := make([]CompactionMarkerWrite, 0, len(values))
	for _, marker := range values {
		key := PrivateRowKey{SourceFileID: marker.SourceFileID, Generation: marker.Generation, StartOffset: marker.StartOffset}
		if index, ok := seen[key]; ok {
			result[index] = marker
			continue
		}
		seen[key] = len(result)
		result = append(result, marker)
	}
	return result
}

func dedupeTurnWrites(values []TurnWrite) []TurnWrite {
	indices := make(map[string]int, len(values))
	result := make([]TurnWrite, 0, len(values))
	for _, turn := range values {
		key := fmt.Sprintf("%d/%d/%s", turn.SourceFileID, turn.Generation, turn.TurnKey)
		if index, ok := indices[key]; ok {
			result[index] = turn
			continue
		}
		indices[key] = len(result)
		result = append(result, turn)
	}
	return result
}

func dedupeCanonicalEvents(values []sharedusage.CanonicalUsageEventWrite) []sharedusage.CanonicalUsageEventWrite {
	indices := make(map[string]int, len(values))
	result := make([]sharedusage.CanonicalUsageEventWrite, 0, len(values))
	for _, event := range values {
		if _, ok := indices[event.EventID]; ok {
			continue
		}
		indices[event.EventID] = len(result)
		result = append(result, event)
	}
	return result
}

func dedupeFactWrites(values []EventFactWrite) []EventFactWrite {
	indices := make(map[string]int, len(values))
	result := make([]EventFactWrite, 0, len(values))
	for _, fact := range values {
		if index, ok := indices[fact.EventID]; ok {
			if fact.Operation == OperationCompaction {
				result[index].Operation = OperationCompaction
			}
			continue
		}
		indices[fact.EventID] = len(result)
		result = append(result, fact)
	}
	return result
}

func validateDurableWindowReferences(reader storage.PrivateReader, epoch int64, window durableWindow) error {
	for _, responseID := range append(append([]string(nil), window.State.ExplicitResponseIDs...), window.State.LegacyCoveredResponseIDs...) {
		var count int
		if err := reader.QueryRow(`SELECT count(*) FROM codex_usage_event_facts f
			JOIN usage_events e ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
			JOIN codex_usage_event_occurrences o ON o.source=f.source AND o.ledger_epoch=f.ledger_epoch AND o.event_id=f.event_id
			WHERE f.source='codex' AND f.ledger_epoch=? AND f.owning_thread_id=? AND f.response_id=?
			  AND f.evidence_kind='explicit' AND e.root_session_id=?
			  AND o.source_file_id=? AND o.file_generation=?`,
			epoch, window.Owner, responseID, window.Root, window.Key.SourceFileID, window.Key.Generation).Scan(&count); err != nil {
			return err
		}
		if count == 0 {
			return fmt.Errorf("durable reconciliation window references missing explicit response %q", responseID)
		}
	}
	for _, eventID := range window.State.ProposalEventIDs {
		var count int
		if err := reader.QueryRow(`SELECT count(*) FROM usage_events e
			JOIN codex_usage_event_facts f ON f.source=e.source AND f.ledger_epoch=e.source_epoch AND f.event_id=e.event_id
			JOIN codex_usage_event_occurrences o ON o.source=e.source AND o.ledger_epoch=e.source_epoch AND o.event_id=e.event_id
			WHERE e.source='codex' AND e.source_epoch=? AND e.event_id=? AND f.owning_thread_id=?
			  AND e.root_session_id=? AND o.source_file_id=? AND o.file_generation=?`,
			epoch, eventID, window.Owner, window.Root, window.Key.SourceFileID, window.Key.Generation).Scan(&count); err != nil {
			return err
		}
		if count == 0 {
			return fmt.Errorf("durable reconciliation window references missing proposal %q", eventID)
		}
	}
	if window.Key.SourceFileID <= 0 || window.Key.Generation <= 0 || window.End <= window.Key.StartOffset || window.Owner == "" || window.Root == "" {
		return ErrInvalidLegacyReconciliationWindow
	}
	return nil
}

func loadWindowBindings(reader storage.PrivateReader, epoch int64, windows []durableWindow, bindings map[ResponseKey]durableBinding) error {
	for _, window := range windows {
		for _, responseID := range window.State.ExplicitResponseIDs {
			key := ResponseKey{OwningThreadID: window.Owner, ResponseID: responseID}
			if _, exists := bindings[key]; exists {
				continue
			}
			rows, err := reader.Query(`SELECT f.event_id,f.owning_thread_id,f.response_id,f.evidence_kind,f.operation,
				e.event_kind,e.occurred_at_ms,e.thread_id,e.root_session_id,e.turn_key,e.model,e.reasoning_effort,
				e.input_tokens,e.cached_tokens,e.cache_write_tokens,e.output_tokens,e.reasoning_tokens,e.total_tokens
				FROM codex_usage_event_facts f JOIN usage_events e
				  ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
				WHERE f.source='codex' AND f.ledger_epoch=? AND f.owning_thread_id=? AND f.response_id=?
			  AND f.evidence_kind='explicit' AND e.root_session_id=? ORDER BY f.event_id`, epoch, window.Owner, responseID, window.Root)
			if err != nil {
				return err
			}
			var binding durableBinding
			found := false
			for rows.Next() {
				if found {
					_ = rows.Close()
					return fmt.Errorf("durable response key has multiple canonical bindings")
				}
				binding, err = scanDurableBinding(rows)
				if err != nil {
					_ = rows.Close()
					return err
				}
				found = true
			}
			if err := rows.Err(); err != nil {
				_ = rows.Close()
				return err
			}
			if err := rows.Close(); err != nil {
				return err
			}
			if !found {
				return fmt.Errorf("durable reconciliation window response binding is missing")
			}
			bindings[key] = binding
		}
	}
	return nil
}

func reconcileLegacyWindows(
	reader storage.PrivateReader,
	epoch int64,
	batch ProcessBatch,
	candidates []UsageCandidate,
	bindings map[ResponseKey]durableBinding,
	windows []durableWindow,
	modern modernWindowRelation,
	assignments map[ResponseKey]PrivateRowKey,
	result *ReconcileResult,
) error {
	byStart := make(map[int64]int, len(windows))
	for index := range windows {
		byStart[windows[index].Key.StartOffset] = index
	}
	for _, candidate := range candidatesForSource(candidates, batch.SourceState) {
		if candidate.EvidenceKind != EvidenceLegacy {
			continue
		}
		index, found := byStart[candidate.StartOffset]
		if !found {
			current := UsageValue{State: UsageValueMissing}
			if candidate.CurrentTotal != nil {
				current = UsageValue{State: UsageValueValid, Value: *cloneUsage(candidate.CurrentTotal)}
			}
			state := LegacyReconciliationWindow{
				Version: 1, PreviousTotal: UsageValue{State: UsageValueMissing},
				CurrentTotal:        current,
				LastUsage:           candidateLastUsage(candidate),
				ExplicitResponseIDs: []string{}, LegacyCoveredResponseIDs: []string{}, ProposalEventIDs: []string{},
				TurnAccountedBefore: sharedusage.Zero(), ChainState: LegacyWindowChainState{Kind: "continuous"}, Closed: true,
			}
			if candidate.PreviousTotal != nil {
				state.PreviousTotal = UsageValue{State: UsageValueValid, Value: *cloneUsage(candidate.PreviousTotal)}
			}
			created := durableWindow{
				Key: PrivateRowKey{SourceFileID: candidate.SourceFileID, Generation: candidate.Generation, StartOffset: candidate.StartOffset},
				End: candidate.EndOffset, Owner: candidate.OwningThreadID, Root: candidate.RootSessionID,
				TurnKey: cloneString(candidate.TurnKey), State: state,
			}
			windows = append(windows, created)
			index = len(windows) - 1
			byStart[candidate.StartOffset] = index
		} else if windows[index].End != candidate.EndOffset || windows[index].Owner != candidate.OwningThreadID ||
			!equalStringPointer(windows[index].TurnKey, candidate.TurnKey) ||
			!sameUsageValue(windows[index].State.LastUsage, candidateLastUsage(candidate)) {
			setFatalAt(result, FatalLegacyCoverage, batch.SourceState, candidate.StartOffset)
			return nil
		}
		if !containsString(windows[index].State.ProposalEventIDs, candidate.EventID) {
			windows[index].State.ProposalEventIDs = append(windows[index].State.ProposalEventIDs, candidate.EventID)
		}
	}
	sort.Slice(windows, func(i, j int) bool { return windows[i].Key.StartOffset < windows[j].Key.StartOffset })
	for index := range windows {
		window := &windows[index]
		if window.Owner != batch.SourceState.OwningThreadID || window.Key.SourceFileID != batch.SourceState.SourceFileID ||
			window.Key.Generation != batch.SourceState.Generation {
			continue
		}
		for _, candidate := range candidates {
			if candidate.EvidenceKind != EvidenceExplicit || candidate.OwningThreadID != window.Owner || candidate.RootSessionID != batch.SourceState.RootSessionID {
				continue
			}
			sameSource := candidate.SourceFileID == window.Key.SourceFileID && candidate.Generation == window.Key.Generation
			turnsConflict := window.TurnKey != nil && candidate.TurnKey != nil && *window.TurnKey != *candidate.TurnKey
			turnMatches := window.TurnKey != nil && candidate.TurnKey != nil && *window.TurnKey == *candidate.TurnKey
			if !sameSource {
				if !turnMatches {
					continue
				}
			} else if turnsConflict || (candidate.EndOffset != window.Key.StartOffset && candidate.StartOffset != window.End) {
				continue
			}
			if candidate.Response == nil {
				continue
			}
			window.State.ExplicitResponseIDs = append(window.State.ExplicitResponseIDs, candidate.Response.ResponseID)
		}
		window.State.ExplicitResponseIDs = sortedUniqueStrings(window.State.ExplicitResponseIDs)
		window.State.LegacyCoveredResponseIDs = sortedUniqueStrings(window.State.LegacyCoveredResponseIDs)
		window.State.ProposalEventIDs = sortedUniqueStrings(window.State.ProposalEventIDs)
		if err := reconcileOneLegacyWindow(reader, epoch, window, candidates, bindings, modern, assignments, result); err != nil || result.Fatal != nil {
			return err
		}
	}
	return nil
}

func candidateLastUsage(candidate UsageCandidate) UsageValue {
	if candidate.EventKind == 1 {
		return UsageValue{State: UsageValueMissing}
	}
	return UsageValue{State: UsageValueValid, Value: candidate.Usage}
}

func reconcileOneLegacyWindow(
	reader storage.PrivateReader,
	epoch int64,
	window *durableWindow,
	candidates []UsageCandidate,
	bindings map[ResponseKey]durableBinding,
	modern modernWindowRelation,
	assignments map[ResponseKey]PrivateRowKey,
	result *ReconcileResult,
) error {
	proposalIDs := append([]string(nil), window.State.ProposalEventIDs...)
	if len(proposalIDs) == 0 {
		if window.State.Closed {
			result.WindowDeletes = append(result.WindowDeletes, window.Key)
		}
		return nil
	}
	delta := legacyWindowDelta(window.State)
	if total, applies, known := modern.currentForWindow(window); applies {
		if !known {
			delta = nil
		} else {
			projectedWindow := window.State
			projectedWindow.CurrentTotal = UsageValue{State: UsageValueValid, Value: total}
			delta = legacyWindowDelta(projectedWindow)
		}
	}
	responseIDs := append([]string(nil), window.State.ExplicitResponseIDs...)
	responseIDs = sortedUniqueStrings(responseIDs)
	var explicit []UsageCandidate
	for _, responseID := range responseIDs {
		key := ResponseKey{OwningThreadID: window.Owner, ResponseID: responseID}
		if candidate, found := candidateForResponse(candidates, key); found {
			if candidateBelongsToWindow(candidate, *window, result.SourceState.RootSessionID) {
				explicit = append(explicit, candidate)
			}
			continue
		}
		if binding, found := bindings[key]; found {
			candidate := UsageCandidate{
				EventID: binding.Event.EventID, EvidenceKind: EvidenceExplicit, Operation: binding.Fact.Operation,
				OwningThreadID: binding.Event.ThreadID, RootSessionID: binding.Event.RootSessionID,
				TurnKey: cloneString(binding.Event.TurnKey), Model: binding.Event.Model,
				ReasoningEffort: cloneString(binding.Event.ReasoningEffort), Usage: binding.Event.Usage,
				Response: &ResponseEvidence{ResponseID: responseID, ThreadID: binding.Event.ThreadID,
					Usage: UsageValue{State: UsageValueValid, Value: binding.Event.Usage}},
			}
			if candidate.RootSessionID == result.SourceState.RootSessionID && equalStringPointer(candidate.TurnKey, window.TurnKey) {
				explicit = append(explicit, candidate)
			}
		}
	}
	covered := make(map[string]struct{})
	for _, id := range window.State.LegacyCoveredResponseIDs {
		covered[id] = struct{}{}
	}
	var legacyLastMatches []string
	if window.State.LastUsage.State == UsageValueValid {
		for _, candidate := range explicit {
			if candidate.Operation == OperationResponse && usageCoverageEqual(candidate.Usage, window.State.LastUsage.Value) == 1 {
				legacyLastMatches = append(legacyLastMatches, candidate.Response.ResponseID)
			}
		}
	}
	if len(legacyLastMatches) > 1 {
		setFatalAt(result, FatalLegacyCoverage, SourceState{SourceFileID: window.Key.SourceFileID, Generation: window.Key.Generation, OwningThreadID: window.Owner}, window.Key.StartOffset)
		return nil
	}
	var coveredExplicit []UsageCandidate
	if len(legacyLastMatches) == 1 {
		for _, candidate := range explicit {
			if candidate.Response.ResponseID == legacyLastMatches[0] {
				coveredExplicit = append(coveredExplicit, candidate)
				covered[legacyLastMatches[0]] = struct{}{}
				break
			}
		}
	} else if delta != nil && len(explicit) > 0 {
		matching := uniqueUsageSubsets(explicit, *delta)
		if len(matching) > 1 {
			setFatalAt(result, FatalLegacyCoverage, SourceState{SourceFileID: window.Key.SourceFileID, Generation: window.Key.Generation, OwningThreadID: window.Owner}, window.Key.StartOffset)
			return nil
		}
		if len(matching) == 1 {
			coveredExplicit = matching[0]
			for _, candidate := range coveredExplicit {
				covered[candidate.Response.ResponseID] = struct{}{}
			}
		}
	}
	residualApplied := false
	if len(coveredExplicit) == 0 && delta != nil && window.State.LastUsage.State == UsageValueMissing &&
		len(proposalIDs) == 1 && len(explicit) > 0 {
		var err error
		residualApplied, err = replaceLegacyResidual(reader, epoch, window, proposalIDs[0], explicit, candidates, result, *delta)
		if err != nil {
			return err
		}
		if residualApplied {
			coveredExplicit = explicit
			for _, candidate := range explicit {
				covered[candidate.Response.ResponseID] = struct{}{}
			}
		}
	}
	if len(coveredExplicit) > 0 {
		for _, candidate := range coveredExplicit {
			key := ResponseKey{OwningThreadID: window.Owner, ResponseID: candidate.Response.ResponseID}
			if previous, exists := assignments[key]; exists && previous != window.Key {
				setFatalAt(result, FatalLegacyCoverage, SourceState{
					SourceFileID: window.Key.SourceFileID, Generation: window.Key.Generation, OwningThreadID: window.Owner,
				}, window.Key.StartOffset)
				return nil
			}
			assignments[key] = window.Key
		}
		if residualApplied {
			window.State.LegacyCoveredResponseIDs = mapKeys(covered)
		} else {
			for _, proposalID := range proposalIDs {
				var legacyUsage *sharedusage.NormalizedTokenUsage
				for _, candidate := range candidates {
					if candidate.EventID == proposalID && candidate.EvidenceKind == EvidenceLegacy {
						usage := candidate.Usage
						legacyUsage = &usage
						break
					}
				}
				if legacyUsage == nil {
					var usage sharedusage.NormalizedTokenUsage
					if err := reader.QueryRow(`SELECT input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens
					FROM usage_events WHERE source='codex' AND source_epoch=? AND event_id=?`, epoch, proposalID).Scan(
						&usage.InputTokens, &usage.CachedTokens, &usage.CacheWriteTokens, &usage.OutputTokens, &usage.ReasoningTokens, &usage.TotalTokens); err != nil {
						return err
					}
					legacyUsage = &usage
				}
				if len(legacyLastMatches) == 1 && usageCoverageEqual(*legacyUsage, window.State.LastUsage.Value) != 1 {
					setFatalAt(result, FatalLegacyCoverage, SourceState{SourceFileID: window.Key.SourceFileID, Generation: window.Key.Generation, OwningThreadID: window.Owner}, window.Key.StartOffset)
					return nil
				}
				result.DeleteEventIDs = append(result.DeleteEventIDs, proposalID)
			}
			window.State.LegacyCoveredResponseIDs = mapKeys(covered)
			window.State.ProposalEventIDs = []string{}
			if len(proposalIDs) == 1 && len(coveredExplicit) == 1 {
				result.Occurrences = append(result.Occurrences, OccurrenceWrite{
					SourceFileID: window.Key.SourceFileID, Generation: window.Key.Generation,
					StartOffset: window.Key.StartOffset, EndOffset: window.End,
					EventID: coveredExplicit[0].EventID,
				})
			}
		}
	}
	if len(window.State.ProposalEventIDs) == 0 && window.State.Closed {
		result.WindowDeletes = append(result.WindowDeletes, window.Key)
		return nil
	}
	encoded, err := CanonicalLegacyReconciliationWindowJSON(window.State)
	if err != nil {
		return err
	}
	result.WindowUpserts = append(result.WindowUpserts, WindowWrite{
		SourceFileID: window.Key.SourceFileID, Generation: window.Key.Generation,
		StartOffset: window.Key.StartOffset, EndOffset: window.End,
		OwningThreadID: window.Owner, TurnKey: cloneString(window.TurnKey), StateJSON: encoded,
	})
	return nil
}

func replaceLegacyResidual(
	reader storage.PrivateReader,
	epoch int64,
	window *durableWindow,
	proposalID string,
	explicit []UsageCandidate,
	candidates []UsageCandidate,
	result *ReconcileResult,
	delta sharedusage.NormalizedTokenUsage,
) (bool, error) {
	if len(explicit) == 0 || window.State.PreviousTotal.State != UsageValueValid || window.State.CurrentTotal.State != UsageValueValid {
		return false, nil
	}
	coveredUsage := sharedusage.Zero()
	for _, candidate := range explicit {
		if candidate.Response == nil || candidate.Operation != OperationResponse ||
			(candidate.Usage.CacheWriteTokens == nil) != (delta.CacheWriteTokens == nil) {
			return false, nil
		}
		next, err := coveredUsage.CheckedAdd(candidate.Usage)
		if err != nil {
			return false, nil
		}
		coveredUsage = next
	}
	residual, err := delta.CheckedSub(coveredUsage)
	if err != nil || usageIsZero(residual) {
		return false, nil
	}
	reconstructed, err := residual.CheckedAdd(coveredUsage)
	if err != nil || !equalNormalizedUsage(reconstructed, delta) {
		return false, nil
	}

	binding, found := durableBinding{}, false
	for _, candidate := range candidates {
		if candidate.EventID != proposalID {
			continue
		}
		binding = durableBinding{
			Fact: EventFactWrite{EventID: candidate.EventID, OwningThreadID: candidate.OwningThreadID,
				EvidenceKind: candidate.EvidenceKind, Operation: candidate.Operation},
			Event: candidateEvent(candidate), HasEvent: true,
		}
		found = true
		break
	}
	if !found {
		binding, found, err = loadDurableFactByEventID(reader, epoch, proposalID)
		if err != nil || !found {
			return false, err
		}
	}
	if !binding.HasEvent || binding.Fact.EvidenceKind != EvidenceLegacy || binding.Fact.Operation != OperationResponse ||
		binding.Fact.ResponseID != nil || binding.Event.Kind != sharedusage.EventKindRecovered ||
		binding.Event.ThreadID != window.Owner || binding.Event.RootSessionID != window.Root ||
		!equalStringPointer(binding.Event.TurnKey, window.TurnKey) || usageCoverageEqual(binding.Event.Usage, delta) != 1 {
		return false, nil
	}
	uniqueOccurrence, err := hasUniqueLegacyProposalOccurrence(reader, epoch, window, proposalID, candidates, result)
	if err != nil || !uniqueOccurrence {
		return false, err
	}
	previous := window.State.PreviousTotal.Value
	current := window.State.CurrentTotal.Value
	residualID := LegacyEventID(binding.Event.ThreadID, binding.Event.TurnKey, 1, binding.Event.OccurredAtMS,
		&previous, current, residual, binding.Event.Model, binding.Event.ReasoningEffort)
	if residualID == proposalID {
		if !equalNormalizedUsage(binding.Event.Usage, residual) {
			setFatalAt(result, FatalLegacyCoverage, SourceState{
				SourceFileID: window.Key.SourceFileID, Generation: window.Key.Generation, OwningThreadID: window.Owner,
			}, window.Key.StartOffset)
			return false, nil
		}
	} else {
		event := binding.Event
		event.EventID = residualID
		event.Usage = residual
		fact := binding.Fact
		fact.EventID = residualID
		result.DeleteEventIDs = append(result.DeleteEventIDs, proposalID)
		result.Events = append(result.Events, event)
		result.Facts = append(result.Facts, fact)
	}
	result.Occurrences = append(result.Occurrences, OccurrenceWrite{
		SourceFileID: window.Key.SourceFileID, Generation: window.Key.Generation,
		StartOffset: window.Key.StartOffset, EndOffset: window.End, EventID: residualID,
	})
	window.State.ProposalEventIDs = []string{residualID}
	return true, nil
}

func hasUniqueLegacyProposalOccurrence(
	reader storage.PrivateReader,
	epoch int64,
	window *durableWindow,
	proposalID string,
	candidates []UsageCandidate,
	result *ReconcileResult,
) (bool, error) {
	current := false
	for _, candidate := range candidates {
		if candidate.EventID == proposalID {
			current = true
			break
		}
	}
	if current {
		count := 0
		for _, occurrence := range result.Occurrences {
			if occurrence.EventID != proposalID {
				continue
			}
			count++
			if occurrence.SourceFileID != window.Key.SourceFileID || occurrence.Generation != window.Key.Generation ||
				occurrence.StartOffset != window.Key.StartOffset || occurrence.EndOffset != window.End {
				return false, nil
			}
		}
		return count == 1, nil
	}
	rows, err := reader.Query(`SELECT source_file_id,file_generation,source_start_offset,source_end_offset
		FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND event_id=?`, epoch, proposalID)
	if err != nil {
		return false, err
	}
	defer rows.Close()
	count := 0
	match := false
	for rows.Next() {
		var sourceFileID, generation, start, end int64
		if err := rows.Scan(&sourceFileID, &generation, &start, &end); err != nil {
			return false, err
		}
		count++
		match = sourceFileID == window.Key.SourceFileID && generation == window.Key.Generation &&
			start == window.Key.StartOffset && end == window.End
	}
	if err := rows.Err(); err != nil {
		return false, err
	}
	return count == 1 && match, nil
}

func candidateBelongsToWindow(candidate UsageCandidate, window durableWindow, rootID string) bool {
	if candidate.RootSessionID != rootID {
		return false
	}
	sameSource := candidate.SourceFileID == window.Key.SourceFileID && candidate.Generation == window.Key.Generation
	if sameSource {
		if candidate.TurnKey != nil && window.TurnKey != nil && *candidate.TurnKey != *window.TurnKey {
			return false
		}
		return candidate.EndOffset == window.Key.StartOffset || candidate.StartOffset == window.End
	}
	return candidate.TurnKey != nil && window.TurnKey != nil && *candidate.TurnKey == *window.TurnKey
}

func sameUsageValue(left, right UsageValue) bool {
	if left.State != right.State {
		return false
	}
	return left.State != UsageValueValid || equalNormalizedUsage(left.Value, right.Value)
}

func legacyWindowDelta(window LegacyReconciliationWindow) *sharedusage.NormalizedTokenUsage {
	if window.ChainState.Kind != "continuous" || window.PreviousTotal.State != UsageValueValid || window.CurrentTotal.State != UsageValueValid {
		return nil
	}
	delta, err := window.CurrentTotal.Value.CheckedSub(window.PreviousTotal.Value)
	if err != nil {
		return nil
	}
	return &delta
}

// usageCoverageEqual uses Rust's tri-state coverage relation: 1 equal, -1
// different, and 0 when cache-write knownness prevents a proof.
func usageCoverageEqual(left, right sharedusage.NormalizedTokenUsage) int {
	if left.InputTokens != right.InputTokens || left.CachedTokens != right.CachedTokens ||
		left.OutputTokens != right.OutputTokens || left.ReasoningTokens != right.ReasoningTokens || left.TotalTokens != right.TotalTokens {
		return -1
	}
	if left.CacheWriteTokens == nil || right.CacheWriteTokens == nil {
		if left.CacheWriteTokens == nil && right.CacheWriteTokens == nil {
			return 1
		}
		return 0
	}
	if *left.CacheWriteTokens == *right.CacheWriteTokens {
		return 1
	}
	return -1
}

func uniqueUsageSubsets(candidates []UsageCandidate, target sharedusage.NormalizedTokenUsage) [][]UsageCandidate {
	ordered := append([]UsageCandidate(nil), candidates...)
	remaining := make([]sharedusage.NormalizedTokenUsage, len(ordered)+1)
	for index := len(ordered) - 1; index >= 0; index-- {
		usage := ordered[index].Usage
		remaining[index] = sharedusage.NormalizedTokenUsage{
			InputTokens:     saturatingTokenAdd(usage.InputTokens, remaining[index+1].InputTokens),
			CachedTokens:    saturatingTokenAdd(usage.CachedTokens, remaining[index+1].CachedTokens),
			OutputTokens:    saturatingTokenAdd(usage.OutputTokens, remaining[index+1].OutputTokens),
			ReasoningTokens: saturatingTokenAdd(usage.ReasoningTokens, remaining[index+1].ReasoningTokens),
			TotalTokens:     saturatingTokenAdd(usage.TotalTokens, remaining[index+1].TotalTokens),
		}
	}
	result := make([][]UsageCandidate, 0, 2)
	selected := make([]UsageCandidate, 0, len(ordered))
	var visit func(int, sharedusage.NormalizedTokenUsage)
	visit = func(index int, total sharedusage.NormalizedTokenUsage) {
		if len(result) > 1 {
			return
		}
		if !subsetCanReach(total, remaining[index], target) {
			return
		}
		if index == len(ordered) {
			if len(selected) > 0 && usageCoverageEqual(total, target) == 1 {
				result = append(result, append([]UsageCandidate(nil), selected...))
			}
			return
		}
		visit(index+1, total)
		next, err := total.CheckedAdd(ordered[index].Usage)
		if err != nil || next.InputTokens > target.InputTokens || next.CachedTokens > target.CachedTokens ||
			next.OutputTokens > target.OutputTokens || next.ReasoningTokens > target.ReasoningTokens || next.TotalTokens > target.TotalTokens {
			return
		}
		selected = append(selected, ordered[index])
		visit(index+1, next)
		selected = selected[:len(selected)-1]
	}
	visit(0, sharedusage.Zero())
	return result
}

func subsetCanReach(total, remaining, target sharedusage.NormalizedTokenUsage) bool {
	return saturatingTokenAdd(total.InputTokens, remaining.InputTokens) >= target.InputTokens &&
		saturatingTokenAdd(total.CachedTokens, remaining.CachedTokens) >= target.CachedTokens &&
		saturatingTokenAdd(total.OutputTokens, remaining.OutputTokens) >= target.OutputTokens &&
		saturatingTokenAdd(total.ReasoningTokens, remaining.ReasoningTokens) >= target.ReasoningTokens &&
		saturatingTokenAdd(total.TotalTokens, remaining.TotalTokens) >= target.TotalTokens
}

func saturatingTokenAdd(left, right int64) int64 {
	if right > 0 && left > int64(^uint64(0)>>1)-right {
		return int64(^uint64(0) >> 1)
	}
	return left + right
}

func mapKeys(values map[string]struct{}) []string {
	keys := make([]string, 0, len(values))
	for value := range values {
		keys = append(keys, value)
	}
	return sortedUniqueStrings(keys)
}

func containsString(values []string, target string) bool {
	for _, value := range values {
		if value == target {
			return true
		}
	}
	return false
}

func reconcileTurns(
	reader storage.PrivateReader,
	epoch int64,
	batch ProcessBatch,
	candidates []UsageCandidate,
	result *ReconcileResult,
) error {
	for _, turn := range batch.TurnUpserts {
		if turn.SourceFileID != batch.SourceState.SourceFileID || turn.Generation != batch.SourceState.Generation ||
			turn.ThreadID != batch.SourceState.OwningThreadID {
			continue
		}
		end := turn.StateThroughOffset
		if turn.EndOffset != nil {
			end = *turn.EndOffset
		}
		accounted := sharedusage.Zero()
		seen := make(map[string]struct{})
		for _, candidate := range candidates {
			if candidate.OwningThreadID != turn.ThreadID || candidate.RootSessionID != batch.SourceState.RootSessionID ||
				candidate.TurnKey == nil || *candidate.TurnKey != turn.TurnKey || candidate.EventKind == 2 ||
				candidate.StartOffset < turn.StartOffset || candidate.StartOffset >= end ||
				containsString(result.DeleteEventIDs, candidate.EventID) {
				continue
			}
			if _, ok := seen[candidate.EventID]; ok {
				continue
			}
			var err error
			accounted, err = accounted.CheckedAdd(candidate.Usage)
			if err != nil {
				setFatalAt(result, FatalArithmeticOverflow, batch.SourceState, candidate.StartOffset)
				return nil
			}
			seen[candidate.EventID] = struct{}{}
		}
		rows, err := reader.Query(`SELECT e.event_id,e.input_tokens,e.cached_tokens,e.cache_write_tokens,e.output_tokens,e.reasoning_tokens,e.total_tokens
			FROM codex_usage_event_occurrences o JOIN usage_events e
			  ON e.source=o.source AND e.source_epoch=o.ledger_epoch AND e.event_id=o.event_id
			WHERE o.source='codex' AND o.ledger_epoch=? AND o.source_file_id=? AND o.file_generation=?
			  AND o.source_start_offset>=? AND o.source_start_offset<? AND e.thread_id=? AND e.turn_key=?
			  AND e.event_kind<>'turn_compensation' ORDER BY o.source_start_offset`,
			epoch, turn.SourceFileID, turn.Generation, turn.StartOffset, end, turn.ThreadID, turn.TurnKey)
		if err != nil {
			return err
		}
		for rows.Next() {
			var eventID string
			var usage sharedusage.NormalizedTokenUsage
			if err := rows.Scan(&eventID, &usage.InputTokens, &usage.CachedTokens, &usage.CacheWriteTokens,
				&usage.OutputTokens, &usage.ReasoningTokens, &usage.TotalTokens); err != nil {
				_ = rows.Close()
				return err
			}
			if _, ok := seen[eventID]; ok || containsString(result.DeleteEventIDs, eventID) {
				continue
			}
			accounted, err = accounted.CheckedAdd(usage)
			if err != nil {
				_ = rows.Close()
				setFatalAt(result, FatalArithmeticOverflow, batch.SourceState, turn.StartOffset)
				return nil
			}
			seen[eventID] = struct{}{}
		}
		if err := rows.Err(); err != nil {
			_ = rows.Close()
			return err
		}
		if err := rows.Close(); err != nil {
			return err
		}
		for _, event := range result.Events {
			if event.Kind == sharedusage.EventKindTurnCompensation || event.ThreadID != turn.ThreadID ||
				event.RootSessionID != batch.SourceState.RootSessionID || event.TurnKey == nil || *event.TurnKey != turn.TurnKey ||
				containsString(result.DeleteEventIDs, event.EventID) {
				continue
			}
			if _, ok := seen[event.EventID]; ok {
				continue
			}
			inTurn := false
			for _, occurrence := range result.Occurrences {
				if occurrence.EventID == event.EventID && occurrence.SourceFileID == turn.SourceFileID &&
					occurrence.Generation == turn.Generation && occurrence.StartOffset >= turn.StartOffset && occurrence.StartOffset < end {
					inTurn = true
					break
				}
			}
			if !inTurn {
				continue
			}
			accounted, err = accounted.CheckedAdd(event.Usage)
			if err != nil {
				setFatalAt(result, FatalArithmeticOverflow, batch.SourceState, turn.StartOffset)
				return nil
			}
			seen[event.EventID] = struct{}{}
		}
		turn.Accounted = accounted
		turn.AccountedCandidateCount = int64(len(seen))
		turn.UpdatedAtMS = batch.SourceState.UpdatedAtMS
		blocked, err := hasUnresolvedTurnCompaction(reader, epoch, turn, candidates, result)
		if err != nil {
			return err
		}
		desiredID := ""
		var desired *sharedusage.CanonicalUsageEventWrite
		if !blocked && turnCompensationAllowed(turn.Blocks) && turn.Status != TurnOpen &&
			turn.StartTotal != nil && turn.LastTotal != nil && turn.EndedAtMS != nil && turn.EndOffset != nil {
			delta, deltaErr := turn.LastTotal.CheckedSub(*turn.StartTotal)
			if deltaErr != nil {
				if len(seen) > 0 {
					setFatalAt(result, FatalArithmeticOverflow, batch.SourceState, turn.StartOffset)
					return nil
				}
			} else if missing, missingErr := delta.CheckedSub(turn.Accounted); missingErr != nil {
				if len(seen) > 0 {
					setFatalAt(result, FatalArithmeticOverflow, batch.SourceState, turn.StartOffset)
					return nil
				}
			} else if !usageIsZero(missing) {
				model := ""
				switch turn.ModelState {
				case TurnValueSingle:
					model = *turn.SingleModel
				case TurnValueMixed:
					model = "unknown"
				}
				if model != "" {
					effort := (*string)(nil)
					if !turn.UnresolvedReasoningEffortSeen && turn.ReasoningEffortState == TurnValueSingle {
						effort = cloneString(turn.SingleReasoningEffort)
					}
					eventID := LegacyEventID(turn.ThreadID, stringPointer(turn.TurnKey), 2, *turn.EndedAtMS,
						turn.StartTotal, *turn.LastTotal, missing, model, effort)
					event := sharedusage.CanonicalUsageEventWrite{
						EventID: eventID, Kind: sharedusage.EventKindTurnCompensation, OccurredAtMS: *turn.EndedAtMS,
						ThreadID: turn.ThreadID, RootSessionID: batch.SourceState.RootSessionID,
						TurnKey: stringPointer(turn.TurnKey), Model: model, ReasoningEffort: effort, Usage: missing,
					}
					desiredID = eventID
					desired = &event
				}
			}
		}
		oldRows, err := reader.Query(`SELECT e.event_id,o.source_start_offset,o.source_end_offset
			FROM usage_events e JOIN codex_usage_event_occurrences o
			  ON o.source=e.source AND o.ledger_epoch=e.source_epoch AND o.event_id=e.event_id
			WHERE e.source='codex' AND e.source_epoch=? AND e.thread_id=? AND e.turn_key=?
			  AND e.event_kind='turn_compensation' AND o.source_file_id=? AND o.file_generation=?
			  AND o.source_start_offset>=? AND o.source_start_offset<?`,
			epoch, turn.ThreadID, turn.TurnKey, turn.SourceFileID, turn.Generation, turn.StartOffset, end)
		if err != nil {
			return err
		}
		var oldIDs []string
		for oldRows.Next() {
			var eventID string
			var startOffset, endOffset int64
			if err := oldRows.Scan(&eventID, &startOffset, &endOffset); err != nil {
				_ = oldRows.Close()
				return err
			}
			if eventID != desiredID {
				oldIDs = append(oldIDs, eventID)
				result.DeleteEventIDs = append(result.DeleteEventIDs, eventID)
			} else {
				result.Occurrences = append(result.Occurrences, OccurrenceWrite{
					SourceFileID: turn.SourceFileID, Generation: turn.Generation,
					StartOffset: startOffset, EndOffset: endOffset, EventID: eventID,
				})
			}
		}
		if err := oldRows.Err(); err != nil {
			_ = oldRows.Close()
			return err
		}
		if err := oldRows.Close(); err != nil {
			return err
		}
		_ = oldIDs
		if desired != nil {
			result.Events = append(result.Events, *desired)
			result.Facts = append(result.Facts, EventFactWrite{
				EventID: desired.EventID, OwningThreadID: turn.ThreadID,
				EvidenceKind: EvidenceLegacy, Operation: OperationResponse,
			})
			if !containsString(result.DeleteEventIDs, desired.EventID) {
				found := false
				for _, occurrence := range result.Occurrences {
					if occurrence.SourceFileID == turn.SourceFileID && occurrence.Generation == turn.Generation &&
						occurrence.EventID == desired.EventID {
						found = true
						break
					}
				}
				if !found {
					result.Occurrences = append(result.Occurrences, OccurrenceWrite{
						SourceFileID: turn.SourceFileID, Generation: turn.Generation,
						StartOffset: turn.StartOffset, EndOffset: end, EventID: desired.EventID,
					})
				}
			}
		}
		result.TurnUpserts = append(result.TurnUpserts, turn)
	}
	return nil
}

func projectModernState(
	batch ProcessBatch,
	candidates []UsageCandidate,
	bindings map[ResponseKey]durableBinding,
) (modernWindowRelation, error) {
	carry, err := LoadReconciliationCarry(batch.SourceState.ReconciliationStateJSON)
	if err != nil {
		return modernWindowRelation{}, err
	}
	relation := modernWindowRelation{
		Boundary: batch.LogicalSafeOffset, Present: carry.ModernCounterDomain != nil || carry.ModernCounterTotal != nil,
		Known: carry.ModernCounterDomain != nil && carry.ModernCounterTotal != nil,
	}
	if carry.OpenWindowStartOffset != nil {
		if *carry.OpenWindowStartOffset > uint64(^uint64(0)>>1) {
			return modernWindowRelation{}, ErrInvalidReconciliationCarry
		}
		boundary := int64(*carry.OpenWindowStartOffset)
		relation.Boundary = boundary
		relation.WindowStart = &boundary
	}
	modernDomain := carry.ModernCounterDomain
	modernTotal := carry.ModernCounterTotal
	if !relation.Present {
		var latest *ResponseEvidence
		latestEnd := int64(-1)
		consider := func(response *ResponseEvidence, endOffset int64) {
			if response == nil || response.ThreadTokenUsage.State == UsageValueMissing || endOffset < latestEnd {
				return
			}
			latest = response
			latestEnd = endOffset
		}
		for _, candidate := range candidatesForSource(candidates, batch.SourceState) {
			if candidate.Response != nil {
				consider(candidate.Response, candidate.EndOffset)
			}
		}
		for _, pending := range batch.Compactions {
			if pending.Record.Compaction != nil && pending.Record.Compaction.Latest != nil {
				consider(pending.Record.Compaction.Latest, int64(pending.Record.EndOffset))
			}
		}
		for _, pending := range carry.PendingEvidence {
			if pending.Record.Kind == PendingResponseUsage && pending.Record.Response != nil {
				consider(pending.Record.Response, int64(pending.Record.EndOffset))
			} else if pending.Record.Kind == PendingCompacted && pending.Record.Compaction != nil && pending.Record.Compaction.Latest != nil {
				consider(pending.Record.Compaction.Latest, int64(pending.Record.EndOffset))
			}
		}
		if latest != nil {
			relation.Present = true
			modernDomain = responseDomain(batch.SourceState.OwningThreadID, *latest)
			if latest.ThreadTokenUsage.State == UsageValueValid {
				modernTotal = cloneUsage(&latest.ThreadTokenUsage.Value)
			}
		}
	}
	if !relation.Present {
		return relation, nil
	}
	relation.Known = modernDomain != nil && modernTotal != nil
	if relation.Known {
		relation.Total = *modernTotal
	}
	if modernDomain == nil || modernTotal == nil {
		return relation, nil
	}
	legacyDomain := ModernCounterDomain{ThreadID: batch.SourceState.OwningThreadID}
	compactions := compactionEvidenceForBatch(batch, candidates, bindings)
	projected, known, err := projectModernCounter(*modernTotal, *modernDomain, legacyDomain, compactions, relation.Boundary)
	if err != nil {
		return modernWindowRelation{}, err
	}
	relation.Known = known
	if known {
		relation.Total = projected
	}
	return relation, nil
}

func hasUnresolvedTurnCompaction(
	reader storage.PrivateReader,
	epoch int64,
	turn TurnWrite,
	candidates []UsageCandidate,
	result *ReconcileResult,
) (bool, error) {
	if turn.EndOffset == nil {
		return false, nil
	}
	markerRows, err := reader.Query(`SELECT source_start_offset,resolved_event_id FROM codex_compaction_markers
		WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=? AND owning_thread_id=?
		  AND source_start_offset>=? AND source_start_offset<?`,
		epoch, turn.SourceFileID, turn.Generation, turn.ThreadID, turn.StartOffset, *turn.EndOffset)
	if err != nil {
		return false, err
	}
	markers := make(map[PrivateRowKey]bool)
	for markerRows.Next() {
		var key PrivateRowKey
		var resolved sql.NullString
		if err := markerRows.Scan(&key.StartOffset, &resolved); err != nil {
			_ = markerRows.Close()
			return false, err
		}
		key.SourceFileID, key.Generation = turn.SourceFileID, turn.Generation
		markers[key] = !resolved.Valid
	}
	if err := markerRows.Err(); err != nil {
		_ = markerRows.Close()
		return false, err
	}
	if err := markerRows.Close(); err != nil {
		return false, err
	}
	for _, key := range result.MarkerDeletes {
		delete(markers, key)
	}
	for _, marker := range result.MarkerUpserts {
		if marker.SourceFileID == turn.SourceFileID && marker.Generation == turn.Generation && marker.OwningThreadID == turn.ThreadID &&
			marker.StartOffset >= turn.StartOffset && marker.StartOffset < *turn.EndOffset {
			key := PrivateRowKey{SourceFileID: marker.SourceFileID, Generation: marker.Generation, StartOffset: marker.StartOffset}
			markers[key] = marker.ResolvedEventID == nil
		}
	}
	for _, unresolved := range markers {
		if unresolved {
			return true, nil
		}
	}
	rows, err := reader.Query(`SELECT source_start_offset,state_json FROM codex_usage_reconciliation_windows
		WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=? AND owning_thread_id=? AND turn_key=?
		  AND source_start_offset>=? AND source_start_offset<? ORDER BY source_start_offset`,
		epoch, turn.SourceFileID, turn.Generation, turn.ThreadID, turn.TurnKey, turn.StartOffset, *turn.EndOffset)
	if err != nil {
		return false, err
	}
	windows := make(map[PrivateRowKey]LegacyReconciliationWindow)
	for rows.Next() {
		var start int64
		var raw string
		if err := rows.Scan(&start, &raw); err != nil {
			return false, err
		}
		window, err := DecodeLegacyReconciliationWindow([]byte(raw))
		if err != nil {
			_ = rows.Close()
			return false, err
		}
		key := PrivateRowKey{SourceFileID: turn.SourceFileID, Generation: turn.Generation, StartOffset: start}
		windows[key] = window
	}
	if err := rows.Err(); err != nil {
		_ = rows.Close()
		return false, err
	}
	if err := rows.Close(); err != nil {
		return false, err
	}
	for _, key := range result.WindowDeletes {
		delete(windows, key)
	}
	for _, write := range result.WindowUpserts {
		if write.SourceFileID != turn.SourceFileID || write.Generation != turn.Generation || write.OwningThreadID != turn.ThreadID ||
			write.TurnKey == nil || *write.TurnKey != turn.TurnKey || write.StartOffset < turn.StartOffset || write.StartOffset >= *turn.EndOffset {
			continue
		}
		window, err := DecodeLegacyReconciliationWindow(write.StateJSON)
		if err != nil {
			return false, err
		}
		key := PrivateRowKey{SourceFileID: write.SourceFileID, Generation: write.Generation, StartOffset: write.StartOffset}
		windows[key] = window
	}
	for _, window := range windows {
		for _, responseID := range window.ExplicitResponseIDs {
			if containsString(window.LegacyCoveredResponseIDs, responseID) {
				continue
			}
			key := ResponseKey{OwningThreadID: turn.ThreadID, ResponseID: responseID}
			if candidate, found := candidateForResponse(candidates, key); found && candidate.Operation == OperationCompaction {
				return true, nil
			}
			var operation string
			err := reader.QueryRow(`SELECT operation FROM codex_usage_event_facts
				WHERE source='codex' AND ledger_epoch=? AND owning_thread_id=? AND response_id=? AND evidence_kind='explicit'`,
				epoch, turn.ThreadID, responseID).Scan(&operation)
			if err == nil && operation == operationName(OperationCompaction) {
				return true, nil
			} else if err != nil && err != sql.ErrNoRows {
				return false, err
			}
		}
	}
	return false, nil
}

func Commit(
	tx *source.WriteTx,
	target source.UsageWriteTarget,
	result ReconcileResult,
	committedAtMS int64,
) (visibleChanged bool, err error) {
	if tx == nil || committedAtMS < 0 {
		return false, ErrInvalidReconciliationPatch
	}
	if result.Fatal != nil {
		return false, fmt.Errorf("cannot commit fatal reconciliation result: %s", result.Fatal.Code)
	}
	if target != source.UsageTargetActive && target != source.UsageTargetBuild {
		return false, ErrInvalidReconciliationPatch
	}
	if err := validatePatchReferences(result); err != nil {
		return false, err
	}
	state, err := tx.UsageEpochState()
	if err != nil {
		return false, err
	}
	activeEpoch := state.ActiveEpoch
	epoch, err := tx.ResolveUsageWriteEpoch(target)
	if err != nil {
		return false, err
	}
	deleteSet := stringSet(result.DeleteEventIDs)
	windowWrites, err := canonicalWindowWrites(result.WindowUpserts, deleteSet)
	if err != nil {
		return false, err
	}
	if err := tx.Private(func(private storage.PrivateTx) error {
		_, err := deleteMarkers(private, epoch, result.MarkerDeletes)
		if err != nil {
			return err
		}
		_, err = writeOccurrences(private, epoch, result.Occurrences, deleteSet, committedAtMS)
		if err != nil {
			return err
		}
		_, err = deleteWindows(private, epoch, result.WindowDeletes)
		if err != nil {
			return err
		}
		_, err = deleteHolds(private, epoch, result.HoldDeletes)
		if err != nil {
			return err
		}
		_, err = writeWindows(private, epoch, windowWrites)
		return err
	}); err != nil {
		return false, err
	}
	if len(result.DeleteEventIDs) > 0 {
		if _, err := stripDeletedEventReferencesFromWindows(tx, target, result.DeleteEventIDs); err != nil {
			return false, err
		}
	}
	if err := tx.Private(func(private storage.PrivateTx) error {
		return validateNoDeletedDependencies(private, epoch, result)
	}); err != nil {
		return false, err
	}
	for _, input := range result.Events {
		if deleteSet[input.EventID] {
			return false, ErrInvalidReconciliationPatch
		}
		event := input
		event.CreatedAtMS = committedAtMS
		event.EstimatedCostNanosUSD = nil
		match, err := tx.CompareUsageNoRevision(target, event)
		if err != nil {
			return false, err
		}
		if target == source.UsageTargetActive {
			switch match {
			case storage.UsageEventIdentical:
				continue
			case storage.UsageEventConflict:
				return false, fmt.Errorf("active canonical usage event %q conflicts with reconciliation", event.EventID)
			case storage.UsageEventAbsent:
				outcome, err := tx.WriteUsageNoRevision(target, event)
				if err != nil {
					return false, err
				}
				visibleChanged = visibleChanged || outcome == storage.UsageInserted
			}
			continue
		}
		switch match {
		case storage.UsageEventIdentical:
			continue
		case storage.UsageEventConflict:
			return false, fmt.Errorf("build canonical usage event %q conflicts with reconciliation", event.EventID)
		}
		if activeEpoch > 0 {
			activeMatch, err := tx.CompareUsageNoRevision(source.UsageTargetActive, event)
			if err != nil {
				return false, err
			}
			if activeMatch == storage.UsageEventIdentical {
				_, err := tx.CopyUsageNoRevision(source.UsageTargetActive, source.UsageTargetBuild, event.EventID)
				if err != nil {
					return false, err
				}
				continue
			}
		}
		if _, err := tx.WriteUsageNoRevision(source.UsageTargetBuild, event); err != nil {
			return false, err
		}
	}
	if err := tx.Private(func(private storage.PrivateTx) error {
		_, err := writeFacts(private, epoch, result.Facts)
		if err != nil {
			return err
		}
		_, err = writeMarkers(private, epoch, result.MarkerUpserts)
		if err != nil {
			return err
		}
		_, err = writeHolds(private, epoch, result.HoldUpserts)
		return err
	}); err != nil {
		return false, err
	}
	for _, turn := range result.TurnUpserts {
		turn.UpdatedAtMS = committedAtMS
		if err := WriteTurn(tx, target, turn); err != nil {
			return false, err
		}
	}
	sourceState := result.SourceState
	sourceState.UpdatedAtMS = committedAtMS
	if sourceState.SourceFileID > 0 {
		if err := WriteSourceState(tx, target, sourceState); err != nil {
			return false, err
		}
	}
	if len(result.DeleteEventIDs) > 0 {
		deleted, err := tx.DeleteUsageNoRevision(target, sortedUniqueStrings(result.DeleteEventIDs))
		if err != nil {
			return false, err
		}
		visibleChanged = visibleChanged || target == source.UsageTargetActive && deleted > 0
	}
	if target == source.UsageTargetBuild {
		visibleChanged = false
	}
	return visibleChanged, nil
}

func StripDeletedEventReferencesFromWindows(
	tx *source.WriteTx,
	target source.UsageWriteTarget,
	eventIDs []string,
) error {
	_, err := stripDeletedEventReferencesFromWindows(tx, target, eventIDs)
	return err
}

func stripDeletedEventReferencesFromWindows(
	tx *source.WriteTx,
	target source.UsageWriteTarget,
	eventIDs []string,
) (bool, error) {
	if tx == nil {
		return false, ErrInvalidReconciliationPatch
	}
	if len(eventIDs) == 0 {
		return false, nil
	}
	epoch, err := tx.ResolveUsageWriteEpoch(target)
	if err != nil {
		return false, err
	}
	remove := stringSet(eventIDs)
	changed := false
	err = tx.Private(func(private storage.PrivateTx) error {
		deletedResponses := make(map[string]bool)
		for _, eventID := range eventIDs {
			var owner string
			var response sql.NullString
			err := private.QueryRow(`SELECT owning_thread_id,response_id FROM codex_usage_event_facts
				WHERE source='codex' AND ledger_epoch=? AND event_id=?`, epoch, eventID).Scan(&owner, &response)
			if err != nil && err != sql.ErrNoRows {
				return err
			}
			if err == nil && response.Valid {
				deletedResponses[owner+"\x00"+response.String] = true
			}
		}
		rows, err := private.Query(`SELECT source_file_id,file_generation,source_start_offset,owning_thread_id,state_json
			FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=? ORDER BY source_file_id,file_generation,source_start_offset`, epoch)
		if err != nil {
			return err
		}
		type rewrite struct {
			key  PrivateRowKey
			json []byte
		}
		var rewrites []rewrite
		for rows.Next() {
			var key PrivateRowKey
			var owner string
			var raw string
			if err := rows.Scan(&key.SourceFileID, &key.Generation, &key.StartOffset, &owner, &raw); err != nil {
				_ = rows.Close()
				return err
			}
			window, err := DecodeLegacyReconciliationWindow([]byte(raw))
			if err != nil {
				_ = rows.Close()
				return err
			}
			oldProposals := len(window.ProposalEventIDs)
			proposals := window.ProposalEventIDs[:0]
			for _, eventID := range window.ProposalEventIDs {
				if !remove[eventID] {
					proposals = append(proposals, eventID)
				}
			}
			window.ProposalEventIDs = proposals
			filterResponses := func(values []string) []string {
				filtered := values[:0]
				for _, responseID := range values {
					if !deletedResponses[owner+"\x00"+responseID] {
						filtered = append(filtered, responseID)
					}
				}
				return filtered
			}
			oldExplicit, oldCovered := len(window.ExplicitResponseIDs), len(window.LegacyCoveredResponseIDs)
			window.ExplicitResponseIDs = filterResponses(window.ExplicitResponseIDs)
			window.LegacyCoveredResponseIDs = filterResponses(window.LegacyCoveredResponseIDs)
			if oldProposals == len(window.ProposalEventIDs) && oldExplicit == len(window.ExplicitResponseIDs) &&
				oldCovered == len(window.LegacyCoveredResponseIDs) {
				continue
			}
			encoded, err := CanonicalLegacyReconciliationWindowJSON(window)
			if err != nil {
				_ = rows.Close()
				return err
			}
			rewrites = append(rewrites, rewrite{key: key, json: encoded})
		}
		if err := rows.Err(); err != nil {
			_ = rows.Close()
			return err
		}
		if err := rows.Close(); err != nil {
			return err
		}
		for _, rewrite := range rewrites {
			if _, err := private.Exec(`UPDATE codex_usage_reconciliation_windows SET state_json=?
				WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=? AND source_start_offset=?`,
				string(rewrite.json), epoch, rewrite.key.SourceFileID, rewrite.key.Generation, rewrite.key.StartOffset); err != nil {
				return err
			}
			changed = true
		}
		return nil
	})
	return changed, err
}

func validatePatchReferences(result ReconcileResult) error {
	deletes := stringSet(result.DeleteEventIDs)
	for _, eventID := range result.DeleteEventIDs {
		if eventID == "" {
			return ErrInvalidReconciliationPatch
		}
	}
	for _, event := range result.Events {
		if deletes[event.EventID] || event.Validate() != nil {
			return ErrInvalidReconciliationPatch
		}
	}
	for _, fact := range result.Facts {
		if fact.EventID == "" || fact.OwningThreadID == "" || deletes[fact.EventID] ||
			(fact.EvidenceKind != EvidenceExplicit && fact.EvidenceKind != EvidenceLegacy) ||
			(fact.Operation != OperationResponse && fact.Operation != OperationCompaction) ||
			(fact.EvidenceKind == EvidenceExplicit) != (fact.ResponseID != nil) ||
			(fact.Operation == OperationCompaction && fact.EvidenceKind != EvidenceExplicit) {
			return ErrInvalidReconciliationPatch
		}
	}
	for _, occurrence := range result.Occurrences {
		if occurrence.SourceFileID <= 0 || occurrence.Generation <= 0 || occurrence.StartOffset < 0 ||
			occurrence.EndOffset <= occurrence.StartOffset || occurrence.EventID == "" || deletes[occurrence.EventID] {
			return ErrInvalidReconciliationPatch
		}
	}
	for _, marker := range result.MarkerUpserts {
		if marker.SourceFileID <= 0 || marker.Generation <= 0 || marker.StartOffset < 0 || marker.EndOffset <= marker.StartOffset ||
			marker.OwningThreadID == "" || marker.RootSessionID == "" ||
			(marker.ResolvedEventID == nil) == (marker.UnknownReason == nil) ||
			(marker.ResolvedEventID != nil && deletes[*marker.ResolvedEventID]) {
			return ErrInvalidReconciliationPatch
		}
	}
	for _, hold := range result.HoldUpserts {
		if hold.SourceFileID <= 0 || hold.Generation <= 0 || hold.EventID == "" || deletes[hold.EventID] {
			return ErrInvalidReconciliationPatch
		}
	}
	return nil
}

func canonicalWindowWrites(windows []WindowWrite, deletes map[string]bool) ([]WindowWrite, error) {
	result := make([]WindowWrite, 0, len(windows))
	for _, write := range windows {
		if write.SourceFileID <= 0 || write.Generation <= 0 || write.StartOffset < 0 || write.EndOffset <= write.StartOffset ||
			write.OwningThreadID == "" {
			return nil, ErrInvalidReconciliationPatch
		}
		window, err := DecodeLegacyReconciliationWindow(write.StateJSON)
		if err != nil {
			return nil, err
		}
		proposals := window.ProposalEventIDs[:0]
		for _, eventID := range window.ProposalEventIDs {
			if !deletes[eventID] {
				proposals = append(proposals, eventID)
			}
		}
		window.ProposalEventIDs = proposals
		encoded, err := CanonicalLegacyReconciliationWindowJSON(window)
		if err != nil {
			return nil, err
		}
		write.StateJSON = encoded
		result = append(result, write)
	}
	return result, nil
}

func deleteMarkers(private storage.PrivateTx, epoch int64, keys []PrivateRowKey) (bool, error) {
	changed := false
	for _, key := range keys {
		result, err := private.Exec(`DELETE FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=?
			AND source_file_id=? AND file_generation=? AND source_start_offset=?`, epoch, key.SourceFileID, key.Generation, key.StartOffset)
		if err != nil {
			return false, err
		}
		count, err := result.RowsAffected()
		if err != nil {
			return false, err
		}
		changed = changed || count > 0
	}
	return changed, nil
}

func writeOccurrences(private storage.PrivateTx, epoch int64, occurrences []OccurrenceWrite, deletes map[string]bool, committedAtMS int64) (bool, error) {
	changed := false
	for _, occurrence := range occurrences {
		var oldEvent string
		err := private.QueryRow(`SELECT event_id FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?
			AND source_file_id=? AND file_generation=? AND source_start_offset=?`, epoch, occurrence.SourceFileID,
			occurrence.Generation, occurrence.StartOffset).Scan(&oldEvent)
		if err != nil && err != sql.ErrNoRows {
			return false, err
		}
		if err == nil && oldEvent != occurrence.EventID && !deletes[oldEvent] {
			return false, fmt.Errorf("%w: usage occurrence identity changed without explicit deletion", ErrInvalidReconciliationPatch)
		}
		if err == nil {
			var oldEnd int64
			if err := private.QueryRow(`SELECT source_end_offset FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?
				AND source_file_id=? AND file_generation=? AND source_start_offset=?`, epoch, occurrence.SourceFileID,
				occurrence.Generation, occurrence.StartOffset).Scan(&oldEnd); err != nil {
				return false, err
			}
			changed = changed || oldEvent != occurrence.EventID || oldEnd != occurrence.EndOffset
		} else {
			changed = true
		}
		if _, err := private.Exec(`INSERT INTO codex_usage_event_occurrences(source,ledger_epoch,source_file_id,file_generation,
			source_start_offset,source_end_offset,event_id,created_at_ms) VALUES('codex',?,?,?,?,?,?,?)
			ON CONFLICT(source,ledger_epoch,source_file_id,file_generation,source_start_offset) DO UPDATE SET
			source_end_offset=excluded.source_end_offset,event_id=excluded.event_id
			WHERE source_end_offset<>excluded.source_end_offset OR event_id<>excluded.event_id`, epoch,
			occurrence.SourceFileID, occurrence.Generation, occurrence.StartOffset, occurrence.EndOffset, occurrence.EventID, committedAtMS); err != nil {
			return false, err
		}
	}
	for eventID := range deletes {
		if _, err := private.Exec(`DELETE FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND event_id=?`, epoch, eventID); err != nil {
			return false, err
		}
	}
	return changed, nil
}

func deleteWindows(private storage.PrivateTx, epoch int64, keys []PrivateRowKey) (bool, error) {
	changed := false
	for _, key := range keys {
		result, err := private.Exec(`DELETE FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=?
			AND source_file_id=? AND file_generation=? AND source_start_offset=?`, epoch, key.SourceFileID, key.Generation, key.StartOffset)
		if err != nil {
			return false, err
		}
		count, err := result.RowsAffected()
		if err != nil {
			return false, err
		}
		changed = changed || count > 0
	}
	return changed, nil
}

func deleteHolds(private storage.PrivateTx, epoch int64, keys []HoldKey) (bool, error) {
	changed := false
	for _, key := range keys {
		result, err := private.Exec(`DELETE FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=?
			AND source_file_id=? AND file_generation=? AND event_id=?`, epoch, key.SourceFileID, key.Generation, key.EventID)
		if err != nil {
			return false, err
		}
		count, err := result.RowsAffected()
		if err != nil {
			return false, err
		}
		changed = changed || count > 0
	}
	return changed, nil
}

func writeWindows(private storage.PrivateTx, epoch int64, windows []WindowWrite) (bool, error) {
	changed := false
	for _, window := range windows {
		var oldEnd int64
		var oldOwner string
		var oldTurn, oldState sql.NullString
		err := private.QueryRow(`SELECT source_end_offset,owning_thread_id,turn_key,state_json FROM codex_usage_reconciliation_windows
			WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=? AND source_start_offset=?`,
			epoch, window.SourceFileID, window.Generation, window.StartOffset).Scan(&oldEnd, &oldOwner, &oldTurn, &oldState)
		if err != nil && err != sql.ErrNoRows {
			return false, err
		}
		newTurn := nullableStringValue(window.TurnKey)
		if err == sql.ErrNoRows || oldEnd != window.EndOffset || oldOwner != window.OwningThreadID ||
			!sqlNullableStringEqual(oldTurn, newTurn) || oldState.String != string(window.StateJSON) {
			changed = true
		}
		if _, err := private.Exec(`INSERT INTO codex_usage_reconciliation_windows(source,ledger_epoch,source_file_id,file_generation,
			source_start_offset,source_end_offset,owning_thread_id,turn_key,state_json) VALUES('codex',?,?,?,?,?,?,?,?)
			ON CONFLICT(source,ledger_epoch,source_file_id,file_generation,source_start_offset) DO UPDATE SET
			source_end_offset=excluded.source_end_offset,owning_thread_id=excluded.owning_thread_id,
			turn_key=excluded.turn_key,state_json=excluded.state_json
			WHERE source_end_offset<>excluded.source_end_offset OR owning_thread_id<>excluded.owning_thread_id
			OR turn_key IS NOT excluded.turn_key OR state_json<>excluded.state_json`, epoch, window.SourceFileID,
			window.Generation, window.StartOffset, window.EndOffset, window.OwningThreadID, newTurn, string(window.StateJSON)); err != nil {
			return false, err
		}
	}
	return changed, nil
}

func writeFacts(private storage.PrivateTx, epoch int64, facts []EventFactWrite) (bool, error) {
	changed := false
	for _, fact := range facts {
		var oldOwner, oldEvidence, oldOperation string
		var oldResponse sql.NullString
		err := private.QueryRow(`SELECT owning_thread_id,response_id,evidence_kind,operation FROM codex_usage_event_facts
			WHERE source='codex' AND ledger_epoch=? AND event_id=?`, epoch, fact.EventID).Scan(&oldOwner, &oldResponse, &oldEvidence, &oldOperation)
		if err != nil && err != sql.ErrNoRows {
			return false, err
		}
		response := nullableStringValue(fact.ResponseID)
		if err == sql.ErrNoRows || oldOwner != fact.OwningThreadID || !sqlNullableStringEqual(oldResponse, response) ||
			oldEvidence != evidenceName(fact.EvidenceKind) || oldOperation != operationName(fact.Operation) {
			changed = true
		}
		if _, err := private.Exec(`INSERT INTO codex_usage_event_facts(source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation)
			VALUES('codex',?,?,?,?,?,?) ON CONFLICT(source,ledger_epoch,event_id) DO UPDATE SET
			owning_thread_id=excluded.owning_thread_id,response_id=excluded.response_id,
			evidence_kind=excluded.evidence_kind,operation=excluded.operation
			WHERE owning_thread_id<>excluded.owning_thread_id OR response_id IS NOT excluded.response_id
			OR evidence_kind<>excluded.evidence_kind OR operation<>excluded.operation`, epoch, fact.EventID, fact.OwningThreadID,
			response, evidenceName(fact.EvidenceKind), operationName(fact.Operation)); err != nil {
			return false, err
		}
	}
	return changed, nil
}

func writeMarkers(private storage.PrivateTx, epoch int64, markers []CompactionMarkerWrite) (bool, error) {
	changed := false
	for _, marker := range markers {
		var old CompactionMarkerWrite
		var oldOccurred, oldModel, oldEffort, oldResponse, oldResolved, oldUnknown sql.NullString
		err := private.QueryRow(`SELECT source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,response_id,resolved_event_id,unknown_reason
			FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=? AND source_file_id=? AND file_generation=? AND source_start_offset=?`,
			epoch, marker.SourceFileID, marker.Generation, marker.StartOffset).Scan(&old.EndOffset, &old.OwningThreadID,
			&old.RootSessionID, &oldOccurred, &oldModel, &oldEffort, &oldResponse, &oldResolved, &oldUnknown)
		if err != nil && err != sql.ErrNoRows {
			return false, err
		}
		if err == sql.ErrNoRows || old.EndOffset != marker.EndOffset || old.OwningThreadID != marker.OwningThreadID ||
			old.RootSessionID != marker.RootSessionID || !sqlNullableStringEqual(oldOccurred, nullableIntString(marker.OccurredAtMS)) ||
			!sqlNullableStringEqual(oldModel, nullableStringValue(marker.Model)) || !sqlNullableStringEqual(oldEffort, nullableStringValue(marker.ReasoningEffort)) ||
			!sqlNullableStringEqual(oldResponse, nullableStringValue(marker.ResponseID)) || !sqlNullableStringEqual(oldResolved, nullableStringValue(marker.ResolvedEventID)) ||
			!sqlNullableStringEqual(oldUnknown, nullableMarkerReason(marker.UnknownReason)) {
			changed = true
		}
		if _, err := private.Exec(`INSERT INTO codex_compaction_markers(source,ledger_epoch,source_file_id,file_generation,source_start_offset,
			source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,response_id,resolved_event_id,unknown_reason)
			VALUES('codex',?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(source,ledger_epoch,source_file_id,file_generation,source_start_offset) DO UPDATE SET
			source_end_offset=excluded.source_end_offset,owning_thread_id=excluded.owning_thread_id,root_session_id=excluded.root_session_id,
			occurred_at_ms=excluded.occurred_at_ms,model=excluded.model,reasoning_effort=excluded.reasoning_effort,response_id=excluded.response_id,
			resolved_event_id=excluded.resolved_event_id,unknown_reason=excluded.unknown_reason`, epoch, marker.SourceFileID, marker.Generation,
			marker.StartOffset, marker.EndOffset, marker.OwningThreadID, marker.RootSessionID, nullableIntValue(marker.OccurredAtMS),
			nullableStringValue(marker.Model), nullableStringValue(marker.ReasoningEffort), nullableStringValue(marker.ResponseID),
			nullableStringValue(marker.ResolvedEventID), nullableMarkerReason(marker.UnknownReason)); err != nil {
			return false, err
		}
	}
	return changed, nil
}

func writeHolds(private storage.PrivateTx, epoch int64, holds []HoldWrite) (bool, error) {
	changed := false
	for _, hold := range holds {
		var oldReason string
		err := private.QueryRow(`SELECT hold_reason FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=?
			AND source_file_id=? AND file_generation=? AND event_id=?`, epoch, hold.SourceFileID, hold.Generation, hold.EventID).Scan(&oldReason)
		if err != nil && err != sql.ErrNoRows {
			return false, err
		}
		if err == sql.ErrNoRows || oldReason != string(hold.Reason) {
			changed = true
		}
		if _, err := private.Exec(`INSERT INTO codex_usage_event_holds(source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
			VALUES('codex',?,?,?,?,?) ON CONFLICT(source,ledger_epoch,source_file_id,file_generation,event_id) DO UPDATE SET
			hold_reason=excluded.hold_reason WHERE hold_reason<>excluded.hold_reason`, epoch, hold.SourceFileID, hold.Generation,
			hold.EventID, hold.Reason); err != nil {
			return false, err
		}
	}
	return changed, nil
}

func validateNoDeletedDependencies(private storage.PrivateTx, epoch int64, result ReconcileResult) error {
	deletedResponses := make(map[string]bool)
	for _, eventID := range result.DeleteEventIDs {
		var owner string
		var response sql.NullString
		if err := private.QueryRow(`SELECT owning_thread_id,response_id FROM codex_usage_event_facts
			WHERE source='codex' AND ledger_epoch=? AND event_id=?`, epoch, eventID).Scan(&owner, &response); err != nil && err != sql.ErrNoRows {
			return err
		} else if err == nil && response.Valid {
			deletedResponses[owner+"\x00"+response.String] = true
		}
		checks := []string{
			`SELECT count(*) FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=? AND resolved_event_id=?`,
			`SELECT count(*) FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=? AND event_id=?`,
			`SELECT count(*) FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=? AND event_id=?`,
		}
		for _, query := range checks {
			var count int
			if err := private.QueryRow(query, epoch, eventID).Scan(&count); err != nil {
				return err
			}
			if count != 0 {
				return fmt.Errorf("%w: event %q still has a durable dependency", ErrInvalidReconciliationPatch, eventID)
			}
		}
	}
	rows, err := private.Query(`SELECT owning_thread_id,state_json FROM codex_usage_reconciliation_windows
		WHERE source='codex' AND ledger_epoch=?`, epoch)
	if err != nil {
		return err
	}
	for rows.Next() {
		var owner, raw string
		if err := rows.Scan(&owner, &raw); err != nil {
			_ = rows.Close()
			return err
		}
		window, err := DecodeLegacyReconciliationWindow([]byte(raw))
		if err != nil {
			_ = rows.Close()
			return err
		}
		for _, eventID := range window.ProposalEventIDs {
			if containsString(result.DeleteEventIDs, eventID) {
				_ = rows.Close()
				return fmt.Errorf("%w: surviving window references deleted proposal %q", ErrInvalidReconciliationPatch, eventID)
			}
		}
		for _, responseID := range append(append([]string(nil), window.ExplicitResponseIDs...), window.LegacyCoveredResponseIDs...) {
			if deletedResponses[owner+"\x00"+responseID] {
				_ = rows.Close()
				return fmt.Errorf("%w: surviving window references deleted response %q", ErrInvalidReconciliationPatch, responseID)
			}
		}
	}
	if err := rows.Err(); err != nil {
		_ = rows.Close()
		return err
	}
	if err := rows.Close(); err != nil {
		return err
	}
	for _, marker := range result.MarkerUpserts {
		if marker.ResolvedEventID != nil && containsString(result.DeleteEventIDs, *marker.ResolvedEventID) {
			return ErrInvalidReconciliationPatch
		}
	}
	for _, hold := range result.HoldUpserts {
		if containsString(result.DeleteEventIDs, hold.EventID) {
			return ErrInvalidReconciliationPatch
		}
	}
	for _, fact := range result.Facts {
		if containsString(result.DeleteEventIDs, fact.EventID) {
			return ErrInvalidReconciliationPatch
		}
	}
	return nil
}

func evidenceName(value EvidenceKind) string {
	if value == EvidenceExplicit {
		return "explicit"
	}
	return "legacy"
}

func operationName(value Operation) string {
	if value == OperationCompaction {
		return "compaction"
	}
	return "response"
}

func nullableMarkerReason(value *MarkerUnknownReason) any {
	if value == nil {
		return nil
	}
	return string(*value)
}

func nullableIntString(value *int64) any {
	if value == nil {
		return nil
	}
	return strconv.FormatInt(*value, 10)
}

func sqlNullableStringEqual(old sql.NullString, value any) bool {
	if value == nil {
		return !old.Valid
	}
	switch item := value.(type) {
	case string:
		return old.Valid && old.String == item
	case int64:
		return old.Valid && old.String == strconv.FormatInt(item, 10)
	default:
		return false
	}
}

func stringSet(values []string) map[string]bool {
	result := make(map[string]bool, len(values))
	for _, value := range values {
		result[value] = true
	}
	return result
}
