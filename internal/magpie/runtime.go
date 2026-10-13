package magpie

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/cookiejar"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"strconv"
	"sync"
	"time"
)

// defaultGatewayURL is where the Magpie desktop app's gateway listens unless
// its Settings move it; the sidecar's startup banner names the real one.
const defaultGatewayURL = "http://127.0.0.1:3425"

var gatewayBannerPattern = regexp.MustCompile(`gateway (https?://[^\s·]+)`)

// bannerScanner discards the sidecar's stdout but remembers the gateway
// address its startup banner prints.
type bannerScanner struct {
	mu      sync.Mutex
	pending []byte
	gateway string
}

func (b *bannerScanner) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.gateway != "" {
		return len(p), nil
	}
	b.pending = append(b.pending, p...)
	for {
		index := bytes.IndexByte(b.pending, '\n')
		if index < 0 {
			break
		}
		if match := gatewayBannerPattern.FindSubmatch(b.pending[:index]); match != nil {
			b.gateway = string(match[1])
			b.pending = nil
			return len(p), nil
		}
		b.pending = b.pending[index+1:]
	}
	if len(b.pending) > 4096 {
		b.pending = nil
	}
	return len(p), nil
}

func (b *bannerScanner) GatewayURL() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.gateway == "" {
		return defaultGatewayURL
	}
	return b.gateway
}

type RuntimeStatus string

const (
	RuntimeStatusStarting    RuntimeStatus = "starting"
	RuntimeStatusReady       RuntimeStatus = "ready"
	RuntimeStatusUnavailable RuntimeStatus = "unavailable"
)

type Runtime struct {
	mu          sync.RWMutex
	status      RuntimeStatus
	client      *Client
	cmd         *exec.Cmd
	done        chan error
	closing     bool
	started     bool
	baseURL     string
	webKey      string
	nextSubID   uint64
	subscribers map[uint64]*runtimeStatusSubscriber
}

type runtimeStatusSubscriber struct {
	mu     sync.Mutex
	queue  []RuntimeStatus
	head   int
	closed bool
	wake   chan struct{}
	done   chan struct{}
	out    chan RuntimeStatus
}

func newRuntimeStatusSubscriber() *runtimeStatusSubscriber {
	s := &runtimeStatusSubscriber{
		wake: make(chan struct{}, 1),
		done: make(chan struct{}),
		out:  make(chan RuntimeStatus),
	}
	go s.run()
	return s
}

func (s *runtimeStatusSubscriber) publish(status RuntimeStatus) {
	s.mu.Lock()
	if !s.closed {
		s.queue = append(s.queue, status)
	}
	s.mu.Unlock()
	select {
	case s.wake <- struct{}{}:
	default:
	}
}

func (s *runtimeStatusSubscriber) close() {
	s.mu.Lock()
	if !s.closed {
		s.closed = true
		close(s.done)
	}
	s.mu.Unlock()
	select {
	case s.wake <- struct{}{}:
	default:
	}
}

func (s *runtimeStatusSubscriber) run() {
	defer close(s.out)
	for {
		s.mu.Lock()
		if s.head < len(s.queue) {
			status := s.queue[s.head]
			s.head++
			if s.head == len(s.queue) {
				s.queue = nil
				s.head = 0
			}
			s.mu.Unlock()
			select {
			case s.out <- status:
			case <-s.done:
				return
			}
			continue
		}
		closed := s.closed
		s.mu.Unlock()
		if closed {
			return
		}
		select {
		case <-s.wake:
		case <-s.done:
			return
		}
	}
}

func NewRuntime() *Runtime {
	return &Runtime{
		status:      RuntimeStatusUnavailable,
		done:        make(chan error, 1),
		subscribers: make(map[uint64]*runtimeStatusSubscriber),
	}
}

func (r *Runtime) Start(ctx context.Context) error {
	r.mu.Lock()
	if r.started {
		r.mu.Unlock()
		return errors.New("magpie runtime can only be started once")
	}
	r.started = true
	r.closing = false
	r.client = nil
	r.setStatusLocked(RuntimeStatusStarting)
	r.mu.Unlock()

	binaryPath, err := locateMagpieBinary()
	if err != nil {
		return r.startFailed(err)
	}

	healthcheck := exec.CommandContext(ctx, binaryPath, "healthcheck")
	healthcheck.Env = os.Environ()
	healthcheck.Stdout = io.Discard
	healthcheck.Stderr = io.Discard
	if err := healthcheck.Run(); err != nil {
		return r.startFailed(fmt.Errorf("magpie healthcheck failed: %w", err))
	}

	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return r.startFailed(err)
	}
	port := listener.Addr().(*net.TCPAddr).Port
	if err := listener.Close(); err != nil {
		return r.startFailed(err)
	}
	baseAddr := net.JoinHostPort("127.0.0.1", strconv.Itoa(port))
	baseURL := "http://" + baseAddr

	keyBytes := make([]byte, 16)
	if _, err := rand.Read(keyBytes); err != nil {
		return r.startFailed(err)
	}
	key := hex.EncodeToString(keyBytes)

	r.mu.Lock()
	r.baseURL = baseURL
	r.webKey = key
	r.mu.Unlock()

	cmd := exec.Command(
		binaryPath,
		"web",
		"--no-open",
		"--addr",
		baseAddr,
	)
	cmd.Env = append(
		os.Environ(),
		"MAGPIE_WEB_KEY="+key,
	)
	banner := &bannerScanner{}
	cmd.Stdout = banner
	cmd.Stderr = io.Discard

	if err := cmd.Start(); err != nil {
		r.mu.Lock()
		r.client = nil
		r.setStatusLocked(RuntimeStatusUnavailable)
		r.mu.Unlock()
		return err
	}
	readinessDeadline := time.Now().Add(5 * time.Second)

	r.mu.Lock()
	r.cmd = cmd
	r.mu.Unlock()

	go func() {
		waitErr := cmd.Wait()

		r.mu.Lock()
		if !r.closing {
			r.client = nil
			r.setStatusLocked(RuntimeStatusUnavailable)
		}
		r.mu.Unlock()

		r.done <- waitErr
	}()

	startupCtx, cancel := context.WithDeadline(ctx, readinessDeadline)
	defer cancel()

	jar, err := cookiejar.New(nil)
	if err != nil {
		return r.startFailedAfterSidecar(err)
	}
	httpClient := &http.Client{Jar: jar}
	client := NewClient(baseURL, httpClient).WithGateway(banner)
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()

	authenticated := false
	var lastErr error
	for {
		if r.Status() != RuntimeStatusStarting {
			return r.startFailedAfterSidecar(errors.New("magpie web sidecar exited during startup"))
		}
		if err := startupCtx.Err(); err != nil {
			return r.startFailedAfterSidecar(r.startupReadinessError(ctx, err, lastErr))
		}

		if !authenticated {
			lastErr = authenticateWeb(startupCtx, httpClient, baseURL, key)
			if lastErr == nil {
				authenticated = true
			}
		} else {
			_, lastErr = client.SessionProgress(startupCtx)
			if lastErr == nil {
				if err := startupCtx.Err(); err != nil {
					return r.startFailedAfterSidecar(r.startupReadinessError(ctx, err, nil))
				}
				r.mu.Lock()
				if r.status != RuntimeStatusStarting || r.closing {
					r.mu.Unlock()
					return r.startFailedAfterSidecar(errors.New("magpie web sidecar exited during startup"))
				}
				r.client = client
				r.setStatusLocked(RuntimeStatusReady)
				r.mu.Unlock()
				return nil
			}
		}

		select {
		case <-startupCtx.Done():
			return r.startFailedAfterSidecar(r.startupReadinessError(ctx, startupCtx.Err(), lastErr))
		case <-ticker.C:
		}
	}
}

func (r *Runtime) Status() RuntimeStatus {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return r.status
}

func (r *Runtime) Subscribe() (<-chan RuntimeStatus, func()) {
	r.mu.Lock()
	if r.subscribers == nil {
		r.subscribers = make(map[uint64]*runtimeStatusSubscriber)
	}
	r.nextSubID++
	id := r.nextSubID
	subscriber := newRuntimeStatusSubscriber()
	r.subscribers[id] = subscriber
	subscriber.publish(r.status)
	r.mu.Unlock()

	var once sync.Once
	return subscriber.out, func() {
		once.Do(func() {
			r.mu.Lock()
			delete(r.subscribers, id)
			r.mu.Unlock()
			subscriber.close()
		})
	}
}

func (r *Runtime) Client() (*Client, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.status != RuntimeStatusReady || r.client == nil {
		return nil, false
	}
	return r.client, true
}

func (r *Runtime) Close() error {
	r.mu.Lock()
	r.closing = true
	cmd := r.cmd
	r.client = nil
	r.setStatusLocked(RuntimeStatusUnavailable)
	if cmd == nil {
		r.mu.Unlock()
		return nil
	}
	r.mu.Unlock()

	var killErr error
	select {
	case <-r.done:
	default:
		if err := cmd.Process.Kill(); err != nil && !errors.Is(err, os.ErrProcessDone) {
			killErr = err
		}
		<-r.done
	}

	r.mu.Lock()
	r.cmd = nil
	r.client = nil
	r.status = RuntimeStatusUnavailable
	r.mu.Unlock()
	return killErr
}

func locateMagpieBinary() (string, error) {
	if runtime.GOOS == "windows" {
		for _, name := range []string{"magpie-windows-amd64.exe", "magpie.exe"} {
			if path, err := exec.LookPath(name); err == nil {
				return path, nil
			}
		}
		return "", errors.New("Magpie executable not found; add its executable directory in Magpie Settings → Command line → Add to PATH and retry")
	}

	if path, err := exec.LookPath("magpie"); err == nil {
		return path, nil
	}
	if runtime.GOOS == "darwin" {
		candidates := []string{"/Applications/magpie.app/Contents/MacOS/magpie"}
		if home := os.Getenv("HOME"); home != "" {
			candidates = append(candidates, filepath.Join(home, "Applications", "magpie.app", "Contents", "MacOS", "magpie"))
		}
		for _, candidate := range candidates {
			if path, err := exec.LookPath(candidate); err == nil {
				return path, nil
			}
		}
	}
	return "", errors.New("magpie executable not found in PATH")
}

func (r *Runtime) startFailed(err error) error {
	r.mu.Lock()
	r.client = nil
	r.setStatusLocked(RuntimeStatusUnavailable)
	r.mu.Unlock()
	return err
}

func (r *Runtime) setStatusLocked(status RuntimeStatus) {
	if r.status == status {
		return
	}
	r.status = status
	for _, subscriber := range r.subscribers {
		subscriber.publish(status)
	}
}

func (r *Runtime) startFailedAfterSidecar(err error) error {
	_ = r.Close()
	return err
}

func (r *Runtime) startupReadinessError(ctx context.Context, deadlineErr, lastErr error) error {
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if lastErr != nil {
		return fmt.Errorf("magpie web readiness failed: %w", lastErr)
	}
	return fmt.Errorf("magpie web readiness failed: %w", deadlineErr)
}
