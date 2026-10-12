package api

import (
	"context"
	"errors"
	"net"
	"net/http"
	"testing"
	"time"
)

type fakeListener struct {
	addr       net.Addr
	acceptErr  error
	closeErr   error
	closeCalls int
}

func (l *fakeListener) Accept() (net.Conn, error) {
	return nil, l.acceptErr
}

func (l *fakeListener) Close() error {
	l.closeCalls++
	return l.closeErr
}

func (l *fakeListener) Addr() net.Addr {
	return l.addr
}

func TestServeUsesBoundListenerAndStopsOnContext(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("net.Listen() error = %v", err)
	}
	server, _, _, _ := newServerFixture(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	serveResult := make(chan error, 1)
	go func() {
		serveResult <- server.Serve(ctx, listener)
	}()

	response, err := http.Get("http://" + listener.Addr().String() + "/api/health")
	if err != nil {
		cancel()
		t.Fatalf("GET /api/health through prebound listener: %v", err)
	}
	response.Body.Close()
	if response.StatusCode != http.StatusNoContent {
		cancel()
		t.Fatalf("GET /api/health status = %d, want 204", response.StatusCode)
	}
	cancel()

	select {
	case err := <-serveResult:
		if err != nil {
			t.Fatalf("Serve() error = %v, want nil", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Serve() did not return after context cancellation")
	}
}

func TestServeReturnsListenerError(t *testing.T) {
	want := errors.New("listener accept failed")
	listener := &fakeListener{
		addr:      &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 37991},
		acceptErr: want,
	}
	server, _, _, _ := newServerFixture(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := server.Serve(ctx, listener); err != want {
		t.Fatalf("Serve() error = %v, want original %v", err, want)
	}
	if listener.closeCalls != 1 {
		t.Fatalf("listener Close() calls = %d, want 1", listener.closeCalls)
	}
}
