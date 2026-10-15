package api

import (
	"time"

	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

type usageDTO struct {
	Calls        int64    `json:"calls"`
	Errors       int64    `json:"errors"`
	Input        int64    `json:"input"`
	Output       int64    `json:"output"`
	CacheRead    int64    `json:"cache_read"`
	CacheWrite   int64    `json:"cache_write"`
	CacheWrite1h int64    `json:"cache_write_1h"`
	Reasoning    int64    `json:"reasoning"`
	Total        int64    `json:"total"`
	CacheHitRate *float64 `json:"cache_hit_rate"`
	Cost         float64  `json:"cost"`
	Unpriced     int64    `json:"unpriced"`
	Retried      int64    `json:"retried"`
	OutputSpeed  *float64 `json:"output_speed"`
	SpeedSamples int64    `json:"speed_samples"`
}

type summaryResponseDTO struct {
	DataRevision   int64             `json:"data_revision"`
	Usage          usageDTO          `json:"usage"`
	RequestMetrics requestMetricsDTO `json:"request_metrics"`
	SessionCount   int64             `json:"session_count"`
	SessionMedian  *int64            `json:"session_median"`
	SessionP90     *int64            `json:"session_p90"`
}

type requestMetricsDTO struct {
	Calls           int64    `json:"calls"`
	Errors          int64    `json:"errors"`
	Retried         int64    `json:"retried"`
	OutputSpeed     *float64 `json:"output_speed"`
	SpeedSamples    int64    `json:"speed_samples"`
	MeanTTFTSeconds *float64 `json:"mean_ttft_seconds"`
	TTFTSamples     int64    `json:"ttft_samples"`
}

type modelDistributionItemDTO struct {
	Model string   `json:"model"`
	Usage usageDTO `json:"usage"`
}

type modelDistributionResponseDTO struct {
	DataRevision int64                      `json:"data_revision"`
	Items        []modelDistributionItemDTO `json:"items"`
}

type projectDTO struct {
	Kind string `json:"kind"`
	Path string `json:"path,omitempty"`
	Name string `json:"name,omitempty"`
}

type projectDistributionItemDTO struct {
	Kind  string   `json:"kind"`
	Path  string   `json:"path,omitempty"`
	Name  string   `json:"name,omitempty"`
	Usage usageDTO `json:"usage"`
}

type projectDistributionResponseDTO struct {
	DataRevision int64                        `json:"data_revision"`
	Items        []projectDistributionItemDTO `json:"items"`
}

type agentOptionDTO struct {
	Agent       string `json:"agent"`
	DisplayName string `json:"display_name"`
}

type modelOptionDTO struct {
	Model        string `json:"model"`
	ProviderID   string `json:"provider_id"`
	ProviderName string `json:"provider_name"`
}

type filterOptionsResponseDTO struct {
	DataRevision int64            `json:"data_revision"`
	Agents       []agentOptionDTO `json:"agents"`
	Models       []modelOptionDTO `json:"models"`
	Projects     []projectDTO     `json:"projects"`
}

type modelEffortDTO struct {
	Model            string   `json:"model"`
	Effort           string   `json:"effort"`
	Usage            usageDTO `json:"usage"`
	CompactionTokens *int64   `json:"compaction_tokens"`
}

type threadKeyDTO struct {
	Agent     string `json:"agent"`
	SessionID string `json:"session_id"`
	ThreadID  string `json:"thread_id"`
}

type sessionThreadDTO struct {
	Key            threadKeyDTO     `json:"key"`
	Type           string           `json:"type"`
	ParentThreadID *string          `json:"parent_thread_id"`
	Title          string           `json:"title"`
	Nickname       string           `json:"nickname"`
	Project        projectDTO       `json:"project"`
	FirstUsageMS   int64            `json:"first_usage_ms"`
	LastUsageMS    int64            `json:"last_usage_ms"`
	ModelEffort    []modelEffortDTO `json:"model_effort"`
	Usage          usageDTO         `json:"usage"`
}

type sessionListItemDTO struct {
	Thread         sessionThreadDTO `json:"thread"`
	SelfUsage      usageDTO         `json:"self_usage"`
	SubagentUsage  usageDTO         `json:"subagent_usage"`
	UnknownUsage   usageDTO         `json:"unknown_usage"`
	InclusiveUsage usageDTO         `json:"inclusive_usage"`
	HasUsageRows   bool             `json:"has_usage_rows"`
	LastActivityMS *int64           `json:"last_activity_ms"`
	SubagentCount  *int64           `json:"subagent_count"`
}

type sessionsResponseDTO struct {
	DataRevision int64                `json:"data_revision"`
	Summary      summaryResponseDTO   `json:"summary"`
	Items        []sessionListItemDTO `json:"items"`
}

type toolUsageItemDTO struct {
	Name  string `json:"name"`
	Calls int64  `json:"calls"`
}

type toolUsageResponseDTO struct {
	Days  int                `json:"days"`
	Calls int64              `json:"calls"`
	Top   []toolUsageItemDTO `json:"top"`
}

type sessionDetailResponseDTO struct {
	DataRevision   int64              `json:"data_revision"`
	LastActivityMS *int64             `json:"last_activity_ms"`
	Thread         sessionThreadDTO   `json:"thread"`
	SelfUsage      usageDTO           `json:"self_usage"`
	SubagentUsage  usageDTO           `json:"subagent_usage"`
	UnknownUsage   usageDTO           `json:"unknown_usage"`
	InclusiveUsage usageDTO           `json:"inclusive_usage"`
	Children       []sessionThreadDTO `json:"children"`
}

type quotaWindowDTO struct {
	Unlimited  bool    `json:"unlimited"`
	Name       string  `json:"name"`
	Used       float64 `json:"used"`
	ResetsAtMS *int64  `json:"resets_at_ms"`
	ResetSecs  int64   `json:"reset_secs"`
	Display    string  `json:"display"`
	Amount     float64 `json:"amount"`
	Limit      float64 `json:"limit"`
	Unit       string  `json:"unit"`
	Family     string  `json:"family"`
	Pool       string  `json:"pool"`
}

type resetCardDTO struct {
	UntilMS *int64 `json:"until_ms"`
	Window  string `json:"window"`
}

type resetCreditsDTO struct {
	Count    int            `json:"count"`
	UntilMS  *int64         `json:"until_ms"`
	ByWindow bool           `json:"by_window"`
	FiveHour int            `json:"five_hour"`
	Weekly   int            `json:"weekly"`
	Team     bool           `json:"team"`
	Each     []resetCardDTO `json:"each"`
}

type quotaProviderDTO struct {
	Provider string           `json:"provider"`
	Name     string           `json:"name"`
	Plan     string           `json:"plan"`
	User     string           `json:"user"`
	Windows  []quotaWindowDTO `json:"windows"`
	Balance  string           `json:"balance"`
	UntilMS  *int64           `json:"until_ms"`
	Renew    string           `json:"renew"`
	Error    string           `json:"error"`
	AsOfMS   *int64           `json:"as_of_ms"`
	ReadAtMS *int64           `json:"read_at_ms"`
	Resets   *resetCreditsDTO `json:"resets"`
	Held     bool             `json:"held"`
	Kind     string           `json:"kind"`
	From     string           `json:"from"`
}

type quotaResponseDTO struct {
	Reading bool               `json:"reading"`
	Items   []quotaProviderDTO `json:"items"`
}

func mapUsageDTO(view projection.UsageView) usageDTO {
	return usageDTO{
		Calls: view.Calls, Errors: view.Errors, Input: view.Input, Output: view.Output,
		CacheRead: view.CacheRead, CacheWrite: view.CacheWrite, CacheWrite1h: view.CacheWrite1h,
		Reasoning: view.Reasoning, Total: view.Total, CacheHitRate: view.CacheHitRate,
		Cost: view.Cost, Unpriced: view.Unpriced,
		Retried: view.Retried, OutputSpeed: view.OutputSpeed, SpeedSamples: view.DecodeN,
	}
}

func mapProjectDTO(project query.ProjectRef) projectDTO {
	return projectDTO{Kind: string(project.Kind), Path: project.Path, Name: project.Name}
}

func mapModelEffortDTOs(values []projection.ModelEffortUsageView) []modelEffortDTO {
	result := make([]modelEffortDTO, 0, len(values))
	for _, value := range values {
		result = append(result, modelEffortDTO{
			Model: value.Model, Effort: value.Effort, Usage: mapUsageDTO(value.Usage),
			CompactionTokens: value.CompactionTokens,
		})
	}
	return result
}

func mapSessionThreadDTO(thread projection.SessionThreadView) sessionThreadDTO {
	var parent *string
	if thread.ParentThreadID != "" {
		value := thread.ParentThreadID
		parent = &value
	}
	return sessionThreadDTO{
		Key:  threadKeyDTO{Agent: thread.Key.Agent, SessionID: thread.Key.SessionID, ThreadID: thread.Key.ThreadID},
		Type: string(thread.Type), ParentThreadID: parent, Title: thread.Title, Nickname: thread.Nickname,
		Project: mapProjectDTO(thread.Project), FirstUsageMS: timeMS(thread.FirstUsage), LastUsageMS: timeMS(thread.LastUsage),
		ModelEffort: mapModelEffortDTOs(thread.ModelEffort), Usage: mapUsageDTO(thread.Usage),
	}
}

func mapQuotaDTO(view projection.QuotaView) quotaResponseDTO {
	result := quotaResponseDTO{Reading: view.Reading, Items: make([]quotaProviderDTO, 0, len(view.Items))}
	for _, item := range view.Items {
		mapped := quotaProviderDTO{
			Provider: item.Provider, Name: item.Name, Plan: item.Plan, User: item.User,
			Windows: make([]quotaWindowDTO, 0, len(item.Windows)), Balance: item.Balance,
			UntilMS: timePointerMS(item.Until), Renew: item.Renew, Error: item.Error,
			AsOfMS: timePointerMS(item.AsOf), ReadAtMS: timePointerMS(item.ReadAt), Held: item.Held,
			Kind: item.Kind, From: item.From,
		}
		for _, window := range item.Windows {
			mapped.Windows = append(mapped.Windows, quotaWindowDTO{
				Unlimited: window.Unlimited, Name: window.Name, Used: window.Used,
				ResetsAtMS: timePointerMS(window.ResetsAt), ResetSecs: window.ResetSecs,
				Display: window.Display, Amount: window.Amount, Limit: window.Limit,
				Unit: window.Unit, Family: window.Family, Pool: window.Pool,
			})
		}
		if item.Resets != nil {
			resets := &resetCreditsDTO{
				Count: item.Resets.Count, UntilMS: timePointerMS(item.Resets.Until),
				ByWindow: item.Resets.ByWindow, FiveHour: item.Resets.FiveHour,
				Weekly: item.Resets.Weekly, Team: item.Resets.Team,
				Each: make([]resetCardDTO, 0, len(item.Resets.Each)),
			}
			for _, card := range item.Resets.Each {
				resets.Each = append(resets.Each, resetCardDTO{UntilMS: timePointerMS(card.Until), Window: card.Window})
			}
			mapped.Resets = resets
		}
		result.Items = append(result.Items, mapped)
	}
	return result
}

func timeMS(value time.Time) int64 {
	if value.IsZero() {
		return 0
	}
	return value.UnixMilli()
}

func timePointerMS(value *time.Time) *int64 {
	if value == nil {
		return nil
	}
	result := value.UnixMilli()
	return &result
}

func nullableTimeMS(value time.Time) *int64 {
	if value.IsZero() {
		return nil
	}
	result := value.UnixMilli()
	return &result
}
