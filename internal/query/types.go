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
	ProjectKindWorkspace   ProjectKind = "workspace"
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

type SessionQuery struct {
	Range  DateRange
	Filter UsageFilter
}

type SessionDatasetResult struct {
	Sessions []SessionUsage
	Rows     []UsageRow
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
	// Retried counts answered requests that succeeded only after a retry.
	Retried int64
	// DecodeMs, DecodeOut and DecodeN describe the requests whose output speed
	// Magpie can measure: the ms and tokens from first token to end, and how
	// many such requests there are.
	DecodeMs  int64
	DecodeOut int64
	DecodeN   int64
}

type ModelEffortUsage struct {
	Model  string
	Effort string
	Totals UsageTotals
}

type SessionUsage struct {
	Agent         string
	ID            string
	Role          string
	ParentSession string
	RootSession   string
	Title         string
	Nickname      string
	Cwd           string
	Project       ProjectRef
	Start         time.Time
	Last          time.Time
	FirstUsage    time.Time
	LastUsage     time.Time
	HasRows       bool
	Matched       bool

	ModelEffort []ModelEffortUsage
	Totals      UsageTotals
}

type SessionRef struct {
	Agent   string
	ID      string
	Matched bool
}

type UsageRow struct {
	Time         time.Time
	Agent        string
	AgentName    string
	Provider     string
	ProviderName string
	Model        string
	Effort       string

	Session       string
	SessionRole   string
	Compaction    *bool
	NativeSession string
	Subagent      string
	ParentAgent   string
	Kind          string
	Origin        string

	Status int
	Error  string

	Tries            int
	Millis           int64
	TTFTMs           int64
	FirstTextMs      int64
	FirstReasoningMs int64
	FlowMs           int64
	ReasoningHidden  bool
	HandedOn         bool

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

// UnknownModel names the model group of request rows Magpie recorded without
// a model, so they still aggregate and display as their own group.
const UnknownModel = "未知模型"

// ModelName returns the model a row aggregates under.
func ModelName(model string) string {
	if model == "" {
		return UnknownModel
	}
	return model
}

var (
	ErrInvalidRange  = errors.New("query: invalid range")
	ErrInvalidFilter = errors.New("query: invalid filter")
)
