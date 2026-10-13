package api

import (
	"net/http"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

func TestSyncInitialGateKeepsLibraryLoadingUntilFirstPassCompletes(t *testing.T) {
	server, projectionService, runtime, queryService := newServerFixture(t)
	runtime.SetStatus(magpie.RuntimeStatusReady)
	queryService.InitialSyncPending = true
	loading := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if loading.Code != http.StatusServiceUnavailable || !strings.Contains(loading.Body.String(), ErrorDataLoading) {
		t.Fatalf("summary before initial sync = %d %s, want 503 %s", loading.Code, loading.Body.String(), ErrorDataLoading)
	}
	if len(projectionService.SummaryQueries) != 0 {
		t.Fatalf("loading response queried projection %d times", len(projectionService.SummaryQueries))
	}

	queryService.InitialSyncPending = false
	ready := request(server, http.MethodGet, "/api/usage/summary?range=today")
	if ready.Code != http.StatusOK {
		t.Fatalf("summary after initial sync = %d %s, want 200", ready.Code, ready.Body.String())
	}
	if len(projectionService.SummaryQueries) != 1 {
		t.Fatalf("completed initial sync queried projection %d times, want 1", len(projectionService.SummaryQueries))
	}
}
