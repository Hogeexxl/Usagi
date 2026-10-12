package query

import (
	"errors"
	"time"
)

type DateRange struct {
	From string // YYYY-MM-DD，包含
	To   string // YYYY-MM-DD，包含
}

type ProjectKind string

const (
	ProjectKindProject     ProjectKind = "project"
	ProjectKindProjectless ProjectKind = "projectless"
	ProjectKindUnknown     ProjectKind = "unknown"
)

type ProjectSelection struct {
	Kind ProjectKind
	Path string
}

type ProjectRef struct {
	Kind ProjectKind
	Path string
	Name string
}

type ModelSelection struct {
	ProviderID string
	Model      string
}

type UsageFilter struct {
	Agents   []string
	Models   []ModelSelection
	Projects []ProjectSelection
}

type UsageGroupBy string

const (
	GroupNone          UsageGroupBy = ""
	GroupAgent         UsageGroupBy = "agent"
	GroupModel         UsageGroupBy = "model"
	GroupProject       UsageGroupBy = "project"
	GroupModelEffort   UsageGroupBy = "model_effort"
	GroupProjectModel  UsageGroupBy = "project_model"
	GroupProjectEffort UsageGroupBy = "project_effort"
)

type UsageQuery struct {
	Range   DateRange
	Filter  UsageFilter
	GroupBy UsageGroupBy
}

type UsageRowsQuery struct {
	Range  DateRange
	Filter UsageFilter
}

type SessionQuery struct {
	Range  DateRange
	Filter UsageFilter
}

type OverviewQuery struct {
	Days  int
	Agent string
	Model string
	Cwd   string
}

type UsageTotals struct {
	Calls        int64
	Errors       int64
	Input        int64
	Output       int64
	CacheRead    int64
	CacheWrite   int64
	CacheWrite1h int64
	Reasoning    int64
	Cost         float64
	Unpriced     int64
}

type UsageGroup struct {
	Agent     string
	AgentName string
	Model     string
	Effort    string
	Project   ProjectRef
	Totals    UsageTotals
}

type UsageResult struct {
	Totals UsageTotals
	Groups []UsageGroup
}

type ModelEffortUsage struct {
	Model  string
	Effort string
	Totals UsageTotals
}

type SessionUsage struct {
	Agent      string
	ID         string
	Title      string
	Cwd        string
	Project    ProjectRef
	Start      time.Time
	Last       time.Time
	FirstUsage time.Time
	LastUsage  time.Time

	ModelEffort []ModelEffortUsage
	Totals      UsageTotals
}

type SessionResult struct {
	Sessions []SessionUsage
}

type SessionRef struct {
	Agent   string
	ID      string
	Matched bool
}

type UsageRow struct {
	Time      time.Time
	Agent     string
	AgentName string
	Provider  string
	Model     string
	Effort    string

	Session       string
	NativeSession string
	Subagent      string
	ParentAgent   string
	Kind          string
	Origin        string

	Status int
	Error  string

	ResolvedSession SessionRef
	Project         ProjectRef

	Input        int64
	Output       int64
	CacheRead    int64
	CacheWrite   int64
	CacheWrite1h int64
	Reasoning    int64
	Cost         float64
	Priced       bool
}

type UsageRowsResult struct {
	Rows []UsageRow
}

type AgentOption struct {
	Agent       string
	DisplayName string
}

type FilterOptionsResult struct {
	Agents   []AgentOption
	Models   []ModelOption
	Projects []ProjectRef
}

type ModelOption struct {
	Model        string
	ProviderID   string
	ProviderName string
}

var (
	ErrInvalidRange   = errors.New("query: invalid range")
	ErrInvalidFilter  = errors.New("query: invalid filter")
	ErrInvalidGroupBy = errors.New("query: invalid group by")
)
