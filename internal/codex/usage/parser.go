package usage

import (
	"bytes"
	"encoding/json"
	"strconv"
	"strings"
	"time"
	"unicode"

	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

type RawKind uint8

const (
	RawMalformed RawKind = iota
	RawUnknown
	RawIgnored
	RawResponseUsage
	RawCompacted
	RawTokenCount
	RawTurnContext
	RawLifecycle
	RawOversized
)

type LifecycleKind uint8

const (
	LifecycleStarted LifecycleKind = iota + 1
	LifecycleCompleted
	LifecycleAborted
	LifecycleFailed
)

type ParsedRecord struct {
	Kind            RawKind
	SourceFileID    int64
	Generation      int64
	StartOffset     int64
	EndOffset       int64
	GapKind         rollout.GapKind
	TimestampMS     *int64
	Response        *ResponseEvidence
	Compaction      *CompactionEvidence
	HasTokenInfo    bool
	Total           UsageValue
	Last            UsageValue
	TurnID          string
	Model           string
	ReasoningEffort string
	Lifecycle       LifecycleKind
}

func ParseRecord(record rollout.Record) ParsedRecord {
	parsed := ParsedRecord{
		SourceFileID: record.SourceFileID,
		Generation:   record.Generation,
		StartOffset:  record.LogicalStartOffset,
		EndOffset:    record.LogicalEndOffset,
		Kind:         RawMalformed,
	}
	if record.LogicalStartOffset < 0 || record.LogicalEndOffset <= record.LogicalStartOffset {
		return parsed
	}
	var object map[string]json.RawMessage
	if err := json.Unmarshal(record.JSON, &object); err != nil || object == nil {
		return parsed
	}
	outerTimestamp := parseTimestamp(object["timestamp"])
	typeName, ok := rawString(object["type"])
	if !ok {
		return parsed
	}
	payload := object["payload"]
	switch typeName {
	case "turn_context":
		var value map[string]json.RawMessage
		if json.Unmarshal(payload, &value) != nil || value == nil {
			return parsed
		}
		parsed.Kind = RawTurnContext
		parsed.TurnID = safeString(value["turn_id"])
		parsed.Model = safeString(value["model"])
		effort, present := value["effort"]
		if !present {
			effort = value["reasoning_effort"]
		}
		parsed.ReasoningEffort = normalizeEffort(effort)
		parsed.TimestampMS = preferTimestamp(value["timestamp"], outerTimestamp)
	case "event_msg":
		var value map[string]json.RawMessage
		if err := json.Unmarshal(payload, &value); err != nil || value == nil {
			return parsed
		}
		eventType, ok := rawString(value["type"])
		if !ok {
			return parsed
		}
		parsed.TimestampMS = preferTimestamp(value["timestamp"], outerTimestamp)
		switch eventType {
		case "token_count":
			parsed.Kind = RawTokenCount
			infoRaw, exists := value["info"]
			if !exists || bytes.Equal(bytes.TrimSpace(infoRaw), []byte("null")) {
				return parsed
			}
			var info map[string]json.RawMessage
			if json.Unmarshal(infoRaw, &info) != nil || info == nil {
				parsed.HasTokenInfo = true
				parsed.Total = UsageValue{State: UsageValueInvalid}
				parsed.Last = UsageValue{State: UsageValueMissing}
				return parsed
			}
			parsed.HasTokenInfo = true
			parsed.Total = parseSnapshot(info["total_token_usage"])
			lastRaw, hasLast := info["last_token_usage"]
			if !hasLast {
				parsed.Last = UsageValue{State: UsageValueMissing}
			} else {
				parsed.Last = parseSnapshot(lastRaw)
			}
		case "task_started", "turn_started":
			parsed.Kind = RawLifecycle
			parsed.Lifecycle = LifecycleStarted
			parsed.TurnID = safeString(value["turn_id"])
		case "task_complete", "turn_complete":
			parsed.Kind = RawLifecycle
			parsed.Lifecycle = LifecycleCompleted
			if errorRaw, exists := value["error"]; exists && !bytes.Equal(bytes.TrimSpace(errorRaw), []byte("null")) {
				parsed.Lifecycle = LifecycleFailed
			}
			parsed.TurnID = safeString(value["turn_id"])
		case "turn_aborted":
			parsed.Kind = RawLifecycle
			parsed.Lifecycle = LifecycleAborted
			parsed.TurnID = safeString(value["turn_id"])
		case "rate_limits":
			parsed.Kind = RawIgnored
		default:
			parsed.Kind = RawUnknown
		}
	case "token_usage_record":
		var value map[string]json.RawMessage
		if json.Unmarshal(payload, &value) != nil || value == nil {
			return parsed
		}
		response, valid := parseResponseEvidence(value)
		if !valid || response == nil {
			return parsed
		}
		parsed.Kind = RawResponseUsage
		parsed.Response = response
		parsed.TimestampMS = preferTimestamp(value["timestamp"], outerTimestamp)
	case "compacted":
		var value map[string]json.RawMessage
		if json.Unmarshal(payload, &value) != nil || value == nil {
			return parsed
		}
		compaction := &CompactionEvidence{ResponseID: safeString(value["compaction_response_id"])}
		latest, exists := value["latest_token_usage_record"]
		if exists && !bytes.Equal(bytes.TrimSpace(latest), []byte("null")) {
			var latestObject map[string]json.RawMessage
			if json.Unmarshal(latest, &latestObject) != nil || latestObject == nil {
				return parsed
			}
			if !validOptionalThreadID(latestObject) {
				return parsed
			}
			latestResponse, valid := parseResponseEvidence(latestObject)
			if !valid || latestResponse == nil {
				return parsed
			}
			compaction.Latest = latestResponse
		}
		parsed.Kind = RawCompacted
		parsed.Compaction = compaction
		parsed.TimestampMS = preferTimestamp(value["timestamp"], outerTimestamp)
	default:
		parsed.Kind = RawUnknown
	}
	return parsed
}

func ParseGap(gap rollout.Gap) ParsedRecord {
	kind := RawMalformed
	switch gap.Kind {
	case rollout.GapMalformed, rollout.GapParser, rollout.GapRequiredInvalid:
		kind = RawMalformed
	case rollout.GapOversized:
		kind = RawOversized
	case rollout.GapOwnership:
		kind = RawUnknown
	}
	return ParsedRecord{
		Kind:         kind,
		SourceFileID: gap.SourceFileID,
		Generation:   gap.Generation,
		StartOffset:  gap.LogicalStartOffset,
		EndOffset:    gap.LogicalEndOffset,
		GapKind:      gap.Kind,
	}
}

func parseResponseEvidence(object map[string]json.RawMessage) (*ResponseEvidence, bool) {
	if !validOptionalThreadID(object) {
		return nil, false
	}
	responseID := safeString(object["response_id"])
	if responseID == "" {
		return nil, true
	}
	usageRaw, hasUsage := object["usage"]
	threadUsageRaw, hasThreadUsage := object["thread_token_usage"]
	return &ResponseEvidence{
		ResponseID:       responseID,
		ThreadID:         safeString(object["thread_id"]),
		SessionID:        safeString(object["session_id"]),
		TurnID:           safeString(object["turn_id"]),
		Usage:            parseEvidenceUsage(usageRaw, hasUsage),
		ThreadTokenUsage: parseEvidenceUsage(threadUsageRaw, hasThreadUsage),
	}, true
}

func validOptionalThreadID(object map[string]json.RawMessage) bool {
	raw, exists := object["thread_id"]
	if !exists || bytes.Equal(bytes.TrimSpace(raw), []byte("null")) {
		return true
	}
	return safeString(raw) != ""
}

func parseEvidenceUsage(raw json.RawMessage, present bool) UsageValue {
	if !present {
		return UsageValue{State: UsageValueMissing}
	}
	return parseSnapshot(raw)
}

func parseSnapshot(raw json.RawMessage) UsageValue {
	if len(raw) == 0 {
		return UsageValue{State: UsageValueInvalid}
	}
	var object map[string]json.RawMessage
	if json.Unmarshal(raw, &object) != nil || object == nil {
		return UsageValue{State: UsageValueInvalid}
	}
	read := func(name string) (int64, bool) {
		value, exists := object[name]
		if !exists {
			return 0, false
		}
		number, ok := rawInteger(value)
		return number, ok
	}
	input, inputOK := read("input_tokens")
	cached, cachedOK := read("cached_input_tokens")
	output, outputOK := read("output_tokens")
	reasoning, reasoningOK := read("reasoning_output_tokens")
	total, totalOK := read("total_tokens")
	if !inputOK || !cachedOK || !outputOK || !reasoningOK || !totalOK {
		return UsageValue{State: UsageValueInvalid}
	}
	var cacheWrite *int64
	if rawValue, exists := object["cache_write_input_tokens"]; exists {
		value, ok := rawInteger(rawValue)
		if !ok {
			return UsageValue{State: UsageValueInvalid}
		}
		cacheWrite = &value
	}
	value, err := sharedusage.NewNormalizedTokenUsage(input, cached, cacheWrite, output, reasoning, total)
	if err != nil {
		return UsageValue{State: UsageValueInvalid}
	}
	return UsageValue{State: UsageValueValid, Value: value}
}

func rawInteger(raw json.RawMessage) (int64, bool) {
	text := strings.TrimSpace(string(raw))
	if text == "" || strings.ContainsAny(text, ".eE") {
		return 0, false
	}
	value, err := strconv.ParseInt(text, 10, 64)
	return value, err == nil && value >= 0
}

func rawString(raw json.RawMessage) (string, bool) {
	var value string
	if len(raw) == 0 || json.Unmarshal(raw, &value) != nil {
		return "", false
	}
	return value, true
}

func safeString(raw json.RawMessage) string {
	value, ok := rawString(raw)
	if !ok || strings.TrimSpace(value) == "" {
		return ""
	}
	for _, r := range value {
		if unicode.IsControl(r) {
			return ""
		}
	}
	return value
}

func normalizeEffort(raw json.RawMessage) string {
	value, ok := rawString(raw)
	if !ok {
		return ""
	}
	for _, r := range value {
		if unicode.IsControl(r) {
			return ""
		}
	}
	return strings.ToLower(strings.TrimSpace(value))
}

func parseTimestamp(raw json.RawMessage) *int64 {
	if value, ok := rawInteger(raw); ok {
		return &value
	}
	text, ok := rawString(raw)
	if !ok {
		return nil
	}
	timestamp, err := time.Parse(time.RFC3339Nano, text)
	if err != nil {
		return nil
	}
	value := timestamp.UnixMilli()
	if value < 0 {
		return nil
	}
	return &value
}

func preferTimestamp(payload json.RawMessage, outer *int64) *int64 {
	if value := parseTimestamp(payload); value != nil {
		return value
	}
	return cloneInt64(outer)
}

func cloneInt64(value *int64) *int64 {
	if value == nil {
		return nil
	}
	copy := *value
	return &copy
}
