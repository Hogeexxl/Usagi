import {
  UsagiClientError,
  type ApiErrorCode,
  type DashboardFilters,
  type DashboardRange,
  type FilterOptionsResponse,
  type ModelDistributionResponse,
  type ModelSelection,
  type ProjectDistributionResponse,
  type ProjectSelection,
  type QuotaResponse,
  type RevisionResponse,
  type RuntimeStatusResponse,
  type SessionDetailResponse,
  type SessionListResponse,
  type SummaryResponse,
  type ThreadKey,
  type ToolUsageResponse,
} from "./types";
import {
  isJsonRecord,
  parseApiErrorCode,
  parseFilterOptions,
  parseModelDistribution,
  parseProjectDistribution,
  parseQuota,
  parseRevision,
  parseSessionDetail,
  parseSessionList,
  parseStatus,
  parseSummary,
  parseToolUsage,
  safeText,
} from "./usagiParsers";

async function parseError(response: Response): Promise<UsagiClientError> {
  let code: ApiErrorCode = "HTTP_ERROR";
  try {
    const body: unknown = await response.json();
    if (isJsonRecord(body)) {
      const candidate = typeof body.error === "string"
        ? body.error
        : isJsonRecord(body.error) && typeof body.error.code === "string"
          ? body.error.code
          : undefined;
      code = parseApiErrorCode(candidate) ?? code;
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
  toolUsage(days: 1 | 7 | 30, signal?: AbortSignal): Promise<ToolUsageResponse>;
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
  let workspace = false;
  for (const project of projects) {
    if (project.kind === "project") {
      if (!project.path || !safeText(project.path)) throw new UsagiClientError("INVALID_FILTER", 0);
      paths.add(project.path);
    } else if (project.kind === "workspace") workspace = true;
    else if (project.kind === "projectless") projectless = true;
    else if (project.kind === "unknown") unknown = true;
    else throw new UsagiClientError("INVALID_FILTER", 0);
  }
  return [
    ...[...paths].sort().map((path) => ({ kind: "project" as const, path })),
    ...(workspace ? [{ kind: "workspace" as const }] : []),
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
    if (project.kind === "workspace") params.append("include_workspace", "1");
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
  async toolUsage(days, signal) {
    const params = new URLSearchParams({ days: String(days) });
    return parseToolUsage(await requestJson<unknown>(`/api/usage/tools?${params.toString()}`, "GET", signal));
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
