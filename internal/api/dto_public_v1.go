package api

type publicV1InfoDTO struct {
	Service      string   `json:"service"`
	AppVersion   string   `json:"app_version"`
	APIVersion   string   `json:"api_version"`
	Capabilities []string `json:"capabilities"`
	RuntimeMode  string   `json:"runtime_mode"`
}

type publicV1RevisionDTO struct {
	DataRevision   int64 `json:"data_revision"`
	StatusRevision int64 `json:"status_revision"`
}

type publicV1StatusDTO struct {
	DataRevision           int64   `json:"data_revision"`
	StatusRevision         int64   `json:"status_revision"`
	ScanState              string  `json:"scan_state"`
	SourceBindingStatus    string  `json:"source_binding_status"`
	LastFinishedScanResult *string `json:"last_finished_scan_result"`
	LastScanStartedAtMS    *int64  `json:"last_scan_started_at_ms"`
	LastScanCompletedAtMS  *int64  `json:"last_scan_completed_at_ms"`
	LastScanFailedAtMS     *int64  `json:"last_scan_failed_at_ms"`
	LastScanErrorCode      *string `json:"last_scan_error_code"`
}

type publicV1QuotaWindowDTO struct {
	UsedPercent        float64 `json:"used_percent"`
	RemainingPercent   float64 `json:"remaining_percent"`
	LimitWindowSeconds uint64  `json:"limit_window_seconds"`
	ResetAtMS          *int64  `json:"reset_at_ms"`
}

type publicV1QuotaDTO struct {
	Status      string                  `json:"status"`
	FiveHour    *publicV1QuotaWindowDTO `json:"five_hour"`
	Weekly      *publicV1QuotaWindowDTO `json:"weekly"`
	FetchedAtMS *int64                  `json:"fetched_at_ms"`
}

type publicV1RangeDTO struct {
	Key      string `json:"key"`
	StartMS  int64  `json:"start_ms"`
	EndMS    int64  `json:"end_ms"`
	Timezone string `json:"timezone"`
}

type publicV1SessionHealthDTO struct {
	TotalSessions      int64 `json:"total_sessions"`
	CompleteSessions   int64 `json:"complete_sessions"`
	IncompleteSessions int64 `json:"incomplete_sessions"`
	ErrorSessions      int64 `json:"error_sessions"`
}

type publicV1UsageDTO struct {
	InputTokens                         int64                    `json:"input_tokens"`
	CachedTokens                        int64                    `json:"cached_tokens"`
	CacheWriteTokens                    *int64                   `json:"cache_write_tokens"`
	UncachedInputTokens                 *int64                   `json:"uncached_input_tokens"`
	OutputTokens                        int64                    `json:"output_tokens"`
	ReasoningTokens                     int64                    `json:"reasoning_tokens"`
	OtherOutputTokens                   int64                    `json:"other_output_tokens"`
	TotalTokens                         int64                    `json:"total_tokens"`
	CacheHitRate                        *float64                 `json:"cache_hit_rate"`
	EstimatedCost                       *float64                 `json:"estimated_cost"`
	EstimatedCostStatus                 string                   `json:"estimated_cost_status"`
	SessionCount                        int64                    `json:"session_count"`
	CostIncompleteSessionCount          int64                    `json:"cost_incomplete_session_count"`
	CompleteSessionCostPerMillionTokens *float64                 `json:"complete_session_cost_per_million_tokens"`
	SessionHealth                       publicV1SessionHealthDTO `json:"session_health"`
}

type publicV1SummaryDTO struct {
	Range        publicV1RangeDTO `json:"range"`
	DataRevision int64            `json:"data_revision"`
	Usage        publicV1UsageDTO `json:"usage"`
}
