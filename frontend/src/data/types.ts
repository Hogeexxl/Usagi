export const PRESET_RANGE_KEYS = ["today", "yesterday", "7d", "30d", "year"] as const;

export const RANGE_KEYS = PRESET_RANGE_KEYS;

export type PresetRangeKey = (typeof PRESET_RANGE_KEYS)[number];

export type RangeKey = PresetRangeKey | "custom";

export type DashboardRange =
  | { key: PresetRangeKey }
  | { key: "custom"; from: string; to: string };

export type UsageDto = {
  calls: number;
  errors: number;
  input: number;
  output: number;
  cache_read: number;
  cache_write: number;
  cache_write_1h: number;
  reasoning: number;
  total: number;
  cache_hit_rate: number | null;
  cost: number;
  unpriced: number;
};

export type SummaryResponse = {
  data_revision: number;
  usage: UsageDto;
  session_count: number;
};

export type ProjectFilterOption =
  | { kind: "project"; path: string; name: string }
  | { kind: "projectless" }
  | { kind: "unknown" };

export type ProjectSelection =
  | { kind: "project"; path: string }
  | { kind: "projectless" }
  | { kind: "unknown" };

export type ModelSelection = {
  provider_id: string;
  model: string;
};

export type DashboardFilters = {
  agents: string[];
  models: ModelSelection[];
  projects: ProjectSelection[];
};

export type AgentFilterOption = {
  agent: string;
  display_name: string;
};

export type ModelFilterOption = {
  model: string;
  provider_id: string;
  provider_name: string;
};

export type FilterOptionsResponse = {
  data_revision: number;
  agents: AgentFilterOption[];
  models: ModelFilterOption[];
  projects: ProjectFilterOption[];
};

export type ModelDistributionItemDto = {
  model: string;
  usage: UsageDto;
};

export type ModelDistributionResponse = {
  data_revision: number;
  items: ModelDistributionItemDto[];
};

export type ProjectDistributionItemDto =
  | { kind: "project"; path: string; name: string; usage: UsageDto }
  | { kind: "projectless" | "unknown"; usage: UsageDto };

export type ProjectDistributionResponse = {
  data_revision: number;
  items: ProjectDistributionItemDto[];
};

// Kept for future chart consumers; Skills are intentionally outside the main Dashboard fetch path.
export type SkillDayDto = {
  date: string;
  start_ms: number;
  end_ms: number;
  total: number;
  skills: Array<{ skill_name: string; count: number }>;
};

export type SkillsUsageResponse = {
  data_revision: number;
  data_status: "ready" | "rebuilding";
  days: SkillDayDto[];
};

export type ThreadKey = {
  agent: string;
  session_id: string;
  thread_id: string;
};

export type SessionModelEffortDto = {
  model: string;
  effort: string;
  usage: UsageDto;
  compaction_tokens: number | null;
};

export type SessionProjectDto =
  | { kind: "project"; path: string; name: string }
  | { kind: "projectless" | "unknown"; path?: string; name?: string };

export type SessionThreadDto = {
  key: ThreadKey;
  type: "main" | "subagent";
  parent_thread_id: string | null;
  title: string | null;
  project: SessionProjectDto;
  first_usage_ms: number;
  last_usage_ms: number;
  model_effort: SessionModelEffortDto[];
  usage: UsageDto;
};

export type SessionListItemDto = {
  thread: SessionThreadDto;
  self_usage: UsageDto;
  subagent_usage: UsageDto;
  inclusive_usage: UsageDto;
  subagent_count: number;
};

export type SessionSortField =
  | "last_activity"
  | "project"
  | "model"
  | "total_tokens"
  | "cost"
  | "cache_hit_rate";

export type SessionSortOrder = "asc" | "desc";

export type SessionListResponse = {
  data_revision: number;
  items: SessionListItemDto[];
};

export type SessionDetailResponse = {
  data_revision: number;
  thread: SessionThreadDto;
  self_usage: UsageDto;
  subagent_usage: UsageDto;
  inclusive_usage: UsageDto;
  children: SessionThreadDto[];
};

export type QuotaWindowDto = {
  unlimited: boolean;
  name: string;
  used: number;
  resets_at_ms: number | null;
  reset_secs: number;
  display: string;
  amount: number;
  limit: number;
  unit: string;
  family: string;
  pool: string;
};

export type QuotaProviderDto = {
  provider: string;
  name: string;
  plan: string;
  user: string;
  windows: QuotaWindowDto[];
  balance: string;
  error: string;
  as_of_ms: number | null;
  read_at_ms: number | null;
};

export type QuotaResponse = {
  reading: boolean;
  items: QuotaProviderDto[];
};

export type RevisionResponse = {
  data_revision: number;
  status_revision: number;
};

export type RuntimeStatusResponse = {
  status_revision: number;
  runtime_state: "starting" | "ready" | "unavailable";
  last_error_code: string | null;
};

export type ApiErrorCode =
  | "INVALID_RANGE"
  | "INVALID_FILTER"
  | "THREAD_NOT_FOUND"
  | "MAGPIE_UNAVAILABLE"
  | "UPSTREAM_FAILED"
  | "INTERNAL_ERROR"
  | "NOT_FOUND"
  | "FORBIDDEN_HOST"
  | "FORBIDDEN_ORIGIN"
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
