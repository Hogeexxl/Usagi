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
  retried: 0,
  output_speed: null,
  speed_samples: 0,
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
const summary = { data_revision: 8, usage, request_metrics: requestMetrics, session_count: 3, session_median: null, session_p90: 24 };
const thread = {
  key: { agent: "claude", session_id: "session-1", thread_id: "thread-1" },
  type: "main",
  parent_thread_id: null,
  title: "Session one",
  nickname: "",
  project: { kind: "project", path: "/work/Usagi", name: "Usagi" },
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

describe("Browser Usagi HTTP transport", () => {
  it("uses relative quota routes, same-origin credentials, and the protected refresh POST", async () => {
    const quota = { reading: false, items: [] };
    const fetchMock = mockJson(quota);

    await usagiClient.quota();
    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/quota", expect.objectContaining({ method: "GET", credentials: "same-origin" }));

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(quota), { status: 200 }));
    await usagiClient.refreshQuota();
    expect(fetchMock).toHaveBeenNthCalledWith(2, "/api/quota/refresh", expect.objectContaining({
      method: "POST",
      credentials: "same-origin",
      headers: { Accept: "application/json", "X-Usagi-Request": "1" },
    }));
  });

  it("keeps range and filter query construction on the Browser transport", async () => {
    const options = { data_revision: 8, agents: [], models: [], projects: [] };
    const fetchMock = mockJson(options);
    await usagiClient.filterOptions({ key: "custom", from: "2026-10-01", to: "2026-10-11" });
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

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(summary), { status: 200 }));
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

  it("uses the existing distribution routes", async () => {
    const fetchMock = mockJson({ data_revision: 8, items: [{ model: "gpt-6", usage }] });
    await usagiClient.modelDistribution({ key: "7d" }, emptyFilters);
    expect(String(fetchMock.mock.calls[0][0])).toContain("/api/usage/model-distribution?");

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ data_revision: 8, items: [{ kind: "workspace", usage }] }), { status: 200 }));
    await usagiClient.projectDistribution({ key: "7d" }, emptyFilters);
    expect(String(fetchMock.mock.calls[1][0])).toContain("/api/usage/projects?");
  });

  it("keeps session list and detail paths and the full ThreadKey query", async () => {
    const workspaceThread = { ...thread, project: { kind: "workspace", path: "/private/tmp/workspace" } };
    const listResponse = {
      data_revision: 8,
      summary,
      items: [{
        thread: workspaceThread,
        self_usage: usage,
        subagent_usage: usage,
        unknown_usage: usage,
        inclusive_usage: usage,
        has_usage_rows: false,
        last_activity_ms: null,
        subagent_count: null,
      }],
    };
    const fetchMock = mockJson(listResponse);
    await usagiClient.getSessions({ range: { key: "today" }, filters: emptyFilters });
    expect(String(fetchMock.mock.calls[0][0])).toContain("/api/usage/sessions?");

    const threadKey: ThreadKey = { agent: "claude", session_id: "session-1", thread_id: "subagent-1" };
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({
      data_revision: 8,
      last_activity_ms: null,
      thread: workspaceThread,
      self_usage: usage,
      subagent_usage: usage,
      unknown_usage: usage,
      inclusive_usage: usage,
      children: [{ ...thread, key: threadKey, type: "subagent", parent_thread_id: "thread-1" }],
    }), { status: 200 }));
    await usagiClient.getSessionDetail({ range: { key: "today" }, filters: emptyFilters, key: threadKey });
    const url = new URL(String(fetchMock.mock.calls[1][0]), "http://localhost");
    expect(url.pathname).toBe("/api/usage/session-detail");
    expect(url.searchParams.get("agent")).toBe("claude");
    expect(url.searchParams.get("session_id")).toBe("session-1");
    expect(url.searchParams.get("thread_id")).toBe("subagent-1");
    expect(threadKeyId(threadKey)).toBe(JSON.stringify(["claude", "session-1", "subagent-1"]));
  });

  it("keeps tool usage, status, and revision on the Browser API routes", async () => {
    const fetchMock = mockJson({ days: 7, calls: 12, top: [{ name: "Read", calls: 8 }] });
    await usagiClient.toolUsage(7);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/usage/tools?days=7");

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ status_revision: 3, runtime_state: "starting", last_error_code: null }), { status: 200 }));
    await usagiClient.getStatus();
    expect(fetchMock.mock.calls[1][0]).toBe("/api/status");
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ data_revision: 8, status_revision: 3 }), { status: 200 }));
    await usagiClient.getRevision();
    expect(fetchMock.mock.calls[2][0]).toBe("/api/revision");
  });

  it("preserves API error codes and falls back to HTTP_ERROR for unknown or unreadable errors", async () => {
    const fetchMock = mockJson({ error: { code: "DATA_LOADING" } }, 503);
    await expect(usagiClient.getSessions({ range: { key: "today" }, filters: emptyFilters }))
      .rejects.toMatchObject({ code: "DATA_LOADING", status: 503 });

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ error: { code: "UNKNOWN_ERROR" } }), { status: 502 }));
    await expect(usagiClient.getRevision()).rejects.toMatchObject({ code: "HTTP_ERROR", status: 502 });

    fetchMock.mockResolvedValueOnce(new Response("not json", { status: 502 }));
    const unreadableError = usagiClient.getRevision();
    await expect(unreadableError).rejects.toBeInstanceOf(UsagiClientError);
    await expect(unreadableError).rejects.toMatchObject({ code: "HTTP_ERROR", status: 502 });

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ error: { code: "UPSTREAM_FAILED" } }), { status: 503 }));
    const upstreamFailure = usagiClient.getRevision();
    await expect(upstreamFailure).rejects.toBeInstanceOf(UsagiClientError);
    await expect(upstreamFailure).rejects.toMatchObject({ code: "UPSTREAM_FAILED", status: 503 });
  });

  it("maps fetch failure to status zero and keeps range parameter updates local", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("offline")));
    await expect(usagiClient.getRevision()).rejects.toMatchObject({ code: "HTTP_ERROR", status: 0 });

    const params = new URLSearchParams("from=old&to=old&agent=agent-a");
    appendRangeParams(params, { key: "today" });
    expect(params.get("range")).toBe("today");
    expect(params.has("from")).toBe(false);
    expect(params.has("to")).toBe(false);
    expect(params.get("agent")).toBe("agent-a");
  });
});
