import { UsagiClientError, type ApiErrorCode, type AgentFilterOption, type FilterOptionsResponse, type ModelDistributionResponse, type ModelFilterOption, type ProjectDistributionResponse, type ProjectFilterOption, type QuotaProviderDto, type QuotaResponse, type QuotaWindowDto, type RequestMetricsDto, type RevisionResponse, type RuntimeStatusResponse, type SessionDetailResponse, type SessionListResponse, type SessionThreadDto, type SummaryResponse, type ToolUsageResponse, type ThreadKey } from "./types";

const API_ERROR_CODES = new Set<Exclude<ApiErrorCode, "HTTP_ERROR">>([
  "DATA_LOADING",
  "INVALID_RANGE",
  "INVALID_FILTER",
  "THREAD_NOT_FOUND",
  "MAGPIE_UNAVAILABLE",
  "UPSTREAM_FAILED",
  "INTERNAL_ERROR",
  "NOT_FOUND",
  "FORBIDDEN_HOST",
  "FORBIDDEN_ORIGIN",
]);

export function parseApiErrorCode(value: unknown): ApiErrorCode | null {
  return typeof value === "string" && API_ERROR_CODES.has(value as Exclude<ApiErrorCode, "HTTP_ERROR">)
    ? value as ApiErrorCode
    : null;
}

export type JsonRecord = Record<string, unknown>;

export function isJsonRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function requiredRecord(value: unknown): JsonRecord {
  if (!isJsonRecord(value)) throw new UsagiClientError("HTTP_ERROR", 200);
  return value;
}

function requiredString(record: JsonRecord, key: string): string {
  const value = record[key];
  if (typeof value !== "string" || value.length === 0) throw new UsagiClientError("HTTP_ERROR", 200);
  return value;
}

function stringField(record: JsonRecord, key: string): string {
  const value = record[key];
  if (typeof value !== "string") throw new UsagiClientError("HTTP_ERROR", 200);
  return value;
}

function nullableString(record: JsonRecord, key: string): string | null {
  const value = record[key];
  if (value === null) return null;
  if (typeof value !== "string") throw new UsagiClientError("HTTP_ERROR", 200);
  return value;
}

function requiredBoolean(record: JsonRecord, key: string): boolean {
  const value = record[key];
  if (typeof value !== "boolean") throw new UsagiClientError("HTTP_ERROR", 200);
  return value;
}

function requiredSafeInteger(record: JsonRecord, key: string): number {
  const value = record[key];
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new UsagiClientError("HTTP_ERROR", 200);
  }
  return value;
}

function nullableSafeInteger(record: JsonRecord, key: string): number | null {
  const value = record[key];
  if (value === null) return null;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new UsagiClientError("HTTP_ERROR", 200);
  }
  return value;
}

function finiteNumber(record: JsonRecord, key: string): number {
  const value = record[key];
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new UsagiClientError("HTTP_ERROR", 200);
  }
  return value;
}

function nullableFiniteNumber(record: JsonRecord, key: string): number | null {
  const value = record[key];
  if (value === null) return null;
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new UsagiClientError("HTTP_ERROR", 200);
  }
  return value;
}

function nullableRatio(record: JsonRecord, key: string): number | null {
  const value = record[key];
  if (value === null) return null;
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0 || value > 1) {
    throw new UsagiClientError("HTTP_ERROR", 200);
  }
  return value;
}

export function safeText(value: string): boolean {
  return [...value].every((character) => character.charCodeAt(0) >= 32);
}

function parseUsage(value: unknown) {
  const record = requiredRecord(value);
  return {
    calls: requiredSafeInteger(record, "calls"),
    errors: requiredSafeInteger(record, "errors"),
    input: requiredSafeInteger(record, "input"),
    output: requiredSafeInteger(record, "output"),
    cache_read: requiredSafeInteger(record, "cache_read"),
    cache_write: requiredSafeInteger(record, "cache_write"),
    cache_write_1h: requiredSafeInteger(record, "cache_write_1h"),
    reasoning: requiredSafeInteger(record, "reasoning"),
    total: requiredSafeInteger(record, "total"),
    cache_hit_rate: nullableRatio(record, "cache_hit_rate"),
    cost: finiteNumber(record, "cost"),
    unpriced: requiredSafeInteger(record, "unpriced"),
    retried: requiredSafeInteger(record, "retried"),
    output_speed: nullableFiniteNumber(record, "output_speed"),
    speed_samples: requiredSafeInteger(record, "speed_samples"),
  };
}

function parseRequestMetrics(value: unknown): RequestMetricsDto {
  const record = requiredRecord(value);
  return {
    calls: requiredSafeInteger(record, "calls"),
    errors: requiredSafeInteger(record, "errors"),
    retried: requiredSafeInteger(record, "retried"),
    output_speed: nullableFiniteNumber(record, "output_speed"),
    speed_samples: requiredSafeInteger(record, "speed_samples"),
    mean_ttft_seconds: nullableFiniteNumber(record, "mean_ttft_seconds"),
    ttft_samples: requiredSafeInteger(record, "ttft_samples"),
  };
}

export function parseSummary(value: unknown): SummaryResponse {
  const record = requiredRecord(value);
  const usage = parseUsage(record.usage);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    usage,
    request_metrics: parseRequestMetrics(record.request_metrics),
    session_count: requiredSafeInteger(record, "session_count"),
    session_median: nullableSafeInteger(record, "session_median"),
    session_p90: nullableSafeInteger(record, "session_p90"),
  };
}

function parseProjectOption(value: unknown): ProjectFilterOption {
  const record = requiredRecord(value);
  const kind = requiredString(record, "kind");
  if (kind === "project") return { kind, path: requiredString(record, "path"), name: requiredString(record, "name") };
  if (kind === "workspace") return { kind };
  if (kind === "projectless" || kind === "unknown") return { kind };
  throw new UsagiClientError("HTTP_ERROR", 200);
}

function parseAgentOption(value: unknown): AgentFilterOption {
  const record = requiredRecord(value);
  const agent = requiredString(record, "agent");
  const displayName = requiredString(record, "display_name");
  if (!safeText(agent) || !safeText(displayName)) throw new UsagiClientError("HTTP_ERROR", 200);
  return { agent, display_name: displayName };
}

function parseModelOption(value: unknown): ModelFilterOption {
  const record = requiredRecord(value);
  const model = requiredString(record, "model");
  const providerId = requiredString(record, "provider_id");
  const providerName = requiredString(record, "provider_name");
  if (![model, providerId, providerName].every(safeText)) throw new UsagiClientError("HTTP_ERROR", 200);
  return { model, provider_id: providerId, provider_name: providerName };
}

export function parseFilterOptions(value: unknown): FilterOptionsResponse {
  const record = requiredRecord(value);
  if (!Array.isArray(record.agents) || !Array.isArray(record.models) || !Array.isArray(record.projects)) {
    throw new UsagiClientError("HTTP_ERROR", 200);
  }
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    agents: record.agents.map(parseAgentOption),
    models: record.models.map(parseModelOption),
    projects: record.projects.map(parseProjectOption),
  };
}

export function parseModelDistribution(value: unknown): ModelDistributionResponse {
  const record = requiredRecord(value);
  if (!Array.isArray(record.items)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    items: record.items.map((value) => {
      const item = requiredRecord(value);
      return { model: requiredString(item, "model"), usage: parseUsage(item.usage) };
    }),
  };
}

export function parseProjectDistribution(value: unknown): ProjectDistributionResponse {
  const record = requiredRecord(value);
  if (!Array.isArray(record.items)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    items: record.items.map((value) => {
      const item = requiredRecord(value);
      const kind = requiredString(item, "kind");
      const usage = parseUsage(item.usage);
      if (kind === "project") return { kind, path: requiredString(item, "path"), name: requiredString(item, "name"), usage };
      if (kind === "workspace") return { kind, usage };
      if (kind === "projectless" || kind === "unknown") return { kind, usage };
      throw new UsagiClientError("HTTP_ERROR", 200);
    }),
  };
}

function parseThreadKey(value: unknown): ThreadKey {
  const record = requiredRecord(value);
  return {
    agent: requiredString(record, "agent"),
    session_id: requiredString(record, "session_id"),
    thread_id: requiredString(record, "thread_id"),
  };
}

function parseProject(value: unknown): SessionThreadDto["project"] {
  const record = requiredRecord(value);
  const kind = requiredString(record, "kind");
  if (kind === "project") return { kind, path: requiredString(record, "path"), name: requiredString(record, "name") };
  if (kind === "workspace") {
    return { kind, path: requiredString(record, "path") };
  }
  if (kind === "projectless" || kind === "unknown") return { kind };
  throw new UsagiClientError("HTTP_ERROR", 200);
}

function parseThread(value: unknown): SessionThreadDto {
  const record = requiredRecord(value);
  const type = requiredString(record, "type");
  if (type !== "main" && type !== "subagent" && type !== "unknown") throw new UsagiClientError("HTTP_ERROR", 200);
  if (!Array.isArray(record.model_effort)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    key: parseThreadKey(record.key),
    type,
    parent_thread_id: nullableString(record, "parent_thread_id"),
    title: nullableString(record, "title"),
    nickname: stringField(record, "nickname"),
    project: parseProject(record.project),
    first_usage_ms: requiredSafeInteger(record, "first_usage_ms"),
    last_usage_ms: requiredSafeInteger(record, "last_usage_ms"),
    model_effort: record.model_effort.map((value) => {
      const item = requiredRecord(value);
      return {
        model: requiredString(item, "model"),
        effort: stringField(item, "effort"),
        usage: parseUsage(item.usage),
        compaction_tokens: nullableSafeInteger(item, "compaction_tokens"),
      };
    }),
    usage: parseUsage(record.usage),
  };
}

export function parseSessionList(value: unknown): SessionListResponse {
  const record = requiredRecord(value);
  if (!Array.isArray(record.items)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    summary: parseSummary(record.summary),
    items: record.items.map((value) => {
      const item = requiredRecord(value);
      return {
        thread: parseThread(item.thread),
        self_usage: parseUsage(item.self_usage),
        subagent_usage: parseUsage(item.subagent_usage),
        unknown_usage: parseUsage(item.unknown_usage),
        inclusive_usage: parseUsage(item.inclusive_usage),
        has_usage_rows: requiredBoolean(item, "has_usage_rows"),
        last_activity_ms: nullableSafeInteger(item, "last_activity_ms"),
        subagent_count: nullableSafeInteger(item, "subagent_count"),
      };
    }),
  };
}

export function parseToolUsage(value: unknown): ToolUsageResponse {
  const record = requiredRecord(value);
  const days = requiredSafeInteger(record, "days");
  if (days !== 1 && days !== 7 && days !== 30) throw new UsagiClientError("HTTP_ERROR", 200);
  if (!Array.isArray(record.top)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    days,
    calls: requiredSafeInteger(record, "calls"),
    top: record.top.map((value) => {
      const item = requiredRecord(value);
      return { name: requiredString(item, "name"), calls: requiredSafeInteger(item, "calls") };
    }),
  };
}

export function parseSessionDetail(value: unknown): SessionDetailResponse {
  const record = requiredRecord(value);
  if (!Array.isArray(record.children)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    last_activity_ms: nullableSafeInteger(record, "last_activity_ms"),
    thread: parseThread(record.thread),
    self_usage: parseUsage(record.self_usage),
    subagent_usage: parseUsage(record.subagent_usage),
    unknown_usage: parseUsage(record.unknown_usage),
    inclusive_usage: parseUsage(record.inclusive_usage),
    children: record.children.map(parseThread),
  };
}

function parseQuotaWindow(value: unknown): QuotaWindowDto {
  const record = requiredRecord(value);
  return {
    unlimited: requiredBoolean(record, "unlimited"),
    name: requiredString(record, "name"),
    used: finiteNumber(record, "used"),
    resets_at_ms: nullableSafeInteger(record, "resets_at_ms"),
    reset_secs: requiredSafeInteger(record, "reset_secs"),
    display: stringField(record, "display"),
    amount: finiteNumber(record, "amount"),
    limit: finiteNumber(record, "limit"),
    unit: stringField(record, "unit"),
    family: stringField(record, "family"),
    pool: stringField(record, "pool"),
  };
}

function parseQuotaProvider(value: unknown): QuotaProviderDto {
  const record = requiredRecord(value);
  if (!Array.isArray(record.windows)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    provider: requiredString(record, "provider"),
    name: stringField(record, "name"),
    plan: stringField(record, "plan"),
    user: stringField(record, "user"),
    windows: record.windows.map(parseQuotaWindow),
    balance: stringField(record, "balance"),
    error: stringField(record, "error"),
    as_of_ms: nullableSafeInteger(record, "as_of_ms"),
    read_at_ms: nullableSafeInteger(record, "read_at_ms"),
  };
}

export function parseQuota(value: unknown): QuotaResponse {
  const record = requiredRecord(value);
  if (!Array.isArray(record.items)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    reading: requiredBoolean(record, "reading"),
    items: record.items.map(parseQuotaProvider),
  };
}

export function parseRevision(value: unknown): RevisionResponse {
  const record = requiredRecord(value);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    status_revision: requiredSafeInteger(record, "status_revision"),
  };
}

export function parseStatus(value: unknown): RuntimeStatusResponse {
  const record = requiredRecord(value);
  const runtimeState = requiredString(record, "runtime_state");
  if (runtimeState !== "starting" && runtimeState !== "ready" && runtimeState !== "unavailable") {
    throw new UsagiClientError("HTTP_ERROR", 200);
  }
  return {
    status_revision: requiredSafeInteger(record, "status_revision"),
    runtime_state: runtimeState,
    last_error_code: nullableString(record, "last_error_code"),
  };
}
