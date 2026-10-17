package codexplugin

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
)

const maxUpstreamBody = 8 << 20

type DashboardOperation string

const (
	OperationProbe               DashboardOperation = "probe"
	OperationQuota               DashboardOperation = "quota"
	OperationFilterOptions       DashboardOperation = "filter_options"
	OperationSummary             DashboardOperation = "summary"
	OperationModelDistribution   DashboardOperation = "model_distribution"
	OperationProjectDistribution DashboardOperation = "project_distribution"
	OperationSessions            DashboardOperation = "sessions"
	OperationToolUsage           DashboardOperation = "tool_usage"
	OperationSessionDetail       DashboardOperation = "session_detail"
	OperationStatus              DashboardOperation = "status"
	OperationRevision            DashboardOperation = "revision"
	OperationServiceState        DashboardOperation = "service_state"
)

type DashboardRange struct {
	Key  string `json:"key"`
	From string `json:"from,omitempty"`
	To   string `json:"to,omitempty"`
}

type ModelSelection struct {
	ProviderID string `json:"provider_id"`
	Model      string `json:"model"`
}

type ProjectSelection struct {
	Kind string `json:"kind"`
	Path string `json:"path,omitempty"`
}

type DashboardFilters struct {
	Agents   []string           `json:"agents"`
	Models   []ModelSelection   `json:"models"`
	Projects []ProjectSelection `json:"projects"`
}

type ThreadKey struct {
	Agent     string `json:"agent"`
	SessionID string `json:"session_id"`
	ThreadID  string `json:"thread_id"`
}

type DashboardReadInput struct {
	Operation DashboardOperation `json:"operation"`
	Range     *DashboardRange    `json:"range,omitempty"`
	Filters   *DashboardFilters  `json:"filters,omitempty"`
	Days      *int               `json:"days,omitempty"`
	Key       *ThreadKey         `json:"key,omitempty"`
}

type DashboardEnvelope struct {
	Status     string          `json:"status"`
	Operation  string          `json:"operation"`
	HTTPStatus int             `json:"http_status,omitempty"`
	Payload    json.RawMessage `json:"payload,omitempty"`
}

type Bridge struct {
	Resolver RuntimeResolver
	Client   *http.Client
}

func (b *Bridge) Read(ctx context.Context, in DashboardReadInput) DashboardEnvelope {
	if err := validateDashboardReadInput(in); err != nil {
		return inputErrorEnvelope(string(in.Operation), err)
	}
	method := http.MethodGet
	var path string
	switch in.Operation {
	case OperationProbe:
		target, err := b.Resolver.Resolve(ctx)
		if err != nil {
			return notRunningEnvelope(string(in.Operation))
		}
		_ = target
		return DashboardEnvelope{Status: "ready", Operation: string(in.Operation), Payload: json.RawMessage("{}")}
	case OperationQuota:
		path = "/api/quota"
	case OperationFilterOptions:
		path = "/api/usage/filter-options"
	case OperationSummary:
		path = "/api/usage/summary"
	case OperationModelDistribution:
		path = "/api/usage/model-distribution"
	case OperationProjectDistribution:
		path = "/api/usage/projects"
	case OperationSessions:
		path = "/api/usage/sessions"
	case OperationToolUsage:
		path = "/api/usage/tools"
	case OperationSessionDetail:
		path = "/api/usage/session-detail"
	case OperationStatus:
		path = "/api/status"
	case OperationRevision:
		path = "/api/revision"
	case OperationServiceState:
		path = "/api/service"
	default:
		return inputErrorEnvelope(string(in.Operation), fmt.Errorf("unsupported operation %q", in.Operation))
	}
	return b.request(ctx, string(in.Operation), method, path, dashboardQuery(in))
}

func (b *Bridge) RefreshQuota(ctx context.Context) DashboardEnvelope {
	return b.request(ctx, "refresh_quota", http.MethodPost, "/api/quota/refresh", nil)
}

func (b *Bridge) StopService(ctx context.Context) DashboardEnvelope {
	return b.request(ctx, "stop_service", http.MethodPost, "/api/service/stop", nil)
}

func validateDashboardReadInput(in DashboardReadInput) error {
	switch in.Operation {
	case OperationProbe, OperationQuota, OperationStatus, OperationRevision, OperationServiceState:
		if in.Range != nil || in.Filters != nil || in.Days != nil || in.Key != nil {
			return fmt.Errorf("operation %q does not accept additional fields", in.Operation)
		}
	case OperationFilterOptions:
		if in.Range == nil || in.Filters != nil || in.Days != nil || in.Key != nil {
			return fmt.Errorf("operation %q requires only range", in.Operation)
		}
		if err := validateDashboardRange(*in.Range); err != nil {
			return err
		}
	case OperationSummary, OperationModelDistribution, OperationProjectDistribution, OperationSessions:
		if in.Range == nil || in.Filters == nil || in.Days != nil || in.Key != nil {
			return fmt.Errorf("operation %q requires range and filters", in.Operation)
		}
		if err := validateDashboardRange(*in.Range); err != nil {
			return err
		}
		if err := validateDashboardFilters(*in.Filters); err != nil {
			return err
		}
	case OperationToolUsage:
		if in.Range != nil || in.Filters != nil || in.Days == nil || in.Key != nil {
			return fmt.Errorf("operation %q requires days", in.Operation)
		}
		if *in.Days != 1 && *in.Days != 7 && *in.Days != 30 {
			return fmt.Errorf("days must be 1, 7, or 30")
		}
	case OperationSessionDetail:
		if in.Range == nil || in.Filters == nil || in.Days != nil || in.Key == nil {
			return fmt.Errorf("operation %q requires range, filters, and key", in.Operation)
		}
		if err := validateDashboardRange(*in.Range); err != nil {
			return err
		}
		if err := validateDashboardFilters(*in.Filters); err != nil {
			return err
		}
	default:
		return fmt.Errorf("unsupported operation %q", in.Operation)
	}
	return nil
}

func validateDashboardRange(r DashboardRange) error {
	switch r.Key {
	case "today", "yesterday", "7d", "30d", "year":
		if r.From != "" || r.To != "" {
			return fmt.Errorf("preset range %q does not accept from or to", r.Key)
		}
	case "custom":
	default:
		return fmt.Errorf("unsupported range %q", r.Key)
	}
	return nil
}

func validateDashboardFilters(filters DashboardFilters) error {
	if filters.Agents == nil || filters.Models == nil || filters.Projects == nil {
		return fmt.Errorf("filters require agents, models, and projects")
	}
	for _, project := range filters.Projects {
		switch project.Kind {
		case "project":
		case "workspace", "projectless", "unknown":
			if project.Path != "" {
				return fmt.Errorf("project kind %q does not accept path", project.Kind)
			}
		default:
			return fmt.Errorf("unsupported project kind %q", project.Kind)
		}
	}
	return nil
}

func dashboardQuery(in DashboardReadInput) url.Values {
	query := make(url.Values)
	if in.Range != nil {
		query.Set("range", in.Range.Key)
		if in.Range.Key == "custom" {
			query.Set("from", in.Range.From)
			query.Set("to", in.Range.To)
		}
	}
	if in.Operation == OperationToolUsage {
		query.Set("days", strconv.Itoa(*in.Days))
	}
	if in.Filters != nil {
		if in.Operation != OperationSessionDetail {
			for _, agent := range in.Filters.Agents {
				query.Add("agent", agent)
			}
		}
		for _, model := range in.Filters.Models {
			query.Add("model_at", model.ProviderID+"/"+model.Model)
		}
		for _, project := range in.Filters.Projects {
			switch project.Kind {
			case "project":
				query.Add("project_path", project.Path)
			case "workspace":
				query.Set("include_workspace", "1")
			case "projectless":
				query.Set("include_projectless", "1")
			case "unknown":
				query.Set("include_unknown_project", "1")
			}
		}
	}
	if in.Operation == OperationSessionDetail {
		query.Set("agent", in.Key.Agent)
		query.Set("session_id", in.Key.SessionID)
		query.Set("thread_id", in.Key.ThreadID)
	}
	return query
}

func (b *Bridge) request(ctx context.Context, operation, method, path string, query url.Values) DashboardEnvelope {
	target, err := b.Resolver.Resolve(ctx)
	if err != nil {
		return notRunningEnvelope(operation)
	}
	endpoint, err := url.Parse(runtimeEndpoint(target, path))
	if err != nil {
		return upstreamErrorEnvelope(operation, 0, nil)
	}
	if len(query) > 0 {
		endpoint.RawQuery = query.Encode()
	}
	request, err := http.NewRequestWithContext(ctx, method, endpoint.String(), nil)
	if err != nil {
		return upstreamErrorEnvelope(operation, 0, nil)
	}
	request.Header.Set("Accept", "application/json")
	if method == http.MethodPost {
		request.Header.Set("X-Usagi-Request", "1")
	}
	response, err := b.Client.Do(request)
	if err != nil {
		if response != nil && response.Body != nil {
			_ = response.Body.Close()
		}
		if _, resolveErr := b.Resolver.Resolve(ctx); resolveErr != nil {
			return notRunningEnvelope(operation)
		}
		return upstreamErrorEnvelope(operation, 0, nil)
	}
	defer response.Body.Close()
	body, tooLarge, readErr := readUpstreamBody(response.Body)
	if tooLarge || readErr != nil {
		return upstreamErrorEnvelope(operation, response.StatusCode, nil)
	}
	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		payload, _ := decodeJSONValue(body)
		return upstreamErrorEnvelope(operation, response.StatusCode, payload)
	}
	payload, err := decodeJSONValue(body)
	if err != nil {
		return upstreamErrorEnvelope(operation, response.StatusCode, nil)
	}
	return DashboardEnvelope{Status: "ready", Operation: operation, Payload: payload}
}

func readUpstreamBody(body io.Reader) ([]byte, bool, error) {
	contents, err := io.ReadAll(io.LimitReader(body, maxUpstreamBody+1))
	if err != nil {
		return nil, false, err
	}
	if len(contents) > maxUpstreamBody {
		return nil, true, nil
	}
	return contents, false, nil
}

func decodeJSONValue(contents []byte) (json.RawMessage, error) {
	decoder := json.NewDecoder(bytes.NewReader(contents))
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		return nil, err
	}
	var trailing any
	if err := decoder.Decode(&trailing); err != io.EOF {
		if err == nil {
			return nil, fmt.Errorf("multiple JSON values")
		}
		return nil, err
	}
	encoded, err := json.Marshal(value)
	return json.RawMessage(encoded), err
}

func notRunningEnvelope(operation string) DashboardEnvelope {
	return DashboardEnvelope{Status: "not_running", Operation: operation}
}

func upstreamErrorEnvelope(operation string, status int, payload json.RawMessage) DashboardEnvelope {
	return DashboardEnvelope{Status: "upstream_error", Operation: operation, HTTPStatus: status, Payload: payload}
}

func inputErrorEnvelope(operation string, err error) DashboardEnvelope {
	payload, _ := json.Marshal(map[string]string{"error": "INVALID_INPUT", "message": err.Error()})
	return upstreamErrorEnvelope(operation, 0, payload)
}
