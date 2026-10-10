package rollout

import (
	"bytes"
	"encoding/json"
	"path/filepath"
	"strings"
	"time"

	"github.com/google/uuid"
)

type EnvelopeKind uint8

const (
	EnvelopeMalformed EnvelopeKind = iota
	EnvelopeSessionMeta
	EnvelopeTurnContext
	EnvelopeTokenCount
	EnvelopeResponseUsage
	EnvelopeCompacted
	EnvelopeLifecycle
	EnvelopeIgnored
	EnvelopeUnknown
)

type CandidateConfidence uint8

const (
	CandidateConfidenceUnresolved CandidateConfidence = iota
	CandidateConfidenceCandidate
	CandidateConfidenceConfirmed
)

type OwningThreadCandidate struct {
	ThreadID   string
	Confidence CandidateConfidence
}

type OwningThreadCandidates struct {
	StateRolloutPath *OwningThreadCandidate
	Filename         *OwningThreadCandidate
}

type OwnershipConfidence uint8

const (
	OwnershipConfidenceUnresolved OwnershipConfidence = iota
	OwnershipConfidenceConfirmed
)

type OwnershipBoundary struct {
	ReplayStartOffset        *int64
	OwningRecordsStartOffset *int64
	Confidence               OwnershipConfidence
}

type RecordMetadataEvidence struct {
	ThreadID                  string
	CreatedAtMS               *int64
	CWD                       *MetadataStringCandidate
	LatestContextModel        *string
	LatestContextTurnID       *string
	LatestContextAtMS         *int64
	LatestContextRecordOffset *int64
	ParentThreadIDHint        *MetadataStringCandidate
	AgentRoleHint             *MetadataStringCandidate
	AgentPath                 *MetadataStringCandidate
	RelationshipConflict      bool
	HasConflict               bool
}

type MetadataStringCandidate struct {
	Value      string
	Provenance string
	Offset     int64
}

type RecordClassification struct {
	Envelope     EnvelopeKind
	Ownership    Ownership
	NextState    OwnershipState
	Boundary     OwnershipBoundary
	Metadata     RecordMetadataEvidence
	NeedsRebuild bool
}

func ClassifyGap(gap Gap, state OwnershipState, boundary OwnershipBoundary) Ownership {
	if boundary.Confidence != OwnershipConfidenceConfirmed {
		return Ownership{Kind: OwnershipUnknown}
	}
	switch state.Phase {
	case OwnershipPhaseOwningBootstrap, OwnershipPhaseOwningLive:
		return Ownership{Kind: OwnershipOwning, ThreadID: state.OwningThreadID}
	case OwnershipPhaseReplayedAncestor:
		return Ownership{Kind: OwnershipReplayedAncestor, ThreadID: state.OwningThreadID}
	default:
		return Ownership{Kind: OwnershipUnknown}
	}
}

func ClassifyRecord(record Record, candidates OwningThreadCandidates, state OwnershipState, boundary OwnershipBoundary) RecordClassification {
	classification := RecordClassification{NextState: state, Boundary: boundary}
	if classification.NextState.Phase > OwnershipPhaseOwningLive || classification.NextState.Phase == OwnershipPhaseAwaitOwningMeta && classification.NextState.OwningThreadID != "" {
		classification.NextState = OwnershipState{}
	}
	stateCandidate := normalizeOwnershipCandidate(candidates.StateRolloutPath)
	filenameCandidate := normalizeOwnershipCandidate(candidates.Filename)
	candidateConflict := stateCandidate != "" && filenameCandidate != "" && stateCandidate != filenameCandidate
	owner := classification.NextState.OwningThreadID
	if owner == "" && !candidateConflict {
		owner = stateCandidate
		if owner == "" {
			owner = filenameCandidate
		}
		if owner != "" {
			classification.NextState.OwningThreadID = owner
			classification.NextState.Phase = OwnershipPhaseOwningBootstrap
			confidence := CandidateConfidenceCandidate
			if stateCandidate != "" && candidates.StateRolloutPath != nil {
				confidence = candidates.StateRolloutPath.Confidence
			} else if filenameCandidate != "" && candidates.Filename != nil {
				confidence = candidates.Filename.Confidence
			}
			if confidence == CandidateConfidenceConfirmed {
				classification.Boundary.Confidence = OwnershipConfidenceConfirmed
			}
		}
	}
	if owner != "" && !candidateConflict && (stateCandidate != "" && stateCandidate != owner || filenameCandidate != "" && filenameCandidate != owner) {
		candidateConflict = true
	}
	if candidateConflict {
		classification.Ownership = Ownership{Kind: OwnershipUnknown}
		classification.NextState = OwnershipState{}
		classification.Boundary.Confidence = OwnershipConfidenceUnresolved
		classification.NeedsRebuild = true
		classification.Envelope = classifyRecordEnvelope(record.JSON)
		return classification
	}
	var envelope codexRecordEnvelope
	if err := json.Unmarshal(record.JSON, &envelope); err != nil || envelope.Type == "" {
		classification.Envelope = EnvelopeMalformed
		classification.Ownership = ownershipForState(classification.NextState, classification.Boundary)
		return classification
	}
	classification.Envelope = classifyEnvelope(envelope)
	payload := payloadObject(envelope.Payload)
	classification.Metadata = metadataEvidenceForEnvelope(record, envelope, payload, classification.Envelope)

	switch classification.Envelope {
	case EnvelopeSessionMeta:
		metaID := normalizeThreadID(payloadString(payload, "id"))
		if metaID == "" {
			classification.Ownership = Ownership{Kind: OwnershipUnknown}
			return classification
		}
		if classification.NextState.OwningThreadID == "" {
			classification.NextState.OwningThreadID = metaID
			classification.NextState.Phase = OwnershipPhaseOwningBootstrap
			classification.Boundary.Confidence = OwnershipConfidenceConfirmed
			setOffset(&classification.Boundary.OwningRecordsStartOffset, record.LogicalStartOffset)
			classification.Ownership = Ownership{Kind: OwnershipOwning, ThreadID: metaID}
			classification.Metadata.ThreadID = metaID
			return classification
		}
		if classification.NextState.OwningThreadID == metaID {
			if classification.NextState.Phase == OwnershipPhaseReplayedAncestor {
				classification.NextState.Phase = OwnershipPhaseOwningLive
				setOffset(&classification.Boundary.OwningRecordsStartOffset, record.LogicalStartOffset)
			}
			classification.Boundary.Confidence = OwnershipConfidenceConfirmed
			if classification.Boundary.OwningRecordsStartOffset == nil && classification.Boundary.ReplayStartOffset == nil {
				setOffset(&classification.Boundary.OwningRecordsStartOffset, record.LogicalStartOffset)
			}
			classification.Ownership = Ownership{Kind: OwnershipOwning, ThreadID: metaID}
			classification.Metadata.ThreadID = metaID
			return classification
		}
		classification.NextState.Phase = OwnershipPhaseReplayedAncestor
		setOffset(&classification.Boundary.ReplayStartOffset, record.LogicalStartOffset)
		classification.Ownership = Ownership{Kind: OwnershipReplayedAncestor, ThreadID: classification.NextState.OwningThreadID}
		classification.Metadata.ThreadID = classification.NextState.OwningThreadID
		return classification
	case EnvelopeTurnContext:
		if classification.NextState.Phase == OwnershipPhaseReplayedAncestor && turnBelongsToOwner(payloadString(payload, "turn_id"), classification.NextState.OwningThreadID) {
			classification.NextState.Phase = OwnershipPhaseOwningLive
			classification.Boundary.Confidence = OwnershipConfidenceConfirmed
			setOffset(&classification.Boundary.OwningRecordsStartOffset, record.LogicalStartOffset)
			classification.Ownership = Ownership{Kind: OwnershipOwning, ThreadID: classification.NextState.OwningThreadID}
		} else {
			classification.Ownership = ownershipForState(classification.NextState, classification.Boundary)
		}
	case EnvelopeResponseUsage, EnvelopeCompacted:
		classification.Ownership = ownershipForState(classification.NextState, classification.Boundary)
		responseThread := payloadString(payload, "thread_id")
		if classification.Envelope == EnvelopeCompacted {
			responseThread = nestedPayloadString(payload, "latest_token_usage_record", "thread_id")
		}
		responseThread = safeNonEmptyString(responseThread)
		if classification.Ownership.Kind == OwnershipOwning && responseThread != "" && responseThread != classification.NextState.OwningThreadID {
			classification.Ownership = Ownership{Kind: OwnershipUnknown}
			classification.NeedsRebuild = true
		}
	default:
		classification.Ownership = ownershipForState(classification.NextState, classification.Boundary)
	}
	if classification.Ownership.Kind == OwnershipUnknown {
		classification.Metadata = RecordMetadataEvidence{}
	} else if classification.Ownership.Kind == OwnershipReplayedAncestor {
		classification.Metadata = RecordMetadataEvidence{}
	} else if classification.Metadata.ThreadID == "" {
		classification.Metadata.ThreadID = classification.Ownership.ThreadID
	}
	return classification
}

func normalizeOwnershipCandidate(candidate *OwningThreadCandidate) string {
	if candidate == nil {
		return ""
	}
	return normalizeThreadID(candidate.ThreadID)
}

func normalizeThreadID(value string) string {
	parsed, err := uuid.Parse(strings.TrimSpace(value))
	if err != nil {
		return ""
	}
	return parsed.String()
}

func ownershipForState(state OwnershipState, boundary OwnershipBoundary) Ownership {
	if boundary.Confidence != OwnershipConfidenceConfirmed || state.OwningThreadID == "" {
		return Ownership{Kind: OwnershipUnknown}
	}
	switch state.Phase {
	case OwnershipPhaseOwningBootstrap, OwnershipPhaseOwningLive:
		return Ownership{Kind: OwnershipOwning, ThreadID: state.OwningThreadID}
	case OwnershipPhaseReplayedAncestor:
		return Ownership{Kind: OwnershipReplayedAncestor, ThreadID: state.OwningThreadID}
	default:
		return Ownership{Kind: OwnershipUnknown}
	}
}

func classifyRecordEnvelope(raw []byte) EnvelopeKind {
	var envelope codexRecordEnvelope
	if json.Unmarshal(raw, &envelope) != nil || envelope.Type == "" {
		return EnvelopeMalformed
	}
	return classifyEnvelope(envelope)
}

type codexRecordEnvelope struct {
	Type      string          `json:"type"`
	Timestamp json.RawMessage `json:"timestamp"`
	Payload   json.RawMessage `json:"payload"`
}

func classifyEnvelope(record codexRecordEnvelope) EnvelopeKind {
	payload := payloadObject(record.Payload)
	switch record.Type {
	case "session_meta":
		return EnvelopeSessionMeta
	case "turn_context":
		return EnvelopeTurnContext
	case "token_count":
		return EnvelopeTokenCount
	case "token_usage_record":
		return EnvelopeResponseUsage
	case "compacted":
		return EnvelopeCompacted
	case "lifecycle", "turn_started", "turn_completed", "turn_aborted":
		return EnvelopeLifecycle
	case "response_item", "ghost_snapshot":
		return EnvelopeIgnored
	case "event_msg":
		switch payloadString(payload, "type") {
		case "token_count":
			return EnvelopeTokenCount
		case "task_started", "task_complete", "turn_started", "turn_complete", "turn_aborted", "session_configured":
			return EnvelopeLifecycle
		case "user_message", "agent_message", "agent_reasoning", "raw_response_item", "context_compacted":
			return EnvelopeIgnored
		default:
			return EnvelopeUnknown
		}
	default:
		return EnvelopeUnknown
	}
}

func metadataEvidenceForEnvelope(record Record, envelope codexRecordEnvelope, payload map[string]json.RawMessage, kind EnvelopeKind) RecordMetadataEvidence {
	offset := record.LogicalStartOffset
	evidence := RecordMetadataEvidence{}
	switch kind {
	case EnvelopeSessionMeta:
		evidence.ThreadID = normalizeThreadID(payloadString(payload, "id"))
		created := parseRolloutTimestamp(payload["timestamp"])
		if created == nil {
			created = parseRolloutTimestamp(envelope.Timestamp)
		}
		evidence.CreatedAtMS = created
		evidence.CWD = stringCandidate(payloadString(payload, "cwd"), "session_meta", offset, normalizeAbsoluteMetadataPath)
		evidence.AgentPath = stringCandidate(payloadString(payload, "agent_path"), "session_meta", offset, normalizeAgentMetadataPath)
		if parent := normalizeThreadID(payloadString(payload, "parent_thread_id")); parent != "" {
			evidence.ParentThreadIDHint = &MetadataStringCandidate{Value: parent, Provenance: "session_meta_parent", Offset: offset}
		}
		subagent, spawn := sessionMetaSource(payload["source"])
		if spawn != nil {
			if parent := normalizeThreadID(spawn["parent_thread_id"]); parent != "" {
				evidence.ParentThreadIDHint = preferredParent(evidence.ParentThreadIDHint,
					MetadataStringCandidate{Value: parent, Provenance: "subagent_source", Offset: offset})
			}
			evidence.AgentPath = preferredAgentPath(evidence.AgentPath,
				stringCandidate(spawn["agent_path"], "thread_spawn", offset, normalizeAgentMetadataPath))
		}
		if roleValue := payloadString(payload, "agent_role"); roleValue != "" {
			role := strings.ToLower(strings.TrimSpace(roleValue))
			if role != "" && !hasControlCharacters(role) {
				evidence.AgentRoleHint = &MetadataStringCandidate{Value: role, Provenance: "session_meta_role", Offset: offset}
			}
		}
		if subagent {
			evidence.AgentRoleHint = &MetadataStringCandidate{Value: "subagent", Provenance: "subagent_source", Offset: offset}
			if parent := normalizeThreadID(payloadString(payload, "forked_from_id")); parent != "" {
				evidence.ParentThreadIDHint = preferredParent(evidence.ParentThreadIDHint,
					MetadataStringCandidate{Value: parent, Provenance: "forked_from_id", Offset: offset})
			}
		}
	case EnvelopeTurnContext:
		turnID := normalizeThreadID(payloadString(payload, "turn_id"))
		if turnID == "" {
			break
		}
		evidence.LatestContextTurnID = stringPointer(turnID)
		evidence.LatestContextAtMS = parseRolloutTimestamp(envelope.Timestamp)
		evidence.LatestContextModel = cleanedString(payloadString(payload, "model"))
		evidence.CWD = stringCandidate(payloadString(payload, "cwd"), "turn_context", offset, normalizeAbsoluteMetadataPath)
		if evidence.LatestContextModel != nil {
			evidence.LatestContextRecordOffset = int64Pointer(offset)
		}
	}
	return evidence
}

func stringCandidate(value, provenance string, offset int64, normalize func(string) string) *MetadataStringCandidate {
	value = normalize(value)
	if value == "" {
		return nil
	}
	return &MetadataStringCandidate{Value: value, Provenance: provenance, Offset: offset}
}

func payloadObject(raw json.RawMessage) map[string]json.RawMessage {
	var payload map[string]json.RawMessage
	if json.Unmarshal(raw, &payload) != nil {
		return nil
	}
	return payload
}

func payloadString(payload map[string]json.RawMessage, key string) string {
	var value string
	if json.Unmarshal(payload[key], &value) != nil {
		return ""
	}
	return value
}

func nestedPayloadString(payload map[string]json.RawMessage, key, nestedKey string) string {
	return payloadString(payloadObject(payload[key]), nestedKey)
}

func sessionMetaSource(raw json.RawMessage) (bool, map[string]string) {
	var source map[string]json.RawMessage
	if json.Unmarshal(raw, &source) != nil {
		return false, nil
	}
	subagent, ok := source["subagent"]
	if !ok {
		return false, nil
	}
	var subagentObject map[string]json.RawMessage
	if json.Unmarshal(subagent, &subagentObject) != nil || subagentObject == nil {
		return false, nil
	}
	spawn, ok := subagentObject["thread_spawn"]
	if !ok {
		return true, nil
	}
	var spawnObject map[string]json.RawMessage
	if json.Unmarshal(spawn, &spawnObject) != nil || spawnObject == nil {
		return true, nil
	}
	return true, map[string]string{
		"parent_thread_id": payloadString(spawnObject, "parent_thread_id"),
		"agent_path":       payloadString(spawnObject, "agent_path"),
	}
}

func safeNonEmptyString(value string) string {
	value = strings.TrimSpace(value)
	if value == "" || hasControlCharacters(value) {
		return ""
	}
	return value
}

func preferredParent(current *MetadataStringCandidate, incoming MetadataStringCandidate) *MetadataStringCandidate {
	priority := func(provenance string) int {
		switch provenance {
		case "session_meta_parent":
			return 3
		case "subagent_source":
			return 2
		case "forked_from_id":
			return 1
		default:
			return 0
		}
	}
	if current == nil || priority(incoming.Provenance) > priority(current.Provenance) {
		return &incoming
	}
	return current
}

func preferredAgentPath(current, incoming *MetadataStringCandidate) *MetadataStringCandidate {
	if current == nil || current.Provenance == "thread_spawn" && incoming != nil {
		return incoming
	}
	return current
}

func cleanedString(value string) *string {
	value = strings.TrimSpace(value)
	if value == "" || hasControlCharacters(value) {
		return nil
	}
	return &value
}

func normalizeAbsoluteMetadataPath(value string) string {
	value = strings.TrimSpace(value)
	if value == "" || hasControlCharacters(value) || !filepath.IsAbs(value) {
		return ""
	}
	clean := filepath.Clean(value)
	return clean
}

func normalizeAgentMetadataPath(value string) string {
	value = strings.TrimSpace(value)
	if value == "" || !strings.HasPrefix(value, "/root/") || strings.HasPrefix(value, "//") || hasControlCharacters(value) {
		return ""
	}
	components := make([]string, 0)
	for _, component := range strings.Split(value[1:], "/") {
		switch component {
		case "", ".":
		case "..":
			if len(components) == 0 {
				return ""
			}
			components = components[:len(components)-1]
		default:
			components = append(components, component)
		}
	}
	if len(components) == 0 {
		return ""
	}
	return "/" + strings.Join(components, "/")
}

func parseRolloutTimestamp(raw json.RawMessage) *int64 {
	if len(raw) == 0 || bytes.Equal(raw, []byte("null")) {
		return nil
	}
	var text string
	if json.Unmarshal(raw, &text) == nil {
		parsed, err := time.Parse(time.RFC3339Nano, strings.TrimSpace(text))
		if err != nil || parsed.UnixMilli() < 0 {
			return nil
		}
		millis := parsed.UnixMilli()
		return &millis
	}
	var number json.Number
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if decoder.Decode(&number) != nil {
		return nil
	}
	value, err := number.Int64()
	if err != nil || value < 0 {
		return nil
	}
	return &value
}

func turnBelongsToOwner(turnID, ownerID string) bool {
	turn, err := uuid.Parse(strings.TrimSpace(turnID))
	if err != nil || turn.Version() != 7 {
		return false
	}
	owner, err := uuid.Parse(strings.TrimSpace(ownerID))
	if err != nil || owner.Version() != 7 {
		return false
	}
	turnTimestamp := uuidTimestamp(turn)
	ownerTimestamp := uuidTimestamp(owner)
	return turnTimestamp >= ownerTimestamp
}

func uuidTimestamp(value uuid.UUID) uint64 {
	var result uint64
	for _, part := range value[:6] {
		result = result<<8 | uint64(part)
	}
	return result
}

func setOffset(target **int64, value int64) {
	if *target == nil {
		copy := value
		*target = &copy
	}
}

func int64Pointer(value int64) *int64    { return &value }
func stringPointer(value string) *string { return &value }

func hasControlCharacters(value string) bool {
	for _, r := range value {
		if r < 0x20 || r == 0x7f {
			return true
		}
	}
	return false
}
