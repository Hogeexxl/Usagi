package api

import (
	"sync"
	"testing"
)

func TestRevisionServiceSeedsAndPublishesDatabaseRevision(t *testing.T) {
	service := NewRevisionService(7)
	if got := service.Current(); got != (RevisionTuple{DataRevision: 7}) {
		t.Fatalf("initial Current() = %+v, want data revision 7", got)
	}
	updates, cancel := service.Subscribe()
	defer cancel()
	if got := service.SetDataRevision(7); got != (RevisionTuple{DataRevision: 7}) {
		t.Fatalf("SetDataRevision(same) = %+v", got)
	}
	select {
	case got := <-updates:
		t.Fatalf("same revision unexpectedly published %+v", got)
	default:
	}
	if got := service.SetDataRevision(8); got != (RevisionTuple{DataRevision: 8}) {
		t.Fatalf("SetDataRevision(changed) = %+v", got)
	}
	if got := service.BumpStatus(); got != (RevisionTuple{DataRevision: 8, StatusRevision: 1}) {
		t.Fatalf("BumpStatus() = %+v", got)
	}
	if got := <-updates; got != (RevisionTuple{DataRevision: 8, StatusRevision: 1}) {
		t.Fatalf("coalesced update = %+v", got)
	}
}

func TestRevisionServiceConcurrentDataRevisionUpdatesAreAtomic(t *testing.T) {
	service := NewRevisionService(0)
	const updateCount = 64
	var wg sync.WaitGroup
	for revision := 1; revision <= updateCount; revision++ {
		wg.Add(1)
		go func(revision int) {
			defer wg.Done()
			service.SetDataRevision(int64(revision))
		}(revision)
	}
	wg.Wait()
	if got := service.Current(); got.DataRevision < 1 || got.DataRevision > updateCount || got.StatusRevision != 0 {
		t.Fatalf("Current() after concurrent updates = %+v, want a published database revision and status=0", got)
	}
}
