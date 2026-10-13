import {
  UsagiClientError,
  type ApiErrorCode,
  type AgentFilterOption,
  type DashboardFilters,
  type DashboardRange,
  type FilterOptionsResponse,
  type ModelDistributionResponse,
  type ModelFilterOption,
  type ModelSelection,
  type ProjectDistributionResponse,
  type ProjectFilterOption,
  type ProjectSelection,
  type QuotaProviderDto,
  type QuotaResponse,
  type QuotaWindowDto,
  type RevisionResponse,
  type RuntimeStatusResponse,
  type SessionDetailResponse,
  type SessionListResponse,
  type SessionThreadDto,
  type SummaryResponse,
  type ThreadKey,
} from "./types";

const API_ERROR_CODES = new Set<ApiErrorCode>([
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

type JsonRecord = Record<string, unknown>;

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function requiredRecord(value: unknown): JsonRecord {
  if (!isRecord(value)) throw new UsagiClientError("HTTP_ERROR", 200);
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

function safeText(value: string): boolean {
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

function parseSummary(value: unknown): SummaryResponse {
  const record = requiredRecord(value);
  const usage = parseUsage(record.usage);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    usage,
    session_count: requiredSafeInteger(record, "session_count"),
  };
}

function parseProjectOption(value: unknown): ProjectFilterOption {
  const record = requiredRecord(value);
  const kind = requiredString(record, "kind");
  if (kind === "project") return { kind, path: requiredString(record, "path"), name: requiredString(record, "name") };
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

function parseFilterOptions(value: unknown): FilterOptionsResponse {
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

function parseModelDistribution(value: unknown): ModelDistributionResponse {
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

function parseProjectDistribution(value: unknown): ProjectDistributionResponse {
  const record = requiredRecord(value);
  if (!Array.isArray(record.items)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    items: record.items.map((value) => {
      const item = requiredRecord(value);
      const kind = requiredString(item, "kind");
      const usage = parseUsage(item.usage);
      if (kind === "project") return { kind, path: requiredString(item, "path"), name: requiredString(item, "name"), usage };
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
  if (kind === "projectless" || kind === "unknown") return { kind };
  throw new UsagiClientError("HTTP_ERROR", 200);
}

function parseThread(value: unknown): SessionThreadDto {
  const record = requiredRecord(value);
  const type = requiredString(record, "type");
  if (type !== "main" && type !== "subagent") throw new UsagiClientError("HTTP_ERROR", 200);
  if (!Array.isArray(record.model_effort)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    key: parseThreadKey(record.key),
    type,
    parent_thread_id: nullableString(record, "parent_thread_id"),
    title: nullableString(record, "title"),
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

function parseSessionList(value: unknown): SessionListResponse {
  const record = requiredRecord(value);
  if (!Array.isArray(record.items)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    items: record.items.map((value) => {
      const item = requiredRecord(value);
      return {
        thread: parseThread(item.thread),
        self_usage: parseUsage(item.self_usage),
        subagent_usage: parseUsage(item.subagent_usage),
        inclusive_usage: parseUsage(item.inclusive_usage),
        subagent_count: requiredSafeInteger(item, "subagent_count"),
      };
    }),
  };
}

function parseSessionDetail(value: unknown): SessionDetailResponse {
  const record = requiredRecord(value);
  if (!Array.isArray(record.children)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    thread: parseThread(record.thread),
    self_usage: parseUsage(record.self_usage),
    subagent_usage: parseUsage(record.subagent_usage),
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

function parseQuota(value: unknown): QuotaResponse {
  const record = requiredRecord(value);
  if (!Array.isArray(record.items)) throw new UsagiClientError("HTTP_ERROR", 200);
  return {
    reading: requiredBoolean(record, "reading"),
    items: record.items.map(parseQuotaProvider),
  };
}

function parseRevision(value: unknown): RevisionResponse {
  const record = requiredRecord(value);
  return {
    data_revision: requiredSafeInteger(record, "data_revision"),
    status_revision: requiredSafeInteger(record, "status_revision"),
  };
}

function parseStatus(value: unknown): RuntimeStatusResponse {
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

async function parseError(response: Response): Promise<UsagiClientError> {
  let code: ApiErrorCode = "HTTP_ERROR";
  try {
    const body: unknown = await response.json();
    if (isRecord(body)) {
      const candidate = typeof body.error === "string"
        ? body.error as ApiErrorCode
        : isRecord(body.error) && typeof body.error.code === "string"
          ? body.error.code as ApiErrorCode
          : "HTTP_ERROR";
      if (API_ERROR_CODES.has(candidate)) code = candidate;
    }
  } catch {
    // Keep the fixed generic error when a server response is not JSON.
  }
  return new UsagiClientError(code, response.status);
}

async function requestJson<T>(path: string, method: "GET" | "POST", signal?: AbortSignal): Promise<T> {
  let response: Response;
  try {
    response = await fetch(path, {
      method,
      signal,
      credentials: "same-origin",
      headers: {
        Accept: "application/json",
        ...(method === "POST" ? { "X-Usagi-Request": "1" } : {}),
      },
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") throw error;
    throw new UsagiClientError("HTTP_ERROR", 0);
  }
  if (!response.ok) throw await parseError(response);
  try {
    return await response.json() as T;
  } catch {
    throw new UsagiClientError("HTTP_ERROR", response.status);
  }
}

export type UsagiClient = {
  quota(signal?: AbortSignal): Promise<QuotaResponse>;
  refreshQuota(signal?: AbortSignal): Promise<QuotaResponse>;
  filterOptions(range: DashboardRange, signal?: AbortSignal): Promise<FilterOptionsResponse>;
  summary(range: DashboardRange, filters: DashboardFilters, signal?: AbortSignal): Promise<SummaryResponse>;
  modelDistribution(range: DashboardRange, filters: DashboardFilters, signal?: AbortSignal): Promise<ModelDistributionResponse>;
  projectDistribution(range: DashboardRange, filters: DashboardFilters, signal?: AbortSignal): Promise<ProjectDistributionResponse>;
  getSessions(request: { range: DashboardRange; filters: DashboardFilters; signal?: AbortSignal }): Promise<SessionListResponse>;
  getSessionDetail(request: { range: DashboardRange; filters: DashboardFilters; key: ThreadKey; signal?: AbortSignal }): Promise<SessionDetailResponse>;
  getStatus(signal?: AbortSignal): Promise<RuntimeStatusResponse>;
  getRevision(signal?: AbortSignal): Promise<RevisionResponse>;
};

function sortedUnique(values: readonly string[]): string[] {
  for (const value of values) {
    if (value.length === 0 || !safeText(value)) throw new UsagiClientError("INVALID_FILTER", 0);
  }
  return [...new Set(values)].sort();
}

export function modelSelectionKey(selection: ModelSelection): string {
  return JSON.stringify([selection.provider_id, selection.model]);
}

function canonicalModelSelections(models: readonly ModelSelection[]): ModelSelection[] {
  const sorted = models.map(({ provider_id, model }) => {
    if (!provider_id || !model || !safeText(provider_id) || !safeText(model)) {
      throw new UsagiClientError("INVALID_FILTER", 0);
    }
    return { provider_id, model };
  }).sort((left, right) => left.provider_id.localeCompare(right.provider_id) || left.model.localeCompare(right.model));
  return sorted.filter((selection, index) => index === 0 || modelSelectionKey(selection) !== modelSelectionKey(sorted[index - 1]));
}

function canonicalProjectSelections(projects: readonly ProjectSelection[]): ProjectSelection[] {
  const paths = new Set<string>();
  let projectless = false;
  let unknown = false;
  for (const project of projects) {
    if (project.kind === "project") {
      if (!project.path || !safeText(project.path)) throw new UsagiClientError("INVALID_FILTER", 0);
      paths.add(project.path);
    } else if (project.kind === "projectless") projectless = true;
    else if (project.kind === "unknown") unknown = true;
    else throw new UsagiClientError("INVALID_FILTER", 0);
  }
  return [
    ...[...paths].sort().map((path) => ({ kind: "project" as const, path })),
    ...(projectless ? [{ kind: "projectless" as const }] : []),
    ...(unknown ? [{ kind: "unknown" as const }] : []),
  ];
}

export function canonicalDashboardFilters(filters: DashboardFilters): DashboardFilters {
  return {
    agents: sortedUnique(filters.agents ?? []),
    models: canonicalModelSelections(filters.models ?? []),
    projects: canonicalProjectSelections(filters.projects ?? []),
  };
}

export function dashboardRangesEqual(left: DashboardRange, right: DashboardRange): boolean {
  if (left.key !== right.key) return false;
  if (left.key !== "custom" || right.key !== "custom") return true;
  return left.from === right.from && left.to === right.to;
}

export function dashboardQueryKey(range: DashboardRange, filters: DashboardFilters): string {
  const canonical = canonicalDashboardFilters(filters);
  return JSON.stringify([range, canonical.agents, canonical.models, canonical.projects]);
}

export function appendRangeParams(params: URLSearchParams, range: DashboardRange): URLSearchParams {
  params.set("range", range.key);
  if (range.key === "custom") {
    params.set("from", range.from);
    params.set("to", range.to);
  } else {
    params.delete("from");
    params.delete("to");
  }
  return params;
}

function filterParams(range: DashboardRange, filters: DashboardFilters, includeAgents = true): URLSearchParams {
  const canonical = canonicalDashboardFilters(filters);
  const params = appendRangeParams(new URLSearchParams(), range);
  if (includeAgents) for (const agent of canonical.agents) params.append("agent", agent);
  for (const model of canonical.models) params.append("model_at", `${model.provider_id}/${model.model}`);
  for (const project of canonical.projects) {
    if (project.kind === "project") params.append("project_path", project.path);
    if (project.kind === "projectless") params.append("include_projectless", "1");
    if (project.kind === "unknown") params.append("include_unknown_project", "1");
  }
  return params;
}

export function threadKeyId(key: ThreadKey): string {
  return JSON.stringify([key.agent, key.session_id, key.thread_id]);
}

export const usagiClient: UsagiClient = {
  async quota(signal) {
    return parseQuota(await requestJson<unknown>("/api/quota", "GET", signal));
  },
  async refreshQuota(signal) {
    return parseQuota(await requestJson<unknown>("/api/quota/refresh", "POST", signal));
  },
  async filterOptions(range, signal) {
    const params = appendRangeParams(new URLSearchParams(), range);
    return parseFilterOptions(await requestJson<unknown>(`/api/usage/filter-options?${params.toString()}`, "GET", signal));
  },
  async summary(range, filters, signal) {
    const params = filterParams(range, filters);
    const response = parseSummary(await requestJson<unknown>(`/api/usage/summary?${params.toString()}`, "GET", signal));
    return response;
  },
  async modelDistribution(range, filters, signal) {
    const params = filterParams(range, filters);
    return parseModelDistribution(await requestJson<unknown>(`/api/usage/model-distribution?${params.toString()}`, "GET", signal));
  },
  async projectDistribution(range, filters, signal) {
    const params = filterParams(range, filters);
    return parseProjectDistribution(await requestJson<unknown>(`/api/usage/projects?${params.toString()}`, "GET", signal));
  },
  async getSessions({ range, filters, signal }) {
    const params = filterParams(range, filters);
    return parseSessionList(await requestJson<unknown>(`/api/usage/sessions?${params.toString()}`, "GET", signal));
  },
  async getSessionDetail({ range, filters, key, signal }) {
    const params = filterParams(range, filters, false);
    params.set("agent", key.agent);
    params.set("session_id", key.session_id);
    params.set("thread_id", key.thread_id);
    return parseSessionDetail(await requestJson<unknown>(`/api/usage/session-detail?${params.toString()}`, "GET", signal));
  },
  async getStatus(signal) {
    return parseStatus(await requestJson<unknown>("/api/status", "GET", signal));
  },
  async getRevision(signal) {
    return parseRevision(await requestJson<unknown>("/api/revision", "GET", signal));
  },
};
