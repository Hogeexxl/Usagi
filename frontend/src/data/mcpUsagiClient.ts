import {
  UsagiClientError,
  type DashboardFilters,
  type DashboardRange,
  type RevisionResponse,
  type ThreadKey,
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
} from "./usagiParsers";
import type { UsagiClient } from "./usagiClient";

export type McpApp = {
  callServerTool(
    params: { name: string; arguments?: Record<string, unknown> },
    options?: { signal?: AbortSignal },
  ): Promise<unknown>;
};

export type McpToolPayload =
  | { status: "ready"; payload: unknown }
  | { status: "not_running" };

export async function callMcpTool(
  app: McpApp,
  toolName: string,
  args: Record<string, unknown>,
  onRuntimeUnavailable: () => void,
  signal?: AbortSignal,
): Promise<McpToolPayload> {
  const params = { name: toolName, arguments: args };
  const result = signal
    ? await app.callServerTool(params, { signal })
    : await app.callServerTool(params);
  const envelope = isJsonRecord(result) && isJsonRecord(result.structuredContent)
    ? result.structuredContent
    : null;
  if (!envelope) throw new UsagiClientError("HTTP_ERROR", 0);

  if (envelope.status === "ready") return { status: "ready", payload: envelope.payload };
  if (envelope.status === "not_running") {
    onRuntimeUnavailable();
    return { status: "not_running" };
  }
  if (envelope.status === "upstream_error") {
    const status = typeof envelope.http_status === "number" && Number.isSafeInteger(envelope.http_status) && envelope.http_status >= 0
      ? envelope.http_status
      : 0;
    const payload = isJsonRecord(envelope.payload) ? envelope.payload : null;
    throw new UsagiClientError(parseApiErrorCode(payload?.error) ?? "HTTP_ERROR", status);
  }
  throw new UsagiClientError("HTTP_ERROR", 0);
}

export function createMcpUsagiClient(app: McpApp, onRuntimeUnavailable: () => void): UsagiClient {
  async function read<T>(operation: string, args: Record<string, unknown>, parse: (payload: unknown) => T, signal?: AbortSignal): Promise<T> {
    const result = await callMcpTool(app, "usagi.dashboard.read", { operation, ...args }, onRuntimeUnavailable, signal);
    if (result.status === "not_running") throw new UsagiClientError("HTTP_ERROR", 0);
    return parse(result.payload);
  }

  return {
    async quota(signal?: AbortSignal) {
      return read("quota", {}, parseQuota, signal);
    },
    async refreshQuota(signal?: AbortSignal) {
      const result = await callMcpTool(app, "usagi.dashboard.refresh_quota", {}, onRuntimeUnavailable, signal);
      if (result.status === "not_running") throw new UsagiClientError("HTTP_ERROR", 0);
      return parseQuota(result.payload);
    },
    async filterOptions(range: DashboardRange, signal?: AbortSignal) {
      return read("filter_options", { range }, parseFilterOptions, signal);
    },
    async summary(range: DashboardRange, filters: DashboardFilters, signal?: AbortSignal) {
      return read("summary", { range, filters }, parseSummary, signal);
    },
    async modelDistribution(range: DashboardRange, filters: DashboardFilters, signal?: AbortSignal) {
      return read("model_distribution", { range, filters }, parseModelDistribution, signal);
    },
    async projectDistribution(range: DashboardRange, filters: DashboardFilters, signal?: AbortSignal) {
      return read("project_distribution", { range, filters }, parseProjectDistribution, signal);
    },
    async getSessions(request: { range: DashboardRange; filters: DashboardFilters; signal?: AbortSignal }) {
      return read("sessions", { range: request.range, filters: request.filters }, parseSessionList, request.signal);
    },
    async toolUsage(days: 1 | 7 | 30, signal?: AbortSignal) {
      return read("tool_usage", { days }, parseToolUsage, signal);
    },
    async getSessionDetail(request: { range: DashboardRange; filters: DashboardFilters; key: ThreadKey; signal?: AbortSignal }) {
      return read("session_detail", { range: request.range, filters: request.filters, key: request.key }, parseSessionDetail, request.signal);
    },
    async getStatus(signal?: AbortSignal) {
      return read("status", {}, parseStatus, signal);
    },
    async getRevision(signal?: AbortSignal): Promise<RevisionResponse> {
      return read("revision", {}, parseRevision, signal);
    },
  };
}
