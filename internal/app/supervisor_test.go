package app

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type fakeRuntimeFactory func() *fakeRuntime

type fakeRuntime struct {
	mu             sync.Mutex
	status         magpie.RuntimeStatus
	client         *magpie.Client
	subscribers    map[uint64]chan magpie.RuntimeStatus
	nextSubscriber uint64
	start          func(context.Context) error
	close          func() error
	startErr       error
	closeErr       error
	startCalls     int
	closeCalls     int
}

func newFakeRuntime(start func(context.Context) error, client *magpie.Client) *fakeRuntime {
	return &fakeRuntime{
		status:      magpie.RuntimeStatusUnavailable,
		client:      client,
		subscribers: make(map[uint64]chan magpie.RuntimeStatus),
		start:       start,
	}
}

func (r *fakeRuntime) Status() magpie.RuntimeStatus {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.status
}

func (r *fakeRuntime) Client() (*magpie.Client, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.status != magpie.RuntimeStatusReady || r.client == nil {
		return nil, false
	}
	return r.client, true
}

func (r *fakeRuntime) Subscribe() (<-chan magpie.RuntimeStatus, func()) {
	r.mu.Lock()
	r.nextSubscriber++
	id := r.nextSubscriber
	statuses := make(chan magpie.RuntimeStatus, 16)
	statuses <- r.status
	r.subscribers[id] = statuses
	r.mu.Unlock()

	var once sync.Once
	return statuses, func() {
		once.Do(func() {
			r.mu.Lock()
			delete(r.subscribers, id)
			close(statuses)
			r.mu.Unlock()
		})
	}
}

func (r *fakeRuntime) Start(ctx context.Context) error {
	r.mu.Lock()
	r.startCalls++
	startCalls := r.startCalls
	start := r.start
	startErr := r.startErr
	r.mu.Unlock()
	if startCalls > 1 {
		return errors.New("fake runtime started more than once")
	}
	if start != nil {
		startErr = start(ctx)
	}
	if startErr == nil {
		r.setStatus(magpie.RuntimeStatusReady)
	} else {
		r.setStatus(magpie.RuntimeStatusUnavailable)
	}
	return startErr
}

func (r *fakeRuntime) Close() error {
	r.mu.Lock()
	r.closeCalls++
	closeFn := r.close
	closeErr := r.closeErr
	r.mu.Unlock()
	if closeFn != nil {
		closeErr = closeFn()
	}
	r.setStatus(magpie.RuntimeStatusUnavailable)
	return closeErr
}

func (r *fakeRuntime) setStatus(status magpie.RuntimeStatus) {
	r.mu.Lock()
	r.status = status
	for _, subscriber := range r.subscribers {
		subscriber <- status
	}
	r.mu.Unlock()
}

func (r *fakeRuntime) emit(status magpie.RuntimeStatus) {
	r.setStatus(status)
}

func (r *fakeRuntime) closeCount() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.closeCalls
}

func (r *fakeRuntime) startCount() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.startCalls
}

func TestMagpieSupervisorSubscribeImmediatelySendsStarting(t *testing.T) {
	supervisor := NewMagpieSupervisor(func() MagpieRuntime {
		t.Fatal("factory called before Run")
		return nil
	}, time.Hour)
	statuses, cancel := supervisor.Subscribe()
	if got := readSupervisorStatus(t, statuses); got != magpie.RuntimeStatusStarting {
		t.Fatalf("initial subscription status = %q, want starting", got)
	}
	cancel()
	if err := supervisor.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestMagpieSupervisorSubscribeImmediatelySendsUnavailable(t *testing.T) {
	failed := newFakeRuntime(nil, nil)
	failed.startErr = errors.New("start failed")
	supervisor := NewMagpieSupervisor(func() MagpieRuntime { return failed }, time.Hour)
	go supervisor.Run(context.Background())
	waitForSupervisor(t, func() bool { return supervisor.Status() == magpie.RuntimeStatusUnavailable })

	statuses, cancel := supervisor.Subscribe()
	if got := readSupervisorStatus(t, statuses); got != magpie.RuntimeStatusUnavailable {
		t.Fatalf("initial subscription status = %q, want unavailable", got)
	}
	cancel()
	if err := supervisor.Close(); err != nil {
		t.Fatal(err)
	}
	if got := failed.closeCount(); got != 1 {
		t.Fatalf("failed runtime close calls = %d, want 1", got)
	}
}

func TestMagpieSupervisorSubscribeImmediatelySendsReady(t *testing.T) {
	runtime := newFakeRuntime(nil, magpie.NewClient("http://127.0.0.1:40000", nil))
	var factoryCalls atomic.Int32
	supervisor := NewMagpieSupervisor(func() MagpieRuntime {
		factoryCalls.Add(1)
		return runtime
	}, time.Millisecond)
	go supervisor.Run(context.Background())
	waitForSupervisor(t, func() bool { return supervisor.Status() == magpie.RuntimeStatusReady })

	statuses, cancel := supervisor.Subscribe()
	if got := readSupervisorStatus(t, statuses); got != magpie.RuntimeStatusReady {
		t.Fatalf("initial subscription status = %q, want ready", got)
	}
	if client, ready := supervisor.Client(); !ready || client != runtime.client {
		t.Fatalf("Client() = (%p, %t), want current ready client", client, ready)
	}
	time.Sleep(20 * time.Millisecond)
	if got := factoryCalls.Load(); got != 1 {
		t.Fatalf("factory calls after initial ready subscription = %d, want 1", got)
	}
	cancel()
	if err := supervisor.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestMagpieSupervisorRetriesWithNewRuntimeAndDeduplicatesUnavailable(t *testing.T) {
	first := newFakeRuntime(nil, nil)
	first.startErr = errors.New("first start failed")
	second := newFakeRuntime(nil, nil)
	second.startErr = errors.New("second start failed")
	third := newFakeRuntime(nil, magpie.NewClient("http://127.0.0.1:40003", nil))
	runtimes := []*fakeRuntime{first, second, third}
	var factoryCalls atomic.Int32
	factory := fakeRuntimeFactory(func() *fakeRuntime {
		index := int(factoryCalls.Add(1)) - 1
		if index > 0 && runtimes[index-1].closeCount() != 1 {
			t.Errorf("runtime %d was replaced before Close completed", index-1)
		}
		if index >= len(runtimes) {
			t.Fatalf("unexpected factory call %d", index+1)
		}
		return runtimes[index]
	})
	supervisor := NewMagpieSupervisor(func() MagpieRuntime { return factory() }, 5*time.Millisecond)
	statuses, cancel := supervisor.Subscribe()
	defer cancel()
	if got := readSupervisorStatus(t, statuses); got != magpie.RuntimeStatusStarting {
		t.Fatalf("initial status = %q, want starting", got)
	}
	go supervisor.Run(context.Background())

	got := []magpie.RuntimeStatus{magpie.RuntimeStatusStarting}
	deadline := time.After(2 * time.Second)
	for got[len(got)-1] != magpie.RuntimeStatusReady {
		select {
		case status := <-statuses:
			got = append(got, status)
		case <-deadline:
			t.Fatalf("supervisor statuses = %v, want unavailable then ready", got)
		}
	}
	want := []magpie.RuntimeStatus{magpie.RuntimeStatusStarting, magpie.RuntimeStatusUnavailable, magpie.RuntimeStatusReady}
	if len(got) != len(want) {
		t.Fatalf("supervisor statuses = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("supervisor statuses = %v, want %v", got, want)
		}
	}
	if factoryCalls.Load() != 3 {
		t.Fatalf("factory calls = %d, want 3", factoryCalls.Load())
	}
	if client, ready := supervisor.Client(); !ready || client != third.client {
		t.Fatalf("Client() = (%p, %t), want third runtime client", client, ready)
	}
	time.Sleep(20 * time.Millisecond)
	select {
	case status := <-statuses:
		t.Fatalf("received duplicate status %q while unavailable retries were hidden", status)
	default:
	}

	if err := supervisor.Close(); err != nil {
		t.Fatal(err)
	}
	for i, runtime := range runtimes {
		if got := runtime.startCount(); got != 1 {
			t.Errorf("runtime %d Start calls = %d, want 1", i, got)
		}
		if got := runtime.closeCount(); got != 1 {
			t.Errorf("runtime %d Close calls = %d, want 1", i, got)
		}
	}
}

func TestMagpieSupervisorRecoversWhenOwnedSidecarBecomesUnavailable(t *testing.T) {
	first := newFakeRuntime(nil, magpie.NewClient("http://127.0.0.1:40001", nil))
	second := newFakeRuntime(nil, magpie.NewClient("http://127.0.0.1:40002", nil))
	var factoryCalls atomic.Int32
	factory := fakeRuntimeFactory(func() *fakeRuntime {
		index := int(factoryCalls.Add(1)) - 1
		if index > 0 && first.closeCount() != 1 {
			t.Errorf("first Runtime Close calls before replacement = %d, want 1", first.closeCount())
		}
		if index == 0 {
			return first
		}
		if index == 1 {
			return second
		}
		t.Fatalf("unexpected factory call %d", index+1)
		return nil
	})
	supervisor := NewMagpieSupervisor(func() MagpieRuntime { return factory() }, 5*time.Millisecond)
	statuses, cancel := supervisor.Subscribe()
	defer cancel()
	go supervisor.Run(context.Background())
	if got := readSupervisorStatus(t, statuses); got != magpie.RuntimeStatusStarting {
		t.Fatalf("initial status = %q, want starting", got)
	}
	if got := readSupervisorStatus(t, statuses); got != magpie.RuntimeStatusReady {
		t.Fatalf("started status = %q, want ready", got)
	}
	first.emit(magpie.RuntimeStatusUnavailable)
	if got := readSupervisorStatus(t, statuses); got != magpie.RuntimeStatusUnavailable {
		t.Fatalf("sidecar exit status = %q, want unavailable", got)
	}
	if got := readSupervisorStatus(t, statuses); got != magpie.RuntimeStatusReady {
		t.Fatalf("recovery status = %q, want ready", got)
	}
	if factoryCalls.Load() != 2 {
		t.Fatalf("factory calls = %d, want 2", factoryCalls.Load())
	}
	if client, ready := supervisor.Client(); !ready || client != second.client {
		t.Fatalf("Client() = (%p, %t), want replacement client", client, ready)
	}
	if got := first.closeCount(); got != 1 {
		t.Fatalf("old Runtime Close calls = %d, want 1", got)
	}
	if err := supervisor.Close(); err != nil {
		t.Fatal(err)
	}
	if got := second.closeCount(); got != 1 {
		t.Fatalf("current Runtime Close calls = %d, want 1", got)
	}
}

func TestMagpieSupervisorCloseCancelsAndClosesRuntimeStarting(t *testing.T) {
	started := make(chan struct{})
	runtime := newFakeRuntime(func(ctx context.Context) error {
		close(started)
		<-ctx.Done()
		return ctx.Err()
	}, nil)
	var factoryCalls atomic.Int32
	supervisor := NewMagpieSupervisor(func() MagpieRuntime {
		factoryCalls.Add(1)
		return runtime
	}, time.Hour)
	runDone := make(chan struct{})
	go func() {
		supervisor.Run(context.Background())
		close(runDone)
	}()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("Runtime.Start did not begin")
	}
	if err := supervisor.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case <-runDone:
	case <-time.After(time.Second):
		t.Fatal("Run did not finish after Close")
	}
	if got := runtime.closeCount(); got != 1 {
		t.Fatalf("in-flight Runtime Close calls = %d, want 1", got)
	}
	if got := factoryCalls.Load(); got != 1 {
		t.Fatalf("factory calls after Close during Start = %d, want 1", got)
	}
}

func readSupervisorStatus(t *testing.T, statuses <-chan magpie.RuntimeStatus) magpie.RuntimeStatus {
	t.Helper()
	select {
	case status := <-statuses:
		return status
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for supervisor status")
		return ""
	}
}

func waitForSupervisor(t *testing.T, ready func() bool) {
	t.Helper()
	deadline := time.Now().Add(time.Second)
	for time.Now().Before(deadline) {
		if ready() {
			return
		}
		time.Sleep(time.Millisecond)
	}
	t.Fatal("supervisor did not reach expected state")
}
