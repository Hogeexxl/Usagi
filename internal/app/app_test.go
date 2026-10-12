package app

import (
	"context"
	"net"
	"testing"
	"testing/fstest"
)

func TestRunRequiresRuntimeDependenciesBeforeBinding(t *testing.T) {
	called := false
	err := run(context.Background(), Dependencies{
		RuntimeFactory: func() MagpieRuntime {
			called = true
			return nil
		},
		StaticFS: fstest.MapFS{"index.html": &fstest.MapFile{Data: []byte("dashboard")}},
	}, PortRange{First: 0, Count: 1})
	if err == nil {
		t.Fatal("run() error = nil, want missing browser dependency")
	}
	if called {
		t.Fatal("runtime factory called before dependencies were validated")
	}
}

func TestListenerURLUsesBoundAddress(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()

	baseURL, port, err := listenerURL(listener)
	if err != nil {
		t.Fatal(err)
	}
	if want := "http://" + listener.Addr().String(); baseURL != want {
		t.Fatalf("listener URL = %q, want %q", baseURL, want)
	}
	if int(port) != listener.Addr().(*net.TCPAddr).Port {
		t.Fatalf("listener port = %d, want %d", port, listener.Addr().(*net.TCPAddr).Port)
	}
}
