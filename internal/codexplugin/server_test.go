package codexplugin

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/modelcontextprotocol/go-sdk/mcp"
)

func TestServerNegotiatesLegacyInitializeAndModernDiscover(t *testing.T) {
	tests := []struct {
		name            string
		protocolVersion string
	}{
		{name: "legacy initialize", protocolVersion: "2025-11-25"},
		{name: "modern discover", protocolVersion: "2026-07-28"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			clientSession, _, closeSessions := connectTestServer(t, test.protocolVersion, testServerDependencies(resolverFunc(func(context.Context) (RuntimeTarget, error) {
				return RuntimeTarget{}, ErrNotRunning
			})))
			defer closeSessions()
			result := clientSession.InitializeResult()
			if result.ProtocolVersion != test.protocolVersion {
				t.Fatalf("negotiated protocol = %q, want %q", result.ProtocolVersion, test.protocolVersion)
			}
			if result.Capabilities == nil {
				t.Fatal("server capabilities are missing")
			}
			if _, ok := result.Capabilities.Extensions[uiExtensionName]; !ok {
				t.Fatalf("UI extension missing from capabilities: %#v", result.Capabilities.Extensions)
			}
		})
	}
}

func TestServerToolsListMetadataAndReadInputSchema(t *testing.T) {
	clientSession, _, closeSessions := connectTestServer(t, "2025-11-25", testServerDependencies(resolverFunc(func(context.Context) (RuntimeTarget, error) {
		return RuntimeTarget{}, ErrNotRunning
	})))
	defer closeSessions()
	result, err := clientSession.ListTools(context.Background(), nil)
	if err != nil {
		t.Fatalf("ListTools(): %v", err)
	}
	if len(result.Tools) != 4 {
		t.Fatalf("tools/list returned %d tools, want 4", len(result.Tools))
	}
	toolsByName := make(map[string]*mcp.Tool, len(result.Tools))
	for _, tool := range result.Tools {
		toolsByName[tool.Name] = tool
		if len(tool.Icons) == 0 || tool.Icons[0].Source != "data:image/svg+xml;base64,PHN2Zz4=" {
			t.Errorf("tool %q icon = %#v", tool.Name, tool.Icons)
		}
	}

	entrypoint := toolsByName[toolEntrypointName]
	if entrypoint == nil || entrypoint.Title != "Usagi" {
		t.Fatalf("entrypoint tool = %#v", entrypoint)
	}
	entrypointMeta := mapValue(t, entrypoint.Meta)
	entrypointUI := mapValue(t, entrypointMeta["ui"])
	if entrypointUI["resourceUri"] != dashboardResourceURI {
		t.Errorf("entrypoint ui metadata = %#v", entrypointUI)
	}
	openaiUI := mapValue(t, entrypointMeta["openai/ui"])
	entrypoints := openaiUI["entrypoints"].([]any)
	if len(entrypoints) != 1 || mapValue(t, entrypoints[0])["type"] != "global" {
		t.Errorf("entrypoint OpenAI metadata = %#v", openaiUI)
	}

	readTool := toolsByName[toolReadName]
	if readTool == nil || readTool.Annotations == nil || !readTool.Annotations.ReadOnlyHint || readTool.Annotations.OpenWorldHint == nil || *readTool.Annotations.OpenWorldHint {
		t.Fatalf("read tool metadata = %#v", readTool)
	}
	readMeta := mapValue(t, readTool.Meta)
	readUI := mapValue(t, readMeta["ui"])
	if !reflect.DeepEqual(readUI["visibility"], []any{"app"}) {
		t.Errorf("read visibility = %#v", readUI["visibility"])
	}
	if _, ok := readUI["resourceUri"]; ok {
		t.Errorf("read tool unexpectedly binds a UI resource: %#v", readUI)
	}

	refresh := toolsByName[toolRefreshName]
	if refresh == nil || refresh.Annotations == nil || refresh.Annotations.ReadOnlyHint || refresh.Annotations.DestructiveHint == nil || *refresh.Annotations.DestructiveHint || refresh.Annotations.OpenWorldHint == nil || *refresh.Annotations.OpenWorldHint {
		t.Fatalf("refresh tool metadata = %#v", refresh)
	}
	stop := toolsByName[toolStopName]
	if stop == nil || stop.Annotations == nil || stop.Annotations.ReadOnlyHint || stop.Annotations.DestructiveHint == nil || !*stop.Annotations.DestructiveHint || !stop.Annotations.IdempotentHint || stop.Annotations.OpenWorldHint == nil || *stop.Annotations.OpenWorldHint {
		t.Fatalf("stop tool metadata = %#v", stop)
	}
	if !reflect.DeepEqual(schemaMap(t, refresh.InputSchema), map[string]any{"type": "object", "properties": map[string]any{}, "additionalProperties": false}) {
		t.Errorf("refresh schema = %#v", schemaMap(t, refresh.InputSchema))
	}
	if !reflect.DeepEqual(schemaMap(t, stop.InputSchema), map[string]any{"type": "object", "properties": map[string]any{}, "additionalProperties": false}) {
		t.Errorf("stop schema = %#v", schemaMap(t, stop.InputSchema))
	}

	schema := schemaMap(t, readTool.InputSchema)
	if schema["type"] != "object" {
		t.Fatalf("read input schema root type = %#v", schema["type"])
	}
	branches, ok := schema["oneOf"].([]any)
	if !ok || len(branches) != 12 {
		t.Fatalf("read input schema oneOf = %#v", schema["oneOf"])
	}
	wantRequired := map[string][]string{
		"probe": {"operation"}, "quota": {"operation"}, "filter_options": {"operation", "range"},
		"summary": {"operation", "range", "filters"}, "model_distribution": {"operation", "range", "filters"},
		"project_distribution": {"operation", "range", "filters"}, "sessions": {"operation", "range", "filters"},
		"tool_usage": {"operation", "days"}, "session_detail": {"operation", "range", "filters", "key"},
		"status": {"operation"}, "revision": {"operation"}, "service_state": {"operation"},
	}
	gotRequired := make(map[string][]string, len(branches))
	for _, branch := range branches {
		branchMap := mapValue(t, branch)
		properties := mapValue(t, branchMap["properties"])
		operation := mapValue(t, properties["operation"])["const"].(string)
		requiredValues := branchMap["required"].([]any)
		required := make([]string, len(requiredValues))
		for index, value := range requiredValues {
			required[index] = value.(string)
		}
		gotRequired[operation] = required
		if branchMap["additionalProperties"] != false {
			t.Errorf("operation %q allows additional fields", operation)
		}
	}
	if !reflect.DeepEqual(gotRequired, wantRequired) {
		t.Errorf("operation required fields = %#v, want %#v", gotRequired, wantRequired)
	}
}

func TestServerToolHandlersValidatePublishedSchemas(t *testing.T) {
	resolveCalls := 0
	clientSession, _, closeSessions := connectTestServer(t, "2025-11-25", testServerDependencies(resolverFunc(func(context.Context) (RuntimeTarget, error) {
		resolveCalls++
		return RuntimeTarget{}, ErrNotRunning
	})))
	defer closeSessions()
	tests := []struct {
		name     string
		toolName string
		input    string
	}{
		{name: "invalid operation", toolName: toolReadName, input: `{"operation":"arbitrary"}`},
		{name: "missing operation-specific input", toolName: toolReadName, input: `{"operation":"summary"}`},
		{name: "custom range missing from", toolName: toolReadName, input: `{"operation":"filter_options","range":{"key":"custom","to":"2026-02-01"}}`},
		{name: "custom range missing to", toolName: toolReadName, input: `{"operation":"filter_options","range":{"key":"custom","from":"2026-01-01"}}`},
		{name: "thread key missing session id", toolName: toolReadName, input: `{"operation":"session_detail","range":{"key":"today"},"filters":{"agents":[],"models":[],"projects":[]},"key":{"agent":"codex","thread_id":"thread"}}`},
		{name: "project selection missing path", toolName: toolReadName, input: `{"operation":"summary","range":{"key":"today"},"filters":{"agents":[],"models":[],"projects":[{"kind":"project"}]}}`},
		{name: "read null", toolName: toolReadName, input: `null`},
		{name: "mutation null", toolName: toolRefreshName, input: `null`},
		{name: "stop mutation null", toolName: toolStopName, input: `null`},
		{name: "mutation extra field", toolName: toolStopName, input: `{"confirm":true}`},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			result, err := clientSession.CallTool(context.Background(), &mcp.CallToolParams{
				Name: test.toolName, Arguments: json.RawMessage(test.input),
			})
			if err != nil {
				t.Fatalf("CallTool(): %v", err)
			}
			if !result.IsError || len(result.Content) == 0 || result.StructuredContent == nil {
				t.Fatalf("invalid call result = %#v", result)
			}
		})
	}
	if resolveCalls != 0 {
		t.Fatalf("invalid inputs called runtime resolver %d times", resolveCalls)
	}

	for _, toolName := range []string{toolEntrypointName, toolRefreshName, toolStopName} {
		result, err := clientSession.CallTool(context.Background(), &mcp.CallToolParams{Name: toolName, Arguments: map[string]any{}})
		if err != nil {
			t.Fatalf("CallTool(%s, {}): %v", toolName, err)
		}
		if result.IsError || result.StructuredContent == nil {
			t.Fatalf("CallTool(%s, {}) result = %#v", toolName, result)
		}
	}
	if resolveCalls != 3 {
		t.Fatalf("valid empty object inputs called runtime resolver %d times, want 3", resolveCalls)
	}
}

func TestServerEntrypointAndReadHandlerCallRuntimeBridge(t *testing.T) {
	upstreamRequests := 0
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
		upstreamRequests++
		if request.Method != http.MethodGet || request.URL.Path != "/api/usage/summary" {
			t.Errorf("upstream request = %s %s", request.Method, request.URL.Path)
		}
		_, _ = w.Write([]byte(`{"summary":true}`))
	}))
	defer upstream.Close()
	resolveCalls := 0
	clientSession, _, closeSessions := connectTestServer(t, "2025-11-25", testServerDependencies(resolverFunc(func(context.Context) (RuntimeTarget, error) {
		resolveCalls++
		return RuntimeTarget{BaseURL: upstream.URL}, nil
	})))
	defer closeSessions()

	entrypoint, err := clientSession.CallTool(context.Background(), &mcp.CallToolParams{Name: toolEntrypointName, Arguments: map[string]any{}})
	if err != nil || entrypoint.IsError || mapValue(t, entrypoint.StructuredContent)["status"] != "ready" || len(entrypoint.Content) == 0 {
		t.Fatalf("entrypoint result = %#v, error = %v", entrypoint, err)
	}
	read, err := clientSession.CallTool(context.Background(), &mcp.CallToolParams{
		Name: toolReadName,
		Arguments: map[string]any{
			"operation": "summary",
			"range":     map[string]any{"key": "7d"},
			"filters":   map[string]any{"agents": []string{}, "models": []any{}, "projects": []any{}},
		},
	})
	if err != nil || read.IsError || mapValue(t, read.StructuredContent)["status"] != "ready" || len(read.Content) == 0 {
		t.Fatalf("read result = %#v, error = %v", read, err)
	}
	if upstreamRequests != 1 || resolveCalls != 2 {
		t.Fatalf("upstream requests = %d, Resolve calls = %d", upstreamRequests, resolveCalls)
	}
}

func TestServerDashboardResourceRead(t *testing.T) {
	appHTML := `<!doctype html><html><body><main>Usagi Dashboard</main></body></html>`
	dependencies := testServerDependencies(resolverFunc(func(context.Context) (RuntimeTarget, error) {
		return RuntimeTarget{}, ErrNotRunning
	}))
	dependencies.AppHTML = appHTML
	clientSession, _, closeSessions := connectTestServer(t, "2025-11-25", dependencies)
	defer closeSessions()
	resources, err := clientSession.ListResources(context.Background(), nil)
	if err != nil {
		t.Fatalf("ListResources(): %v", err)
	}
	if len(resources.Resources) != 1 || resources.Resources[0].URI != dashboardResourceURI || resources.Resources[0].Name != "Usagi Dashboard" || resources.Resources[0].MIMEType != dashboardResourceMIMEType {
		t.Fatalf("resource descriptors = %#v", resources.Resources)
	}
	result, err := clientSession.ReadResource(context.Background(), &mcp.ReadResourceParams{URI: dashboardResourceURI})
	if err != nil {
		t.Fatalf("ReadResource(): %v", err)
	}
	if len(result.Contents) != 1 {
		t.Fatalf("resource content count = %d, want 1", len(result.Contents))
	}
	content := result.Contents[0]
	if content.URI != dashboardResourceURI || content.MIMEType != dashboardResourceMIMEType || content.Text != appHTML {
		t.Fatalf("resource content = %#v", content)
	}
	uiMeta := mapValue(t, content.Meta["ui"])
	if uiMeta["prefersBorder"] != false {
		t.Errorf("resource UI metadata = %#v", uiMeta)
	}
	for _, forbidden := range []string{"frameDomains", "connectDomains", "resourceDomains", "domain"} {
		if _, ok := uiMeta[forbidden]; ok {
			t.Errorf("resource metadata includes %q", forbidden)
		}
	}
	for _, forbidden := range []string{"<iframe", "http://127.0.0.1", "http://localhost"} {
		if strings.Contains(content.Text, forbidden) {
			t.Errorf("resource HTML contains forbidden string %q", forbidden)
		}
	}
}

func connectTestServer(t *testing.T, protocolVersion string, dependencies Dependencies) (*mcp.ClientSession, *mcp.ServerSession, func()) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	server := NewServer(dependencies)
	clientTransport, serverTransport := mcp.NewInMemoryTransports()
	serverSession, err := server.Connect(ctx, serverTransport, nil)
	if err != nil {
		cancel()
		t.Fatalf("server.Connect(): %v", err)
	}
	client := mcp.NewClient(&mcp.Implementation{Name: "test-client", Version: "1"}, &mcp.ClientOptions{Capabilities: &mcp.ClientCapabilities{}})
	clientSession, err := client.Connect(ctx, clientTransport, &mcp.ClientSessionOptions{ProtocolVersion: protocolVersion})
	if err != nil {
		_ = serverSession.Close()
		cancel()
		t.Fatalf("client.Connect(): %v", err)
	}
	return clientSession, serverSession, func() {
		_ = clientSession.Close()
		_ = serverSession.Close()
		cancel()
	}
}

func testServerDependencies(resolver RuntimeResolver) Dependencies {
	return Dependencies{
		Runtime:     resolver,
		AppHTML:     "<!doctype html><html><body>Usagi</body></html>",
		ToolIconURI: "data:image/svg+xml;base64,PHN2Zz4=",
		HTTPClient:  NewHTTPClient(),
	}
}

func mapValue(t *testing.T, value any) map[string]any {
	t.Helper()
	switch result := value.(type) {
	case map[string]any:
		return result
	case mcp.Meta:
		return map[string]any(result)
	default:
		t.Fatalf("value %T = %#v, want object", value, value)
		return nil
	}
}

func schemaMap(t *testing.T, value any) map[string]any {
	t.Helper()
	contents, err := json.Marshal(value)
	if err != nil {
		t.Fatalf("marshal schema: %v", err)
	}
	var result map[string]any
	if err := json.Unmarshal(contents, &result); err != nil {
		t.Fatalf("unmarshal schema: %v", err)
	}
	return result
}
