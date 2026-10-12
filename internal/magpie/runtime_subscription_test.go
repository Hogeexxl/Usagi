package magpie

import (
	"reflect"
	"testing"
	"time"
)

func TestRuntimeSubscriptionDeliversInitialAndEveryTransitionInOrder(t *testing.T) {
	runtime := NewRuntime()
	statuses, cancel := runtime.Subscribe()
	defer cancel()

	runtime.mu.Lock()
	runtime.setStatusLocked(RuntimeStatusStarting)
	runtime.setStatusLocked(RuntimeStatusReady)
	runtime.setStatusLocked(RuntimeStatusUnavailable)
	runtime.setStatusLocked(RuntimeStatusUnavailable)
	runtime.mu.Unlock()

	got := make([]RuntimeStatus, 0, 4)
	for range 4 {
		select {
		case status := <-statuses:
			got = append(got, status)
		case <-time.After(time.Second):
			t.Fatalf("subscription timed out after %v", got)
		}
	}
	want := []RuntimeStatus{RuntimeStatusUnavailable, RuntimeStatusStarting, RuntimeStatusReady, RuntimeStatusUnavailable}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("subscription statuses = %v, want %v", got, want)
	}
}
