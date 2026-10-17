import { describe, expect, it, vi } from "vitest";

import { createMcpUsagiClient, type McpApp } from "./mcpUsagiClient";
import type { UsagiClient } from "./usagiClient";
import { UsagiClientError, type DashboardFilters, type DashboardRange, type ThreadKey } from "./types";

const range: DashboardRange = { key: "custom", from: "2026-10-01", to: "2026-10-11" };
const filters: DashboardFilters = {
  agents: ["codex"],
  models: [{ provider_id: "openai", model: "gpt-6" }],
  projects: [{ kind: "workspace" }, { kind: "project", path: "/work/Usagi" }],
};
const key: ThreadKey = { agent: "codex", session_id: "session-1", thread_id: "thread-2" };

function fakeApp(response: unknown) {
  const callServerTool = vi.fn(async (
    _params: { name: string; arguments?: Record<string, unknown> },
    _options?: { signal?: AbortSignal },
  ) => response);
  return { app: { callServerTool } satisfies McpApp, callServerTool };
}

const readMethods: Array<{
  name: string;
  invoke: (client: UsagiClient) => Promise<unknown>;
  args: Record<string, unknown>;
}> = [
  { name: "quota", invoke: (client) => client.quota(), args: { operation: "quota" } },
  { name: "filterOptions", invoke: (client) => client.filterOptions(range), args: { operation: "filter_options", range } },
  { name: "summary", invoke: (client) => client.summary(range, filters), args: { operation: "summary", range, filters } },
  { name: "modelDistribution", invoke: (client) => client.modelDistribution(range, filters), args: { operation: "model_distribution", range, filters } },
  { name: "projectDistribution", invoke: (client) => client.projectDistribution(range, filters), args: { operation: "project_distribution", range, filters } },
  { name: "getSessions", invoke: (client) => client.getSessions({ range, filters }), args: { operation: "sessions", range, filters } },
  { name: "toolUsage", invoke: (client) => client.toolUsage(7), args: { operation: "tool_usage", days: 7 } },
  { name: "getSessionDetail", invoke: (client) => client.getSessionDetail({ range, filters, key }), args: { operation: "session_detail", range, filters, key } },
  { name: "getStatus", invoke: (client) => client.getStatus(), args: { operation: "status" } },
  { name: "getRevision", invoke: (client) => client.getRevision(), args: { operation: "revision" } },
];

describe("McpUsagiClient", () => {
  it.each(readMethods)("maps $name to its read operation and structured arguments", async ({ invoke, args }) => {
    const { app, callServerTool } = fakeApp({ structuredContent: { status: "not_running", operation: "read" } });
    const onRuntimeUnavailable = vi.fn();
    const client = createMcpUsagiClient(app, onRuntimeUnavailable);

    await expect(invoke(client)).rejects.toMatchObject({ code: "HTTP_ERROR", status: 0 });
    expect(callServerTool).toHaveBeenCalledTimes(1);
    expect(callServerTool).toHaveBeenCalledWith({ name: "usagi.dashboard.read", arguments: args });
    expect(onRuntimeUnavailable).toHaveBeenCalledTimes(1);
  });

  it("uses the refresh tool and parses its ready payload", async () => {
    const quota = { reading: false, items: [] };
    const { app, callServerTool } = fakeApp({ structuredContent: { status: "ready", operation: "refresh_quota", payload: quota } });
    const client = createMcpUsagiClient(app, vi.fn());

    await expect(client.refreshQuota()).resolves.toEqual(quota);
    expect(callServerTool).toHaveBeenCalledTimes(1);
    expect(callServerTool).toHaveBeenCalledWith({ name: "usagi.dashboard.refresh_quota", arguments: {} });
  });

  it("passes cancellation through the official call options shape", async () => {
    const { app, callServerTool } = fakeApp({
      structuredContent: { status: "ready", operation: "revision", payload: { data_revision: 1, status_revision: 2 } },
    });
    const client = createMcpUsagiClient(app, vi.fn());
    const controller = new AbortController();

    await client.getRevision(controller.signal);
    expect(callServerTool).toHaveBeenCalledWith(
      { name: "usagi.dashboard.read", arguments: { operation: "revision" } },
      { signal: controller.signal },
    );
  });

  it("parses a ready payload through the shared response parser", async () => {
    const summary = {
      data_revision: 8,
      usage: { calls: 0, errors: 0, input: 0, output: 0, cache_read: 0, cache_write: 0, cache_write_1h: 0, reasoning: 0, total: 0, cache_hit_rate: null, cost: 0, unpriced: 0, retried: 0, output_speed: null, speed_samples: 0 },
      request_metrics: { calls: 0, errors: 0, retried: 0, output_speed: null, speed_samples: 0, mean_ttft_seconds: null, ttft_samples: 0 },
      session_count: 0,
      session_median: null,
      session_p90: null,
    };
    const { app } = fakeApp({ structuredContent: { status: "ready", operation: "summary", payload: summary } });
    const client = createMcpUsagiClient(app, vi.fn());

    await expect(client.summary({ key: "today" }, { agents: [], models: [], projects: [] })).resolves.toEqual(summary);
  });

  it("rejects malformed ready payloads through the shared parser", async () => {
    const { app } = fakeApp({ structuredContent: { status: "ready", operation: "summary", payload: {} } });
    const client = createMcpUsagiClient(app, vi.fn());

    await expect(client.summary({ key: "today" }, { agents: [], models: [], projects: [] }))
      .rejects.toMatchObject({ code: "HTTP_ERROR", status: 200 });
  });

  it.each([
    ["DATA_LOADING", "DATA_LOADING"],
    ["MAGPIE_UNAVAILABLE", "MAGPIE_UNAVAILABLE"],
    ["UNKNOWN_UPSTREAM_CODE", "HTTP_ERROR"],
    [undefined, "HTTP_ERROR"],
  ] as const)("maps upstream error %s and preserves HTTP status", async (errorCode, expectedCode) => {
    const { app } = fakeApp({
      structuredContent: {
        status: "upstream_error",
        operation: "quota",
        http_status: 503,
        payload: errorCode === undefined ? {} : { error: errorCode },
      },
    });
    const client = createMcpUsagiClient(app, vi.fn());

    await expect(client.quota()).rejects.toMatchObject({ code: expectedCode, status: 503 });
  });

  it("rejects results without structuredContent instead of parsing content text", async () => {
    const { app } = fakeApp({ content: [{ type: "text", text: '{"status":"ready","payload":{}}' }] });
    const client = createMcpUsagiClient(app, vi.fn());

    await expect(client.quota()).rejects.toBeInstanceOf(UsagiClientError);
    await expect(client.quota()).rejects.toMatchObject({ code: "HTTP_ERROR", status: 0 });
  });
});
