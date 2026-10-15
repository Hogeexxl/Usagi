import { afterEach, describe, expect, it, vi } from "vitest";

import { appendRangeParams, canonicalDashboardFilters, dashboardQueryKey, modelSelectionKey, threadKeyId, usagiClient } from "./usagiClient";
import { UsagiClientError, type DashboardFilters, type RequestMetricsDto, type ThreadKey, type UsageDto } from "./types";

const emptyFilters: DashboardFilters = { agents: [], models: [], projects: [] };
const usage: UsageDto = {
  calls: 2,
  errors: 1,
  input: 10,
  output: 20,
  cache_read: 4,
  cache_write: 2,
  cache_write_1h: 1,
  reasoning: 5,
  total: 36,
  cache_hit_rate: 0.25,
  cost: 1.25,
  unpriced: 1,
  retried: 0, output_speed: null, speed_samples: 0,
};
const zeroUsage: UsageDto = {
  ...usage,
  calls: 0,
  errors: 0,
  input: 0,
  output: 0,
  cache_read: 0,
  cache_write: 0,
  cache_write_1h: 0,
  reasoning: 0,
  total: 0,
  cache_hit_rate: null,
  cost: 0,
  unpriced: 0,
};
const requestMetrics: RequestMetricsDto = {
  calls: 2,
  errors: 1,
  retried: 0,
  output_speed: null,
  speed_samples: 0,
  mean_ttft_seconds: null,
  ttft_samples: 0,
};

const thread = {
  key: { agent: "claude", session_id: "session-1", thread_id: "thread-1" },
  type: "main" as const,
  parent_thread_id: null,
  title: "Session one",
  nickname: "",
  project: { kind: "project" as const, path: "/work/Usagi", name: "Usagi" },
  first_usage_ms: 100,
  last_usage_ms: 200,
  model_effort: [{ model: "gpt-6", effort: "high", usage, compaction_tokens: 5 }],
  usage,
};

function mockJson(body: unknown, status = 200) {
  const fetchMock = vi.fn((..._args: Parameters<typeof fetch>) => Promise.resolve(
    new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } }),
  ));
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

afterEach(() => vi.unstubAllGlobals());

describe("Usagi Spec04 HTTP client", () => {
  it("uses unified quota GET and explicit quota refresh POST", async () => {
    const quota = {
      reading: false,
      items: [{
        provider: "codex",
        name: "Codex",
        plan: "Pro",
        user: "user@example.test",
        windows: [{ unlimited: false, name: "5H", used: 0.2, resets_at_ms: null, reset_secs: 0, display: "", amount: 2, limit: 10, unit: "calls", family: "usage", pool: "primary" }],
        balance: "",
        error: "",
        as_of_ms: 100,
        read_at_ms: 101,
      }],
    };
    const fetchMock = mockJson(quota);

    await expect(usagiClient.quota()).resolves.toEqual(quota);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/quota");
    expect(fetchMock.mock.calls[0][1]).toMatchObject({ method: "GET", credentials: "same-origin" });

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(quota), { status: 200 }));
    await usagiClient.refreshQuota();
    expect(fetchMock.mock.calls[1][0]).toBe("/api/quota/refresh");
    expect(fetchMock.mock.calls[1][1]).toMatchObject({ method: "POST", headers: { "X-Usagi-Request": "1" } });
  });

  it("parses SummaryResponse request_metrics as an independent required DTO", async () => {
    const fetchMock = mockJson({ data_revision: 8, usage, request_metrics: requestMetrics, session_count: 3, session_median: null, session_p90: 24 });
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).resolves.toEqual({
      data_revision: 8,
      usage,
      request_metrics: requestMetrics,
      session_count: 3,
      session_median: null,
      session_p90: 24,
    });
    const url = new URL(String(fetchMock.mock.calls[0][0]), "http://localhost");
    expect(url.pathname).toBe("/api/usage/summary");
    expect(url.searchParams.get("range")).toBe("today");
    expect(url.searchParams.has("session_count")).toBe(false);
    expect(url.searchParams.has("source")).toBe(false);
  });

  it("rejects missing or malformed request_metrics in a summary", async () => {
    const fetchMock = mockJson({ data_revision: 8, usage, session_count: 3, session_median: null, session_p90: 24 });
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).rejects.toMatchObject({ code: "HTTP_ERROR", status: 200 });

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({
      data_revision: 8,
      usage,
      request_metrics: { ...requestMetrics, ttft_samples: -1 },
      session_count: 3,
      session_median: null,
      session_p90: 24,
    }), { status: 200 }));
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).rejects.toMatchObject({ code: "HTTP_ERROR", status: 200 });
  });

  it("uses dynamic agents and provider/model pairs in filter options, query keys, and wire filters", async () => {
    const options = {
      data_revision: 8,
      agents: [{ agent: "aider", display_name: "Aider" }],
      models: [{ model: "same-model", provider_id: "provider-a", provider_name: "Provider A" }, { model: "same-model", provider_id: "provider-b", provider_name: "Provider B" }],
      projects: [
        { kind: "project", path: "/work/Usagi", name: "Usagi" },
        { kind: "workspace" },
      ],
    };
    const fetchMock = mockJson(options);
    await expect(usagiClient.filterOptions({ key: "custom", from: "2026-10-01", to: "2026-10-11" })).resolves.toEqual(options);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/usage/filter-options?range=custom&from=2026-10-01&to=2026-10-11");

    const filters: DashboardFilters = {
      agents: ["aider", "aider"],
      models: [
        { provider_id: "provider-b", model: "same-model" },
        { provider_id: "provider-a", model: "same-model" },
        { provider_id: "provider-a", model: "same-model" },
      ],
      projects: [{ kind: "unknown" }, { kind: "workspace" }, { kind: "projectless" }, { kind: "project", path: "/work/Usagi" }],
    };
    expect(canonicalDashboardFilters(filters)).toEqual({
      agents: ["aider"],
      models: [{ provider_id: "provider-a", model: "same-model" }, { provider_id: "provider-b", model: "same-model" }],
      projects: [{ kind: "project", path: "/work/Usagi" }, { kind: "workspace" }, { kind: "projectless" }, { kind: "unknown" }],
    });
    expect(modelSelectionKey({ provider_id: "provider-a", model: "same-model" })).not.toBe(modelSelectionKey({ provider_id: "provider-b", model: "same-model" }));
    expect(dashboardQueryKey({ key: "today" }, filters)).not.toBe(dashboardQueryKey({ key: "today" }, {
      ...filters,
      models: [{ provider_id: "provider-b", model: "same-model" }],
    }));

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ data_revision: 8, usage, request_metrics: requestMetrics, session_count: 3, session_median: null, session_p90: 24 }), { status: 200 }));
    await usagiClient.summary({ key: "custom", from: "2026-09-01", to: "2026-09-03" }, filters);
    const url = new URL(String(fetchMock.mock.calls[1][0]), "http://localhost");
    expect(url.pathname).toBe("/api/usage/summary");
    expect(url.searchParams.getAll("agent")).toEqual(["aider"]);
    expect(url.searchParams.getAll("model_at")).toEqual(["provider-a/same-model", "provider-b/same-model"]);
    expect(url.searchParams.get("project_path")).toBe("/work/Usagi");
    expect(url.searchParams.get("include_workspace")).toBe("1");
    expect(url.searchParams.get("include_projectless")).toBe("1");
    expect(url.searchParams.get("include_unknown_project")).toBe("1");
    expect(url.searchParams.get("from")).toBe("2026-09-01");
    expect(url.searchParams.get("to")).toBe("2026-09-03");
  });

  it("requests model and project distribution from Usage endpoints", async () => {
    const fetchMock = mockJson({ data_revision: 8, items: [{ model: "gpt-6", usage }] });
    await expect(usagiClient.modelDistribution({ key: "7d" }, emptyFilters)).resolves.toEqual({ data_revision: 8, items: [{ model: "gpt-6", usage }] });
    expect(String(fetchMock.mock.calls[0][0])).toContain("/api/usage/model-distribution?");

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ data_revision: 8, items: [{ kind: "project", path: "/work/Usagi", name: "Usagi", usage }, { kind: "workspace", usage }] }), { status: 200 }));
    await expect(usagiClient.projectDistribution({ key: "7d" }, emptyFilters)).resolves.toMatchObject({ items: [{ kind: "project", path: "/work/Usagi", name: "Usagi" }, { kind: "workspace" }] });
    expect(String(fetchMock.mock.calls[1][0])).toContain("/api/usage/projects?");
  });

  it("loads SessionListItemView and sends the full ThreadKey for on-demand Detail", async () => {
    const workspaceThread = { ...thread, project: { kind: "workspace" as const, path: "/private/tmp/claude-501/child" } };
    const listResponse = {
      data_revision: 8,
      summary: { data_revision: 8, usage, request_metrics: requestMetrics, session_count: 1, session_median: null, session_p90: null },
      items: [{ thread: workspaceThread, self_usage: usage, subagent_usage: { ...usage, total: 1 }, unknown_usage: zeroUsage, inclusive_usage: usage, has_usage_rows: false, last_activity_ms: null, subagent_count: null }],
    };
    const fetchMock = mockJson(listResponse);
    await expect(usagiClient.getSessions({ range: { key: "today" }, filters: emptyFilters })).resolves.toEqual(listResponse);
    expect(listResponse.items[0].thread.project).toEqual({ kind: "workspace", path: "/private/tmp/claude-501/child" });
    expect(String(fetchMock.mock.calls[0][0])).toContain("/api/usage/sessions?");
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({
      ...listResponse,
      items: [{ ...listResponse.items[0], last_activity_ms: "not-a-number" }],
    }), { status: 200 }));
    await expect(usagiClient.getSessions({ range: { key: "today" }, filters: emptyFilters })).rejects.toMatchObject({ code: "HTTP_ERROR", status: 200 });

    const threadKey: ThreadKey = { agent: "claude", session_id: "session-1", thread_id: "subagent-1" };
    const child = { ...thread, key: threadKey, type: "subagent" as const, parent_thread_id: "thread-1" };
    const detailResponse = {
      data_revision: 8,
      last_activity_ms: null,
      thread: workspaceThread,
      self_usage: usage,
      subagent_usage: usage,
      unknown_usage: zeroUsage,
      inclusive_usage: usage,
      children: [child],
    };
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(detailResponse), { status: 200 }));
    await expect(usagiClient.getSessionDetail({ range: { key: "today" }, filters: emptyFilters, key: threadKey })).resolves.toEqual(detailResponse);
    const url = new URL(String(fetchMock.mock.calls[2][0]), "http://localhost");
    expect(url.pathname).toBe("/api/usage/session-detail");
    expect(url.searchParams.get("agent")).toBe("claude");
    expect(url.searchParams.get("session_id")).toBe("session-1");
    expect(url.searchParams.get("thread_id")).toBe("subagent-1");
    expect(threadKeyId(threadKey)).toBe(JSON.stringify(["claude", "session-1", "subagent-1"]));
    expect(detailResponse.children).toEqual([child]);
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ ...detailResponse, last_activity_ms: "not-a-number" }), { status: 200 }));
    await expect(usagiClient.getSessionDetail({ range: { key: "today" }, filters: emptyFilters, key: threadKey })).rejects.toMatchObject({ code: "HTTP_ERROR", status: 200 });
    const { last_activity_ms: _lastActivity, ...missingActivity } = detailResponse;
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(missingActivity), { status: 200 }));
    await expect(usagiClient.getSessionDetail({ range: { key: "today" }, filters: emptyFilters, key: threadKey })).rejects.toMatchObject({ code: "HTTP_ERROR", status: 200 });
  });

  it("parses Codex main and child identities alongside unknown thread usage", async () => {
    const mainId = "01a135e4-9b71-7fa1-8bb0-2dd2bdaf5497";
    const childId = "01a138ab-909f-7990-a365-9555077faea2";
    const mainThread = {
      ...thread,
      key: { agent: "codex", session_id: mainId, thread_id: mainId },
      type: "main" as const,
      parent_thread_id: null,
    };
    const childThread = {
      ...thread,
      key: { agent: "codex", session_id: mainId, thread_id: childId },
      type: "subagent" as const,
      parent_thread_id: mainId,
      title: "Codex child",
      nickname: "Child nickname",
    };
    const unknownThread = {
      ...thread,
      key: { agent: "codex", session_id: mainId, thread_id: "unknown-calls" },
      type: "unknown" as const,
      parent_thread_id: null,
      model_effort: [],
      usage: zeroUsage,
    };
    const unknownUsage = { ...zeroUsage, calls: 1, input: 4, total: 4, cost: 0.02 };
    const mainItem = {
      thread: mainThread,
      self_usage: usage,
      subagent_usage: usage,
      unknown_usage: zeroUsage,
      inclusive_usage: usage,
      has_usage_rows: true,
      last_activity_ms: 200,
      subagent_count: 1,
    };
    const unknownItem = {
      thread: unknownThread,
      self_usage: zeroUsage,
      subagent_usage: zeroUsage,
      unknown_usage: unknownUsage,
      inclusive_usage: unknownUsage,
      has_usage_rows: true,
      last_activity_ms: 200,
      subagent_count: null,
    };
    const summary = { data_revision: 8, usage, request_metrics: requestMetrics, session_count: 2, session_median: null, session_p90: null };
    const fetchMock = mockJson({ data_revision: 8, summary, items: [mainItem, unknownItem] });

    const sessions = await usagiClient.getSessions({ range: { key: "today" }, filters: emptyFilters });
    expect(sessions.items.map((item) => item.thread.type)).toEqual(["main", "unknown"]);
    expect(sessions.items[1].unknown_usage).toEqual(unknownUsage);

    const mainDetail = {
      data_revision: 8,
      last_activity_ms: 200,
      thread: mainThread,
      self_usage: usage,
      subagent_usage: usage,
      unknown_usage: zeroUsage,
      inclusive_usage: usage,
      children: [childThread],
    };
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(mainDetail), { status: 200 }));
    const parsedMainDetail = await usagiClient.getSessionDetail({
      range: { key: "today" }, filters: emptyFilters, key: mainThread.key,
    });
    expect(parsedMainDetail.children).toEqual([childThread]);
    expect(parsedMainDetail.last_activity_ms).toBe(200);
    expect(parsedMainDetail.children[0].parent_thread_id).toBe(mainId);
    expect(parsedMainDetail.children[0].model_effort[0].usage).toEqual(usage);

    const unknownDetail = {
      data_revision: 8,
      last_activity_ms: null,
      thread: unknownThread,
      self_usage: zeroUsage,
      subagent_usage: zeroUsage,
      unknown_usage: unknownUsage,
      inclusive_usage: unknownUsage,
      children: [],
    };
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(unknownDetail), { status: 200 }));
    const parsedUnknownDetail = await usagiClient.getSessionDetail({
      range: { key: "today" }, filters: emptyFilters, key: unknownThread.key,
    });
    expect(parsedUnknownDetail.thread.type).toBe("unknown");
    expect(parsedUnknownDetail.unknown_usage).toEqual(unknownUsage);
  });

  it("loads Tool usage by days and parses the original top order", async () => {
    const toolUsage = { days: 7, calls: 12, top: [{ name: "Read", calls: 8 }, { name: "Edit", calls: 4 }] };
    const fetchMock = mockJson(toolUsage);
    await expect(usagiClient.toolUsage(7)).resolves.toEqual(toolUsage);
    expect(String(fetchMock.mock.calls[0][0])).toBe("/api/usage/tools?days=7");
  });

  it("uses the internal Status and Revision routes and accepts only the new status fields", async () => {
    const fetchMock = mockJson({ status_revision: 3, runtime_state: "starting", last_error_code: null });
    await expect(usagiClient.getStatus()).resolves.toEqual({ status_revision: 3, runtime_state: "starting", last_error_code: null });
    expect(fetchMock.mock.calls[0][0]).toBe("/api/status");
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ data_revision: 8, status_revision: 3 }), { status: 200 }));
    await expect(usagiClient.getRevision()).resolves.toEqual({ data_revision: 8, status_revision: 3 });
    expect(fetchMock.mock.calls[1][0]).toBe("/api/revision");
  });

  it("rejects malformed Usage DTO data and preserves structured API errors", async () => {
    mockJson({ data_revision: 8, usage: { ...usage, total: -1 }, request_metrics: requestMetrics, session_count: 3, session_median: null, session_p90: null });
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).rejects.toMatchObject({ code: "HTTP_ERROR", status: 200 });

    mockJson({ error: { code: "DATA_LOADING" } }, 503);
    await expect(usagiClient.getSessions({ range: { key: "today" }, filters: emptyFilters })).rejects.toMatchObject({ code: "DATA_LOADING", status: 503 });

    mockJson({ error: { code: "UPSTREAM_FAILED" } }, 503);
    await expect(usagiClient.getRevision()).rejects.toBeInstanceOf(UsagiClientError);
    await expect(usagiClient.getRevision()).rejects.toMatchObject({ code: "UPSTREAM_FAILED", status: 503 });
  });

  it("does not mutate range query parameters when switching to a preset", () => {
    const params = new URLSearchParams("from=old&to=old&agent=agent-a");
    appendRangeParams(params, { key: "today" });
    expect(params.get("range")).toBe("today");
    expect(params.has("from")).toBe(false);
    expect(params.has("to")).toBe(false);
    expect(params.get("agent")).toBe("agent-a");
  });
});
