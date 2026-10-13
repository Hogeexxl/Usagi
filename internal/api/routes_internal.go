package api

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

type statusResponseDTO struct {
	StatusRevision int64   `json:"status_revision"`
	RuntimeState   string  `json:"runtime_state"`
	LastErrorCode  *string `json:"last_error_code"`
}

func (s *Server) handleSummary(w http.ResponseWriter, r *http.Request) {
	scope, _, err := parseScopeRequest(r, false)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	dataRevision := s.revisions.Current().DataRevision
	view, err := s.projection.Summary(r.Context(), scope)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, mapSummaryDTO(dataRevision, view))
}

func (s *Server) handleModelDistribution(w http.ResponseWriter, r *http.Request) {
	scope, _, err := parseScopeRequest(r, false)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	dataRevision := s.revisions.Current().DataRevision
	view, err := s.projection.ModelDistribution(r.Context(), scope)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	response := modelDistributionResponseDTO{DataRevision: dataRevision, Items: make([]modelDistributionItemDTO, 0, len(view.Items))}
	for _, item := range view.Items {
		response.Items = append(response.Items, modelDistributionItemDTO{Model: item.Model, Usage: mapUsageDTO(item.Usage)})
	}
	writeJSON(w, http.StatusOK, response)
}

func (s *Server) handleProjectDistribution(w http.ResponseWriter, r *http.Request) {
	scope, _, err := parseScopeRequest(r, false)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	dataRevision := s.revisions.Current().DataRevision
	view, err := s.projection.ProjectDistribution(r.Context(), scope)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	response := projectDistributionResponseDTO{DataRevision: dataRevision, Items: make([]projectDistributionItemDTO, 0, len(view.Items))}
	for _, item := range view.Items {
		project := mapProjectDTO(item.Project)
		response.Items = append(response.Items, projectDistributionItemDTO{
			Kind: project.Kind, Path: project.Path, Name: project.Name, Usage: mapUsageDTO(item.Usage),
		})
	}
	writeJSON(w, http.StatusOK, response)
}

func (s *Server) handleFilterOptions(w http.ResponseWriter, r *http.Request) {
	values, err := url.ParseQuery(r.URL.RawQuery)
	if err != nil {
		writeProjectionError(w, query.ErrInvalidRange)
		return
	}
	for name := range values {
		if name != "range" && name != "from" && name != "to" {
			writeProjectionError(w, query.ErrInvalidFilter)
			return
		}
	}
	rangeValue, err := ResolveRange(values)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	dataRevision := s.revisions.Current().DataRevision
	view, err := s.projection.FilterOptions(r.Context(), projection.ScopeQuery{Range: rangeValue.Query})
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	response := filterOptionsResponseDTO{
		DataRevision: dataRevision,
		Agents:       make([]agentOptionDTO, 0, len(view.Agents)),
		Models:       make([]modelOptionDTO, 0, len(view.Models)),
		Projects:     make([]projectDTO, 0, len(view.Projects)),
	}
	for _, agent := range view.Agents {
		response.Agents = append(response.Agents, agentOptionDTO{Agent: agent.Agent, DisplayName: agent.DisplayName})
	}
	for _, model := range view.Models {
		response.Models = append(response.Models, modelOptionDTO{
			Model: model.Model, ProviderID: model.ProviderID, ProviderName: model.ProviderName,
		})
	}
	for _, project := range view.Projects {
		response.Projects = append(response.Projects, mapProjectDTO(project))
	}
	writeJSON(w, http.StatusOK, response)
}

func (s *Server) handleSessions(w http.ResponseWriter, r *http.Request) {
	scope, _, err := parseScopeRequest(r, false)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	dataRevision := s.revisions.Current().DataRevision
	view, err := s.projection.Sessions(r.Context(), scope)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	response := sessionsResponseDTO{DataRevision: dataRevision, Summary: mapSummaryDTO(dataRevision, view.Summary), Items: make([]sessionListItemDTO, 0, len(view.Items))}
	for _, item := range view.Items {
		var lastActivityMS *int64
		if !item.LastActivity.IsZero() {
			value := item.LastActivity.UnixMilli()
			lastActivityMS = &value
		}
		response.Items = append(response.Items, sessionListItemDTO{
			Thread: mapSessionThreadDTO(item.Thread), SelfUsage: mapUsageDTO(item.SelfUsage),
			SubagentUsage: mapUsageDTO(item.SubagentUsage), InclusiveUsage: mapUsageDTO(item.InclusiveUsage),
			HasUsageRows: item.HasUsageRows, LastActivityMS: lastActivityMS, SubagentCount: item.SubagentCount,
		})
	}
	writeJSON(w, http.StatusOK, response)
}

func (s *Server) handleSessionDetail(w http.ResponseWriter, r *http.Request) {
	scope, key, err := parseScopeRequest(r, true)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	dataRevision := s.revisions.Current().DataRevision
	view, err := s.projection.SessionDetail(r.Context(), scope, key)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	response := sessionDetailResponseDTO{
		DataRevision: dataRevision, Thread: mapSessionThreadDTO(view.Thread),
		SelfUsage: mapUsageDTO(view.SelfUsage), SubagentUsage: mapUsageDTO(view.SubagentUsage),
		InclusiveUsage: mapUsageDTO(view.InclusiveUsage), Children: make([]sessionThreadDTO, 0, len(view.Children)),
	}
	for _, child := range view.Children {
		response.Children = append(response.Children, mapSessionThreadDTO(child))
	}
	writeJSON(w, http.StatusOK, response)
}

func (s *Server) handleToolUsage(w http.ResponseWriter, r *http.Request) {
	values, err := url.ParseQuery(r.URL.RawQuery)
	if err != nil || len(values) != 1 || len(values["days"]) != 1 {
		writeProjectionError(w, query.ErrInvalidFilter)
		return
	}
	var days int
	switch values.Get("days") {
	case "1":
		days = 1
	case "7":
		days = 7
	case "30":
		days = 30
	default:
		writeProjectionError(w, query.ErrInvalidFilter)
		return
	}
	if !s.requireRuntimeReady(w) {
		return
	}
	view, err := s.projection.ToolUsage(r.Context(), days)
	if err != nil {
		writeProjectionError(w, err)
		return
	}
	response := toolUsageResponseDTO{Days: view.Days, Calls: view.Calls, Top: make([]toolUsageItemDTO, 0, len(view.Top))}
	for _, item := range view.Top {
		response.Top = append(response.Top, toolUsageItemDTO{Name: item.Name, Calls: item.Calls})
	}
	writeJSON(w, http.StatusOK, response)
}

func mapSummaryDTO(dataRevision int64, view projection.SummaryView) summaryResponseDTO {
	return summaryResponseDTO{
		DataRevision: dataRevision, Usage: mapUsageDTO(view.Usage), SessionCount: view.RootSessionCount,
		SessionMedian: view.SessionMedian, SessionP90: view.SessionP90,
	}
}

func parseScopeRequest(r *http.Request, detail bool) (projection.ScopeQuery, projection.ThreadKey, error) {
	values, err := url.ParseQuery(r.URL.RawQuery)
	if err != nil {
		return projection.ScopeQuery{}, projection.ThreadKey{}, query.ErrInvalidFilter
	}
	allowed := map[string]bool{
		"range": true, "from": true, "to": true, "agent": true,
		"model_at": true, "project_path": true, "include_projectless": true,
		"include_unknown_project": true,
	}
	if detail {
		allowed["session_id"] = true
		allowed["thread_id"] = true
	}
	for name := range values {
		if !allowed[name] {
			return projection.ScopeQuery{}, projection.ThreadKey{}, query.ErrInvalidFilter
		}
	}
	rangeValue, err := ResolveRange(values)
	if err != nil {
		return projection.ScopeQuery{}, projection.ThreadKey{}, err
	}
	filter, key, err := parseFilterValues(values, detail)
	if err != nil {
		return projection.ScopeQuery{}, projection.ThreadKey{}, err
	}
	return projection.ScopeQuery{Range: rangeValue.Query, Filter: filter}, key, nil
}

func parseFilterValues(values url.Values, detail bool) (query.UsageFilter, projection.ThreadKey, error) {
	filter := query.UsageFilter{}
	key := projection.ThreadKey{}
	agents := values["agent"]
	if detail {
		if len(agents) != 1 || !validFilterValue(agents[0]) {
			return query.UsageFilter{}, projection.ThreadKey{}, query.ErrInvalidFilter
		}
		sessionIDs, threadIDs := values["session_id"], values["thread_id"]
		if len(sessionIDs) != 1 || len(threadIDs) != 1 || !validFilterValue(sessionIDs[0]) || !validFilterValue(threadIDs[0]) {
			return query.UsageFilter{}, projection.ThreadKey{}, query.ErrInvalidFilter
		}
		key = projection.ThreadKey{Agent: agents[0], SessionID: sessionIDs[0], ThreadID: threadIDs[0]}
	} else {
		for _, agent := range agents {
			if !validFilterValue(agent) {
				return query.UsageFilter{}, projection.ThreadKey{}, query.ErrInvalidFilter
			}
			filter.Agents = append(filter.Agents, agent)
		}
		filter.Agents = sortedUnique(filter.Agents)
	}
	for _, raw := range values["model_at"] {
		providerID, model, ok := strings.Cut(raw, "/")
		if !ok || !validFilterValue(providerID) || !validFilterValue(model) {
			return query.UsageFilter{}, projection.ThreadKey{}, query.ErrInvalidFilter
		}
		filter.Models = append(filter.Models, query.ModelSelection{ProviderID: providerID, Model: model})
	}
	slices.SortFunc(filter.Models, func(a, b query.ModelSelection) int {
		if a.ProviderID != b.ProviderID {
			return strings.Compare(a.ProviderID, b.ProviderID)
		}
		return strings.Compare(a.Model, b.Model)
	})
	filter.Models = slices.Compact(filter.Models)
	for _, projectPath := range values["project_path"] {
		if !validFilterValue(projectPath) || !filepath.IsAbs(projectPath) {
			return query.UsageFilter{}, projection.ThreadKey{}, query.ErrInvalidFilter
		}
		filter.Projects = append(filter.Projects, query.ProjectSelection{Kind: query.ProjectKindProject, Path: projectPath})
	}
	if err := appendFlagSelection(values, "include_projectless", query.ProjectKindProjectless, &filter); err != nil {
		return query.UsageFilter{}, projection.ThreadKey{}, err
	}
	if err := appendFlagSelection(values, "include_unknown_project", query.ProjectKindUnknown, &filter); err != nil {
		return query.UsageFilter{}, projection.ThreadKey{}, err
	}
	slices.SortFunc(filter.Projects, func(a, b query.ProjectSelection) int {
		if a.Kind != b.Kind {
			return strings.Compare(string(a.Kind), string(b.Kind))
		}
		return strings.Compare(a.Path, b.Path)
	})
	filter.Projects = slices.Compact(filter.Projects)
	return filter, key, nil
}

func appendFlagSelection(values url.Values, name string, kind query.ProjectKind, filter *query.UsageFilter) error {
	for _, value := range values[name] {
		if value != "1" {
			return query.ErrInvalidFilter
		}
		filter.Projects = append(filter.Projects, query.ProjectSelection{Kind: kind})
	}
	return nil
}

func validFilterValue(value string) bool {
	if value == "" {
		return false
	}
	for _, r := range value {
		if r < 0x20 || r == 0x7f {
			return false
		}
	}
	return true
}

func sortedUnique(values []string) []string {
	slices.Sort(values)
	return slices.Compact(values)
}

func requestHasNoQuery(w http.ResponseWriter, r *http.Request) bool {
	values, err := url.ParseQuery(r.URL.RawQuery)
	if err != nil || len(values) != 0 {
		writeAPIError(w, http.StatusBadRequest, ErrorInvalidFilter)
		return false
	}
	return true
}

func (s *Server) handleEvents(w http.ResponseWriter, r *http.Request) {
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
	if writeRevisionEvent(w, flusher, current) != nil {
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
			if writeRevisionEvent(w, flusher, tuple) != nil {
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

func writeRevisionEvent(w http.ResponseWriter, flusher http.Flusher, tuple RevisionTuple) error {
	data, err := json.Marshal(tuple)
	if err != nil {
		return err
	}
	if _, err := fmt.Fprintf(w, "id: %d-%d\nevent: revision\ndata: %s\n\n", tuple.StatusRevision, tuple.DataRevision, data); err != nil {
		return err
	}
	flusher.Flush()
	return nil
}
