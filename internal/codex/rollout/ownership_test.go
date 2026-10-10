package rollout

import (
	"testing"
)

const ownerTestID = "01900000-0000-7000-8000-000000000000"

func TestOwnershipSessionMetaAllowsSourceString(t *testing.T) {
	for _, source := range []string{"cli", "vscode"} {
		record := Record{LogicalStartOffset: 8, JSON: []byte(`{"type":"session_meta","payload":{"id":"` + ownerTestID + `","source":"` + source + `"}}`)}
		result := ClassifyRecord(record, OwningThreadCandidates{}, OwnershipState{}, OwnershipBoundary{})
		if result.Envelope != EnvelopeSessionMeta || result.Ownership.Kind != OwnershipOwning || result.Ownership.ThreadID != ownerTestID {
			t.Fatalf("source %q changed session_meta decoding: %#v", source, result)
		}
	}
}

func TestOwnershipTurnContextMissingTurnIDKeepsOwnershipWithoutMetadata(t *testing.T) {
	state := OwnershipState{OwningThreadID: ownerTestID, Phase: OwnershipPhaseOwningLive}
	boundary := OwnershipBoundary{Confidence: OwnershipConfidenceConfirmed}
	record := Record{LogicalStartOffset: 16, JSON: []byte(`{"timestamp":"2026-08-08T00:00:01Z","type":"turn_context","payload":{"model":"gpt-main","cwd":"/tmp/project"}}`)}
	result := ClassifyRecord(record, OwningThreadCandidates{}, state, boundary)
	if result.Ownership.Kind != OwnershipOwning || result.Ownership.ThreadID != ownerTestID || result.Metadata.LatestContextModel != nil || result.Metadata.CWD != nil || result.Metadata.LatestContextAtMS != nil {
		t.Fatalf("missing UUID turn_id must keep ownership without metadata: %#v", result)
	}
}

func TestOwnershipSessionMetaSourceStringDoesNotImplyMainRole(t *testing.T) {
	record := Record{LogicalStartOffset: 8, JSON: []byte(`{"type":"session_meta","payload":{"id":"` + ownerTestID + `","source":"cli"}}`)}
	result := ClassifyRecord(record, OwningThreadCandidates{}, OwnershipState{}, OwnershipBoundary{})
	if result.Ownership.Kind != OwnershipOwning || result.Metadata.AgentRoleHint != nil {
		t.Fatalf("source string must not imply main role: %#v", result)
	}
}

func TestOwnershipConflictingCandidatesRequireRebuild(t *testing.T) {
	candidates := OwningThreadCandidates{
		StateRolloutPath: &OwningThreadCandidate{ThreadID: ownerTestID, Confidence: CandidateConfidenceConfirmed},
		Filename:         &OwningThreadCandidate{ThreadID: "01900000-0000-7000-8000-000000000001", Confidence: CandidateConfidenceConfirmed},
	}
	record := Record{LogicalStartOffset: 12, JSON: []byte(`{"type":"turn_context","payload":{"turn_id":"01900000-0001-7000-8000-000000000000","model":"gpt-main"}}`)}
	result := ClassifyRecord(record, candidates, OwnershipState{}, OwnershipBoundary{})
	if result.Ownership.Kind != OwnershipUnknown || !result.NeedsRebuild || result.NextState.OwningThreadID != "" {
		t.Fatalf("conflicting ownership candidates were accepted: %#v", result)
	}
}

func TestOwnershipResponseMismatchingSafeThreadIDRequiresRebuild(t *testing.T) {
	state := OwnershipState{OwningThreadID: ownerTestID, Phase: OwnershipPhaseOwningLive}
	boundary := OwnershipBoundary{Confidence: OwnershipConfidenceConfirmed}
	record := Record{LogicalStartOffset: 24, JSON: []byte(`{"type":"token_usage_record","payload":{"thread_id":"different-safe-id"}}`)}
	result := ClassifyRecord(record, OwningThreadCandidates{}, state, boundary)
	if result.Ownership.Kind != OwnershipUnknown || !result.NeedsRebuild {
		t.Fatalf("safe non-UUID mismatch was treated as absent: %#v", result)
	}
}

func TestOwnershipIgnoresMalformedUnwatchedNestedValues(t *testing.T) {
	state := OwnershipState{OwningThreadID: ownerTestID, Phase: OwnershipPhaseOwningLive}
	boundary := OwnershipBoundary{Confidence: OwnershipConfidenceConfirmed}
	record := Record{LogicalStartOffset: 32, JSON: []byte(`{"type":"compacted","payload":{"latest_token_usage_record":["unwatched shape"]}}`)}
	result := ClassifyRecord(record, OwningThreadCandidates{}, state, boundary)
	if result.Envelope != EnvelopeCompacted || result.Ownership.Kind != OwnershipOwning || result.NeedsRebuild {
		t.Fatalf("unwatched nested value damaged envelope: %#v", result)
	}
}

func TestOwnershipReplayToOwningBoundary(t *testing.T) {
	state := OwnershipState{OwningThreadID: ownerTestID, Phase: OwnershipPhaseReplayedAncestor}
	boundary := OwnershipBoundary{Confidence: OwnershipConfidenceConfirmed}
	record := Record{LogicalStartOffset: 40, JSON: []byte(`{"type":"turn_context","payload":{"turn_id":"01900000-0001-7000-8000-000000000000","model":"new"}}`)}
	result := ClassifyRecord(record, OwningThreadCandidates{}, state, boundary)
	if result.Ownership.Kind != OwnershipOwning || result.NextState.Phase != OwnershipPhaseOwningLive || result.Boundary.OwningRecordsStartOffset == nil || *result.Boundary.OwningRecordsStartOffset != 40 {
		t.Fatalf("replay boundary was not resumed: %#v", result)
	}
}
