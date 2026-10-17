//go:build windows

package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"sync/atomic"
	"time"

	"github.com/Hogeexxl/Usagi/internal/codexplugin"
	"github.com/Hogeexxl/Usagi/internal/platform"
	"github.com/modelcontextprotocol/go-sdk/mcp"
)

const (
	protocolVersion = "2025-11-25"
	entrypointTool  = "usagi.dashboard"
	readTool        = "usagi.dashboard.read"
)

type options struct {
	pluginRoot string
	binary     string
}

type fixture struct {
	server     *httptest.Server
	healthOK   atomic.Bool
	modeOK     atomic.Bool
	healthHits atomic.Int32
	infoHits   atomic.Int32
}

type surface struct {
	Tools            json.RawMessage `json:"tools"`
	Resources        json.RawMessage `json:"resources"`
	ResourceContents json.RawMessage `json:"resource_contents"`
}

type descriptorGuard struct {
	path        string
	dataRoot    string
	rootExisted bool
	fileExisted bool
	contents    []byte
}

type staticResolver struct {
	err error
}

func (r staticResolver) Resolve(context.Context) (codexplugin.RuntimeTarget, error) {
	return codexplugin.RuntimeTarget{}, r.err
}

func main() {
	var opts options
	flag.StringVar(&opts.pluginRoot, "PluginRoot", "", "Windows Plugin package root")
	flag.StringVar(&opts.binary, "Binary", "", "Windows Plugin MCP server binary")
	flag.Parse()

	if err := run(opts); err != nil {
		_, _ = fmt.Fprintln(os.Stderr, "FAIL:", err)
		os.Exit(1)
	}
	fmt.Println("PASS: Windows Plugin stdio, runtime descriptor, identity, and schema checks")
}

func run(opts options) (returnErr error) {
	if opts.pluginRoot == "" || opts.binary == "" {
		return errors.New("-PluginRoot and -Binary are required")
	}
	pluginRoot, err := filepath.Abs(opts.pluginRoot)
	if err != nil {
		return fmt.Errorf("resolve PluginRoot: %w", err)
	}
	binary, err := filepath.Abs(opts.binary)
	if err != nil {
		return fmt.Errorf("resolve Binary: %w", err)
	}
	appHTML, err := os.ReadFile(filepath.Join(pluginRoot, "app", "usagi-dashboard.html"))
	if err != nil {
		return fmt.Errorf("read packaged Dashboard HTML: %w", err)
	}
	sidebar, err := os.ReadFile(filepath.Join(pluginRoot, "assets", "sidebar.svg"))
	if err != nil {
		return fmt.Errorf("read packaged sidebar icon: %w", err)
	}
	iconURI := "data:image/svg+xml;base64," + base64.StdEncoding.EncodeToString(sidebar)

	guard, err := prepareDescriptor()
	if err != nil {
		return err
	}
	defer func() {
		if restoreErr := guard.Restore(); restoreErr != nil {
			if returnErr == nil {
				returnErr = restoreErr
			} else {
				fmt.Fprintln(os.Stderr, "restore runtime descriptor:", restoreErr)
			}
		}
	}()

	first := newFixture()
	defer first.server.Close()
	second := newFixture()
	defer second.server.Close()
	if err := first.assertDynamicEndpoint("first"); err != nil {
		return err
	}
	if err := second.assertDynamicEndpoint("second"); err != nil {
		return err
	}
	if first.server.URL == second.server.URL {
		return errors.New("test runtime endpoints did not change")
	}
	if err := guard.Write(first.server.URL); err != nil {
		return err
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	session, stderr, closeSession, err := connectPackage(ctx, pluginRoot, binary)
	if err != nil {
		return err
	}
	defer func() {
		if closeErr := closeSession(); closeErr != nil {
			if returnErr == nil {
				returnErr = fmt.Errorf("close Plugin stdio session: %w", closeErr)
			} else {
				fmt.Fprintln(os.Stderr, "close Plugin stdio session:", closeErr)
			}
		}
		if stderr.Len() > 0 {
			fmt.Fprintln(os.Stderr, "Plugin stderr:", stderr.String())
		}
	}()

	packageSurface, err := captureSurface(ctx, session)
	if err != nil {
		return fmt.Errorf("capture packaged Plugin surface: %w", err)
	}
	sourceSurface, err := captureSourceSurface(ctx, appHTML, iconURI)
	if err != nil {
		return fmt.Errorf("capture shared source surface: %w", err)
	}
	if !equalJSON(packageSurface, sourceSurface) {
		return fmt.Errorf("tool/resource schema snapshot differs from the shared macOS/Windows source\npackage: %s\nsource:  %s",
			marshalSurface(packageSurface), marshalSurface(sourceSurface))
	}

	if err := expectToolStatus(ctx, session, entrypointTool, nil, "ready"); err != nil {
		return err
	}
	if err := expectToolStatus(ctx, session, readTool, map[string]any{"operation": "probe"}, "ready"); err != nil {
		return err
	}
	firstHealthBeforeChange := first.healthHits.Load()
	firstInfoBeforeChange := first.infoHits.Load()

	if err := guard.Write(second.server.URL); err != nil {
		return err
	}
	if err := expectToolStatus(ctx, session, entrypointTool, nil, "ready"); err != nil {
		return fmt.Errorf("resolve changed descriptor: %w", err)
	}
	if second.healthHits.Load() != 1 || second.infoHits.Load() != 1 {
		return fmt.Errorf("changed endpoint identity requests = health %d, info %d; want one each",
			second.healthHits.Load(), second.infoHits.Load())
	}
	if first.healthHits.Load() != firstHealthBeforeChange || first.infoHits.Load() != firstInfoBeforeChange {
		return errors.New("resolver reused the previous descriptor endpoint after it changed")
	}

	second.healthOK.Store(false)
	if err := expectToolStatus(ctx, session, entrypointTool, nil, "not_running"); err != nil {
		return fmt.Errorf("invalid /api/health identity: %w", err)
	}
	second.healthOK.Store(true)
	second.modeOK.Store(false)
	if err := expectToolStatus(ctx, session, entrypointTool, nil, "not_running"); err != nil {
		return fmt.Errorf("invalid /api/v1/info production mode: %w", err)
	}
	second.modeOK.Store(true)

	if err := guard.Remove(); err != nil {
		return err
	}
	if err := expectToolStatus(ctx, session, entrypointTool, nil, "not_running"); err != nil {
		return fmt.Errorf("missing descriptor: %w", err)
	}
	if err := guard.Write(first.server.URL); err != nil {
		return err
	}
	if err := expectToolStatus(ctx, session, readTool, map[string]any{"operation": "probe"}, "ready"); err != nil {
		return fmt.Errorf("resolve restored descriptor: %w", err)
	}
	return nil
}

func prepareDescriptor() (*descriptorGuard, error) {
	dataRoot, err := platform.UsagiDataDir()
	if err != nil {
		return nil, fmt.Errorf("resolve shared Usagi data root: %w", err)
	}
	guard := &descriptorGuard{
		path:     filepath.Join(dataRoot, "runtime.json"),
		dataRoot: dataRoot,
	}
	if _, err := os.Stat(dataRoot); err == nil {
		guard.rootExisted = true
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, fmt.Errorf("stat Usagi data root: %w", err)
	}
	contents, err := os.ReadFile(guard.path)
	if err == nil {
		guard.fileExisted = true
		guard.contents = contents
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, fmt.Errorf("read existing runtime descriptor: %w", err)
	}
	if err := os.MkdirAll(dataRoot, 0700); err != nil {
		return nil, fmt.Errorf("create Usagi data root: %w", err)
	}
	return guard, nil
}

func (g *descriptorGuard) Write(baseURL string) error {
	parsed, err := url.Parse(baseURL)
	if err != nil {
		return fmt.Errorf("parse test endpoint: %w", err)
	}
	port, err := strconv.Atoi(parsed.Port())
	if err != nil {
		return fmt.Errorf("parse test endpoint port: %w", err)
	}
	descriptor := struct {
		SchemaVersion int    `json:"schema_version"`
		PID           int    `json:"pid"`
		Port          int    `json:"port"`
		BaseURL       string `json:"base_url"`
		RuntimeMode   string `json:"runtime_mode"`
	}{
		SchemaVersion: 1,
		PID:           os.Getpid(),
		Port:          port,
		BaseURL:       baseURL,
		RuntimeMode:   "production",
	}
	contents, err := json.Marshal(descriptor)
	if err != nil {
		return fmt.Errorf("encode test runtime descriptor: %w", err)
	}
	if err := os.WriteFile(g.path, append(contents, '\n'), 0600); err != nil {
		return fmt.Errorf("write test runtime descriptor: %w", err)
	}
	return nil
}

func (g *descriptorGuard) Remove() error {
	if err := os.Remove(g.path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove test runtime descriptor: %w", err)
	}
	return nil
}

func (g *descriptorGuard) Restore() error {
	if g.fileExisted {
		if err := os.MkdirAll(g.dataRoot, 0700); err != nil {
			return fmt.Errorf("recreate Usagi data root: %w", err)
		}
		if err := os.WriteFile(g.path, g.contents, 0600); err != nil {
			return fmt.Errorf("restore runtime descriptor: %w", err)
		}
		return nil
	}
	if err := os.Remove(g.path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove test runtime descriptor: %w", err)
	}
	if !g.rootExisted {
		if err := os.Remove(g.dataRoot); err != nil && !errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("remove test data root: %w", err)
		}
	}
	return nil
}

func newFixture() *fixture {
	f := &fixture{}
	f.healthOK.Store(true)
	f.modeOK.Store(true)
	f.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/api/health":
			f.healthHits.Add(1)
			if f.healthOK.Load() {
				w.Header().Set("X-Usagi-App", "Usagi")
				w.WriteHeader(http.StatusNoContent)
				return
			}
			w.Header().Set("X-Usagi-App", "Other")
			w.WriteHeader(http.StatusServiceUnavailable)
		case "/api/v1/info":
			f.infoHits.Add(1)
			if f.modeOK.Load() {
				_, _ = w.Write([]byte(`{"runtime_mode":"production"}`))
				return
			}
			_, _ = w.Write([]byte(`{"runtime_mode":"development"}`))
		default:
			http.NotFound(w, request)
		}
	}))
	return f
}

func (f *fixture) assertDynamicEndpoint(label string) error {
	parsed, err := url.Parse(f.server.URL)
	if err != nil {
		return fmt.Errorf("parse %s fixture endpoint: %w", label, err)
	}
	host, portText, err := net.SplitHostPort(parsed.Host)
	if err != nil {
		return fmt.Errorf("split %s fixture endpoint: %w", label, err)
	}
	port, err := strconv.Atoi(portText)
	if err != nil || port < 1 || port > 65535 {
		return fmt.Errorf("%s fixture endpoint has invalid port %q", label, portText)
	}
	if host != "127.0.0.1" && host != "::1" && host != "localhost" {
		return fmt.Errorf("%s fixture endpoint is not loopback: %s", label, host)
	}
	if port == 37891 {
		return fmt.Errorf("%s fixture unexpectedly used the production default port", label)
	}
	return nil
}

func connectPackage(ctx context.Context, pluginRoot, binary string) (*mcp.ClientSession, *bytes.Buffer, func() error, error) {
	cmd := exec.Command(binary)
	cmd.Dir = pluginRoot
	stderr := new(bytes.Buffer)
	cmd.Stderr = stderr
	transport := &mcp.CommandTransport{Command: cmd}
	client := mcp.NewClient(&mcp.Implementation{Name: "usagi-windows-plugin-gate", Version: "1"}, nil)
	session, err := client.Connect(ctx, transport, &mcp.ClientSessionOptions{ProtocolVersion: protocolVersion})
	if err != nil {
		return nil, stderr, func() error { return nil }, fmt.Errorf("connect packaged stdio server: %w; stderr: %s", err, stderr.String())
	}
	return session, stderr, session.Close, nil
}

func captureSurface(ctx context.Context, session *mcp.ClientSession) (surface, error) {
	toolsResult, err := session.ListTools(ctx, nil)
	if err != nil {
		return surface{}, err
	}
	resourcesResult, err := session.ListResources(ctx, nil)
	if err != nil {
		return surface{}, err
	}
	if len(resourcesResult.Resources) != 1 {
		return surface{}, fmt.Errorf("resources/list returned %d resources, want 1", len(resourcesResult.Resources))
	}
	readResult, err := session.ReadResource(ctx, &mcp.ReadResourceParams{URI: resourcesResult.Resources[0].URI})
	if err != nil {
		return surface{}, err
	}
	tools, err := json.Marshal(toolsResult.Tools)
	if err != nil {
		return surface{}, err
	}
	resources, err := json.Marshal(resourcesResult.Resources)
	if err != nil {
		return surface{}, err
	}
	contents, err := json.Marshal(readResult.Contents)
	if err != nil {
		return surface{}, err
	}
	return surface{Tools: tools, Resources: resources, ResourceContents: contents}, nil
}

func captureSourceSurface(ctx context.Context, appHTML []byte, iconURI string) (surface, error) {
	server := codexplugin.NewServer(codexplugin.Dependencies{
		Runtime:     staticResolver{err: codexplugin.ErrNotRunning},
		AppHTML:     string(appHTML),
		ToolIconURI: iconURI,
		HTTPClient:  codexplugin.NewHTTPClient(),
	})
	clientTransport, serverTransport := mcp.NewInMemoryTransports()
	serverSession, err := server.Connect(ctx, serverTransport, nil)
	if err != nil {
		return surface{}, err
	}
	defer serverSession.Close()
	client := mcp.NewClient(&mcp.Implementation{Name: "usagi-shared-source-schema", Version: "1"}, nil)
	clientSession, err := client.Connect(ctx, clientTransport, &mcp.ClientSessionOptions{ProtocolVersion: protocolVersion})
	if err != nil {
		return surface{}, err
	}
	defer clientSession.Close()
	return captureSurface(ctx, clientSession)
}

func expectToolStatus(ctx context.Context, session *mcp.ClientSession, name string, arguments any, want string) error {
	result, err := session.CallTool(ctx, &mcp.CallToolParams{Name: name, Arguments: arguments})
	if err != nil {
		return fmt.Errorf("call %s: %w", name, err)
	}
	if result.IsError {
		return fmt.Errorf("call %s returned an error result: %#v", name, result)
	}
	contents, err := json.Marshal(result.StructuredContent)
	if err != nil {
		return fmt.Errorf("encode %s result: %w", name, err)
	}
	var envelope struct {
		Status string `json:"status"`
	}
	if err := json.Unmarshal(contents, &envelope); err != nil {
		return fmt.Errorf("decode %s result: %w", name, err)
	}
	if envelope.Status != want {
		return fmt.Errorf("call %s status = %q, want %q; result: %s", name, envelope.Status, want, contents)
	}
	return nil
}

func equalJSON(left, right surface) bool {
	return bytes.Equal(marshalSurface(left), marshalSurface(right))
}

func marshalSurface(value surface) []byte {
	contents, _ := json.Marshal(value)
	return contents
}
