package query

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type testClientProvider struct {
	results []struct {
		client *magpie.Client
		ready  bool
	}
	calls int
}

func (p *testClientProvider) Client() (*magpie.Client, bool) {
	result := p.results[p.calls]
	p.calls++
	return result.client, result.ready
}

func TestRuntimeUpstreamLooksUpCurrentClientAtEachOperation(t *testing.T) {
	provider := &testClientProvider{results: make([]struct {
		client *magpie.Client
		ready  bool
	}, 7)}
	for i := range provider.results {
		provider.results[i].ready = i%2 == 1
	}
	upstream := &runtimeUpstream{provider: provider}
	ctx := context.Background()
	checks := []struct {
		name string
		run  func() error
	}{
		{name: "usage", run: func() error { _, err := upstream.Usage(ctx, "all"); return err }},
		{name: "usage requests", run: func() error { _, err := upstream.UsageRequests(ctx, magpie.UsageRequestsQuery{}); return err }},
		{name: "usage day digests", run: func() error { _, err := upstream.UsageDayDigests(ctx, "2026-10-14", "2026-10-14"); return err }},
		{name: "sessions", run: func() error { _, err := upstream.Sessions(ctx, 0); return err }},
		{name: "quotas", run: func() error { _, err := upstream.Quotas(ctx, false); return err }},
		{name: "session overview", run: func() error { _, err := upstream.SessionOverview(ctx, magpie.SessionOverviewQuery{}); return err }},
	}
	for _, check := range checks {
		if err := check.run(); !errors.Is(err, ErrUpstreamUnavailable) {
			t.Errorf("%s error = %v, want ErrUpstreamUnavailable", check.name, err)
		}
	}
	if provider.calls != len(checks) {
		t.Fatalf("Client() calls = %d, want one for each of %d operations", provider.calls, len(checks))
	}
}

func TestRuntimeServiceRechecksProviderAfterAvailabilityChanges(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte("[]"))
	}))
	defer server.Close()
	provider := &testClientProvider{results: make([]struct {
		client *magpie.Client
		ready  bool
	}, 2)}
	provider.results[1] = struct {
		client *magpie.Client
		ready  bool
	}{client: magpie.NewClient(server.URL, server.Client()), ready: true}
	service := NewRuntimeService(queryTestDB(t), provider, time.Local)
	if _, err := service.Quota(context.Background(), false); !errors.Is(err, ErrUpstreamUnavailable) {
		t.Fatalf("Quota before availability = %v, want ErrUpstreamUnavailable", err)
	}
	if got, err := service.Quota(context.Background(), false); err != nil || len(got.Quotas) != 0 {
		t.Fatalf("Quota after availability = %+v, %v; want empty response", got, err)
	}
	if provider.calls != 2 {
		t.Fatalf("Client() calls = %d, want 2", provider.calls)
	}
}
