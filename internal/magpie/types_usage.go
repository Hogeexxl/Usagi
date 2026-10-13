package magpie

import "time"

type UsageTotals struct {
	Calls        int     `json:"calls"`
	Errors       int     `json:"errors"`
	Input        int     `json:"input"`
	Output       int     `json:"output"`
	CacheRead    int     `json:"cache_read"`
	CacheWrite   int     `json:"cache_write"`
	CacheWrite1h int     `json:"cache_write_1h,omitempty"`
	Reasoning    int     `json:"reasoning"`
	Cost         float64 `json:"cost"`
	Unpriced     int     `json:"unpriced"`
	Timed        int     `json:"timed,omitempty"`
	TTFTMs       int64   `json:"ttft_ms,omitempty"`
	DecodeMs     int64   `json:"decode_ms,omitempty"`
	DecodeOut    int     `json:"decode_out,omitempty"`
}

type UsageGroup struct {
	ID       string `json:"id"`
	Provider string `json:"provider,omitempty"`
	Model    string `json:"model,omitempty"`
	Account  string `json:"account,omitempty"`
	Agent    string `json:"agent,omitempty"`
	Name     string `json:"name"`
	Sub      string `json:"sub,omitempty"`
	Icon     string `json:"icon,omitempty"`
	UsageTotals
}

type UsagePoint struct {
	Label string    `json:"label"`
	Time  time.Time `json:"time"`
	UsageTotals
}

type UsageSummary struct {
	Period     string       `json:"period"`
	Since      time.Time    `json:"since"`
	Bucket     string       `json:"bucket"`
	Series     []UsagePoint `json:"series"`
	Agents     []UsageGroup `json:"agents"`
	Models     []UsageGroup `json:"models"`
	Accounts   []UsageGroup `json:"accounts"`
	CallerKeys []UsageGroup `json:"callerKeys"`
	UsageTotals
}

type UsageRequest struct {
	Time            time.Time `json:"t"`
	Agent           string    `json:"agent"`
	AgentName       string    `json:"agentName"`
	Provider        string    `json:"provider"`
	ProviderName    string    `json:"providerName"`
	ProviderAccount string    `json:"providerAccount,omitempty"`
	Model           string    `json:"model"`
	Requested       string    `json:"req,omitempty"`
	Served          string    `json:"served,omitempty"`
	Input           int       `json:"in"`
	Output          int       `json:"out"`
	CacheRead       int       `json:"cache_read,omitempty"`
	CacheWrite      int       `json:"cache_write,omitempty"`
	CacheWrite1h    int       `json:"cache_write_1h,omitempty"`
	Reasoning       int       `json:"reasoning,omitempty"`
	Effort          string    `json:"effort,omitempty"`
	Millis          int64     `json:"ms"`
	TTFTMs          int64     `json:"ttft_ms,omitempty"`
	FirstTextMs     int64     `json:"first_text_ms,omitempty"`
	FlowMs          int64     `json:"flow_ms,omitempty"`
	Tries           int       `json:"tries,omitempty"`
	Status          int       `json:"status"`
	Rejected        bool      `json:"rejected,omitempty"`
	Error           string    `json:"err,omitempty"`
	Session         string    `json:"session,omitempty"`
	NativeSession   string    `json:"native_session,omitempty"`
	Subagent        string    `json:"subagent,omitempty"`
	ParentAgent     string    `json:"parent_agent,omitempty"`
	Kind            string    `json:"kind,omitempty"`
	Cost            float64   `json:"cost"`
	Priced          bool      `json:"priced"`
	Source          string    `json:"source,omitempty"`
}

type UsageShare struct {
	ID   string `json:"id"`
	Name string `json:"name"`
	Icon string `json:"icon,omitempty"`
	UsageTotals
}

type UsageNamed struct {
	ID   string `json:"id"`
	Name string `json:"name"`
	Icon string `json:"icon"`
}

type UsageAccountOption struct {
	ID        string   `json:"id"`
	Providers []string `json:"providers"`
}

type UsageRequestsPage struct {
	Period     string                  `json:"period"`
	Rows       []UsageRequest          `json:"rows"`
	Offset     int                     `json:"offset"`
	Total      int                     `json:"total"`
	Bucket     string                  `json:"bucket"`
	Series     []UsagePoint            `json:"series"`
	By         map[string][]UsageShare `json:"by"`
	ChartBy    map[string][]UsageShare `json:"chartBy,omitempty"`
	Day        string                  `json:"day,omitempty"`
	Through    UsageTotals             `json:"through"`
	Direct     UsageTotals             `json:"direct"`
	Agents     []UsageNamed            `json:"agents"`
	Providers  []UsageNamed            `json:"providers"`
	Purposes   []string                `json:"purposes"`
	Accounts   []UsageAccountOption    `json:"accounts"`
	CallerKeys []UsageGroup            `json:"callerKeys"`
	UsageTotals
}

// UsageDayDigest fingerprints one local day of Magpie's usage series. Any row
// change that moves that day's totals or per-agent/model breakdown changes it.
type UsageDayDigest struct {
	Day    string // YYYY-MM-DD in Magpie's local time
	Calls  int
	Digest [32]byte
}

type UsageRequestsQuery struct {
	Period   string
	Day      string
	Model    string
	Agent    string
	Provider string
	Purpose  string
	Account  string
	Failed   bool
	Search   string
	Via      string
	Offset   int
	Limit    int
}
