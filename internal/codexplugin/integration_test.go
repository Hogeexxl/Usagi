package codexplugin

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/url"
	"reflect"
	"strings"
	"testing"

	"github.com/modelcontextprotocol/go-sdk/mcp"
)

func TestMCPHTTPIntegration(t *testing.T) {
	const appHTML = `<!doctype html><html><body><main>Usagi Dashboard</main></body></html>`

	requestCounts := make(map[string]int)
	checkRequest := func(request *http.Request, method, path string, wantQuery url.Values) {
		t.Helper()
		if request.Method != method || request.URL.Path != path {
			t.Errorf("upstream request = %s %s, want %s %s", request.Method, request.URL.Path, method, path)
		}
		gotQuery, err := url.ParseQuery(request.URL.RawQuery)
		if err != nil {
			t.Errorf("parse query for %s %s: %v", request.Method, request.URL.Path, err)
		}
		if !reflect.DeepEqual(gotQuery, wantQuery) {
			t.Errorf("query for %s %s = %#v, want %#v", request.Method, request.URL.Path, gotQuery, wantQuery)
		}
		if got := request.Header.Get("Accept"); got != "application/json" {
			t.Errorf("Accept for %s %s = %q, want application/json", request.Method, request.URL.Path, got)
		}
		if method == http.MethodPost {
			if got := request.Header.Get("X-Usagi-Request"); got != "1" {
				t.Errorf("X-Usagi-Request for %s %s = %q, want 1", request.Method, request.URL.Path, got)
			}
		} else if got := request.Header.Get("X-Usagi-Request"); got != "" {
			t.Errorf("unexpected X-Usagi-Request for %s %s = %q", request.Method, request.URL.Path, got)
		}
	}
	writeJSON := func(w http.ResponseWriter, status int, body string) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_, _ = w.Write([]byte(body))
	}

	wantSummaryQuery := url.Values{
		"range":                   {"7d"},
		"agent":                   {"codex", "claude"},
		"model_at":                {"openai/gpt 6"},
		"project_path":            {"/work/one & two"},
		"include_workspace":       {"1"},
		"include_projectless":     {"1"},
		"include_unknown_project": {"1"},
	}
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
		requestCounts[request.Method+" "+request.URL.Path]++
		emptyQuery := url.Values{}
		switch request.URL.Path {
		case "/api/health":
			checkRequest(request, http.MethodGet, "/api/health", emptyQuery)
			w.Header().Add("X-Usagi-App", "Usagi")
			w.WriteHeader(http.StatusNoContent)
		case "/api/v1/info":
			checkRequest(request, http.MethodGet, "/api/v1/info", emptyQuery)
			writeJSON(w, http.StatusOK, `{"runtime_mode":"production"}`)
		case "/api/usage/summary":
			checkRequest(request, http.MethodGet, "/api/usage/summary", wantSummaryQuery)
			writeJSON(w, http.StatusOK, `{"summary":{"requests":12}}`)
		case "/api/revision":
			checkRequest(request, http.MethodGet, "/api/revision", emptyQuery)
			writeJSON(w, http.StatusOK, `{"data_revision":8,"status_revision":3}`)
		case "/api/service":
			checkRequest(request, http.MethodGet, "/api/service", emptyQuery)
			writeJSON(w, http.StatusOK, `{"state":"running"}`)
		case "/api/usage/filter-options":
			checkRequest(request, http.MethodGet, "/api/usage/filter-options", url.Values{
				"range": {"custom"}, "from": {"2026-01-02"}, "to": {"2026-03-04"},
			})
			writeJSON(w, http.StatusServiceUnavailable, `{"error":"DATA_LOADING"}`)
		case "/api/quota":
			checkRequest(request, http.MethodGet, "/api/quota", emptyQuery)
			writeJSON(w, http.StatusServiceUnavailable, `{"error":"MAGPIE_UNAVAILABLE"}`)
		case "/api/quota/refresh":
			checkRequest(request, http.MethodPost, "/api/quota/refresh", emptyQuery)
			writeJSON(w, http.StatusOK, `{"refreshed":true}`)
		case "/api/service/stop":
			checkRequest(request, http.MethodPost, "/api/service/stop", emptyQuery)
			writeJSON(w, http.StatusOK, `{"state":"stopped"}`)
		default:
			t.Errorf("unexpected upstream request: %s %s?%s", request.Method, request.URL.Path, request.URL.RawQuery)
			http.NotFound(w, request)
		}
	}))
	defer upstream.Close()

	resolver := newTestResolver(t, upstream.URL, NewHTTPClient())
	dependencies := testServerDependencies(resolver)
	dependencies.AppHTML = appHTML
	clientSession, _, closeSessions := connectTestServer(t, "2025-11-25", dependencies)
	defer closeSessions()

	initialize := clientSession.InitializeResult()
	if initialize.ProtocolVersion != "2025-11-25" {
		t.Fatalf("negotiated protocol = %q, want 2025-11-25", initialize.ProtocolVersion)
	}
	if initialize.Capabilities == nil {
		t.Fatal("initialize result has no server capabilities")
	}
	if _, ok := initialize.Capabilities.Extensions[uiExtensionName]; !ok {
		t.Fatalf("initialize capabilities lack %q extension: %#v", uiExtensionName, initialize.Capabilities.Extensions)
	}

	toolsResult, err := clientSession.ListTools(context.Background(), nil)
	if err != nil {
		t.Fatalf("tools/list: %v", err)
	}
	if len(toolsResult.Tools) != 4 {
		t.Fatalf("tools/list returned %d tools, want 4", len(toolsResult.Tools))
	}
	toolsByName := make(map[string]*mcp.Tool, len(toolsResult.Tools))
	for _, tool := range toolsResult.Tools {
		toolsByName[tool.Name] = tool
	}
	entrypointTool := toolsByName[toolEntrypointName]
	if entrypointTool == nil || entrypointTool.Title != "Usagi" {
		t.Fatalf("entrypoint tool = %#v", entrypointTool)
	}
	entrypointMeta := mapValue(t, entrypointTool.Meta)
	if mapValue(t, entrypointMeta["ui"])["resourceUri"] != dashboardResourceURI {
		t.Errorf("entrypoint UI metadata = %#v", entrypointMeta["ui"])
	}
	entrypoints, ok := mapValue(t, entrypointMeta["openai/ui"])["entrypoints"].([]any)
	if !ok || len(entrypoints) != 1 || mapValue(t, entrypoints[0])["type"] != "global" {
		t.Errorf("entrypoint global metadata = %#v", entrypointMeta["openai/ui"])
	}
	readTool := toolsByName[toolReadName]
	if readTool == nil || readTool.Annotations == nil || !readTool.Annotations.ReadOnlyHint || readTool.Annotations.OpenWorldHint == nil || *readTool.Annotations.OpenWorldHint {
		t.Fatalf("read tool metadata = %#v", readTool)
	}
	readMeta := mapValue(t, readTool.Meta)
	readUI := mapValue(t, readMeta["ui"])
	if !reflect.DeepEqual(readUI["visibility"], []any{"app"}) {
		t.Errorf("read tool visibility = %#v", readUI["visibility"])
	}
	if _, ok := readUI["resourceUri"]; ok {
		t.Errorf("read tool unexpectedly binds a UI resource: %#v", readUI)
	}
	refreshTool := toolsByName[toolRefreshName]
	if refreshTool == nil || refreshTool.Annotations == nil || refreshTool.Annotations.ReadOnlyHint || refreshTool.Annotations.DestructiveHint == nil || *refreshTool.Annotations.DestructiveHint || refreshTool.Annotations.OpenWorldHint == nil || *refreshTool.Annotations.OpenWorldHint {
		t.Fatalf("refresh tool metadata = %#v", refreshTool)
	}
	refreshMeta := mapValue(t, refreshTool.Meta)
	if !reflect.DeepEqual(mapValue(t, refreshMeta["ui"])["visibility"], []any{"app"}) {
		t.Errorf("refresh tool visibility = %#v", refreshMeta["ui"])
	}
	stopTool := toolsByName[toolStopName]
	if stopTool == nil || stopTool.Annotations == nil || stopTool.Annotations.ReadOnlyHint || stopTool.Annotations.DestructiveHint == nil || !*stopTool.Annotations.DestructiveHint || !stopTool.Annotations.IdempotentHint || stopTool.Annotations.OpenWorldHint == nil || *stopTool.Annotations.OpenWorldHint {
		t.Fatalf("stop tool metadata = %#v", stopTool)
	}
	stopMeta := mapValue(t, stopTool.Meta)
	if !reflect.DeepEqual(mapValue(t, stopMeta["ui"])["visibility"], []any{"app"}) {
		t.Errorf("stop tool visibility = %#v", stopMeta["ui"])
	}

	callTool := func(name string, arguments any) *mcp.CallToolResult {
		t.Helper()
		result, err := clientSession.CallTool(context.Background(), &mcp.CallToolParams{Name: name, Arguments: arguments})
		if err != nil {
			t.Fatalf("tools/call %s: %v", name, err)
		}
		if len(result.Content) == 0 {
			t.Fatalf("tools/call %s returned no text content", name)
		}
		content, ok := result.Content[0].(*mcp.TextContent)
		if !ok || strings.TrimSpace(content.Text) == "" {
			t.Fatalf("tools/call %s content = %#v, want meaningful text", name, result.Content)
		}
		return result
	}
	structuredContent := func(result *mcp.CallToolResult) map[string]any {
		t.Helper()
		encoded, err := json.Marshal(result.StructuredContent)
		if err != nil {
			t.Fatalf("marshal structured content: %v", err)
		}
		var decoded map[string]any
		if err := json.Unmarshal(encoded, &decoded); err != nil {
			t.Fatalf("decode structured content: %v", err)
		}
		return decoded
	}
	assertEnvelope := func(result *mcp.CallToolResult, status, operation string, payload any) map[string]any {
		t.Helper()
		got := structuredContent(result)
		if got["status"] != status || got["operation"] != operation {
			t.Fatalf("envelope = %#v, want status %q operation %q", got, status, operation)
		}
		if payload != nil && !reflect.DeepEqual(got["payload"], payload) {
			t.Fatalf("%s payload = %#v, want %#v", operation, got["payload"], payload)
		}
		return got
	}

	entrypoint := callTool(toolEntrypointName, map[string]any{})
	entrypointContent := structuredContent(entrypoint)
	if entrypointContent["status"] != "ready" || len(entrypointContent) != 1 {
		t.Fatalf("entrypoint structured content = %#v, want only status ready", entrypointContent)
	}

	resource, err := clientSession.ReadResource(context.Background(), &mcp.ReadResourceParams{URI: dashboardResourceURI})
	if err != nil {
		t.Fatalf("resources/read: %v", err)
	}
	if len(resource.Contents) != 1 {
		t.Fatalf("resources/read returned %d contents, want 1", len(resource.Contents))
	}
	content := resource.Contents[0]
	if content.URI != dashboardResourceURI || content.MIMEType != dashboardResourceMIMEType || content.Text != appHTML {
		t.Fatalf("resource content = %#v", content)
	}
	if mapValue(t, content.Meta["ui"])["prefersBorder"] != false {
		t.Errorf("resource UI metadata = %#v", content.Meta["ui"])
	}

	probe := callTool(toolReadName, map[string]any{"operation": "probe"})
	assertEnvelope(probe, "ready", "probe", map[string]any{})
	summary := callTool(toolReadName, map[string]any{
		"operation": "summary",
		"range":     map[string]any{"key": "7d"},
		"filters": map[string]any{
			"agents":   []string{"codex", "claude"},
			"models":   []map[string]string{{"provider_id": "openai", "model": "gpt 6"}},
			"projects": []map[string]string{{"kind": "project", "path": "/work/one & two"}, {"kind": "workspace"}, {"kind": "projectless"}, {"kind": "unknown"}},
		},
	})
	assertEnvelope(summary, "ready", "summary", map[string]any{"summary": map[string]any{"requests": float64(12)}})
	revision := callTool(toolReadName, map[string]any{"operation": "revision"})
	assertEnvelope(revision, "ready", "revision", map[string]any{"data_revision": float64(8), "status_revision": float64(3)})
	serviceState := callTool(toolReadName, map[string]any{"operation": "service_state"})
	assertEnvelope(serviceState, "ready", "service_state", map[string]any{"state": "running"})

	dataLoading := callTool(toolReadName, map[string]any{
		"operation": "filter_options",
		"range":     map[string]any{"key": "custom", "from": "2026-01-02", "to": "2026-03-04"},
	})
	dataLoadingEnvelope := assertEnvelope(dataLoading, "upstream_error", "filter_options", map[string]any{"error": "DATA_LOADING"})
	if dataLoadingEnvelope["http_status"] != float64(http.StatusServiceUnavailable) {
		t.Errorf("DATA_LOADING HTTP status = %#v, want %d", dataLoadingEnvelope["http_status"], http.StatusServiceUnavailable)
	}
	magpieUnavailable := callTool(toolReadName, map[string]any{"operation": "quota"})
	magpieEnvelope := assertEnvelope(magpieUnavailable, "upstream_error", "quota", map[string]any{"error": "MAGPIE_UNAVAILABLE"})
	if magpieEnvelope["http_status"] != float64(http.StatusServiceUnavailable) {
		t.Errorf("MAGPIE_UNAVAILABLE HTTP status = %#v, want %d", magpieEnvelope["http_status"], http.StatusServiceUnavailable)
	}

	refresh := callTool(toolRefreshName, map[string]any{})
	assertEnvelope(refresh, "ready", "refresh_quota", map[string]any{"refreshed": true})
	stop := callTool(toolStopName, map[string]any{})
	assertEnvelope(stop, "ready", "stop_service", map[string]any{"state": "stopped"})

	wantRequestCounts := map[string]int{
		"GET /api/health":               9,
		"GET /api/v1/info":              9,
		"GET /api/usage/summary":        1,
		"GET /api/revision":             1,
		"GET /api/service":              1,
		"GET /api/usage/filter-options": 1,
		"GET /api/quota":                1,
		"POST /api/quota/refresh":       1,
		"POST /api/service/stop":        1,
	}
	if !reflect.DeepEqual(requestCounts, wantRequestCounts) {
		t.Errorf("upstream request counts = %#v, want %#v", requestCounts, wantRequestCounts)
	}
	postRequests := requestCounts["POST /api/quota/refresh"] + requestCounts["POST /api/service/stop"]
	if postRequests != 2 {
		t.Errorf("POST request count = %d, want exactly the quota refresh and service stop routes", postRequests)
	}
}
