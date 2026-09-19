export const PRESET_RANGE_KEYS = ["today", "yesterday", "7d", "30d", "year"] as const;

export const RANGE_KEYS = PRESET_RANGE_KEYS;

export type PresetRangeKey = (typeof PRESET_RANGE_KEYS)[number];

export type RangeKey = PresetRangeKey | "custom";

export type DashboardRange =
  | { key: PresetRangeKey }
  | { key: "custom"; from: string; to: string };

export type EstimatedCostStatus = "complete" | "partial" | "unknown";

export type RangeDto = {
  key: RangeKey;
  start_ms: number;
  end_ms: number;
  timezone: string;
};

export type UsageDto = {
  input_tokens: number;
  cached_tokens: number;
  cache_write_tokens: number | null;
  uncached_input_tokens: number | null;
  output_tokens: number;
  reasoning_tokens: number;
  other_output_tokens: number;
  total_tokens: number;
  cache_hit_rate: number | null;
  estimated_cost: number | null;
  estimated_cost_status: EstimatedCostStatus;
};

export type SessionHealthDto = {
  total_sessions: number;
  complete_sessions: number;
  incomplete_sessions: number;
  error_sessions: number;
};

export type SummaryUsageDto = UsageDto & {
  session_count: number;
  cost_incomplete_session_count: number;
  complete_session_cost_per_million_tokens: number | null;
  session_health: SessionHealthDto;
};

export type SummaryResponse = {
  range: RangeDto;
  data_revision: number;
  usage: SummaryUsageDto;
};

export type CodexQuotaWindowDto = {
  used_percent: number;
  remaining_percent: number;
  limit_window_seconds: number;
  reset_at_ms: number | null;
};

export type CodexQuotaResponse = {
  status: "loading" | "ready" | "auth_required" | "unavailable";
  account_email: string | null;
  plan_type: string | null;
  session: CodexQuotaWindowDto | null;
  weekly: CodexQuotaWindowDto | null;
  reset_credits_available: number | null;
  fetched_at_ms: number | null;
};

export type ProjectFilterOption =
  | { kind: "project"; project_name: string; project_path: string }
  | { kind: "projectless" }
  | { kind: "unknown" };

export type ProjectSelection =
  | { kind: "project"; project_path: string }
  | { kind: "projectless" }
  | { kind: "unknown" };

export type DashboardFilters = {
  models: string[];
  projects: ProjectSelection[];
};

export type ModelFilterProvider = "openai" | "route-models";

export type ModelFilterOption = {
  model: string;
  provider: ModelFilterProvider;
};

export type FilterOptionsResponse = {
  data_revision: number;
  models: ModelFilterOption[];
  projects: ProjectFilterOption[];
};

export type DistributionUsageDto = {
  total_tokens: number;
  estimated_cost: number | null;
  estimated_cost_status: EstimatedCostStatus;
};

export type ModelDistributionItemDto = {
  model: string;
  usage: DistributionUsageDto;
};

export type ModelDistributionResponse = {
  range: RangeDto;
  data_revision: number;
  items: ModelDistributionItemDto[];
};

export type ProjectDistributionItemDto = {
  kind: "project" | "projectless" | "unknown";
  project_name: string | null;
  project_path: string | null;
  usage: DistributionUsageDto;
};

export type ProjectDistributionResponse = {
  range: RangeDto;
  data_revision: number;
  items: ProjectDistributionItemDto[];
};

export type SkillDayDto = {
  date: string;
  start_ms: number;
  end_ms: number;
  total: number;
  skills: Array<{ skill_name: string; count: number }>;
};

export type SkillsUsageResponse = {
  range: RangeDto;
  data_revision: number;
  data_status: "ready" | "rebuilding";
  days: SkillDayDto[];
};

export type SessionItemDto = {
  root_session_id: string;
  title: string | null;
  project_name: string | null;
  project_path: string | null;
  last_activity_at_ms: number;
  models_used: string[];
  subagent_count: number;
  inclusive_usage: UsageDto | null;
  self_usage: UsageDto | null;
  subagent_usage: UsageDto | null;
  data_status: "complete" | "incomplete" | "error";
  error_code: string | null;
};

export type SessionSortField =
  | "last_activity"
  | "project"
  | "model"
  | "total_tokens"
  | "combined_total_tokens"
  | "combined_estimated_cost"
  | "cache_hit_rate";

export type SessionSortOrder = "asc" | "desc";

export type SessionSortIndexItem = {
  root_session_id: string;
  last_activity_at_ms: number;
  project_sort_key: string | null;
  model_sort_key: string | null;
  total_tokens: number | null;
  combined_total_tokens: number | null;
  combined_estimated_cost: number | null;
  cache_hit_rate: number | null;
  data_status: "complete" | "incomplete" | "error";
  error_code: string | null;
};

export type SessionSnapshotResponse = {
  range: RangeDto;
  data_revision: number;
  total_items: number;
  sort_index: SessionSortIndexItem[];
  items: SessionItemDto[];
};

export type SessionRowsResponse = {
  range: RangeDto;
  data_revision: number;
  items: SessionItemDto[];
};

export type MainModelUsageDto = {
  model: string;
  reasoning_effort: string | null;
  usage: UsageDto;
};

export type MainSessionDetailDto = {
  title: string | null;
  thread_id: string;
  root_session_id: string;
  models_used: string[];
  model_usage: MainModelUsageDto[];
  self_usage: UsageDto;
  subagent_count: number;
  inclusive_usage: UsageDto;
};

export type SubagentDetailDto = {
  thread_id: string;
  parent_thread_id: string | null;
  root_session_id: string;
  title: string | null;
  last_activity_at_ms: number;
  model_usage: Array<{
    model: string;
    reasoning_effort: string | null;
    last_activity_at_ms: number;
    usage: UsageDto;
  }>;
};

export type SessionDetailResponse = {
  range: RangeDto;
  data_revision: number;
  root_session_id: string;
  last_activity_at_ms: number;
  main: MainSessionDetailDto;
  subagents: SubagentDetailDto[];
};

export type RevisionResponse = {
  data_revision: number;
  status_revision: number;
};

export type FollowupDto = {
  scan_id: string;
  state: "queued" | "start_failed";
  enqueued_status_revision: number;
  requested_at_ms: number;
  error_code: string | null;
};

export type TargetScanDto = {
  scan_id: string;
  state: "queued" | "running" | "completed" | "failed" | "start_failed";
  started_status_revision: number | null;
  terminal_status_revision: number | null;
  error_code: string | null;
};

export type StatusResponse = {
  data_revision: number;
  status_revision: number;
  scan_state: "startup" | "running" | "idle" | "failed" | "source_changed";
  active_scan_id: string | null;
  last_finished_scan_id: string | null;
  last_finished_scan_result: string | null;
  followup: FollowupDto | null;
  target_scan: TargetScanDto | null;
  last_scan_started_at_ms: number | null;
  last_scan_completed_at_ms: number | null;
  last_scan_failed_at_ms: number | null;
  last_scan_error_code: string | null;
  source_binding_status: "unbound" | "ready" | "source_changed";
};

export type UpdateStatusResponse = {
  current_version: string;
  latest_version: string | null;
  update_available: boolean;
  release_url: string | null;
  last_checked_at_ms: number | null;
  checking: boolean;
};

export type RefreshAccepted = {
  http_status: 200 | 202;
  disposition: "started" | "coalesced";
  scan_id: string;
  status_revision: number;
};

export type ApiErrorCode =
  | "INVALID_RANGE"
  | "INVALID_FILTER"
  | "INVALID_SESSION_IDS"
  | "INVALID_SCAN_ID"
  | "SCAN_NOT_FOUND"
  | "STALE_DATA_REVISION"
  | "FORBIDDEN"
  | "FORBIDDEN_HOST"
  | "FORBIDDEN_ORIGIN"
  | "NOT_FOUND"
  | "SOURCE_CHANGED"
  | "SCANNER_UNAVAILABLE"
  | "LOCAL_TIME_UNAVAILABLE"
  | "QUERY_OVERFLOW"
  | "DATABASE_BUSY"
  | "QUERY_FAILED"
  | "SCAN_START_FAILED"
  | "SCAN_ENQUEUE_FAILED"
  | "UPDATE_CHECK_FAILED"
  | "UPDATE_NOT_AVAILABLE"
  | "UPDATE_BROWSER_OPEN_FAILED"
  | "INTERNAL_ERROR"
  | "HTTP_ERROR";

export class UsagiClientError extends Error {
  readonly code: ApiErrorCode;
  readonly status: number;

  constructor(code: ApiErrorCode, status: number) {
    super(code);
    this.name = "UsagiClientError";
    this.code = code;
    this.status = status;
  }
}

export type RevisionTuple = Pick<RevisionResponse, "data_revision" | "status_revision">;

export type SyncResponse = {
  status: "accepted" | "already_running" | "queued";
  scan_id: string | null;
};

export type UpdateCheckResponse = {
  status: "up_to_date" | "update_available";
  current_version: string;
  latest_version: string;
  release_url: string | null;
};

export type UpdateOpenResponse = {
  status: "opened";
};
