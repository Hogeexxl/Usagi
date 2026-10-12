package api

import (
	"context"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"sync"
	"testing"
	"time"
)

type fakeShutdownSignal struct {
	calls int
}

func (s *fakeShutdownSignal) signal() {
	s.calls++
}

func TestConcurrentServiceStopRequestsSignalShutdownOnce(t *testing.T) {
	shutdown := &fakeShutdownSignal{}
	server, _, _, _ := newServerFixtureWithShutdown(t, shutdown.signal)
	const requestCount = 16
	statuses := make(chan int, requestCount)
	var requests sync.WaitGroup
	requests.Add(requestCount)
	for range requestCount {
		go func() {
			defer requests.Done()
			response := request(server, http.MethodPost, "/api/service/stop")
			statuses <- response.Code
		}()
	}
	requests.Wait()
	close(statuses)
	for status := range statuses {
		if status != http.StatusOK {
			t.Errorf("POST /api/service/stop status = %d, want 200", status)
		}
	}
	if shutdown.calls != 1 {
		t.Fatalf("shutdown calls = %d, want 1", shutdown.calls)
	}
}

func TestServiceEndpointsAndStopShutdown(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("net.Listen() error = %v", err)
	}
	shutdown := &fakeShutdownSignal{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	server, _, _, _ := newServerFixtureWithShutdown(t, func() {
		shutdown.signal()
		cancel()
	})
	serveResult := make(chan error, 1)
	go func() {
		serveResult <- server.Serve(ctx, listener)
	}()

	client := &http.Client{Timeout: time.Second}
	response, err := client.Get("http://" + listener.Addr().String() + "/api/service")
	if err != nil {
		t.Fatalf("GET /api/service error = %v", err)
	}
	if got := decodeServiceState(t, response); got != "running" {
		t.Fatalf("GET /api/service state = %q, want running", got)
	}

	stopRequest, err := http.NewRequest(http.MethodPost, "http://"+listener.Addr().String()+"/api/service/stop", nil)
	if err != nil {
		t.Fatalf("http.NewRequest() error = %v", err)
	}
	response, err = client.Do(stopRequest)
	if err != nil {
		t.Fatalf("POST /api/service/stop error = %v", err)
	}
	if got := decodeServiceState(t, response); got != "stopped" {
		t.Fatalf("POST /api/service/stop state = %q, want stopped", got)
	}
	if shutdown.calls != 1 {
		t.Fatalf("shutdown calls = %d, want 1", shutdown.calls)
	}

	select {
	case err := <-serveResult:
		if err != nil {
			t.Fatalf("Serve() error = %v, want nil", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Serve() did not return after service stop")
	}

	repeatedResponse := request(server, http.MethodPost, "/api/service/stop")
	if repeatedResponse.Code != http.StatusOK || shutdown.calls != 1 {
		t.Fatalf("repeated stop status/calls = %d/%d, want 200/1", repeatedResponse.Code, shutdown.calls)
	}
}

func decodeServiceState(t *testing.T, response *http.Response) string {
	t.Helper()
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(response.Body)
		t.Fatalf("service status = %d, want 200: %s", response.StatusCode, body)
	}
	var got serviceStateDTO
	if err := json.NewDecoder(response.Body).Decode(&got); err != nil {
		t.Fatalf("decode service response: %v", err)
	}
	return got.State
}
