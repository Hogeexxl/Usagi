package usage

import (
	"testing"

	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
)

func parseTestRecord(value string) ParsedRecord {
	return ParseRecord(rollout.Record{
		SourceFileID:       1,
		Generation:         1,
		LogicalStartOffset: 10,
		LogicalEndOffset:   int64(10 + len(value)),
		JSON:               []byte(value),
	})
}

func TestParserTurnContextRequiresObjectPayload(t *testing.T) {
	for _, payload := range []string{"null", `"context"`, `[]`, `12`} {
		t.Run(payload, func(t *testing.T) {
			parsed := parseTestRecord(`{"type":"turn_context","payload":` + payload + `}`)
			if parsed.Kind != RawMalformed {
				t.Fatalf("turn_context payload %s parsed as kind %d", payload, parsed.Kind)
			}
		})
	}
	if parsed := parseTestRecord(`{"type":"turn_context","payload":{"model":"gpt"}}`); parsed.Kind != RawTurnContext || parsed.Model != "gpt" {
		t.Fatalf("valid turn_context parsed as %+v", parsed)
	}
}

func TestParserCompactedEmbeddedResponseMustBeValid(t *testing.T) {
	for _, embedded := range []string{
		`{"usage":null}`,
		`{"response_id":"resp","thread_id":7}`,
		`[]`,
	} {
		t.Run(embedded, func(t *testing.T) {
			parsed := parseTestRecord(`{"type":"compacted","payload":{"latest_token_usage_record":` + embedded + `}}`)
			if parsed.Kind != RawMalformed {
				t.Fatalf("invalid embedded record parsed as kind %d", parsed.Kind)
			}
		})
	}
	parsed := parseTestRecord(`{"type":"compacted","payload":{"compaction_response_id":"compact-id"}}`)
	if parsed.Kind != RawCompacted || parsed.Compaction == nil || parsed.Compaction.ResponseID != "compact-id" || parsed.Compaction.Latest != nil {
		t.Fatalf("ID-only compaction parsed as %+v", parsed)
	}
	parsed = parseTestRecord(`{"type":"compacted","payload":{"latest_token_usage_record":{"response_id":"embedded-response","usage":{"input_tokens":3,"cached_input_tokens":1,"output_tokens":2,"reasoning_output_tokens":1,"total_tokens":5}}}}`)
	if parsed.Kind != RawCompacted || parsed.Compaction == nil || parsed.Compaction.Latest == nil ||
		parsed.Compaction.Latest.ResponseID != "embedded-response" || parsed.Compaction.Latest.Usage.State != UsageValueValid {
		t.Fatalf("valid embedded response parsed as %+v", parsed)
	}
}

func TestParserTokenSnapshotsAndLastUsage(t *testing.T) {
	valid := `{"input_tokens":10,"cached_input_tokens":2,"cache_write_input_tokens":3,"output_tokens":4,"reasoning_output_tokens":1,"total_tokens":14}`
	parsed := parseTestRecord(`{"type":"event_msg","timestamp":123,"payload":{"type":"token_count","info":{"total_token_usage":` + valid + `}}}`)
	if parsed.Kind != RawTokenCount || !parsed.HasTokenInfo || parsed.Total.State != UsageValueValid || parsed.Last.State != UsageValueMissing || parsed.TimestampMS == nil || *parsed.TimestampMS != 123 {
		t.Fatalf("token_count parse = %+v", parsed)
	}
	for _, invalid := range []string{
		`{"input_tokens":10.0,"cached_input_tokens":2,"output_tokens":4,"reasoning_output_tokens":1,"total_tokens":14}`,
		`{"input_tokens":"10","cached_input_tokens":2,"output_tokens":4,"reasoning_output_tokens":1,"total_tokens":14}`,
		`{"input_tokens":-1,"cached_input_tokens":0,"output_tokens":4,"reasoning_output_tokens":1,"total_tokens":3}`,
		`{"input_tokens":9223372036854775808,"cached_input_tokens":0,"output_tokens":4,"reasoning_output_tokens":1,"total_tokens":4}`,
	} {
		parsed := parseTestRecord(`{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":` + invalid + `,"last_token_usage":null}}}`)
		if parsed.Total.State != UsageValueInvalid || parsed.Last.State != UsageValueInvalid {
			t.Fatalf("invalid snapshot parse = %+v for %s", parsed, invalid)
		}
	}
	parsed = parseTestRecord(`{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":4,"reasoning_output_tokens":1,"total_tokens":14},"last_token_usage":false}}}`)
	if parsed.Total.State != UsageValueValid || parsed.Last.State != UsageValueInvalid {
		t.Fatalf("invalid last usage parse = %+v", parsed)
	}
	parsed = parseTestRecord(`{"type":"event_msg","payload":{"type":"token_count","info":null}}`)
	if parsed.Kind != RawTokenCount || parsed.HasTokenInfo {
		t.Fatalf("null info must have no snapshot: %+v", parsed)
	}
	parsed = parseTestRecord(`{"type":"event_msg","payload":{"type":"token_count","info":false}}`)
	if parsed.Kind != RawTokenCount || !parsed.HasTokenInfo || parsed.Total.State != UsageValueInvalid {
		t.Fatalf("non-object info must have invalid total: %+v", parsed)
	}
}

func TestParserGapMapping(t *testing.T) {
	for _, test := range []struct {
		gap  rollout.GapKind
		kind RawKind
	}{
		{rollout.GapMalformed, RawMalformed},
		{rollout.GapParser, RawMalformed},
		{rollout.GapRequiredInvalid, RawMalformed},
		{rollout.GapOversized, RawOversized},
		{rollout.GapOwnership, RawUnknown},
	} {
		parsed := ParseGap(rollout.Gap{SourceFileID: 1, Generation: 1, LogicalStartOffset: 10, LogicalEndOffset: 20, Kind: test.gap})
		if parsed.Kind != test.kind || parsed.GapKind != test.gap || parsed.StartOffset != 10 || parsed.EndOffset != 20 {
			t.Errorf("ParseGap(%d) = %+v", test.gap, parsed)
		}
	}
}

func TestParserResponseAndEffortMapping(t *testing.T) {
	parsed := parseTestRecord(`{"timestamp":"1970-01-01T00:00:01Z","type":"token_usage_record","payload":{"response_id":"r","thread_id":"t","session_id":"s","turn_id":"u","timestamp":2000,"usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":4,"reasoning_output_tokens":1,"total_tokens":14},"thread_token_usage":null}}`)
	if parsed.Kind != RawResponseUsage || parsed.Response == nil || parsed.Response.ResponseID != "r" || parsed.Response.ThreadID != "t" || parsed.Response.Usage.State != UsageValueValid || parsed.Response.ThreadTokenUsage.State != UsageValueInvalid || parsed.TimestampMS == nil || *parsed.TimestampMS != 2000 {
		t.Fatalf("response parse = %+v", parsed)
	}
	if parsed := parseTestRecord(`{"type":"token_usage_record","payload":{"response_id":"  "}}`); parsed.Kind != RawMalformed {
		t.Fatalf("response without safe ID parsed as kind %d", parsed.Kind)
	}
	parsed = parseTestRecord(`{"type":"turn_context","payload":{"effort":" HIGH ","reasoning_effort":"low"}}`)
	if parsed.Kind != RawTurnContext || parsed.ReasoningEffort != "high" {
		t.Fatalf("canonical effort parse = %+v", parsed)
	}
	parsed = parseTestRecord(`{"type":"turn_context","payload":{"reasoning_effort":" Medium "}}`)
	if parsed.ReasoningEffort != "medium" {
		t.Fatalf("effort fallback parse = %+v", parsed)
	}
	parsed = parseTestRecord(`{"type":"turn_context","payload":{"effort":"","reasoning_effort":"high"}}`)
	if parsed.ReasoningEffort != "" {
		t.Fatalf("present empty effort must not use fallback: %+v", parsed)
	}
	parsed = parseTestRecord(`{"type":"turn_context","payload":{"effort":"high\n"}}`)
	if parsed.ReasoningEffort != "" {
		t.Fatalf("effort containing a control character must be unavailable: %+v", parsed)
	}
}

func TestParserUnknownAndIgnored(t *testing.T) {
	if parsed := parseTestRecord(`{"type":"event_msg","payload":{"type":"rate_limits"}}`); parsed.Kind != RawIgnored {
		t.Fatalf("rate_limits parsed as kind %d", parsed.Kind)
	}
	if parsed := parseTestRecord(`{"type":"event_msg","payload":{"type":"future_event"}}`); parsed.Kind != RawUnknown {
		t.Fatalf("future event parsed as kind %d", parsed.Kind)
	}
	if parsed := parseTestRecord(`{"type":"future_top_level"}`); parsed.Kind != RawUnknown {
		t.Fatalf("future record parsed as kind %d", parsed.Kind)
	}
}

func TestParserLifecycleMapping(t *testing.T) {
	for _, test := range []struct {
		name string
		json string
		want LifecycleKind
	}{
		{"start", `{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn"}}`, LifecycleStarted},
		{"complete", `{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn"}}`, LifecycleCompleted},
		{"abort", `{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn"}}`, LifecycleAborted},
		{"fail", `{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn","error":"failed"}}`, LifecycleFailed},
	} {
		t.Run(test.name, func(t *testing.T) {
			parsed := parseTestRecord(test.json)
			if parsed.Kind != RawLifecycle || parsed.Lifecycle != test.want || parsed.TurnID != "turn" {
				t.Fatalf("lifecycle parsed as %+v", parsed)
			}
		})
	}
}
