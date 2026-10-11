package magpie

import "time"

type QuotasResponse struct {
	Quotas  []Quota
	Reading bool
}

type QuotaWindow struct {
	Unlimited bool       `json:"unlimited,omitempty"`
	Name      string     `json:"name"`
	Used      float64    `json:"used"`
	ResetsAt  *time.Time `json:"resetsAt,omitempty"`
	ResetSecs int64      `json:"resetSecs,omitempty"`
	Display   string     `json:"display,omitempty"`
	Amount    float64    `json:"amount,omitempty"`
	Limit     float64    `json:"limit,omitempty"`
	Unit      string     `json:"unit,omitempty"`
	Family    string     `json:"family,omitempty"`
	Pool      string     `json:"pool,omitempty"`
}

type ResetCard struct {
	Until  *time.Time `json:"until,omitempty"`
	Window string     `json:"window,omitempty"`
}

type ResetCredits struct {
	Count    int         `json:"count"`
	Until    *time.Time  `json:"until,omitempty"`
	ByWindow bool        `json:"byWindow,omitempty"`
	FiveHour int         `json:"fiveHour,omitempty"`
	Weekly   int         `json:"weekly,omitempty"`
	Team     bool        `json:"team,omitempty"`
	Each     []ResetCard `json:"each,omitempty"`
}

type Quota struct {
	Provider string        `json:"provider"`
	Name     string        `json:"name"`
	Icon     string        `json:"icon"`
	Plan     string        `json:"plan,omitempty"`
	User     string        `json:"user,omitempty"`
	Windows  []QuotaWindow `json:"windows"`
	Balance  string        `json:"balance,omitempty"`
	Until    *time.Time    `json:"until,omitempty"`
	Renew    string        `json:"renew,omitempty"`
	Error    string        `json:"error,omitempty"`
	AsOf     *time.Time    `json:"asOf,omitempty"`
	ReadAt   *time.Time    `json:"readAt,omitempty"`
	Resets   *ResetCredits `json:"resets,omitempty"`
	Held     bool          `json:"held,omitempty"`
	Kind     string        `json:"kind,omitempty"`
	From     string        `json:"from,omitempty"`
}
