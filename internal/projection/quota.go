package projection

import (
	"context"
	"fmt"
)

func (s *Service) Quotas(ctx context.Context, asked bool) (QuotaView, error) {
	response, err := s.query.Quota(ctx, asked)
	if err != nil {
		return QuotaView{}, fmt.Errorf("projection: quotas: %w", err)
	}
	view := QuotaView{Reading: response.Reading}
	if response.Quotas != nil {
		view.Items = make([]QuotaProviderView, 0, len(response.Quotas))
	}
	for _, quota := range response.Quotas {
		provider := QuotaProviderView{
			Provider: quota.Provider,
			Name:     quota.Name,
			Plan:     quota.Plan,
			User:     quota.User,
			Balance:  quota.Balance,
			Until:    quota.Until,
			Renew:    quota.Renew,
			Error:    quota.Error,
			AsOf:     quota.AsOf,
			ReadAt:   quota.ReadAt,
			Held:     quota.Held,
			Kind:     quota.Kind,
			From:     quota.From,
		}
		if quota.Windows != nil {
			provider.Windows = make([]QuotaWindowView, len(quota.Windows))
			for i, window := range quota.Windows {
				provider.Windows[i] = QuotaWindowView{
					Unlimited: window.Unlimited,
					Name:      window.Name,
					Used:      window.Used,
					ResetsAt:  window.ResetsAt,
					ResetSecs: window.ResetSecs,
					Display:   window.Display,
					Amount:    window.Amount,
					Limit:     window.Limit,
					Unit:      window.Unit,
					Family:    window.Family,
					Pool:      window.Pool,
				}
			}
		}
		if quota.Resets != nil {
			resets := &ResetCreditsView{
				Count:    quota.Resets.Count,
				Until:    quota.Resets.Until,
				ByWindow: quota.Resets.ByWindow,
				FiveHour: quota.Resets.FiveHour,
				Weekly:   quota.Resets.Weekly,
				Team:     quota.Resets.Team,
			}
			if quota.Resets.Each != nil {
				resets.Each = make([]ResetCardView, len(quota.Resets.Each))
				for i, card := range quota.Resets.Each {
					resets.Each[i] = ResetCardView{Until: card.Until, Window: card.Window}
				}
			}
			provider.Resets = resets
		}
		view.Items = append(view.Items, provider)
	}
	return view, nil
}
