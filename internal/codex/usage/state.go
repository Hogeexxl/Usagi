package usage

import (
	"bytes"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

var (
	ErrInvalidReconciliationCarry            = errors.New("invalid reconciliation carry")
	ErrUnsupportedReconciliationCarryVersion = errors.New("unsupported reconciliation carry version")
)

func NewReconciliationCarry() ReconciliationCarry {
	return ReconciliationCarry{Version: 1, PendingResponseIDs: []string{}, PendingEvidence: []PendingUsageEvidence{}}
}

func CanonicalReconciliationCarryJSON(carry ReconciliationCarry) ([]byte, error) {
	if carry.Version != 1 {
		return nil, ErrUnsupportedReconciliationCarryVersion
	}
	if carry.OpenWindowStartOffset != nil && *carry.OpenWindowStartOffset > uint64(^uint64(0)>>1) {
		return nil, ErrInvalidReconciliationCarry
	}
	if carry.ModernCounterDomain != nil && (!validCarryIdentity(carry.ModernCounterDomain.ThreadID) ||
		(carry.ModernCounterDomain.SessionID != nil && !validCarryIdentity(*carry.ModernCounterDomain.SessionID))) {
		return nil, ErrInvalidReconciliationCarry
	}
	if carry.ModernCounterTotal != nil {
		if err := carry.ModernCounterTotal.Validate(); err != nil {
			return nil, ErrInvalidReconciliationCarry
		}
	}
	for _, responseID := range carry.PendingResponseIDs {
		if !validCarryIdentity(responseID) {
			return nil, ErrInvalidReconciliationCarry
		}
	}
	for _, pending := range carry.PendingEvidence {
		if err := validatePendingUsageEvidence(pending); err != nil {
			return nil, err
		}
	}

	responseIDs := append([]string(nil), carry.PendingResponseIDs...)
	sort.Strings(responseIDs)
	responseIDs = compactStrings(responseIDs)
	entries := append([]PendingUsageEvidence(nil), carry.PendingEvidence...)
	sort.SliceStable(entries, func(i, j int) bool {
		left, right := entries[i].Record, entries[j].Record
		if left.StartOffset != right.StartOffset {
			return left.StartOffset < right.StartOffset
		}
		return left.EndOffset < right.EndOffset
	})

	encoded := make([]byte, 0, 256)
	encoded = append(encoded, `{"version":`...)
	encoded = strconv.AppendUint(encoded, uint64(carry.Version), 10)
	encoded = append(encoded, `,"open_window_start_offset":`...)
	encoded = appendOptionalUint(encoded, carry.OpenWindowStartOffset)
	encoded = append(encoded, `,"pending_response_ids":[`...)
	for i, responseID := range responseIDs {
		if i > 0 {
			encoded = append(encoded, ',')
		}
		encoded = appendJSONString(encoded, responseID)
	}
	encoded = append(encoded, `],"modern_counter_domain":`...)
	encoded = appendCounterDomain(encoded, carry.ModernCounterDomain)
	encoded = append(encoded, `,"modern_counter_total":`...)
	if carry.ModernCounterTotal == nil {
		encoded = append(encoded, "null"...)
	} else {
		encoded = appendNormalizedUsage(encoded, *carry.ModernCounterTotal)
	}
	encoded = append(encoded, `,"pending_evidence":[`...)
	for i, pending := range entries {
		if i > 0 {
			encoded = append(encoded, ',')
		}
		encoded = appendPendingUsageEvidence(encoded, pending)
	}
	encoded = append(encoded, "]}"...)
	return encoded, nil
}

func DecodeReconciliationCarryJSON(data []byte) (ReconciliationCarry, error) {
	var carry ReconciliationCarry
	fields, err := decodeObject(data)
	if err != nil || !hasExactKeys(fields,
		"version", "open_window_start_offset", "pending_response_ids",
		"modern_counter_domain", "modern_counter_total", "pending_evidence") {
		return carry, ErrInvalidReconciliationCarry
	}
	var wire struct {
		Version               uint8             `json:"version"`
		OpenWindowStartOffset *uint64           `json:"open_window_start_offset"`
		PendingResponseIDs    []string          `json:"pending_response_ids"`
		ModernCounterDomain   json.RawMessage   `json:"modern_counter_domain"`
		ModernCounterTotal    json.RawMessage   `json:"modern_counter_total"`
		PendingEvidence       []json.RawMessage `json:"pending_evidence"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return carry, ErrInvalidReconciliationCarry
	}
	carry.Version = wire.Version
	carry.OpenWindowStartOffset = wire.OpenWindowStartOffset
	carry.PendingResponseIDs = wire.PendingResponseIDs
	carry.PendingEvidence = make([]PendingUsageEvidence, 0, len(wire.PendingEvidence))
	if !isJSONNull(wire.ModernCounterDomain) {
		domain, err := decodeCounterDomain(wire.ModernCounterDomain)
		if err != nil {
			return ReconciliationCarry{}, ErrInvalidReconciliationCarry
		}
		carry.ModernCounterDomain = domain
	}
	if !isJSONNull(wire.ModernCounterTotal) {
		usage, err := decodeNormalizedUsage(wire.ModernCounterTotal)
		if err != nil {
			return ReconciliationCarry{}, ErrInvalidReconciliationCarry
		}
		carry.ModernCounterTotal = &usage
	}
	for _, raw := range wire.PendingEvidence {
		pending, err := decodePendingUsageEvidence(raw)
		if err != nil {
			return ReconciliationCarry{}, ErrInvalidReconciliationCarry
		}
		carry.PendingEvidence = append(carry.PendingEvidence, pending)
	}
	if carry.Version != 1 {
		return ReconciliationCarry{}, ErrUnsupportedReconciliationCarryVersion
	}
	canonical, err := CanonicalReconciliationCarryJSON(carry)
	if err != nil {
		return ReconciliationCarry{}, err
	}
	if !bytes.Equal(canonical, data) {
		return ReconciliationCarry{}, ErrInvalidReconciliationCarry
	}
	return carry, nil
}

func decodePendingUsageEvidence(data []byte) (PendingUsageEvidence, error) {
	var pending PendingUsageEvidence
	fields, err := decodeObject(data)
	if err != nil || !hasExactKeys(fields, "record", "model", "reasoning_effort") {
		return pending, ErrInvalidReconciliationCarry
	}
	var wire struct {
		Record          json.RawMessage `json:"record"`
		Model           *string         `json:"model"`
		ReasoningEffort *string         `json:"reasoning_effort"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return pending, ErrInvalidReconciliationCarry
	}
	record, err := decodePendingEvidenceRecord(wire.Record)
	if err != nil {
		return pending, err
	}
	pending.Record = record
	pending.Model = wire.Model
	pending.ReasoningEffort = wire.ReasoningEffort
	return pending, nil
}

func decodePendingEvidenceRecord(data []byte) (PendingEvidenceRecord, error) {
	var record PendingEvidenceRecord
	fields, err := decodeObject(data)
	if err != nil {
		return record, ErrInvalidReconciliationCarry
	}
	var kind string
	if err := json.Unmarshal(fields["kind"], &kind); err != nil {
		return record, ErrInvalidReconciliationCarry
	}
	expected := []string{"kind", "timestamp_ms", "start_offset", "end_offset", "evidence"}
	if !hasExactKeys(fields, expected...) {
		return record, ErrInvalidReconciliationCarry
	}
	var wire struct {
		Kind        string          `json:"kind"`
		TimestampMS *int64          `json:"timestamp_ms"`
		StartOffset uint64          `json:"start_offset"`
		EndOffset   uint64          `json:"end_offset"`
		Evidence    json.RawMessage `json:"evidence"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return record, ErrInvalidReconciliationCarry
	}
	record = PendingEvidenceRecord{
		TimestampMS: wire.TimestampMS,
		StartOffset: wire.StartOffset,
		EndOffset:   wire.EndOffset,
	}
	switch wire.Kind {
	case string(PendingResponseUsage):
		evidence, err := decodeResponseEvidence(wire.Evidence)
		if err != nil {
			return PendingEvidenceRecord{}, err
		}
		record.Kind = PendingResponseUsage
		record.Response = &evidence
	case string(PendingCompacted):
		evidence, err := decodeCompactionEvidence(wire.Evidence)
		if err != nil {
			return PendingEvidenceRecord{}, err
		}
		record.Kind = PendingCompacted
		record.Compaction = &evidence
	default:
		return PendingEvidenceRecord{}, ErrInvalidReconciliationCarry
	}
	return record, nil
}

func decodeResponseEvidence(data []byte) (ResponseEvidence, error) {
	var wire struct {
		ResponseID       string          `json:"response_id"`
		ThreadID         *string         `json:"thread_id"`
		SessionID        *string         `json:"session_id"`
		TurnID           *string         `json:"turn_id"`
		Usage            json.RawMessage `json:"usage"`
		ThreadTokenUsage json.RawMessage `json:"thread_token_usage"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return ResponseEvidence{}, ErrInvalidReconciliationCarry
	}
	usage, err := decodeUsageValue(wire.Usage)
	if err != nil {
		return ResponseEvidence{}, err
	}
	threadUsage, err := decodeUsageValue(wire.ThreadTokenUsage)
	if err != nil {
		return ResponseEvidence{}, err
	}
	return ResponseEvidence{
		ResponseID:       wire.ResponseID,
		ThreadID:         stringValue(wire.ThreadID),
		SessionID:        stringValue(wire.SessionID),
		TurnID:           stringValue(wire.TurnID),
		Usage:            usage,
		ThreadTokenUsage: threadUsage,
	}, nil
}

func decodeCompactionEvidence(data []byte) (CompactionEvidence, error) {
	var wire struct {
		ResponseID *string         `json:"compaction_response_id"`
		Latest     json.RawMessage `json:"latest_token_usage_record"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return CompactionEvidence{}, ErrInvalidReconciliationCarry
	}
	evidence := CompactionEvidence{ResponseID: stringValue(wire.ResponseID)}
	if !isJSONNull(wire.Latest) {
		latest, err := decodeResponseEvidence(wire.Latest)
		if err != nil {
			return CompactionEvidence{}, err
		}
		evidence.Latest = &latest
	}
	return evidence, nil
}

func decodeUsageValue(data []byte) (UsageValue, error) {
	fields, err := decodeObject(data)
	if err != nil {
		return UsageValue{}, ErrInvalidReconciliationCarry
	}
	var kind string
	if err := json.Unmarshal(fields["kind"], &kind); err != nil {
		return UsageValue{}, ErrInvalidReconciliationCarry
	}
	switch kind {
	case "missing":
		return UsageValue{State: UsageValueMissing}, nil
	case "invalid":
		return UsageValue{State: UsageValueInvalid}, nil
	case "valid":
		var wire struct {
			Usage json.RawMessage `json:"usage"`
		}
		if err := json.Unmarshal(data, &wire); err != nil {
			return UsageValue{}, ErrInvalidReconciliationCarry
		}
		usage, err := decodeNormalizedUsage(wire.Usage)
		if err != nil {
			return UsageValue{}, err
		}
		return UsageValue{State: UsageValueValid, Value: usage}, nil
	default:
		return UsageValue{}, ErrInvalidReconciliationCarry
	}
}

func decodeNormalizedUsage(data []byte) (sharedusage.NormalizedTokenUsage, error) {
	var wire struct {
		InputTokens      int64  `json:"input_tokens"`
		CachedTokens     int64  `json:"cached_tokens"`
		CacheWriteTokens *int64 `json:"cache_write_tokens"`
		OutputTokens     int64  `json:"output_tokens"`
		ReasoningTokens  int64  `json:"reasoning_tokens"`
		TotalTokens      int64  `json:"total_tokens"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return sharedusage.NormalizedTokenUsage{}, ErrInvalidReconciliationCarry
	}
	return sharedusage.NormalizedTokenUsage{
		InputTokens:      wire.InputTokens,
		CachedTokens:     wire.CachedTokens,
		CacheWriteTokens: wire.CacheWriteTokens,
		OutputTokens:     wire.OutputTokens,
		ReasoningTokens:  wire.ReasoningTokens,
		TotalTokens:      wire.TotalTokens,
	}, nil
}

func decodeCounterDomain(data []byte) (*ModernCounterDomain, error) {
	var tuple []json.RawMessage
	if err := json.Unmarshal(data, &tuple); err != nil || len(tuple) != 2 {
		return nil, ErrInvalidReconciliationCarry
	}
	var domain ModernCounterDomain
	if err := json.Unmarshal(tuple[0], &domain.ThreadID); err != nil {
		return nil, ErrInvalidReconciliationCarry
	}
	if !isJSONNull(tuple[1]) {
		var sessionID string
		if err := json.Unmarshal(tuple[1], &sessionID); err != nil {
			return nil, ErrInvalidReconciliationCarry
		}
		domain.SessionID = &sessionID
	}
	return &domain, nil
}

func validatePendingUsageEvidence(pending PendingUsageEvidence) error {
	if (pending.Model != nil && !validCarryIdentity(*pending.Model)) ||
		(pending.ReasoningEffort != nil && !validCarryIdentity(*pending.ReasoningEffort)) {
		return ErrInvalidReconciliationCarry
	}
	if pending.Record.StartOffset >= pending.Record.EndOffset || pending.Record.EndOffset > uint64(^uint64(0)>>1) {
		return ErrInvalidReconciliationCarry
	}
	switch pending.Record.Kind {
	case PendingResponseUsage:
		if pending.Record.Response == nil || pending.Record.Compaction != nil || validateResponseEvidence(*pending.Record.Response) != nil {
			return ErrInvalidReconciliationCarry
		}
	case PendingCompacted:
		if pending.Record.Compaction == nil || pending.Record.Response != nil || validateCompactionEvidence(*pending.Record.Compaction) != nil {
			return ErrInvalidReconciliationCarry
		}
	default:
		return ErrInvalidReconciliationCarry
	}
	return nil
}

func validateResponseEvidence(evidence ResponseEvidence) error {
	if !validCarryIdentity(evidence.ResponseID) ||
		(evidence.ThreadID != "" && !validCarryIdentity(evidence.ThreadID)) ||
		(evidence.SessionID != "" && !validCarryIdentity(evidence.SessionID)) ||
		(evidence.TurnID != "" && !validCarryIdentity(evidence.TurnID)) ||
		validateUsageValue(evidence.Usage) != nil || validateUsageValue(evidence.ThreadTokenUsage) != nil {
		return ErrInvalidReconciliationCarry
	}
	return nil
}

func validateCompactionEvidence(evidence CompactionEvidence) error {
	if evidence.ResponseID != "" && !validCarryIdentity(evidence.ResponseID) {
		return ErrInvalidReconciliationCarry
	}
	if evidence.Latest != nil && validateResponseEvidence(*evidence.Latest) != nil {
		return ErrInvalidReconciliationCarry
	}
	return nil
}

func validateUsageValue(value UsageValue) error {
	switch value.State {
	case UsageValueMissing, UsageValueInvalid:
		return nil
	case UsageValueValid:
		if err := value.Value.Validate(); err != nil {
			return ErrInvalidReconciliationCarry
		}
		return nil
	default:
		return ErrInvalidReconciliationCarry
	}
}

func appendPendingUsageEvidence(dst []byte, pending PendingUsageEvidence) []byte {
	dst = append(dst, `{"record":`...)
	dst = appendPendingEvidenceRecord(dst, pending.Record)
	dst = append(dst, `,"model":`...)
	dst = appendOptionalString(dst, pending.Model)
	dst = append(dst, `,"reasoning_effort":`...)
	dst = appendOptionalString(dst, pending.ReasoningEffort)
	return append(dst, '}')
}

func appendPendingEvidenceRecord(dst []byte, record PendingEvidenceRecord) []byte {
	dst = append(dst, `{"kind":`...)
	dst = appendJSONString(dst, string(record.Kind))
	dst = append(dst, `,"timestamp_ms":`...)
	dst = appendOptionalInt(dst, record.TimestampMS)
	dst = append(dst, `,"start_offset":`...)
	dst = strconv.AppendUint(dst, record.StartOffset, 10)
	dst = append(dst, `,"end_offset":`...)
	dst = strconv.AppendUint(dst, record.EndOffset, 10)
	dst = append(dst, `,"evidence":`...)
	switch record.Kind {
	case PendingResponseUsage:
		dst = appendResponseEvidence(dst, *record.Response)
	case PendingCompacted:
		dst = appendCompactionEvidence(dst, *record.Compaction)
	}
	return append(dst, '}')
}

func appendResponseEvidence(dst []byte, evidence ResponseEvidence) []byte {
	dst = append(dst, `{"response_id":`...)
	dst = appendJSONString(dst, evidence.ResponseID)
	dst = append(dst, `,"thread_id":`...)
	dst = appendNullableString(dst, evidence.ThreadID)
	dst = append(dst, `,"session_id":`...)
	dst = appendNullableString(dst, evidence.SessionID)
	dst = append(dst, `,"turn_id":`...)
	dst = appendNullableString(dst, evidence.TurnID)
	dst = append(dst, `,"usage":`...)
	dst = appendUsageValue(dst, evidence.Usage)
	dst = append(dst, `,"thread_token_usage":`...)
	dst = appendUsageValue(dst, evidence.ThreadTokenUsage)
	return append(dst, '}')
}

func appendCompactionEvidence(dst []byte, evidence CompactionEvidence) []byte {
	dst = append(dst, `{"compaction_response_id":`...)
	dst = appendNullableString(dst, evidence.ResponseID)
	dst = append(dst, `,"latest_token_usage_record":`...)
	if evidence.Latest == nil {
		dst = append(dst, "null"...)
	} else {
		dst = appendResponseEvidence(dst, *evidence.Latest)
	}
	return append(dst, '}')
}

func appendUsageValue(dst []byte, value UsageValue) []byte {
	switch value.State {
	case UsageValueMissing:
		return append(dst, `{"kind":"missing"}`...)
	case UsageValueInvalid:
		return append(dst, `{"kind":"invalid"}`...)
	default:
		dst = append(dst, `{"kind":"valid","usage":`...)
		dst = appendNormalizedUsage(dst, value.Value)
		return append(dst, '}')
	}
}

func appendNormalizedUsage(dst []byte, value sharedusage.NormalizedTokenUsage) []byte {
	dst = append(dst, `{"input_tokens":`...)
	dst = strconv.AppendInt(dst, value.InputTokens, 10)
	dst = append(dst, `,"cached_tokens":`...)
	dst = strconv.AppendInt(dst, value.CachedTokens, 10)
	dst = append(dst, `,"cache_write_tokens":`...)
	dst = appendOptionalInt(dst, value.CacheWriteTokens)
	dst = append(dst, `,"output_tokens":`...)
	dst = strconv.AppendInt(dst, value.OutputTokens, 10)
	dst = append(dst, `,"reasoning_tokens":`...)
	dst = strconv.AppendInt(dst, value.ReasoningTokens, 10)
	dst = append(dst, `,"total_tokens":`...)
	dst = strconv.AppendInt(dst, value.TotalTokens, 10)
	return append(dst, '}')
}

func appendCounterDomain(dst []byte, domain *ModernCounterDomain) []byte {
	if domain == nil {
		return append(dst, "null"...)
	}
	dst = append(dst, '[')
	dst = appendJSONString(dst, domain.ThreadID)
	dst = append(dst, ',')
	if domain.SessionID == nil {
		dst = append(dst, "null"...)
	} else {
		dst = appendJSONString(dst, *domain.SessionID)
	}
	return append(dst, ']')
}

func appendOptionalUint(dst []byte, value *uint64) []byte {
	if value == nil {
		return append(dst, "null"...)
	}
	return strconv.AppendUint(dst, *value, 10)
}

func appendOptionalInt(dst []byte, value *int64) []byte {
	if value == nil {
		return append(dst, "null"...)
	}
	return strconv.AppendInt(dst, *value, 10)
}

func appendOptionalString(dst []byte, value *string) []byte {
	if value == nil {
		return append(dst, "null"...)
	}
	return appendJSONString(dst, *value)
}

func appendNullableString(dst []byte, value string) []byte {
	if value == "" {
		return append(dst, "null"...)
	}
	return appendJSONString(dst, value)
}

func appendJSONString(dst []byte, value string) []byte {
	dst = append(dst, '"')
	for i := 0; i < len(value); i++ {
		b := value[i]
		switch b {
		case '"', '\\':
			dst = append(dst, '\\', b)
		case '\b':
			dst = append(dst, `\b`...)
		case '\f':
			dst = append(dst, `\f`...)
		case '\n':
			dst = append(dst, `\n`...)
		case '\r':
			dst = append(dst, `\r`...)
		case '\t':
			dst = append(dst, `\t`...)
		default:
			if b < 0x20 {
				dst = append(dst, `\u00`...)
				dst = append(dst, "0123456789abcdef"[b>>4], "0123456789abcdef"[b&0x0f])
			} else {
				dst = append(dst, b)
			}
		}
	}
	return append(dst, '"')
}

func decodeObject(data []byte) (map[string]json.RawMessage, error) {
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(data, &fields); err != nil || fields == nil {
		return nil, ErrInvalidReconciliationCarry
	}
	return fields, nil
}

func hasExactKeys(fields map[string]json.RawMessage, expected ...string) bool {
	if len(fields) != len(expected) {
		return false
	}
	for _, key := range expected {
		if _, ok := fields[key]; !ok {
			return false
		}
	}
	return true
}

func isJSONNull(data []byte) bool {
	return bytes.Equal(bytes.TrimSpace(data), []byte("null"))
}

func stringValue(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}

func validCarryIdentity(value string) bool {
	if strings.TrimSpace(value) == "" || !utf8.ValidString(value) {
		return false
	}
	for _, r := range value {
		if unicode.IsControl(r) {
			return false
		}
	}
	return true
}

func compactStrings(values []string) []string {
	if len(values) < 2 {
		return values
	}
	write := 1
	for read := 1; read < len(values); read++ {
		if values[read] == values[write-1] {
			continue
		}
		values[write] = values[read]
		write++
	}
	return values[:write]
}

func LoadReconciliationCarry(raw []byte) (ReconciliationCarry, error) {
	if len(raw) == 0 {
		return NewReconciliationCarry(), nil
	}
	return DecodeReconciliationCarryJSON(raw)
}

func SetReconciliationCarry(state *SourceState, carry ReconciliationCarry) error {
	if state == nil {
		return fmt.Errorf("source state is nil")
	}
	encoded, err := CanonicalReconciliationCarryJSON(carry)
	if err != nil {
		return err
	}
	state.ReconciliationStateJSON = encoded
	return nil
}

func loadCarryJSON(value sql.NullString) ([]byte, error) {
	if !value.Valid {
		return nil, ErrInvalidReconciliationCarry
	}
	return []byte(value.String), nil
}

type CounterState struct {
	Source   SourceState
	OpenTurn *TurnWrite
	Carry    ReconciliationCarry
}

func CounterStateFromSourceState(state SourceState) (CounterState, error) {
	carry, err := LoadReconciliationCarry(state.ReconciliationStateJSON)
	if err != nil {
		return CounterState{}, err
	}
	return CounterState{Source: state, Carry: carry}, nil
}

func LoadCounterState(
	tx *source.WriteTx,
	target source.UsageWriteTarget,
	sourceFileID int64,
	generation int64,
) (CounterState, bool, error) {
	var loaded CounterState
	found := false
	if sourceFileID <= 0 || generation <= 0 {
		return loaded, false, fmt.Errorf("usage source identity must be positive")
	}
	epoch, err := tx.ResolveUsageWriteEpoch(target)
	if err != nil {
		return loaded, false, err
	}
	err = tx.Private(func(private storage.PrivateTx) error {
		var state SourceState
		var rawTailStart sql.NullInt64
		var previousInput, previousCached, previousCacheWrite sql.NullInt64
		var previousOutput, previousReasoning, previousTotal sql.NullInt64
		var previousFingerprint []byte
		var previousOffset sql.NullInt64
		var blockReason sql.NullString
		var activeTurnKey, activeModel, activeEffort sql.NullString
		var activeModelOffset, activeEffortOffset sql.NullInt64
		var rawCarry sql.NullString
		err := private.QueryRow(`
			SELECT file_generation,device_id,inode,usage_parser_version,canonical_algorithm_version,
			       resolved_through_offset,observed_raw_size,raw_tail_status,raw_tail_start_offset,
			       owning_thread_id,root_session_id,continuation_state,
			       previous_total_input_tokens,previous_total_cached_tokens,previous_total_cache_write_tokens,
			       previous_total_output_tokens,previous_total_reasoning_tokens,previous_total_total_tokens,
			       previous_total_fingerprint,previous_total_offset,chain_state,chain_block_reason,
			       active_turn_key,active_model,active_model_offset,active_reasoning_effort,
			       active_reasoning_effort_offset,updated_at_ms,reconciliation_state_json
			FROM codex_usage_source_states WHERE ledger_epoch=? AND source_file_id=?`, epoch, sourceFileID).Scan(
			&state.Generation, &state.DeviceID, &state.Inode, &state.UsageParserVersion,
			&state.CanonicalAlgorithmVersion, &state.ResolvedThroughOffset, &state.ObservedRawSize,
			&state.RawTailStatus, &rawTailStart, &state.OwningThreadID, &state.RootSessionID,
			&state.ContinuationState, &previousInput, &previousCached, &previousCacheWrite,
			&previousOutput, &previousReasoning, &previousTotal, &previousFingerprint, &previousOffset,
			&state.ChainState, &blockReason, &activeTurnKey, &activeModel, &activeModelOffset,
			&activeEffort, &activeEffortOffset, &state.UpdatedAtMS, &rawCarry,
		)
		if errors.Is(err, sql.ErrNoRows) {
			return nil
		}
		if err != nil {
			return err
		}
		if state.Generation != generation {
			return fmt.Errorf("usage source state generation mismatch")
		}
		state.SourceFileID = sourceFileID
		state.RawTailStartOffset = nullableIntPointer(rawTailStart)
		if blockReason.Valid {
			reason := ChainBlockReason(blockReason.String)
			state.ChainBlockReason = &reason
		}
		state.ActiveTurnKey = nullableStringPointer(activeTurnKey)
		state.ActiveModel = nullableStringPointer(activeModel)
		state.ActiveModelOffset = nullableIntPointer(activeModelOffset)
		state.ActiveReasoningEffort = nullableStringPointer(activeEffort)
		state.ActiveReasoningEffortOffset = nullableIntPointer(activeEffortOffset)
		carryJSON, err := loadCarryJSON(rawCarry)
		if err != nil {
			return err
		}
		carry, err := DecodeReconciliationCarryJSON(carryJSON)
		if err != nil {
			return err
		}
		previous, err := decodePreviousTotal(
			previousInput, previousCached, previousCacheWrite, previousOutput,
			previousReasoning, previousTotal, previousFingerprint, previousOffset,
		)
		if err != nil {
			return err
		}
		state.PreviousTotal = previous
		state.PreviousTotalOffset = nullableIntPointer(previousOffset)
		if err := validateSourceState(state); err != nil {
			return err
		}
		openTurn, err := loadOpenTurn(private, epoch, state)
		if err != nil {
			return err
		}
		loaded = CounterState{Source: state, OpenTurn: openTurn, Carry: carry}
		found = true
		return nil
	})
	return loaded, found, err
}

func WriteSourceState(tx *source.WriteTx, target source.UsageWriteTarget, state SourceState) error {
	epoch, err := tx.ResolveUsageWriteEpoch(target)
	if err != nil {
		return err
	}
	carry, err := LoadReconciliationCarry(state.ReconciliationStateJSON)
	if err != nil {
		return err
	}
	carryJSON, err := CanonicalReconciliationCarryJSON(carry)
	if err != nil {
		return err
	}
	state.ReconciliationStateJSON = carryJSON
	if err := validateSourceState(state); err != nil {
		return err
	}
	var previousInput, previousCached, previousCacheWrite any
	var previousOutput, previousReasoning, previousTotal any
	var previousFingerprint, previousOffset any
	if state.PreviousTotal != nil {
		value := state.PreviousTotal
		fingerprint := UsageFingerprint(*value)
		previousInput = value.InputTokens
		previousCached = value.CachedTokens
		previousCacheWrite = nullableIntValue(value.CacheWriteTokens)
		previousOutput = value.OutputTokens
		previousReasoning = value.ReasoningTokens
		previousTotal = value.TotalTokens
		previousFingerprint = fingerprint[:]
		previousOffset = *state.PreviousTotalOffset
	}
	return tx.Private(func(private storage.PrivateTx) error {
		_, err := private.Exec(`
			INSERT INTO codex_usage_source_states (
				ledger_epoch,source_file_id,file_generation,device_id,inode,
				usage_parser_version,canonical_algorithm_version,resolved_through_offset,
				observed_raw_size,raw_tail_status,raw_tail_start_offset,owning_thread_id,
				root_session_id,continuation_state,previous_total_input_tokens,
				previous_total_cached_tokens,previous_total_cache_write_tokens,
				previous_total_output_tokens,previous_total_reasoning_tokens,
				previous_total_total_tokens,previous_total_fingerprint,previous_total_offset,
				chain_state,chain_block_reason,active_turn_key,active_model,active_model_offset,
				active_reasoning_effort,active_reasoning_effort_offset,updated_at_ms,
				reconciliation_state_json
			) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
			ON CONFLICT(ledger_epoch,source_file_id) DO UPDATE SET
				file_generation=excluded.file_generation,device_id=excluded.device_id,inode=excluded.inode,
				usage_parser_version=excluded.usage_parser_version,
				canonical_algorithm_version=excluded.canonical_algorithm_version,
				resolved_through_offset=excluded.resolved_through_offset,
				observed_raw_size=excluded.observed_raw_size,raw_tail_status=excluded.raw_tail_status,
				raw_tail_start_offset=excluded.raw_tail_start_offset,
				owning_thread_id=excluded.owning_thread_id,root_session_id=excluded.root_session_id,
				continuation_state=excluded.continuation_state,
				previous_total_input_tokens=excluded.previous_total_input_tokens,
				previous_total_cached_tokens=excluded.previous_total_cached_tokens,
				previous_total_cache_write_tokens=excluded.previous_total_cache_write_tokens,
				previous_total_output_tokens=excluded.previous_total_output_tokens,
				previous_total_reasoning_tokens=excluded.previous_total_reasoning_tokens,
				previous_total_total_tokens=excluded.previous_total_total_tokens,
				previous_total_fingerprint=excluded.previous_total_fingerprint,
				previous_total_offset=excluded.previous_total_offset,chain_state=excluded.chain_state,
				chain_block_reason=excluded.chain_block_reason,active_turn_key=excluded.active_turn_key,
				active_model=excluded.active_model,active_model_offset=excluded.active_model_offset,
				active_reasoning_effort=excluded.active_reasoning_effort,
				active_reasoning_effort_offset=excluded.active_reasoning_effort_offset,
				updated_at_ms=excluded.updated_at_ms,reconciliation_state_json=excluded.reconciliation_state_json`,
			epoch, state.SourceFileID, state.Generation, state.DeviceID, state.Inode,
			state.UsageParserVersion, state.CanonicalAlgorithmVersion, state.ResolvedThroughOffset,
			state.ObservedRawSize, state.RawTailStatus, nullableIntValue(state.RawTailStartOffset),
			state.OwningThreadID, state.RootSessionID, state.ContinuationState,
			previousInput, previousCached, previousCacheWrite, previousOutput, previousReasoning,
			previousTotal, previousFingerprint, previousOffset, state.ChainState,
			nullableChainBlock(state.ChainBlockReason), nullableStringValue(state.ActiveTurnKey),
			nullableStringValue(state.ActiveModel), nullableIntValue(state.ActiveModelOffset),
			nullableStringValue(state.ActiveReasoningEffort), nullableIntValue(state.ActiveReasoningEffortOffset),
			state.UpdatedAtMS, string(carryJSON),
		)
		return err
	})
}

func decodePreviousTotal(
	input, cached, cacheWrite, output, reasoning, total sql.NullInt64,
	fingerprint []byte,
	offset sql.NullInt64,
) (*sharedusage.NormalizedTokenUsage, error) {
	required := []sql.NullInt64{input, cached, output, reasoning, total}
	allMissing := true
	allPresent := true
	for _, value := range required {
		allMissing = allMissing && !value.Valid
		allPresent = allPresent && value.Valid
	}
	if allMissing && !cacheWrite.Valid && len(fingerprint) == 0 && !offset.Valid {
		return nil, nil
	}
	if !allPresent || len(fingerprint) != 32 || !offset.Valid {
		return nil, ErrInvalidReconciliationCarry
	}
	value := sharedusage.NormalizedTokenUsage{
		InputTokens:      input.Int64,
		CachedTokens:     cached.Int64,
		CacheWriteTokens: nullableIntPointer(cacheWrite),
		OutputTokens:     output.Int64,
		ReasoningTokens:  reasoning.Int64,
		TotalTokens:      total.Int64,
	}
	if err := value.Validate(); err != nil {
		return nil, ErrInvalidReconciliationCarry
	}
	fingerprintValue := UsageFingerprint(value)
	if !bytes.Equal(fingerprint, fingerprintValue[:]) {
		return nil, ErrInvalidReconciliationCarry
	}
	return &value, nil
}

func validateSourceState(state SourceState) error {
	if state.SourceFileID <= 0 || state.Generation <= 0 || state.DeviceID < 0 || state.Inode < 0 ||
		state.UsageParserVersion != UsageParserVersion || state.CanonicalAlgorithmVersion != UsageCanonicalAlgorithmVersion ||
		state.ResolvedThroughOffset < 0 || state.ObservedRawSize < state.ResolvedThroughOffset || state.UpdatedAtMS < 0 ||
		!validCarryIdentity(state.OwningThreadID) || !validCarryIdentity(state.RootSessionID) {
		return ErrInvalidReconciliationCarry
	}
	if _, ok := CanonicalAlgorithmFor(state.UsageParserVersion); !ok {
		return ErrInvalidReconciliationCarry
	}
	switch state.RawTailStatus {
	case RawTailUnverified:
		if state.RawTailStartOffset != nil {
			return ErrInvalidReconciliationCarry
		}
	case RawTailNone:
		if state.RawTailStartOffset != nil || state.ResolvedThroughOffset != state.ObservedRawSize {
			return ErrInvalidReconciliationCarry
		}
	case RawTailHalfLine:
		if state.RawTailStartOffset == nil || *state.RawTailStartOffset != state.ResolvedThroughOffset || state.ResolvedThroughOffset >= state.ObservedRawSize {
			return ErrInvalidReconciliationCarry
		}
	default:
		return ErrInvalidReconciliationCarry
	}
	if state.PreviousTotal == nil {
		if state.PreviousTotalOffset != nil {
			return ErrInvalidReconciliationCarry
		}
	} else {
		if state.PreviousTotalOffset == nil || *state.PreviousTotalOffset < 0 || *state.PreviousTotalOffset > state.ResolvedThroughOffset || state.PreviousTotal.Validate() != nil {
			return ErrInvalidReconciliationCarry
		}
	}
	if (state.ActiveModel == nil) != (state.ActiveModelOffset == nil) ||
		(state.ActiveReasoningEffort == nil) != (state.ActiveReasoningEffortOffset == nil) {
		return ErrInvalidReconciliationCarry
	}
	if state.ActiveModel != nil && !validCarryIdentity(*state.ActiveModel) || state.ActiveReasoningEffort != nil && !validCarryIdentity(*state.ActiveReasoningEffort) {
		return ErrInvalidReconciliationCarry
	}
	if state.ActiveModelOffset != nil && *state.ActiveModelOffset < 0 ||
		state.ActiveReasoningEffortOffset != nil && *state.ActiveReasoningEffortOffset < 0 {
		return ErrInvalidReconciliationCarry
	}
	switch state.ContinuationState {
	case ContinuationReplayedAncestor, ContinuationOwningLive:
	default:
		return ErrInvalidReconciliationCarry
	}
	switch state.ChainState {
	case ChainContinuous:
		if state.ChainBlockReason != nil {
			return ErrInvalidReconciliationCarry
		}
	case ChainInterrupted:
		if state.ChainBlockReason == nil {
			return ErrInvalidReconciliationCarry
		}
	default:
		return ErrInvalidReconciliationCarry
	}
	if state.ChainBlockReason != nil {
		switch *state.ChainBlockReason {
		case ChainBlockMalformed, ChainBlockOversized, ChainBlockTotalInvalid, ChainBlockOwnershipGap, ChainBlockParserGap:
		default:
			return ErrInvalidReconciliationCarry
		}
	}
	if state.ActiveTurnKey != nil && !validCarryIdentity(*state.ActiveTurnKey) {
		return ErrInvalidReconciliationCarry
	}
	return nil
}

func nullableIntPointer(value sql.NullInt64) *int64 {
	if !value.Valid {
		return nil
	}
	copy := value.Int64
	return &copy
}

func nullableStringPointer(value sql.NullString) *string {
	if !value.Valid {
		return nil
	}
	copy := value.String
	return &copy
}

func nullableIntValue(value *int64) any {
	if value == nil {
		return nil
	}
	return *value
}

func nullableStringValue(value *string) any {
	if value == nil {
		return nil
	}
	return *value
}

func nullableChainBlock(value *ChainBlockReason) any {
	if value == nil {
		return nil
	}
	return *value
}
