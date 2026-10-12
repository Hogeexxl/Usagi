package testsupport

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

type revisionDTO struct {
	DataRevision   int64 `json:"data_revision"`
	StatusRevision int64 `json:"status_revision"`
}

type infoDTO struct {
	Service      string   `json:"service"`
	AppVersion   string   `json:"app_version"`
	APIVersion   string   `json:"api_version"`
	Capabilities []string `json:"capabilities"`
}

type statusDTO struct {
	revisionDTO
	ScanState              string  `json:"scan_state"`
	SourceBindingStatus    string  `json:"source_binding_status"`
	LastFinishedScanResult *string `json:"last_finished_scan_result"`
	LastScanStartedAtMS    *int64  `json:"last_scan_started_at_ms"`
	LastScanCompletedAtMS  *int64  `json:"last_scan_completed_at_ms"`
	LastScanFailedAtMS     *int64  `json:"last_scan_failed_at_ms"`
	LastScanErrorCode      *string `json:"last_scan_error_code"`
}

type quotaWindowDTO struct {
	UsedPercent        float64 `json:"used_percent"`
	RemainingPercent   float64 `json:"remaining_percent"`
	LimitWindowSeconds uint64  `json:"limit_window_seconds"`
	ResetAtMS          *int64  `json:"reset_at_ms"`
}

type quotaDTO struct {
	Status      string          `json:"status"`
	FiveHour    *quotaWindowDTO `json:"five_hour"`
	Weekly      *quotaWindowDTO `json:"weekly"`
	FetchedAtMS *int64          `json:"fetched_at_ms"`
}

type sessionHealthDTO struct {
	TotalSessions      int64 `json:"total_sessions"`
	CompleteSessions   int64 `json:"complete_sessions"`
	IncompleteSessions int64 `json:"incomplete_sessions"`
	ErrorSessions      int64 `json:"error_sessions"`
}

type usageDTO struct {
	InputTokens                         int64            `json:"input_tokens"`
	CachedTokens                        int64            `json:"cached_tokens"`
	CacheWriteTokens                    *int64           `json:"cache_write_tokens"`
	UncachedInputTokens                 *int64           `json:"uncached_input_tokens"`
	OutputTokens                        int64            `json:"output_tokens"`
	ReasoningTokens                     int64            `json:"reasoning_tokens"`
	OtherOutputTokens                   int64            `json:"other_output_tokens"`
	TotalTokens                         int64            `json:"total_tokens"`
	CacheHitRate                        *float64         `json:"cache_hit_rate"`
	EstimatedCost                       *float64         `json:"estimated_cost"`
	EstimatedCostStatus                 string           `json:"estimated_cost_status"`
	SessionCount                        int64            `json:"session_count"`
	CostIncompleteSessionCount          int64            `json:"cost_incomplete_session_count"`
	CompleteSessionCostPerMillionTokens *float64         `json:"complete_session_cost_per_million_tokens"`
	SessionHealth                       sessionHealthDTO `json:"session_health"`
}

type rangeDTO struct {
	Key      string `json:"key"`
	StartMS  int64  `json:"start_ms"`
	EndMS    int64  `json:"end_ms"`
	Timezone string `json:"timezone"`
}

type summaryDTO struct {
	Range        rangeDTO `json:"range"`
	DataRevision int64    `json:"data_revision"`
	Usage        usageDTO `json:"usage"`
}

func fixtureBytes(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("testdata", "public_v1", name))
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func decodeFixture[T any](t *testing.T, name string) T {
	t.Helper()
	var value T
	if err := json.Unmarshal(fixtureBytes(t, name), &value); err != nil {
		t.Fatalf("decode %s: %v", name, err)
	}
	return value
}

func TestPublicV1FixturesDecodeLikePeekFlowDTOs(t *testing.T) {
	info := decodeFixture[infoDTO](t, "info.json")
	if info.Service != "usagi" || info.AppVersion != "0.2.6" || info.APIVersion != "1" || len(info.Capabilities) != 5 {
		t.Fatalf("unexpected info fixture: %+v", info)
	}

	revision := decodeFixture[revisionDTO](t, "revision.json")
	if revision != (revisionDTO{DataRevision: 41, StatusRevision: 9}) {
		t.Fatalf("unexpected revision fixture: %+v", revision)
	}

	status := decodeFixture[statusDTO](t, "status.json")
	if status.ScanState != "idle" || status.SourceBindingStatus != "ready" || status.LastFinishedScanResult == nil || *status.LastFinishedScanResult != "completed" {
		t.Fatalf("unexpected status fixture: %+v", status)
	}

	quota := decodeFixture[quotaDTO](t, "codex_quota.json")
	if quota.Status != "ready" || quota.FiveHour == nil || quota.Weekly == nil || quota.FetchedAtMS == nil || quota.Weekly.LimitWindowSeconds != 604800 {
		t.Fatalf("unexpected quota fixture: %+v", quota)
	}

	summary := decodeFixture[summaryDTO](t, "usage_summary.json")
	if summary.Range.Key != "today" || summary.Range.StartMS != 1736985600000 || summary.Range.EndMS != 1737072000000 || summary.DataRevision != 41 || summary.Usage.SessionHealth.CompleteSessions != 2 {
		t.Fatalf("unexpected summary fixture: %+v", summary)
	}

	frame := string(fixtureBytes(t, "events_revision.sse"))
	var eventID string
	var eventName string
	var eventData string
	for _, line := range strings.Split(frame, "\n") {
		switch {
		case strings.HasPrefix(line, "id: "):
			eventID = strings.TrimPrefix(line, "id: ")
		case strings.HasPrefix(line, "event: "):
			eventName = strings.TrimPrefix(line, "event: ")
		case strings.HasPrefix(line, "data: "):
			eventData = strings.TrimPrefix(line, "data: ")
		}
	}
	var eventRevision revisionDTO
	if err := json.Unmarshal([]byte(eventData), &eventRevision); err != nil {
		t.Fatalf("decode SSE data: %v", err)
	}
	if eventID != "9-41" || eventName != "revision" || eventRevision != revision {
		t.Fatalf("unexpected SSE fixture: id=%q event=%q revision=%+v", eventID, eventName, eventRevision)
	}
}
