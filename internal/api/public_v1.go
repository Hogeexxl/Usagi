package api

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

const (
	publicV1InfoPath     = "/api/v1/info"
	publicV1RevisionPath = "/api/v1/revision"
	publicV1EventsPath   = "/api/v1/events"
	publicV1StatusPath   = "/api/v1/status"
	publicV1QuotaPath    = "/api/v1/codex/quota"
	publicV1SummaryPath  = "/api/v1/usage/summary"
)

var publicV1Capabilities = []string{
	"revision",
	"revision-events",
	"status",
	"codex-quota",
	"usage-summary",
}

func (s *Server) handlePublicV1Info(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	writeJSON(w, http.StatusOK, publicV1InfoDTO{
		Service: "usagi", AppVersion: s.config.AppVersion, APIVersion: "1", Capabilities: publicV1Capabilities,
		RuntimeMode: s.config.RuntimeMode,
	})
}

func (s *Server) handlePublicV1Revision(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	writeJSON(w, http.StatusOK, mapPublicV1Revision(s.revisions.Current()))
}

func mapPublicV1Revision(tuple RevisionTuple) publicV1RevisionDTO {
	return publicV1RevisionDTO{DataRevision: tuple.DataRevision, StatusRevision: tuple.StatusRevision}
}

func (s *Server) handlePublicV1Status(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	revision := s.revisions.Current()
	result := publicV1StatusDTO{
		DataRevision: revision.DataRevision, StatusRevision: revision.StatusRevision,
	}
	switch s.runtime.Status() {
	case magpie.RuntimeStatusReady:
		result.ScanState = "idle"
		result.SourceBindingStatus = "ready"
		result.LastFinishedScanResult = publicV1String("completed")
	case magpie.RuntimeStatusUnavailable:
		result.ScanState = "failed"
		result.SourceBindingStatus = "unbound"
		result.LastFinishedScanResult = publicV1String("failed")
		result.LastScanErrorCode = publicV1String(ErrorMagpieUnavailable)
	default:
		result.ScanState = "running"
		result.SourceBindingStatus = "unbound"
	}
	writeJSON(w, http.StatusOK, result)
}

func publicV1String(value string) *string {
	return &value
}

func (s *Server) handlePublicV1Quota(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	view, loaded, lastError := s.quotas.Current()
	if lastError != nil || s.runtime.Status() == magpie.RuntimeStatusUnavailable {
		writeJSON(w, http.StatusOK, publicV1QuotaDTO{Status: "unavailable"})
		return
	}
	if !loaded {
		writeJSON(w, http.StatusOK, publicV1QuotaDTO{Status: "loading"})
		return
	}

	var codex *projection.QuotaProviderView
	for index := range view.Items {
		if view.Items[index].Provider == "codex" {
			codex = &view.Items[index]
			break
		}
	}
	if codex == nil {
		writeJSON(w, http.StatusOK, publicV1QuotaDTO{Status: "auth_required"})
		return
	}
	if codex.Error != "" {
		writeJSON(w, http.StatusOK, publicV1QuotaDTO{Status: "unavailable"})
		return
	}
	writeJSON(w, http.StatusOK, mapPublicV1Quota(*codex))
}

func mapPublicV1Quota(card projection.QuotaProviderView) publicV1QuotaDTO {
	result := publicV1QuotaDTO{Status: "ready"}
	switch {
	case card.ReadAt != nil:
		result.FetchedAtMS = publicV1TimeMS(card.ReadAt)
	case card.AsOf != nil:
		result.FetchedAtMS = publicV1TimeMS(card.AsOf)
	}
	for _, window := range card.Windows {
		mapped := &publicV1QuotaWindowDTO{
			UsedPercent: window.Used, RemainingPercent: 100 - window.Used,
			ResetAtMS: publicV1TimeMS(window.ResetsAt),
		}
		switch window.Name {
		case "5 hours":
			if result.FiveHour == nil {
				mapped.LimitWindowSeconds = 18_000
				result.FiveHour = mapped
			}
		case "7 days":
			if result.Weekly == nil {
				mapped.LimitWindowSeconds = 604_800
				result.Weekly = mapped
			}
		}
	}
	return result
}

func publicV1TimeMS(value *time.Time) *int64 {
	if value == nil {
		return nil
	}
	result := value.UnixMilli()
	return &result
}

func (s *Server) handlePublicV1Summary(w http.ResponseWriter, r *http.Request) {
	resolved, err := parsePublicV1Range(r)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	dataRevision := s.revisions.Current().DataRevision
	view, err := s.projection.Summary(r.Context(), projection.ScopeQuery{
		Range: resolved.Query, Filter: query.UsageFilter{Agents: []string{"codex"}},
	})
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, publicV1SummaryDTO{
		Range: publicV1RangeDTO{
			Key: resolved.Key, StartMS: resolved.StartMS, EndMS: resolved.EndMS, Timezone: resolved.Timezone,
		},
		DataRevision: dataRevision,
		Usage:        mapPublicV1Usage(view),
	})
}

func parsePublicV1Range(r *http.Request) (ResolvedRange, error) {
	values, err := url.ParseQuery(r.URL.RawQuery)
	if err != nil {
		return ResolvedRange{}, query.ErrInvalidFilter
	}
	for name := range values {
		if name != "range" && name != "from" && name != "to" {
			return ResolvedRange{}, query.ErrInvalidFilter
		}
	}
	return ResolveRange(values)
}

func mapPublicV1Usage(view projection.SummaryView) publicV1UsageDTO {
	usage := view.Usage
	cacheWrite := usage.CacheWrite
	uncachedInput := usage.Input
	estimatedCostStatus := "unknown"
	var estimatedCost *float64
	switch {
	case usage.Total == 0 && view.RootSessionCount == 0:
		estimatedCostStatus = "complete"
		estimatedCost = publicV1Float64(0)
	case usage.Unpriced == 0:
		estimatedCostStatus = "complete"
		estimatedCost = publicV1Float64(usage.Cost)
	case usage.Cost > 0:
		estimatedCostStatus = "partial"
		estimatedCost = publicV1Float64(usage.Cost)
	}
	completeSessions := view.RootSessionCount - view.RootUnpricedSessionCount
	return publicV1UsageDTO{
		InputTokens:  usage.Input + usage.CacheRead + usage.CacheWrite,
		CachedTokens: usage.CacheRead, CacheWriteTokens: &cacheWrite,
		UncachedInputTokens: &uncachedInput, OutputTokens: usage.Output,
		ReasoningTokens: usage.Reasoning, OtherOutputTokens: usage.Output - usage.Reasoning,
		TotalTokens: usage.Total, CacheHitRate: usage.CacheHitRate,
		EstimatedCost: estimatedCost, EstimatedCostStatus: estimatedCostStatus,
		SessionCount: view.RootSessionCount, CostIncompleteSessionCount: view.RootUnpricedSessionCount,
		SessionHealth: publicV1SessionHealthDTO{
			TotalSessions: view.RootSessionCount, CompleteSessions: completeSessions,
			IncompleteSessions: view.RootUnpricedSessionCount, ErrorSessions: 0,
		},
	}
}

func publicV1Float64(value float64) *float64 {
	return &value
}

func (s *Server) handlePublicV1Events(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	flusher, ok := w.(http.Flusher)
	if !ok {
		writeAPIError(w, http.StatusInternalServerError, ErrorInternal)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	updates, cancel := s.revisions.Subscribe()
	defer cancel()
	current := s.revisions.Current()
	if writePublicV1RevisionEvent(w, flusher, current) != nil {
		return
	}
	ticker := time.NewTicker(15 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-r.Context().Done():
			return
		case tuple, open := <-updates:
			if !open {
				return
			}
			if tuple == current {
				continue
			}
			if writePublicV1RevisionEvent(w, flusher, tuple) != nil {
				return
			}
			current = tuple
		case <-ticker.C:
			if _, err := fmt.Fprint(w, ": keepalive\n\n"); err != nil {
				return
			}
			flusher.Flush()
		}
	}
}

func writePublicV1RevisionEvent(w http.ResponseWriter, flusher http.Flusher, tuple RevisionTuple) error {
	data, err := json.Marshal(mapPublicV1Revision(tuple))
	if err != nil {
		return err
	}
	if _, err := fmt.Fprintf(w, "id: %d-%d\nevent: revision\ndata: %s\n\n", tuple.StatusRevision, tuple.DataRevision, data); err != nil {
		return err
	}
	flusher.Flush()
	return nil
}
