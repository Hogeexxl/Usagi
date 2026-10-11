package projection

import (
	"context"
	"reflect"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

func TestQuota(t *testing.T) {
	resetAt := time.Date(2026, 10, 12, 0, 0, 0, 0, time.UTC)
	until := resetAt.Add(24 * time.Hour)
	readAt := resetAt.Add(-time.Hour)
	response := magpie.QuotasResponse{
		Reading: true,
		Quotas: []magpie.Quota{
			{
				Provider: "provider-a", Name: "Provider A", Icon: "icon-a", Plan: "pro", User: "user-a",
				Windows: []magpie.QuotaWindow{{
					Unlimited: true, Name: "monthly", Used: 0.25, ResetsAt: &resetAt, ResetSecs: 3600,
					Display: "quarter", Amount: 25, Limit: 100, Unit: "requests", Family: "family-a", Pool: "pool-a",
				}},
				Balance: "25 credits", Until: &until, Renew: "monthly", Error: "warning", AsOf: &resetAt, ReadAt: &readAt,
				Resets: &magpie.ResetCredits{
					Count: 2, Until: &until, ByWindow: true, FiveHour: 1, Weekly: 2, Team: true,
					Each: []magpie.ResetCard{{Until: &resetAt, Window: "short"}, {Until: &until, Window: "long"}},
				},
				Held: true, Kind: "account", From: "source-a",
			},
			{
				Provider: "provider-b", Name: "Provider B", Windows: []magpie.QuotaWindow{
					{Name: "short", Used: 0.1},
					{Name: "medium", Used: 0.2},
					{Name: "long", Used: 0.3},
				},
			},
		},
	}
	calls := 0
	fake := &fakeQuery{onQuota: func(_ context.Context, asked bool) (magpie.QuotasResponse, error) {
		calls++
		if !asked {
			t.Fatalf("asked = false, want true")
		}
		return response, nil
	}}

	got, err := newService(fake).Quotas(context.Background(), true)
	if err != nil {
		t.Fatalf("Quotas() error = %v", err)
	}
	want := QuotaView{
		Reading: true,
		Items: []QuotaProviderView{
			{
				Provider: "provider-a", Name: "Provider A", Plan: "pro", User: "user-a",
				Windows: []QuotaWindowView{{
					Unlimited: true, Name: "monthly", Used: 0.25, ResetsAt: &resetAt, ResetSecs: 3600,
					Display: "quarter", Amount: 25, Limit: 100, Unit: "requests", Family: "family-a", Pool: "pool-a",
				}},
				Balance: "25 credits", Until: &until, Renew: "monthly", Error: "warning", AsOf: &resetAt, ReadAt: &readAt,
				Resets: &ResetCreditsView{
					Count: 2, Until: &until, ByWindow: true, FiveHour: 1, Weekly: 2, Team: true,
					Each: []ResetCardView{{Until: &resetAt, Window: "short"}, {Until: &until, Window: "long"}},
				},
				Held: true, Kind: "account", From: "source-a",
			},
			{
				Provider: "provider-b", Name: "Provider B", Windows: []QuotaWindowView{
					{Name: "short", Used: 0.1},
					{Name: "medium", Used: 0.2},
					{Name: "long", Used: 0.3},
				},
			},
		},
	}
	if !reflect.DeepEqual(got, want) || calls != 1 {
		t.Fatalf("Quotas() = %+v, calls %d; want %+v and one call", got, calls, want)
	}
}
