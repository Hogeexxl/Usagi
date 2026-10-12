package api

import (
	"sync"
	"testing"
)

func TestRevisionServiceBumpsAndPublishesCurrentTuple(t *testing.T) {
	service := NewRevisionService()
	if got := service.Current(); got != (RevisionTuple{}) {
		t.Fatalf("initial Current() = %+v, want zero tuple", got)
	}
	updates, cancel := service.Subscribe()
	defer cancel()

	if got := service.BumpData(); got != (RevisionTuple{DataRevision: 1}) {
		t.Fatalf("BumpData() = %+v", got)
	}
	if got := service.BumpStatus(); got != (RevisionTuple{DataRevision: 1, StatusRevision: 1}) {
		t.Fatalf("BumpStatus() = %+v", got)
	}
	if got := service.Current(); got != (RevisionTuple{DataRevision: 1, StatusRevision: 1}) {
		t.Fatalf("Current() = %+v", got)
	}
	if got := <-updates; got != (RevisionTuple{DataRevision: 1, StatusRevision: 1}) {
		t.Fatalf("coalesced update = %+v", got)
	}
}

func TestRevisionServiceConcurrentBumpsAreAtomic(t *testing.T) {
	service := NewRevisionService()
	const bumpCount = 64
	var wg sync.WaitGroup
	for range bumpCount {
		wg.Add(1)
		go func() {
			defer wg.Done()
			service.BumpData()
		}()
	}
	wg.Wait()
	if got := service.Current(); got.DataRevision != bumpCount || got.StatusRevision != 0 {
		t.Fatalf("Current() after concurrent bumps = %+v, want data=%d status=0", got, bumpCount)
	}
}
