package projection

import (
	"errors"
	"time"

	"github.com/Hogeexxl/Usagi/internal/query"
)

type ScopeQuery struct {
	Range  query.DateRange
	Filter query.UsageFilter
}

type UsageView struct {
	Calls        int64
	Errors       int64
	Input        int64
	Output       int64
	CacheRead    int64
	CacheWrite   int64
	CacheWrite1h int64
	Reasoning    int64
	Total        int64
	CacheHitRate *float64
	Cost         float64
	Unpriced     int64
}

type ModelEffortUsageView struct {
	Model            string
	Effort           string
	Usage            UsageView
	CompactionTokens *int64
}

type ThreadType string

const (
	ThreadMain     ThreadType = "main"
	ThreadSubagent ThreadType = "subagent"
)

type ThreadKey struct {
	Agent     string
	SessionID string
	ThreadID  string
}

type SessionThreadView struct {
	Key            ThreadKey
	Type           ThreadType
	ParentThreadID string
	Title          string
	Project        query.ProjectRef
	FirstUsage     time.Time
	LastUsage      time.Time
	ModelEffort    []ModelEffortUsageView
	Usage          UsageView
}

type SessionListItemView struct {
	Thread         SessionThreadView
	SelfUsage      UsageView
	SubagentUsage  UsageView
	InclusiveUsage UsageView
	SubagentCount  int64
}

type SessionListView struct {
	Items []SessionListItemView
}

type SessionDetailView struct {
	Thread         SessionThreadView
	SelfUsage      UsageView
	SubagentUsage  UsageView
	InclusiveUsage UsageView
	Children       []SessionThreadView
}

type SummaryView struct {
	Usage                    UsageView
	RootSessionCount         int64
	RootUnpricedSessionCount int64
}

type ModelDistributionItem struct {
	Model string
	Usage UsageView
}

type ModelDistributionView struct {
	Items []ModelDistributionItem
}

type ProjectDistributionItem struct {
	Project query.ProjectRef
	Usage   UsageView
}

type ProjectDistributionView struct {
	Items []ProjectDistributionItem
}

type FilterOptionsView struct {
	Agents   []query.AgentOption
	Models   []query.ModelOption
	Projects []query.ProjectRef
}

type QuotaWindowView struct {
	Unlimited bool
	Name      string
	Used      float64
	ResetsAt  *time.Time
	ResetSecs int64
	Display   string
	Amount    float64
	Limit     float64
	Unit      string
	Family    string
	Pool      string
}

type ResetCardView struct {
	Until  *time.Time
	Window string
}

type ResetCreditsView struct {
	Count    int
	Until    *time.Time
	ByWindow bool
	FiveHour int
	Weekly   int
	Team     bool
	Each     []ResetCardView
}

type QuotaProviderView struct {
	Provider string
	Name     string
	Plan     string
	User     string
	Windows  []QuotaWindowView
	Balance  string
	Until    *time.Time
	Renew    string
	Error    string
	AsOf     *time.Time
	ReadAt   *time.Time
	Resets   *ResetCreditsView
	Held     bool
	Kind     string
	From     string
}

type QuotaView struct {
	Reading bool
	Items   []QuotaProviderView
}

var (
	ErrThreadNotFound          = errors.New("projection: thread not found")
	ErrConflictingThreadParent = errors.New("projection: conflicting thread parent")
)
