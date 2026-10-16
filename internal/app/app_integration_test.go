package app

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"testing/fstest"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type appTestBrowser struct {
	opened chan string
	err    error
}

func (b appTestBrowser) Open(target string) error {
	b.opened <- target
	return b.err
}

func TestRunUnavailableRuntimeRecoversQueryAndStops(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	descriptorPath := runtimeDescriptorPath(home)

	now := time.Now().Truncate(time.Second)
	point := magpie.UsagePoint{Label: now.Format(time.DateOnly), Time: now, UsageTotals: magpie.UsageTotals{Calls: 1, Input: 42, Output: 7}}
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/api/usage":
			_ = json.NewEncoder(w).Encode(magpie.UsageSummary{Period: "all", Bucket: "day", Series: []magpie.UsagePoint{point}})
			return
		case "/api/sessions":
			_ = json.NewEncoder(w).Encode(magpie.SessionsResponse{Sessions: []magpie.Session{{
				Agent: "codex", ID: "session-1", Cwd: "/repo", Title: "Session", Start: now, Last: now,
			}}})
			return
		case "/api/usage/requests":
			_ = json.NewEncoder(w).Encode(magpie.UsageRequestsPage{
				Bucket: "day", Series: []magpie.UsagePoint{point},
				Rows: []magpie.UsageRequest{{
					Time: time.Now(), Agent: "codex", AgentName: "Codex", Provider: "openai",
					Model: "gpt-6", Input: 42, Output: 7, Status: http.StatusOK, Session: "session-1", NativeSession: "session-1", Cost: 0.25, Priced: true,
				}},
				Total: 1,
			})
		default:
			http.NotFound(w, r)
		}
	}))
	defer upstream.Close()
	client := magpie.NewClient(upstream.URL, upstream.Client())
	first := newFakeRuntime(nil, nil)
	first.startErr = errors.New("Magpie is unavailable")
	second := newFakeRuntime(nil, client)
	runtimes := []MagpieRuntime{first, second}
	var factoryCalls atomic.Int32
	factory := func() MagpieRuntime {
		index := int(factoryCalls.Add(1)) - 1
		if index >= len(runtimes) {
			t.Errorf("runtime factory called %d times, want at most 2", index+1)
			return second
		}
		return runtimes[index]
	}
	browser := appTestBrowser{opened: make(chan string, 1), err: errors.New("browser unavailable")}
	assets := fstest.MapFS{"index.html": &fstest.MapFile{Data: []byte("test dashboard")}}
	finished := make(chan error, 1)
	runCtx, cancelRun := context.WithCancel(context.Background())
	runComplete := make(chan struct{})
	go func() {
		defer close(runComplete)
		finished <- run(runCtx, Dependencies{RuntimeFactory: factory, Browser: browser, StaticFS: assets}, PortRange{First: 0, Count: 1})
	}()
	t.Cleanup(func() {
		cancelRun()
		select {
		case <-runComplete:
		case <-time.After(6 * time.Second):
			t.Error("Run() did not stop after integration test cleanup")
		}
	})

	var baseURL string
	select {
	case target := <-browser.opened:
		parsed, err := url.Parse(target)
		if err != nil {
			t.Fatal(err)
		}
		if parsed.Path != "/" {
			t.Fatalf("browser path = %q, want /", parsed.Path)
		}
		baseURL = strings.TrimSuffix(target, "/")
	case err := <-finished:
		t.Fatalf("Run() exited before opening dashboard: %v", err)
	case <-time.After(5 * time.Second):
		t.Fatal("timed out waiting for browser open")
	}

	contents, err := os.ReadFile(descriptorPath)
	if err != nil {
		t.Fatalf("read %s runtime descriptor: %v", runtimeMode(), err)
	}
	var descriptor RuntimeDescriptor
	if err := json.Unmarshal(contents, &descriptor); err != nil {
		t.Fatalf("decode runtime descriptor %q: %v", contents, err)
	}
	parsed, err := url.Parse(baseURL)
	if err != nil {
		t.Fatal(err)
	}
	port, err := strconv.Atoi(parsed.Port())
	if err != nil {
		t.Fatal(err)
	}
	if descriptor.SchemaVersion != 1 || descriptor.PID != os.Getpid() || descriptor.Port != port || descriptor.BaseURL != baseURL || descriptor.RuntimeMode != runtimeMode() {
		t.Fatalf("runtime descriptor = %+v, want current process, mode %s, and %s", descriptor, runtimeMode(), baseURL)
	}

	clientWithoutProxy := &http.Client{Transport: &http.Transport{Proxy: nil}, Timeout: time.Second}
	defer clientWithoutProxy.CloseIdleConnections()
	status := waitForAppStatus(t, clientWithoutProxy, baseURL, magpie.RuntimeStatusUnavailable)
	if status != magpie.RuntimeStatusUnavailable {
		t.Fatalf("runtime status = %q, want unavailable", status)
	}

	response, body := appRequest(t, clientWithoutProxy, http.MethodGet, baseURL+"/api/health", nil)
	if response.StatusCode != http.StatusNoContent || response.Header.Get("X-Usagi-App") != "Usagi" {
		t.Fatalf("health response = %d marker %q body %q", response.StatusCode, response.Header.Get("X-Usagi-App"), body)
	}
	response, body = appRequest(t, clientWithoutProxy, http.MethodGet, baseURL+"/api/v1/info", nil)
	var info struct {
		RuntimeMode string `json:"runtime_mode"`
	}
	if response.StatusCode != http.StatusOK || json.Unmarshal(body, &info) != nil || info.RuntimeMode != runtimeMode() {
		t.Fatalf("info response = %d %s, want runtime_mode %q", response.StatusCode, body, runtimeMode())
	}
	response, body = appRequest(t, clientWithoutProxy, http.MethodGet, baseURL+"/", nil)
	if response.StatusCode != http.StatusOK || string(body) != "test dashboard" {
		t.Fatalf("static response = %d %q, want embedded test dashboard", response.StatusCode, body)
	}
	response, body = appRequest(t, clientWithoutProxy, http.MethodGet, baseURL+"/api/usage/summary?range=today", nil)
	if response.StatusCode != http.StatusServiceUnavailable || !strings.Contains(string(body), "DATA_LOADING") {
		t.Fatalf("initial summary response = %d %s, want DATA_LOADING while the first sync is incomplete", response.StatusCode, body)
	}

	waitForAppStatus(t, clientWithoutProxy, baseURL, magpie.RuntimeStatusReady)
	deadline := time.Now().Add(5 * time.Second)
	for {
		response, body = appRequest(t, clientWithoutProxy, http.MethodGet, baseURL+"/api/usage/summary?range=today", nil)
		if response.StatusCode == http.StatusOK && strings.Contains(string(body), `"input":42`) {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("recovered summary response = %d %s, want local query result after Sync", response.StatusCode, body)
		}
		time.Sleep(25 * time.Millisecond)
	}
	if got := factoryCalls.Load(); got != 2 {
		t.Fatalf("runtime factory calls = %d, want 2", got)
	}

	response, body = appRequest(t, clientWithoutProxy, http.MethodPost, baseURL+"/api/service/stop", nil)
	if response.StatusCode != http.StatusOK || string(body) != "{\"state\":\"stopped\"}\n" {
		t.Fatalf("service stop response = %d %q", response.StatusCode, body)
	}
	select {
	case err := <-finished:
		if err != nil {
			t.Fatalf("Run() error after service stop: %v", err)
		}
	case <-time.After(6 * time.Second):
		t.Fatal("Run() did not stop after service-stop request")
	}
	if got := first.closeCount(); got != 1 {
		t.Fatalf("unavailable Runtime Close calls = %d, want 1", got)
	}
	if got := second.closeCount(); got != 1 {
		t.Fatalf("ready Runtime Close calls = %d, want 1", got)
	}
	if _, err := os.Stat(descriptorPath); !os.IsNotExist(err) {
		t.Fatalf("runtime descriptor remains after shutdown: %v", err)
	}
}

func TestRunExistingInstanceDoesNotCreateRuntime(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	descriptorPath := runtimeDescriptorPath(home)
	if err := os.MkdirAll(filepath.Dir(descriptorPath), 0700); err != nil {
		t.Fatal(err)
	}
	original := []byte("leave existing descriptor alone")
	if err := os.WriteFile(descriptorPath, original, 0600); err != nil {
		t.Fatal(err)
	}

	existing := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/health":
			w.Header().Set("X-Usagi-App", "Usagi")
			w.WriteHeader(http.StatusNoContent)
		case "/api/v1/info":
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(map[string]string{"runtime_mode": runtimeMode()})
		default:
			http.NotFound(w, r)
		}
	}))
	defer existing.Close()
	parsed, err := url.Parse(existing.URL)
	if err != nil {
		t.Fatal(err)
	}
	port, err := strconv.Atoi(parsed.Port())
	if err != nil {
		t.Fatal(err)
	}
	var factoryCalls atomic.Int32
	browser := appTestBrowser{opened: make(chan string, 1)}
	err = run(context.Background(), Dependencies{
		RuntimeFactory: func() MagpieRuntime {
			factoryCalls.Add(1)
			return nil
		},
		Browser:  browser,
		StaticFS: fstest.MapFS{"index.html": &fstest.MapFile{Data: []byte("unused")}},
	}, PortRange{First: uint16(port), Count: 1})
	if err != nil {
		t.Fatal(err)
	}
	if got := factoryCalls.Load(); got != 0 {
		t.Fatalf("runtime factory calls for existing instance = %d, want 0", got)
	}
	select {
	case target := <-browser.opened:
		if want := existing.URL + "/"; target != want {
			t.Fatalf("existing dashboard URL = %q, want %q", target, want)
		}
	default:
		t.Fatal("existing Dashboard was not opened")
	}
	assertFileBytes(t, descriptorPath, original)
}

func TestRunMovesToNextFixturePortAndDescriptor(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	ports := reserveTestPortRange(t, 4)
	descriptorPath := runtimeDescriptorPath(home)

	first := startIsolatedRun(t, ports)
	firstDescriptor := readRuntimeDescriptor(t, descriptorPath)
	if firstDescriptor.RuntimeMode != runtimeMode() || firstDescriptor.BaseURL != first.baseURL {
		t.Fatalf("first descriptor = %+v, want mode %s and base URL %s", firstDescriptor, runtimeMode(), first.baseURL)
	}
	directoryInfo, err := os.Stat(filepath.Dir(descriptorPath))
	if err != nil {
		t.Fatal(err)
	}
	if mode := directoryInfo.Mode().Perm(); mode != 0700 {
		t.Fatalf("descriptor directory mode = %04o, want 0700", mode)
	}
	if _, err := os.Stat(filepath.Join(filepath.Dir(descriptorPath), "usagi.sqlite")); err != nil {
		t.Fatalf("isolated database was not created under HOME: %v", err)
	}
	firstPort := uint16(firstDescriptor.Port)
	first.stop()
	if _, err := os.Stat(descriptorPath); !os.IsNotExist(err) {
		t.Fatalf("descriptor remains after first run: %v", err)
	}

	occupied, err := net.Listen("tcp", net.JoinHostPort("127.0.0.1", strconv.Itoa(int(firstPort))))
	if err != nil {
		t.Fatalf("occupy old fixture port %d: %v", firstPort, err)
	}
	defer occupied.Close()

	second := startIsolatedRun(t, ports)
	secondDescriptor := readRuntimeDescriptor(t, descriptorPath)
	if secondDescriptor.RuntimeMode != runtimeMode() || secondDescriptor.BaseURL != second.baseURL || secondDescriptor.Port == int(firstPort) {
		t.Fatalf("second descriptor = %+v, want mode %s, base URL %s, and a new fixture port", secondDescriptor, runtimeMode(), second.baseURL)
	}
	second.stop()
	if _, err := os.Stat(descriptorPath); !os.IsNotExist(err) {
		t.Fatalf("descriptor remains after second run: %v", err)
	}
}

type runningIsolatedApp struct {
	baseURL string
	stop    func()
}

func startIsolatedRun(t *testing.T, ports PortRange) runningIsolatedApp {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	var runErr error
	browser := appTestBrowser{opened: make(chan string, 1)}
	runtime := newFakeRuntime(nil, nil)
	runtime.startErr = errors.New("Magpie is unavailable")
	go func() {
		defer close(done)
		runErr = run(ctx, Dependencies{
			RuntimeFactory: func() MagpieRuntime { return runtime },
			Browser:        browser,
			StaticFS:       fstest.MapFS{"index.html": &fstest.MapFile{Data: []byte("test dashboard")}},
		}, ports)
	}()

	stopped := false
	stop := func() {
		if stopped {
			return
		}
		stopped = true
		cancel()
		select {
		case <-done:
			if runErr != nil {
				t.Errorf("Run() error during shutdown: %v", runErr)
			}
		case <-time.After(6 * time.Second):
			t.Error("Run() did not stop during cleanup")
		}
	}
	t.Cleanup(stop)

	select {
	case target := <-browser.opened:
		baseURL := strings.TrimSuffix(target, "/")
		return runningIsolatedApp{baseURL: baseURL, stop: stop}
	case <-done:
		t.Fatalf("Run() exited before opening Dashboard: %v", runErr)
	case <-time.After(6 * time.Second):
		t.Fatal("timed out waiting for Dashboard startup")
	}
	return runningIsolatedApp{}
}

func reserveTestPortRange(t *testing.T, count uint16) PortRange {
	t.Helper()
	for attempt := 0; attempt < 100; attempt++ {
		listener, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		first := listener.Addr().(*net.TCPAddr).Port
		_ = listener.Close()
		if first+int(count)-1 > 65535 {
			continue
		}

		reserved := make([]net.Listener, 0, count)
		for port := first; port < first+int(count); port++ {
			candidate, err := net.Listen("tcp", net.JoinHostPort("127.0.0.1", strconv.Itoa(port)))
			if err != nil {
				break
			}
			reserved = append(reserved, candidate)
		}
		if len(reserved) == int(count) {
			for _, candidate := range reserved {
				_ = candidate.Close()
			}
			return PortRange{First: uint16(first), Count: count}
		}
		for _, candidate := range reserved {
			_ = candidate.Close()
		}
	}
	t.Fatal("could not reserve a small temporary loopback port range")
	return PortRange{}
}

func readRuntimeDescriptor(t *testing.T, path string) RuntimeDescriptor {
	t.Helper()
	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var descriptor RuntimeDescriptor
	if err := json.Unmarshal(contents, &descriptor); err != nil {
		t.Fatalf("decode runtime descriptor %q: %v", contents, err)
	}
	return descriptor
}

func appRequest(t *testing.T, client *http.Client, method, target string, body io.Reader) (*http.Response, []byte) {
	t.Helper()
	request, err := http.NewRequest(method, target, body)
	if err != nil {
		t.Fatal(err)
	}
	response, err := client.Do(request)
	if err != nil {
		t.Fatalf("%s %s: %v", method, target, err)
	}
	defer response.Body.Close()
	contents, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	return response, contents
}

func waitForAppStatus(t *testing.T, client *http.Client, baseURL string, expected magpie.RuntimeStatus) magpie.RuntimeStatus {
	t.Helper()
	deadline := time.Now().Add(12 * time.Second)
	for time.Now().Before(deadline) {
		response, body := appRequest(t, client, http.MethodGet, baseURL+"/api/status", nil)
		if response.StatusCode == http.StatusOK {
			var status struct {
				RuntimeState string `json:"runtime_state"`
			}
			if err := json.Unmarshal(body, &status); err != nil {
				t.Fatalf("decode runtime status %q: %v", body, err)
			}
			if magpie.RuntimeStatus(status.RuntimeState) == expected {
				return expected
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("runtime did not reach %q", expected)
	return ""
}

func assertFileBytes(t *testing.T, path string, expected []byte) {
	t.Helper()
	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	if string(contents) != string(expected) {
		t.Fatalf("contents of %s = %q, want %q", path, contents, expected)
	}
}
