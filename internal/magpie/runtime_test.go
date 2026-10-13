package magpie

import (
	"context"
	"encoding/hex"
	"errors"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

const (
	runtimeHelperProcessEnv = "USAGI_MAGPIE_RUNTIME_TEST_HELPER_PROCESS"
	runtimeHelperHealthFail = "USAGI_MAGPIE_RUNTIME_TEST_HEALTH_FAIL"
	runtimeHelperReadyFail  = "USAGI_MAGPIE_RUNTIME_TEST_READY_FAIL"
	runtimeHelperStarted    = "USAGI_MAGPIE_RUNTIME_TEST_WEB_STARTED"
	runtimeHelperExpected   = "USAGI_MAGPIE_RUNTIME_TEST_EXPECT_ADDR"
	runtimeHelperHoldMarker = "USAGI_MAGPIE_RUNTIME_TEST_HOLD_MARKER"
)

func init() {
	if os.Getenv(runtimeHelperProcessEnv) == "1" {
		os.Exit(runRuntimeHelperProcess(os.Args[1:]))
	}
}

func TestRuntimeClientUnavailableUntilReady(t *testing.T) {
	r := NewRuntime()
	if r.Status() != RuntimeStatusUnavailable {
		t.Fatalf("initial Status() = %q, want unavailable", r.Status())
	}
	if client, ok := r.Client(); ok || client != nil {
		t.Fatalf("initial Client() = (%v, %t), want (nil, false)", client, ok)
	}
}

func TestRuntimeStartCloseAndClientLifetime(t *testing.T) {
	installRuntimeHelper(t)
	r := NewRuntime()
	if cap(r.done) != 1 {
		t.Fatalf("NewRuntime() done capacity = %d, want 1", cap(r.done))
	}

	startupCtx, cancel := context.WithCancel(context.Background())
	if err := r.Start(startupCtx); err != nil {
		cancel()
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = r.Close() })
	if r.Status() != RuntimeStatusReady {
		t.Fatalf("Status() after Start() = %q, want ready", r.Status())
	}
	client, ok := r.Client()
	if !ok || client == nil {
		t.Fatal("Client() after Start() did not return the ready client")
	}
	cancel()

	progress, err := client.SessionProgress(context.Background())
	if err != nil {
		t.Fatalf("SessionProgress() after startup context cancellation: %v", err)
	}
	if progress.Files != 8 || progress.Read != 512 {
		t.Fatalf("SessionProgress() = %+v", progress)
	}
	if err := r.Start(context.Background()); err == nil {
		t.Fatal("second Start() succeeded")
	}
	if got, ready := r.Client(); !ready || got != client {
		t.Fatal("second Start() changed the published Client")
	}

	r.mu.RLock()
	cmd := r.cmd
	r.mu.RUnlock()
	if cmd == nil {
		t.Fatal("Start() did not publish its owned command")
	}
	if err := r.Close(); err != nil {
		t.Fatal(err)
	}
	if r.Status() != RuntimeStatusUnavailable {
		t.Fatalf("Status() after Close() = %q, want unavailable", r.Status())
	}
	if got, ready := r.Client(); ready || got != nil {
		t.Fatalf("Client() after Close() = (%v, %t), want (nil, false)", got, ready)
	}
	r.mu.RLock()
	cmdAfterClose := r.cmd
	r.mu.RUnlock()
	if cmdAfterClose != nil {
		t.Fatal("Close() did not clear r.cmd after Wait")
	}
	if len(r.done) != 0 {
		t.Fatal("Close() did not consume the Wait result")
	}
	if _, err := client.SessionProgress(context.Background()); err == nil {
		t.Fatal("published Client still reached the sidecar after Close()")
	}

	closed := make(chan error, 1)
	go func() { closed <- r.Close() }()
	select {
	case err := <-closed:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("second Close() blocked")
	}
}

func TestRuntimeUnexpectedExitRevokesClientBeforeDone(t *testing.T) {
	installRuntimeHelper(t)
	r := NewRuntime()
	if cap(r.done) != 1 {
		t.Fatalf("NewRuntime() done capacity = %d, want 1", cap(r.done))
	}
	sentinel := errors.New("hold Wait result until exit state is checked")
	r.done <- sentinel
	sentinelTaken := false
	t.Cleanup(func() {
		if !sentinelTaken {
			select {
			case got := <-r.done:
				if got != sentinel {
					r.done <- got
				}
			default:
			}
		}
		_ = r.Close()
	})
	if err := r.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	client, ok := r.Client()
	if !ok {
		t.Fatal("Runtime did not publish a ready Client")
	}

	r.mu.RLock()
	cmd := r.cmd
	r.mu.RUnlock()
	if cmd == nil {
		t.Fatal("Runtime has no owned sidecar command")
	}
	if err := cmd.Process.Kill(); err != nil {
		t.Fatal(err)
	}
	waitForRuntimeStatus(t, r, RuntimeStatusUnavailable, 2*time.Second)
	if got, ready := r.Client(); ready || got != nil {
		t.Fatalf("Client() after sidecar exit = (%v, %t), want (nil, false)", got, ready)
	}
	if len(r.done) != 1 {
		t.Fatalf("r.done length after exit = %d, want the sentinel to remain buffered", len(r.done))
	}
	if got := <-r.done; got != sentinel {
		t.Fatalf("r.done contained %v, want the sentinel", got)
	}
	sentinelTaken = true
	// The cap=1 channel stays full with the sentinel until Client revocation is
	// observed. Removing it lets the real cmd.Wait result reach Close below.

	if err := r.Close(); err != nil {
		t.Fatal(err)
	}
	r.mu.RLock()
	cmdAfterClose := r.cmd
	r.mu.RUnlock()
	if cmdAfterClose != nil {
		t.Fatal("Close() did not clear r.cmd after receiving Wait")
	}
	if _, err := client.SessionProgress(context.Background()); err == nil {
		t.Fatal("Client reached the sidecar after its unexpected exit")
	}
}

func TestRuntimeStartupFailures(t *testing.T) {
	t.Run("healthcheck fails before sidecar", func(t *testing.T) {
		installRuntimeHelper(t)
		startedFile := filepath.Join(t.TempDir(), "web-started")
		t.Setenv(runtimeHelperStarted, startedFile)
		t.Setenv(runtimeHelperHealthFail, "1")

		r := NewRuntime()
		if err := r.Start(context.Background()); err == nil {
			t.Fatal("Start() succeeded after healthcheck failure")
		}
		assertRuntimeUnavailable(t, r)
		if _, err := os.Stat(startedFile); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("sidecar start marker stat error = %v, want not-exist", err)
		}
		r.mu.RLock()
		cmd := r.cmd
		r.mu.RUnlock()
		if cmd != nil {
			t.Fatal("healthcheck failure published a sidecar command")
		}
	})

	t.Run("readiness fails after sidecar start", func(t *testing.T) {
		installRuntimeHelper(t)
		startedFile := filepath.Join(t.TempDir(), "web-started")
		t.Setenv(runtimeHelperStarted, startedFile)
		t.Setenv(runtimeHelperReadyFail, "progress")

		r := NewRuntime()
		startupCtx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		if err := r.Start(startupCtx); !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("Start() error = %v, want deadline exceeded", err)
		}
		assertRuntimeUnavailable(t, r)
		if _, err := os.Stat(startedFile); err != nil {
			t.Fatalf("sidecar start marker: %v", err)
		}
		r.mu.RLock()
		cmd := r.cmd
		r.mu.RUnlock()
		if cmd != nil {
			t.Fatal("readiness failure returned before sidecar Close() cleanup")
		}
		if len(r.done) != 0 {
			t.Fatal("readiness failure did not wait for and consume cmd.Wait()")
		}
	})
}

func TestRuntimeCloseDoesNotHoldMutexWhileWaiting(t *testing.T) {
	installRuntimeHelper(t)
	holdMarker := filepath.Join(t.TempDir(), "inherited-output-held")
	t.Setenv(runtimeHelperHoldMarker, holdMarker)
	r := NewRuntime()
	if err := r.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	closeStarted := false
	t.Cleanup(func() {
		if !closeStarted {
			_ = r.Close()
		}
	})
	client, ok := r.Client()
	if !ok {
		t.Fatal("Runtime did not publish a ready Client")
	}
	if _, err := client.SessionProgress(context.Background()); err != nil {
		t.Fatalf("sidecar was not reachable before Close(): %v", err)
	}

	closed := make(chan error, 1)
	closeStarted = true
	go func() { closed <- r.Close() }()
	deadline := time.Now().Add(2 * time.Second)
	locked := false
	for time.Now().Before(deadline) {
		if r.mu.TryLock() {
			if r.closing && r.cmd != nil {
				locked = true
				break
			}
			r.mu.Unlock()
		}
		time.Sleep(time.Millisecond)
	}
	if !locked {
		t.Fatal("Close() did not release r.mu while its owned command was still being collected")
	}
	defer func() {
		if locked {
			r.mu.Unlock()
		}
	}()

	deadline = time.Now().Add(2 * time.Second)
	sidecarExited := false
	for time.Now().Before(deadline) {
		requestCtx, cancel := context.WithTimeout(context.Background(), time.Second)
		_, err := client.SessionProgress(requestCtx)
		timedOut := errors.Is(err, context.DeadlineExceeded)
		cancel()
		if err != nil && !timedOut {
			sidecarExited = true
			break
		}
		time.Sleep(time.Millisecond)
	}
	if !sidecarExited {
		t.Fatal("sidecar did not exit while Close() waited for cmd.Wait()")
	}
	select {
	case err := <-closed:
		t.Fatalf("Close() returned while the test held r.mu: %v", err)
	default:
	}
	select {
	case <-r.done:
		t.Fatal("Wait result was published while Wait goroutine could not acquire r.mu")
	default:
	}
	r.mu.Unlock()
	locked = false

	select {
	case err := <-closed:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(4 * time.Second):
		t.Fatal("Close() and cmd.Wait() did not finish after releasing r.mu")
	}
	assertRuntimeUnavailable(t, r)
}

func TestLiveRuntime(t *testing.T) {
	if os.Getenv("USAGI_LIVE_MAGPIE") != "1" {
		t.Skip("set USAGI_LIVE_MAGPIE=1 to run the live Magpie smoke test")
	}

	r := NewRuntime()
	t.Cleanup(func() { _ = r.Close() })
	if err := r.Start(context.Background()); err != nil {
		t.Fatalf("Runtime.Start() failed: %v", err)
	}
	if r.Status() != RuntimeStatusReady {
		t.Fatalf("Runtime status = %q, want ready", r.Status())
	}
	client, ok := r.Client()
	if !ok {
		t.Fatal("Runtime.Client() did not return the ready client")
	}

	ctx := context.Background()
	sessions, err := client.Sessions(ctx, 0)
	if err != nil {
		t.Fatalf("Client.Sessions() failed: %v", err)
	}
	if len(sessions.Sessions) == 0 {
		t.Fatal("Live Magpie has no sessions")
	}
	if _, err := client.Usage(ctx, ""); err != nil {
		t.Fatalf("Client.Usage() failed: %v", err)
	}
	if _, err := client.UsageRequests(ctx, UsageRequestsQuery{}); err != nil {
		t.Fatalf("Client.UsageRequests() failed: %v", err)
	}
	if _, err := client.Quotas(ctx, false); err != nil {
		t.Fatalf("Client.Quotas() failed: %v", err)
	}
	if _, err := client.SessionStats(ctx, 0); err != nil {
		t.Fatalf("Client.SessionStats() failed: %v", err)
	}
	if _, err := client.SessionOverview(ctx, SessionOverviewQuery{}); err != nil {
		t.Fatalf("Client.SessionOverview() failed: %v", err)
	}
	if _, err := client.SessionProgress(ctx); err != nil {
		t.Fatalf("Client.SessionProgress() failed: %v", err)
	}

	if err := r.Close(); err != nil {
		t.Fatalf("Runtime.Close() failed: %v", err)
	}
	if r.Status() != RuntimeStatusUnavailable {
		t.Fatalf("Runtime status after Close() = %q, want unavailable", r.Status())
	}
}

func assertRuntimeUnavailable(t *testing.T, r *Runtime) {
	t.Helper()
	if r.Status() != RuntimeStatusUnavailable {
		t.Fatalf("Status() = %q, want unavailable", r.Status())
	}
	if client, ok := r.Client(); ok || client != nil {
		t.Fatalf("Client() = (%v, %t), want (nil, false)", client, ok)
	}
}

func installRuntimeHelper(t *testing.T) {
	t.Helper()
	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	contents, err := os.ReadFile(executable)
	if err != nil {
		t.Fatal(err)
	}
	directory := t.TempDir()
	names := []string{"magpie"}
	if runtime.GOOS == "windows" {
		names = []string{"magpie-windows-amd64.exe", "magpie.exe"}
	}
	for _, name := range names {
		if err := os.WriteFile(filepath.Join(directory, name), contents, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	t.Setenv("PATH", directory+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv(runtimeHelperProcessEnv, "1")
	t.Setenv(runtimeHelperHealthFail, "")
	t.Setenv(runtimeHelperReadyFail, "")
	t.Setenv(runtimeHelperStarted, "")
	t.Setenv(runtimeHelperHoldMarker, "")
	t.Setenv("MAGPIE_WEB_KEY", "")
	t.Setenv("MAGPIE_ADDR", "unchanged-test-address")
	t.Setenv(runtimeHelperExpected, "unchanged-test-address")
}

func waitForRuntimeStatus(t *testing.T, r *Runtime, want RuntimeStatus, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if r.Status() == want {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("Status() = %q, want %q", r.Status(), want)
}

func runRuntimeHelperProcess(args []string) int {
	if expected := os.Getenv(runtimeHelperExpected); expected != "" && os.Getenv("MAGPIE_ADDR") != expected {
		return 20
	}
	if len(args) == 0 {
		return 21
	}
	switch args[0] {
	case "healthcheck":
		if len(args) != 1 {
			return 22
		}
		if os.Getenv(runtimeHelperHealthFail) == "1" {
			return 23
		}
		return 0
	case "hold-stdio":
		if len(args) != 2 {
			return 32
		}
		if err := os.WriteFile(args[1], []byte("holding"), 0o600); err != nil {
			return 33
		}
		time.Sleep(2 * time.Second)
		return 0
	case "web":
		return runRuntimeHelperWeb(args)
	default:
		return 24
	}
}

func runRuntimeHelperWeb(args []string) int {
	if marker := os.Getenv(runtimeHelperStarted); marker != "" {
		if err := os.WriteFile(marker, []byte("started"), 0o600); err != nil {
			return 25
		}
	}
	if len(args) != 4 || args[1] != "--no-open" || args[2] != "--addr" {
		return 26
	}
	host, port, err := net.SplitHostPort(args[3])
	if err != nil || host != "127.0.0.1" || port == "0" {
		return 27
	}
	key := os.Getenv("MAGPIE_WEB_KEY")
	if len(key) != 32 || strings.ToLower(key) != key {
		return 28
	}
	if _, err := hex.DecodeString(key); err != nil {
		return 29
	}
	listener, err := net.Listen("tcp", args[3])
	if err != nil {
		return 30
	}
	if marker := os.Getenv(runtimeHelperHoldMarker); marker != "" {
		hold := exec.Command(os.Args[0], "hold-stdio", marker)
		hold.Stdout = os.Stdout
		hold.Stderr = os.Stderr
		if err := hold.Start(); err != nil {
			return 34
		}
		deadline := time.Now().Add(time.Second)
		for time.Now().Before(deadline) {
			if _, err := os.Stat(marker); err == nil {
				break
			}
			time.Sleep(time.Millisecond)
		}
		if _, err := os.Stat(marker); err != nil {
			return 35
		}
	}
	mux := http.NewServeMux()
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet {
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		if r.URL.Query().Has("k") {
			if r.URL.Query().Get("k") != key || os.Getenv(runtimeHelperReadyFail) == "auth" {
				http.Error(w, "unauthorized", http.StatusUnauthorized)
				return
			}
			http.SetCookie(w, &http.Cookie{Name: "magpie-web", Value: key, Path: "/", HttpOnly: true})
			http.Redirect(w, r, "/", http.StatusSeeOther)
			return
		}
		if cookie, err := r.Cookie("magpie-web"); err != nil || cookie.Value != key {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		w.WriteHeader(http.StatusOK)
	})
	mux.HandleFunc("/api/sessions/progress", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet || r.URL.Query().Has("k") {
			http.Error(w, "bad request", http.StatusBadRequest)
			return
		}
		if cookie, err := r.Cookie("magpie-web"); err != nil || cookie.Value != key {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		if os.Getenv(runtimeHelperReadyFail) == "progress" {
			http.Error(w, "not ready", http.StatusServiceUnavailable)
			return
		}
		_, _ = w.Write([]byte(`{"indexing":true,"files":8,"done":3,"bytes":1024,"read":512}`))
	})
	if err := http.Serve(listener, mux); err != nil {
		return 31
	}
	return 0
}
