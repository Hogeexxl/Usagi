package query

import (
	"context"
	"errors"
	"testing"

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
		{name: "sessions", run: func() error { _, err := upstream.Sessions(ctx, 0); return err }},
		{name: "session", run: func() error { _, err := upstream.Session(ctx, "key"); return err }},
		{name: "session stats", run: func() error { _, err := upstream.SessionStats(ctx, 0); return err }},
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
	provider := &testClientProvider{results: make([]struct {
		client *magpie.Client
		ready  bool
	}, 2)}
	provider.results[1].ready = true
	service := NewRuntimeService(provider)
	for i := 0; i < 2; i++ {
		if _, err := service.UsageSnapshot(context.Background()); !errors.Is(err, ErrUpstreamUnavailable) {
			t.Fatalf("UsageSnapshot call %d error = %v, want ErrUpstreamUnavailable", i, err)
		}
	}
	if provider.calls != 2 {
		t.Fatalf("Client() calls = %d, want 2", provider.calls)
	}
}
