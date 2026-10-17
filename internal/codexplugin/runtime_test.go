package codexplugin

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/platform"
)

type resolverFunc func(context.Context) (RuntimeTarget, error)

func (f resolverFunc) Resolve(ctx context.Context) (RuntimeTarget, error) {
	return f(ctx)
}

func TestProductionRuntimeResolverUsesSharedDataRoot(t *testing.T) {
	dataRoot, err := platform.UsagiDataDir()
	if err != nil {
		t.Fatalf("platform.UsagiDataDir(): %v", err)
	}
	resolver, err := NewProductionRuntimeResolver()
	if err != nil {
		t.Fatalf("NewProductionRuntimeResolver(): %v", err)
	}
	concrete, ok := resolver.(*runtimeResolver)
	if !ok {
		t.Fatalf("NewProductionRuntimeResolver() returned %T", resolver)
	}
	want := filepath.Join(dataRoot, "runtime.json")
	if concrete.descriptorPath != want {
		t.Fatalf("descriptor path = %q, want %q", concrete.descriptorPath, want)
	}
}

func TestRuntimeResolverDescriptorErrorsNormalizeInBridge(t *testing.T) {
	tests := []struct {
		name      string
		contents  []byte
		writeFile bool
		wantError error
	}{
		{name: "missing", wantError: ErrNotRunning},
		{name: "invalid JSON", contents: []byte("{"), writeFile: true, wantError: ErrInvalidDescriptor},
		{name: "invalid fields", contents: []byte(`{"schema_version":1,"pid":1,"port":0,"base_url":"http://127.0.0.1","runtime_mode":"development"}`), writeFile: true, wantError: ErrInvalidDescriptor},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "runtime.json")
			if test.writeFile {
				if err := os.WriteFile(path, test.contents, 0600); err != nil {
					t.Fatal(err)
				}
			}
			resolver := &runtimeResolver{descriptorPath: path, client: NewHTTPClient()}
			if _, err := resolver.Resolve(context.Background()); !errors.Is(err, test.wantError) {
				t.Fatalf("Resolve() error = %v, want %v", err, test.wantError)
			}
			bridge := &Bridge{Resolver: resolver, Client: NewHTTPClient()}
			got := bridge.Read(context.Background(), DashboardReadInput{Operation: OperationProbe})
			if got.Status != "not_running" {
				t.Fatalf("Bridge.Read() status = %q, want not_running", got.Status)
			}
		})
	}
}

func TestRuntimeResolverValidatesHealthAndInfoIdentity(t *testing.T) {
	var healthCalls, infoCalls int
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/api/health":
			healthCalls++
			w.Header().Add("X-Usagi-App", "Usagi")
			w.WriteHeader(http.StatusNoContent)
		case "/api/v1/info":
			infoCalls++
			_, _ = w.Write([]byte(`{"runtime_mode":"production"}`))
		default:
			http.NotFound(w, request)
		}
	}))
	defer server.Close()

	resolver := newTestResolver(t, server.URL, NewHTTPClient())
	target, err := resolver.Resolve(context.Background())
	if err != nil {
		t.Fatalf("Resolve() error = %v", err)
	}
	if target.BaseURL != server.URL || target.PID <= 0 || target.RuntimeMode != "production" {
		t.Fatalf("Resolve() target = %#v", target)
	}
	if healthCalls != 1 || infoCalls != 1 {
		t.Fatalf("identity requests = health %d, info %d; want one each", healthCalls, infoCalls)
	}
}

func TestRuntimeResolverRejectsInvalidIdentity(t *testing.T) {
	tests := []struct {
		name   string
		health http.HandlerFunc
		info   string
	}{
		{name: "wrong health marker", health: func(w http.ResponseWriter, _ *http.Request) { w.Header().Set("X-Usagi-App", "Other") }, info: `{"runtime_mode":"production"}`},
		{name: "duplicate health marker", health: func(w http.ResponseWriter, _ *http.Request) {
			w.Header().Add("X-Usagi-App", "Usagi")
			w.Header().Add("X-Usagi-App", "Usagi")
		}, info: `{"runtime_mode":"production"}`},
		{name: "wrong runtime mode", health: func(w http.ResponseWriter, _ *http.Request) {
			w.Header().Set("X-Usagi-App", "Usagi")
			w.WriteHeader(http.StatusNoContent)
		}, info: `{"runtime_mode":"development"}`},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				switch request.URL.Path {
				case "/api/health":
					test.health(w, request)
				case "/api/v1/info":
					_, _ = w.Write([]byte(test.info))
				default:
					http.NotFound(w, request)
				}
			}))
			defer server.Close()
			resolver := newTestResolver(t, server.URL, NewHTTPClient())
			if _, err := resolver.Resolve(context.Background()); !errors.Is(err, ErrInvalidIdentity) {
				t.Fatalf("Resolve() error = %v, want ErrInvalidIdentity", err)
			}
		})
	}
}

func TestRuntimeResolverReadsDescriptorOnEveryCall(t *testing.T) {
	first := identityTestServer(t)
	defer first.Close()
	second := identityTestServer(t)
	defer second.Close()
	path := filepath.Join(t.TempDir(), "runtime.json")
	writeTestDescriptor(t, path, first.URL)
	resolver := &runtimeResolver{descriptorPath: path, client: NewHTTPClient()}

	firstTarget, err := resolver.Resolve(context.Background())
	if err != nil {
		t.Fatalf("first Resolve() error = %v", err)
	}
	writeTestDescriptor(t, path, second.URL)
	secondTarget, err := resolver.Resolve(context.Background())
	if err != nil {
		t.Fatalf("second Resolve() error = %v", err)
	}
	if firstTarget.BaseURL != first.URL || secondTarget.BaseURL != second.URL {
		t.Fatalf("resolved endpoints = %q then %q, want %q then %q", firstTarget.BaseURL, secondTarget.BaseURL, first.URL, second.URL)
	}
}

func newTestResolver(t *testing.T, baseURL string, client *http.Client) *runtimeResolver {
	t.Helper()
	path := filepath.Join(t.TempDir(), "runtime.json")
	writeTestDescriptor(t, path, baseURL)
	return &runtimeResolver{descriptorPath: path, client: client}
}

func writeTestDescriptor(t *testing.T, path, baseURL string) {
	t.Helper()
	parsed, err := url.Parse(baseURL)
	if err != nil {
		t.Fatal(err)
	}
	port, err := strconv.Atoi(parsed.Port())
	if err != nil {
		t.Fatal(err)
	}
	contents, err := json.Marshal(runtimeDescriptor{
		SchemaVersion: 1,
		PID:           os.Getpid(),
		Port:          port,
		BaseURL:       baseURL,
		RuntimeMode:   "production",
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, contents, 0600); err != nil {
		t.Fatal(err)
	}
}

func identityTestServer(t *testing.T) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/api/health":
			w.Header().Set("X-Usagi-App", "Usagi")
			w.WriteHeader(http.StatusNoContent)
		case "/api/v1/info":
			_, _ = w.Write([]byte(`{"runtime_mode":"production"}`))
		default:
			http.NotFound(w, request)
		}
	}))
}
