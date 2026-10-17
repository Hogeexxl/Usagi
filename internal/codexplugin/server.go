package codexplugin

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"

	"github.com/google/jsonschema-go/jsonschema"
	"github.com/modelcontextprotocol/go-sdk/mcp"
)

const (
	toolEntrypointName = "usagi.dashboard"
	toolReadName       = "usagi.dashboard.read"
	toolRefreshName    = "usagi.dashboard.refresh_quota"
	toolStopName       = "usagi.dashboard.stop_service"
	uiExtensionName    = "io.modelcontextprotocol/ui"
)

type Dependencies struct {
	Runtime     RuntimeResolver
	AppHTML     string
	ToolIconURI string
	HTTPClient  *http.Client
}

var dashboardReadInputSchema = json.RawMessage(`{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "oneOf": [
    {"properties": {"operation": {"const": "probe"}}, "required": ["operation"], "additionalProperties": false},
    {"properties": {"operation": {"const": "quota"}}, "required": ["operation"], "additionalProperties": false},
    {"properties": {"operation": {"const": "filter_options"}, "range": {"$ref": "#/$defs/range"}}, "required": ["operation", "range"], "additionalProperties": false},
    {"properties": {"operation": {"const": "summary"}, "range": {"$ref": "#/$defs/range"}, "filters": {"$ref": "#/$defs/filters"}}, "required": ["operation", "range", "filters"], "additionalProperties": false},
    {"properties": {"operation": {"const": "model_distribution"}, "range": {"$ref": "#/$defs/range"}, "filters": {"$ref": "#/$defs/filters"}}, "required": ["operation", "range", "filters"], "additionalProperties": false},
    {"properties": {"operation": {"const": "project_distribution"}, "range": {"$ref": "#/$defs/range"}, "filters": {"$ref": "#/$defs/filters"}}, "required": ["operation", "range", "filters"], "additionalProperties": false},
    {"properties": {"operation": {"const": "sessions"}, "range": {"$ref": "#/$defs/range"}, "filters": {"$ref": "#/$defs/filters"}}, "required": ["operation", "range", "filters"], "additionalProperties": false},
    {"properties": {"operation": {"const": "tool_usage"}, "days": {"enum": [1, 7, 30]}}, "required": ["operation", "days"], "additionalProperties": false},
    {"properties": {"operation": {"const": "session_detail"}, "range": {"$ref": "#/$defs/range"}, "filters": {"$ref": "#/$defs/filters"}, "key": {"$ref": "#/$defs/threadKey"}}, "required": ["operation", "range", "filters", "key"], "additionalProperties": false},
    {"properties": {"operation": {"const": "status"}}, "required": ["operation"], "additionalProperties": false},
    {"properties": {"operation": {"const": "revision"}}, "required": ["operation"], "additionalProperties": false},
    {"properties": {"operation": {"const": "service_state"}}, "required": ["operation"], "additionalProperties": false}
  ],
  "$defs": {
    "range": {
      "type": "object",
      "oneOf": [
        {"properties": {"key": {"const": "today"}}, "required": ["key"], "additionalProperties": false},
        {"properties": {"key": {"const": "yesterday"}}, "required": ["key"], "additionalProperties": false},
        {"properties": {"key": {"const": "7d"}}, "required": ["key"], "additionalProperties": false},
        {"properties": {"key": {"const": "30d"}}, "required": ["key"], "additionalProperties": false},
        {"properties": {"key": {"const": "year"}}, "required": ["key"], "additionalProperties": false},
        {"properties": {"key": {"const": "custom"}, "from": {"type": "string"}, "to": {"type": "string"}}, "required": ["key", "from", "to"], "additionalProperties": false}
      ]
    },
    "filters": {
      "type": "object",
      "properties": {
        "agents": {"type": "array", "items": {"type": "string"}},
        "models": {"type": "array", "items": {"$ref": "#/$defs/modelSelection"}},
        "projects": {"type": "array", "items": {"$ref": "#/$defs/projectSelection"}}
      },
      "required": ["agents", "models", "projects"],
      "additionalProperties": false
    },
    "modelSelection": {
      "type": "object",
      "properties": {"provider_id": {"type": "string"}, "model": {"type": "string"}},
      "required": ["provider_id", "model"],
      "additionalProperties": false
    },
    "projectSelection": {
      "type": "object",
      "oneOf": [
        {"properties": {"kind": {"const": "project"}, "path": {"type": "string"}}, "required": ["kind", "path"], "additionalProperties": false},
        {"properties": {"kind": {"const": "workspace"}}, "required": ["kind"], "additionalProperties": false},
        {"properties": {"kind": {"const": "projectless"}}, "required": ["kind"], "additionalProperties": false},
        {"properties": {"kind": {"const": "unknown"}}, "required": ["kind"], "additionalProperties": false}
      ]
    },
    "threadKey": {
      "type": "object",
      "properties": {"agent": {"type": "string"}, "session_id": {"type": "string"}, "thread_id": {"type": "string"}},
      "required": ["agent", "session_id", "thread_id"],
      "additionalProperties": false
    }
  }
}`)

var emptyObjectInputSchema = json.RawMessage(`{"type":"object","properties":{},"additionalProperties":false}`)
var dashboardReadResolvedInputSchema = resolveInputSchema(dashboardReadInputSchema)
var emptyObjectResolvedInputSchema = resolveInputSchema(emptyObjectInputSchema)

func NewServer(dependencies Dependencies) *mcp.Server {
	capabilities := &mcp.ServerCapabilities{}
	capabilities.AddExtension(uiExtensionName, nil)
	server := mcp.NewServer(&mcp.Implementation{
		Name:    "usagi-codex-plugin",
		Title:   "Usagi",
		Version: "1.7.0",
	}, &mcp.ServerOptions{Capabilities: capabilities})
	bridge := &Bridge{Resolver: dependencies.Runtime, Client: dependencies.HTTPClient}
	icons := []mcp.Icon{{Source: dependencies.ToolIconURI, MIMEType: "image/svg+xml"}}
	falseValue := false
	trueValue := true

	server.AddTool(&mcp.Tool{
		Name:  toolEntrypointName,
		Title: "Usagi",
		Meta: mcp.Meta{
			"ui":        map[string]any{"resourceUri": dashboardResourceURI},
			"openai/ui": map[string]any{"entrypoints": []any{map[string]any{"type": "global"}}},
		},
		InputSchema: emptyObjectInputSchema,
		Icons:       icons,
	}, func(ctx context.Context, request *mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		if _, err := validateToolInput(request, emptyObjectResolvedInputSchema, true); err != nil {
			return invalidCallResult(err), nil
		}
		status := "not_running"
		if _, err := dependencies.Runtime.Resolve(ctx); err == nil {
			status = "ready"
		}
		text := "Usagi is not running."
		if status == "ready" {
			text = "Usagi is running."
		}
		return &mcp.CallToolResult{
			Content:           []mcp.Content{&mcp.TextContent{Text: text}},
			StructuredContent: map[string]string{"status": status},
		}, nil
	})

	server.AddTool(&mcp.Tool{
		Name:        toolReadName,
		Title:       "Read Usagi dashboard data",
		Meta:        mcp.Meta{"ui": map[string]any{"visibility": []string{"app"}}},
		Annotations: &mcp.ToolAnnotations{ReadOnlyHint: true, OpenWorldHint: &falseValue},
		InputSchema: dashboardReadInputSchema,
		Icons:       icons,
	}, func(ctx context.Context, request *mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		contents, err := validateToolInput(request, dashboardReadResolvedInputSchema, false)
		if err != nil {
			return invalidCallResult(err), nil
		}
		var input DashboardReadInput
		if err := decodeCallInput(contents, &input); err != nil {
			return invalidCallResult(err), nil
		}
		if err := validateDashboardReadInput(input); err != nil {
			return &mcp.CallToolResult{
				Content:           []mcp.Content{&mcp.TextContent{Text: "Invalid read input: " + err.Error()}},
				StructuredContent: inputErrorEnvelope(string(input.Operation), err),
				IsError:           true,
			}, nil
		}
		return bridgeCallResult(bridge.Read(ctx, input)), nil
	})

	server.AddTool(&mcp.Tool{
		Name:        toolRefreshName,
		Title:       "Refresh Usagi quota",
		Meta:        mcp.Meta{"ui": map[string]any{"visibility": []string{"app"}}},
		Annotations: &mcp.ToolAnnotations{ReadOnlyHint: false, DestructiveHint: &falseValue, OpenWorldHint: &falseValue},
		InputSchema: emptyObjectInputSchema,
		Icons:       icons,
	}, func(ctx context.Context, request *mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		if _, err := validateToolInput(request, emptyObjectResolvedInputSchema, true); err != nil {
			return invalidCallResult(err), nil
		}
		return bridgeCallResult(bridge.RefreshQuota(ctx)), nil
	})

	server.AddTool(&mcp.Tool{
		Name:        toolStopName,
		Title:       "Stop Usagi service",
		Meta:        mcp.Meta{"ui": map[string]any{"visibility": []string{"app"}}},
		Annotations: &mcp.ToolAnnotations{ReadOnlyHint: false, DestructiveHint: &trueValue, IdempotentHint: true, OpenWorldHint: &falseValue},
		InputSchema: emptyObjectInputSchema,
		Icons:       icons,
	}, func(ctx context.Context, request *mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		if _, err := validateToolInput(request, emptyObjectResolvedInputSchema, true); err != nil {
			return invalidCallResult(err), nil
		}
		return bridgeCallResult(bridge.StopService(ctx)), nil
	})

	addDashboardResource(server, dependencies.AppHTML)
	return server
}

func validateToolInput(request *mcp.CallToolRequest, schema *jsonschema.Resolved, allowMissing bool) (json.RawMessage, error) {
	if request == nil || request.Params == nil {
		return nil, fmt.Errorf("tool arguments are missing")
	}
	contents := request.Params.Arguments
	if len(contents) == 0 {
		if !allowMissing {
			return nil, fmt.Errorf("arguments must be a JSON object")
		}
		contents = json.RawMessage(`{}`)
	}
	var instance any
	if err := json.Unmarshal(contents, &instance); err != nil {
		return nil, fmt.Errorf("decode arguments for schema validation: %w", err)
	}
	if err := schema.Validate(instance); err != nil {
		return nil, fmt.Errorf("arguments do not match the tool input schema: %w", err)
	}
	return contents, nil
}

func decodeCallInput(contents json.RawMessage, target any) error {
	decoder := json.NewDecoder(bytes.NewReader(contents))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(target); err != nil {
		return fmt.Errorf("decode arguments: %w", err)
	}
	if err := decoder.Decode(new(any)); err != io.EOF {
		if err == nil {
			return fmt.Errorf("arguments contain multiple JSON values")
		}
		return fmt.Errorf("decode trailing arguments: %w", err)
	}
	return nil
}

func resolveInputSchema(contents json.RawMessage) *jsonschema.Resolved {
	var schema jsonschema.Schema
	if err := json.Unmarshal(contents, &schema); err != nil {
		panic(fmt.Errorf("decode MCP input schema: %w", err))
	}
	resolved, err := schema.Resolve(nil)
	if err != nil {
		panic(fmt.Errorf("resolve MCP input schema: %w", err))
	}
	return resolved
}

func invalidCallResult(err error) *mcp.CallToolResult {
	payload, _ := json.Marshal(map[string]string{"error": "INVALID_INPUT", "message": err.Error()})
	return &mcp.CallToolResult{
		Content:           []mcp.Content{&mcp.TextContent{Text: "Invalid tool input: " + err.Error()}},
		StructuredContent: json.RawMessage(payload),
		IsError:           true,
	}
}

func bridgeCallResult(envelope DashboardEnvelope) *mcp.CallToolResult {
	text := "Usagi " + envelope.Operation + " succeeded."
	switch envelope.Status {
	case "not_running":
		text = "Usagi is not running."
	case "upstream_error":
		text = "Usagi " + envelope.Operation + " failed."
		if envelope.HTTPStatus != 0 {
			text = fmt.Sprintf("Usagi %s failed with HTTP status %d.", envelope.Operation, envelope.HTTPStatus)
		}
	}
	return &mcp.CallToolResult{
		Content:           []mcp.Content{&mcp.TextContent{Text: text}},
		StructuredContent: envelope,
	}
}
