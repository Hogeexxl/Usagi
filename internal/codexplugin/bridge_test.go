package codexplugin

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"net/url"
	"reflect"
	"testing"
	"time"
)

func TestBridgeReadOperationRoutesAndQueries(t *testing.T) {
	filters := &DashboardFilters{
		Agents: []string{"codex", "claude"},
		Models: []ModelSelection{{ProviderID: "openai", Model: "gpt 6"}, {ProviderID: "anthropic", Model: "sonnet"}},
		Projects: []ProjectSelection{
			{Kind: "project", Path: "/work/one & two"},
			{Kind: "workspace"},
			{Kind: "projectless"},
			{Kind: "unknown"},
		},
	}
	filteredInput := func(operation DashboardOperation) DashboardReadInput {
		return DashboardReadInput{Operation: operation, Range: &DashboardRange{Key: "7d"}, Filters: filters}
	}
	filteredQuery := url.Values{
		"range":                   {"7d"},
		"agent":                   {"codex", "claude"},
		"model_at":                {"openai/gpt 6", "anthropic/sonnet"},
		"project_path":            {"/work/one & two"},
		"include_workspace":       {"1"},
		"include_projectless":     {"1"},
		"include_unknown_project": {"1"},
	}
	tests := []struct {
		name      string
		input     DashboardReadInput
		path      string
		query     url.Values
		noRequest bool
	}{
		{name: "probe", input: DashboardReadInput{Operation: OperationProbe}, noRequest: true},
		{name: "quota", input: DashboardReadInput{Operation: OperationQuota}, path: "/api/quota", query: url.Values{}},
		{name: "filter_options", input: DashboardReadInput{Operation: OperationFilterOptions, Range: &DashboardRange{Key: "custom", From: "2026-01-02", To: "2026-03-04"}}, path: "/api/usage/filter-options", query: url.Values{"range": {"custom"}, "from": {"2026-01-02"}, "to": {"2026-03-04"}}},
		{name: "summary", input: filteredInput(OperationSummary), path: "/api/usage/summary", query: filteredQuery},
		{name: "model_distribution", input: filteredInput(OperationModelDistribution), path: "/api/usage/model-distribution", query: filteredQuery},
		{name: "project_distribution", input: filteredInput(OperationProjectDistribution), path: "/api/usage/projects", query: filteredQuery},
		{name: "sessions", input: filteredInput(OperationSessions), path: "/api/usage/sessions", query: filteredQuery},
		{name: "tool_usage", input: DashboardReadInput{Operation: OperationToolUsage, Days: intPointer(30)}, path: "/api/usage/tools", query: url.Values{"days": {"30"}}},
		{name: "session_detail", input: DashboardReadInput{
			Operation: OperationSessionDetail,
			Range:     &DashboardRange{Key: "year"},
			Filters: &DashboardFilters{
				Agents:   []string{"must-not-be-sent"},
				Models:   []ModelSelection{{ProviderID: "openai", Model: "gpt"}},
				Projects: []ProjectSelection{{Kind: "project", Path: "/work/detail"}, {Kind: "workspace"}},
			},
			Key: &ThreadKey{Agent: "codex", SessionID: "session / one", ThreadID: "thread&one"},
		}, path: "/api/usage/session-detail", query: url.Values{
			"range": {"year"}, "model_at": {"openai/gpt"}, "project_path": {"/work/detail"}, "include_workspace": {"1"},
			"agent": {"codex"}, "session_id": {"session / one"}, "thread_id": {"thread&one"},
		}},
		{name: "status", input: DashboardReadInput{Operation: OperationStatus}, path: "/api/status", query: url.Values{}},
		{name: "revision", input: DashboardReadInput{Operation: OperationRevision}, path: "/api/revision", query: url.Values{}},
		{name: "service_state", input: DashboardReadInput{Operation: OperationServiceState}, path: "/api/service", query: url.Values{}},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var requestCount int
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				requestCount++
				if request.Method != http.MethodGet || request.URL.Path != test.path {
					t.Errorf("upstream request = %s %s, want GET %s", request.Method, request.URL.Path, test.path)
				}
				if request.Header.Get("Accept") != "application/json" {
					t.Errorf("Accept = %q, want application/json", request.Header.Get("Accept"))
				}
				gotQuery, err := url.ParseQuery(request.URL.RawQuery)
				if err != nil {
					t.Errorf("parse query: %v", err)
				}
				if !reflect.DeepEqual(gotQuery, test.query) {
					t.Errorf("query = %#v, want %#v", gotQuery, test.query)
				}
				_, _ = w.Write([]byte(`{"ok":true}`))
			}))
			defer server.Close()

			var resolveCalls int
			bridge := &Bridge{
				Resolver: resolverFunc(func(context.Context) (RuntimeTarget, error) {
					resolveCalls++
					return RuntimeTarget{BaseURL: server.URL}, nil
				}),
				Client: NewHTTPClient(),
			}
			got := bridge.Read(context.Background(), test.input)
			if got.Status != "ready" || got.Operation != string(test.input.Operation) {
				t.Fatalf("Read() envelope = %#v", got)
			}
			if test.noRequest && requestCount != 0 || !test.noRequest && requestCount != 1 {
				t.Fatalf("business HTTP request count = %d", requestCount)
			}
			if resolveCalls != 1 {
				t.Fatalf("Resolve calls = %d, want 1", resolveCalls)
			}
		})
	}
}

func TestBridgeRejectsInvalidInputsBeforeResolvingRuntime(t *testing.T) {
	seven := 7
	tests := []struct {
		name  string
		input DashboardReadInput
	}{
		{name: "unknown operation", input: DashboardReadInput{Operation: "arbitrary"}},
		{name: "filter options missing range", input: DashboardReadInput{Operation: OperationFilterOptions}},
		{name: "summary missing filters", input: DashboardReadInput{Operation: OperationSummary, Range: &DashboardRange{Key: "today"}}},
		{name: "tool usage missing days", input: DashboardReadInput{Operation: OperationToolUsage}},
		{name: "tool usage invalid days", input: DashboardReadInput{Operation: OperationToolUsage, Days: intPointer(2)}},
		{name: "session detail missing key", input: DashboardReadInput{Operation: OperationSessionDetail, Range: &DashboardRange{Key: "7d"}, Filters: validEmptyFilters()}},
		{name: "unused field", input: DashboardReadInput{Operation: OperationQuota, Days: &seven}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			resolveCalls := 0
			bridge := &Bridge{
				Resolver: resolverFunc(func(context.Context) (RuntimeTarget, error) {
					resolveCalls++
					return RuntimeTarget{}, nil
				}),
				Client: NewHTTPClient(),
			}
			got := bridge.Read(context.Background(), test.input)
			if got.Status != "upstream_error" || resolveCalls != 0 {
				t.Fatalf("Read() = %#v, Resolve calls %d; expected input rejection before Resolve", got, resolveCalls)
			}
		})
	}
}

func TestBridgeRefreshAndStopUseOnlyTheirPOSTRoutes(t *testing.T) {
	tests := []struct {
		name      string
		call      func(*Bridge) DashboardEnvelope
		path      string
		operation string
	}{
		{name: "quota refresh", call: func(bridge *Bridge) DashboardEnvelope { return bridge.RefreshQuota(context.Background()) }, path: "/api/quota/refresh", operation: "refresh_quota"},
		{name: "service stop", call: func(bridge *Bridge) DashboardEnvelope { return bridge.StopService(context.Background()) }, path: "/api/service/stop", operation: "stop_service"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			requestCount := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				requestCount++
				if request.Method != http.MethodPost || request.URL.Path != test.path || request.URL.RawQuery != "" {
					t.Errorf("request = %s %s?%s", request.Method, request.URL.Path, request.URL.RawQuery)
				}
				if request.Header.Get("Accept") != "application/json" || request.Header.Get("X-Usagi-Request") != "1" {
					t.Errorf("POST headers = %#v", request.Header)
				}
				_, _ = w.Write([]byte(`{"state":"stopped"}`))
			}))
			defer server.Close()
			bridge := &Bridge{
				Resolver: resolverFunc(func(context.Context) (RuntimeTarget, error) { return RuntimeTarget{BaseURL: server.URL}, nil }),
				Client:   NewHTTPClient(),
			}
			got := test.call(bridge)
			if got.Status != "ready" || got.Operation != test.operation || requestCount != 1 {
				t.Fatalf("envelope = %#v, business request count = %d", got, requestCount)
			}
		})
	}
}

func TestBridgePreservesUpstreamJSONErrors(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusServiceUnavailable)
		_, _ = w.Write([]byte(`{"error":"MAGPIE_UNAVAILABLE","detail":{"provider":"codex"}}`))
	}))
	defer server.Close()
	bridge := &Bridge{
		Resolver: resolverFunc(func(context.Context) (RuntimeTarget, error) { return RuntimeTarget{BaseURL: server.URL}, nil }),
		Client:   NewHTTPClient(),
	}
	got := bridge.Read(context.Background(), DashboardReadInput{Operation: OperationQuota})
	if got.Status != "upstream_error" || got.HTTPStatus != http.StatusServiceUnavailable {
		t.Fatalf("envelope = %#v", got)
	}
	var payload map[string]any
	if err := json.Unmarshal(got.Payload, &payload); err != nil {
		t.Fatalf("decode payload: %v", err)
	}
	if payload["error"] != "MAGPIE_UNAVAILABLE" || payload["detail"].(map[string]any)["provider"] != "codex" {
		t.Fatalf("payload = %#v", payload)
	}
}

func TestBridgeTransportErrorRechecksRuntimeWithoutRetry(t *testing.T) {
	t.Run("runtime disappeared", func(t *testing.T) {
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { _, _ = w.Write([]byte(`{}`)) }))
		baseURL := server.URL
		server.Close()
		resolveCalls := 0
		bridge := &Bridge{
			Resolver: resolverFunc(func(context.Context) (RuntimeTarget, error) {
				resolveCalls++
				if resolveCalls == 1 {
					return RuntimeTarget{BaseURL: baseURL}, nil
				}
				return RuntimeTarget{}, ErrNotRunning
			}),
			Client: NewHTTPClient(),
		}
		got := bridge.Read(context.Background(), DashboardReadInput{Operation: OperationQuota})
		if got.Status != "not_running" || resolveCalls != 2 {
			t.Fatalf("envelope = %#v, Resolve calls = %d", got, resolveCalls)
		}
	})

	t.Run("runtime remains and business request is not replayed", func(t *testing.T) {
		resolveCalls, businessRequests := 0, 0
		client := &http.Client{Transport: roundTripperFunc(func(*http.Request) (*http.Response, error) {
			businessRequests++
			return nil, errors.New("transport failed")
		})}
		bridge := &Bridge{
			Resolver: resolverFunc(func(context.Context) (RuntimeTarget, error) {
				resolveCalls++
				return RuntimeTarget{BaseURL: "http://127.0.0.1"}, nil
			}),
			Client: client,
		}
		got := bridge.Read(context.Background(), DashboardReadInput{Operation: OperationQuota})
		if got.Status != "upstream_error" || resolveCalls != 2 || businessRequests != 1 {
			t.Fatalf("envelope = %#v, Resolve calls = %d, business requests = %d", got, resolveCalls, businessRequests)
		}
	})
}

func TestBridgeRejectsResponseBodiesOverEightMiB(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		chunk := bytes.Repeat([]byte("x"), 64<<10)
		for written := 0; written <= maxUpstreamBody; written += len(chunk) {
			if _, err := w.Write(chunk); err != nil {
				return
			}
		}
	}))
	defer server.Close()
	bridge := &Bridge{
		Resolver: resolverFunc(func(context.Context) (RuntimeTarget, error) { return RuntimeTarget{BaseURL: server.URL}, nil }),
		Client:   NewHTTPClient(),
	}
	got := bridge.Read(context.Background(), DashboardReadInput{Operation: OperationQuota})
	if got.Status != "upstream_error" || got.HTTPStatus != http.StatusOK || len(got.Payload) != 0 {
		t.Fatalf("oversized body envelope = %#v", got)
	}
}

func TestHTTPClientDisablesProxyAndRejectsCrossOriginRedirects(t *testing.T) {
	client := NewHTTPClient()
	transport, ok := client.Transport.(*http.Transport)
	if !ok || transport.Proxy != nil {
		t.Fatalf("HTTP transport = %#v, want proxy disabled", client.Transport)
	}
	if client.Timeout != 5*time.Second {
		t.Fatalf("HTTP timeout = %s, want 5s", client.Timeout)
	}
	redirectHits := 0
	target := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { redirectHits++ }))
	defer target.Close()
	origin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
		http.Redirect(w, request, target.URL, http.StatusFound)
	}))
	defer origin.Close()
	response, err := client.Get(origin.URL)
	if err != nil {
		t.Fatalf("GET redirected endpoint: %v", err)
	}
	_ = response.Body.Close()
	if response.StatusCode != http.StatusFound || redirectHits != 0 {
		t.Fatalf("redirect status = %d, target hits = %d", response.StatusCode, redirectHits)
	}
}

type roundTripperFunc func(*http.Request) (*http.Response, error)

func (f roundTripperFunc) RoundTrip(request *http.Request) (*http.Response, error) {
	return f(request)
}

func validEmptyFilters() *DashboardFilters {
	return &DashboardFilters{Agents: []string{}, Models: []ModelSelection{}, Projects: []ProjectSelection{}}
}

func intPointer(value int) *int {
	return &value
}
