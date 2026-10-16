package app

import (
	"errors"
	"net"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strconv"
	"strings"
	"syscall"
	"testing"
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

type fakeAddr string

func (a fakeAddr) Network() string { return "tcp" }
func (a fakeAddr) String() string  { return string(a) }

func TestBindLoopbackUsesProductionDefaultPortWhenAvailable(t *testing.T) {
	first := &fakeListener{addr: fakeAddr("127.0.0.1:37891")}
	var scanned int
	result, err := bindLoopback(PortRange{First: productionPortFirst, Count: runtimePortCount}, func(_, _ string) (net.Listener, error) {
		scanned++
		if scanned == 1 {
			return first, nil
		}
		return &fakeListener{addr: fakeAddr("127.0.0.1:extra")}, nil
	}, func(uint16) bool { return false })
	if err != nil {
		t.Fatal(err)
	}
	if result.Listener != first || result.Listener.Addr().String() != "127.0.0.1:37891" {
		t.Fatalf("listener = %v, want production default port 37891", result.Listener)
	}
	if scanned != 32 {
		t.Fatalf("ports scanned = %d, want complete 32-port range", scanned)
	}
}

func TestBindLoopbackFallsBackTo37892WhenProductionDefaultIsOccupied(t *testing.T) {
	fallback := &fakeListener{addr: fakeAddr("127.0.0.1:37892")}
	var scanned []uint16
	var extraListeners []*fakeListener
	result, err := bindLoopback(PortRange{First: productionPortFirst, Count: runtimePortCount}, func(_, address string) (net.Listener, error) {
		_, portText, err := net.SplitHostPort(address)
		if err != nil {
			t.Fatal(err)
		}
		port, err := strconv.Atoi(portText)
		if err != nil {
			t.Fatal(err)
		}
		scanned = append(scanned, uint16(port))
		if port == 37891 {
			return nil, syscall.EADDRINUSE
		}
		if port == 37892 {
			return fallback, nil
		}
		listener := &fakeListener{addr: fakeAddr(address)}
		extraListeners = append(extraListeners, listener)
		return listener, nil
	}, func(uint16) bool { return false })
	if err != nil {
		t.Fatal(err)
	}
	if result.Listener != fallback || result.Listener.Addr().String() != "127.0.0.1:37892" {
		t.Fatalf("listener = %v, want fallback port 37892", result.Listener)
	}
	if len(scanned) != 32 || scanned[0] != 37891 || scanned[1] != 37892 {
		t.Fatalf("scanned ports start = %v, want full range beginning 37891, 37892", scanned[:min(2, len(scanned))])
	}
	for _, listener := range extraListeners {
		if listener.closeCalls != 1 {
			t.Errorf("extra listener close calls = %d, want 1", listener.closeCalls)
		}
	}
}

func TestBindLoopbackScansFullRangeAndKeepsFirstListener(t *testing.T) {
	first := &fakeListener{addr: fakeAddr("127.0.0.1:40001")}
	extra := &fakeListener{addr: fakeAddr("127.0.0.1:40003")}
	var scanned []uint16
	result, err := bindLoopback(PortRange{First: 40001, Count: 3}, func(_, address string) (net.Listener, error) {
		_, portText, err := net.SplitHostPort(address)
		if err != nil {
			t.Fatal(err)
		}
		port, _ := strconv.Atoi(portText)
		scanned = append(scanned, uint16(port))
		switch uint16(port) {
		case 40001:
			return first, nil
		case 40002:
			return nil, syscall.EADDRINUSE
		case 40003:
			if first.closeCalls != 0 {
				t.Fatal("first listener was closed before the scan completed")
			}
			return extra, nil
		default:
			t.Fatalf("unexpected port %d", port)
			return nil, nil
		}
	}, func(port uint16) bool {
		if port != 40002 {
			t.Fatalf("probe port = %d, want 40002", port)
		}
		return false
	})
	if err != nil {
		t.Fatal(err)
	}
	if result.Listener != first || result.Existing != nil {
		t.Fatalf("BindLoopback result = %+v, want first listener", result)
	}
	if !reflect.DeepEqual(scanned, []uint16{40001, 40002, 40003}) {
		t.Fatalf("scanned ports = %v", scanned)
	}
	if first.closeCalls != 0 || extra.closeCalls != 1 {
		t.Fatalf("listener close counts = first %d, extra %d", first.closeCalls, extra.closeCalls)
	}
}

func TestBindLoopbackFindsExistingAfterFreeDefault(t *testing.T) {
	first := &fakeListener{addr: fakeAddr("127.0.0.1:40101")}
	var scanned []uint16
	result, err := bindLoopback(PortRange{First: 40101, Count: 3}, func(_, address string) (net.Listener, error) {
		_, portText, _ := net.SplitHostPort(address)
		port, _ := strconv.Atoi(portText)
		scanned = append(scanned, uint16(port))
		if port == 40101 {
			return first, nil
		}
		return nil, syscall.EADDRINUSE
	}, func(port uint16) bool { return port == 40102 })
	if err != nil {
		t.Fatal(err)
	}
	if result.Listener != nil || result.Existing == nil || result.Existing.String() != "http://127.0.0.1:40102" {
		t.Fatalf("BindLoopback result = %+v, want existing port 40102", result)
	}
	if first.closeCalls != 1 {
		t.Fatalf("reserved listener close calls = %d, want 1", first.closeCalls)
	}
	if !reflect.DeepEqual(scanned, []uint16{40101, 40102}) {
		t.Fatalf("scanned ports = %v, want stop after existing instance", scanned)
	}
}

func TestBindLoopbackReportsNoAvailablePort(t *testing.T) {
	var scanned int
	result, err := bindLoopback(PortRange{First: 40201, Count: 32}, func(_, _ string) (net.Listener, error) {
		scanned++
		return nil, syscall.EADDRINUSE
	}, func(uint16) bool { return false })
	if !errors.Is(err, ErrNoAvailablePort) {
		t.Fatalf("BindLoopback error = %v, want ErrNoAvailablePort", err)
	}
	if result.Listener != nil || result.Existing != nil || scanned != 32 {
		t.Fatalf("result = %+v, scanned = %d; want empty result after all 32 ports", result, scanned)
	}
}

func TestBindLoopbackClosesReservedListenerOnOtherBindError(t *testing.T) {
	first := &fakeListener{addr: fakeAddr("127.0.0.1:40301")}
	failure := errors.New("permission denied")
	calls := 0
	_, err := bindLoopback(PortRange{First: 40301, Count: 2}, func(_, _ string) (net.Listener, error) {
		calls++
		if calls == 1 {
			return first, nil
		}
		return nil, failure
	}, func(uint16) bool { return false })
	if !errors.Is(err, failure) {
		t.Fatalf("BindLoopback error = %v, want %v", err, failure)
	}
	if first.closeCalls != 1 {
		t.Fatalf("reserved listener close calls = %d, want 1", first.closeCalls)
	}
}

func TestProbeUsagiHealthRequiresSuccessAndExactMarkerWithoutProxy(t *testing.T) {
	for _, test := range []struct {
		name       string
		status     int
		infoStatus int
		infoMode   string
		markers    []string
		want       bool
	}{
		{name: "exact marker and matching mode", status: http.StatusOK, infoStatus: http.StatusOK, infoMode: runtimeMode(), markers: []string{"Usagi"}, want: true},
		{name: "case mismatch", status: http.StatusOK, infoStatus: http.StatusOK, infoMode: runtimeMode(), markers: []string{"usagi"}},
		{name: "unsuccessful health status", status: http.StatusServiceUnavailable, markers: []string{"Usagi"}},
		{name: "duplicate health marker", status: http.StatusOK, infoStatus: http.StatusOK, infoMode: runtimeMode(), markers: []string{"Usagi", "Usagi"}},
		{name: "different runtime mode", status: http.StatusOK, infoStatus: http.StatusOK, infoMode: "other", markers: []string{"Usagi"}},
		{name: "info unavailable", status: http.StatusOK, infoStatus: http.StatusServiceUnavailable, infoMode: runtimeMode(), markers: []string{"Usagi"}},
	} {
		t.Run(test.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				switch r.URL.Path {
				case "/api/health":
					for _, marker := range test.markers {
						w.Header().Add("X-Usagi-App", marker)
					}
					w.WriteHeader(test.status)
				case "/api/v1/info":
					status := test.infoStatus
					if status == 0 {
						status = http.StatusOK
					}
					w.WriteHeader(status)
					_, _ = w.Write([]byte(`{"runtime_mode":"` + test.infoMode + `"}`))
				default:
					t.Errorf("probe path = %q", r.URL.Path)
					http.NotFound(w, r)
				}
			}))
			defer server.Close()
			_, portText, err := net.SplitHostPort(strings.TrimPrefix(server.URL, "http://"))
			if err != nil {
				t.Fatal(err)
			}
			port, err := strconv.Atoi(portText)
			if err != nil {
				t.Fatal(err)
			}
			t.Setenv("HTTP_PROXY", "http://127.0.0.1:1")
			t.Setenv("http_proxy", "http://127.0.0.1:1")
			t.Setenv("NO_PROXY", "")
			t.Setenv("no_proxy", "")
			if got := probeUsagiHealth(uint16(port)); got != test.want {
				t.Fatalf("probeUsagiHealth() = %t, want %t", got, test.want)
			}
		})
	}
}
