import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { App } from "@modelcontextprotocol/ext-apps";
import { describe, expect, it, vi } from "vitest";

import { usagiClient, type UsagiClient } from "../data/usagiClient";
import * as revisionFeedModule from "../data/revisionFeed";
import { serviceClient } from "../data/serviceClient";
import type {
  DashboardFilters,
  ModelDistributionResponse,
  ProjectDistributionResponse,
  RuntimeStatusResponse,
  SessionDetailResponse,
  SessionListItemDto,
  SessionListResponse,
  SummaryResponse,
  RequestMetricsDto,
  UsageDto,
} from "../data/types";
import { UsagiClientError } from "../data/types";
import { ThemeProvider } from "../theme/ThemeProvider";
import { DashboardPage } from "./DashboardPage";

vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count, estimateSize }: { count: number; estimateSize: () => number }) => {
    const size = estimateSize();
    return {
      getVirtualItems: () => Array.from({ length: count }, (_, index) => ({
        index,
        key: index,
        start: index * size,
        size,
        end: (index + 1) * size,
        lane: null,
      })),
      getTotalSize: () => count * size,
    };
  },
}));

const emptyFilters: DashboardFilters = { agents: [], models: [], projects: [] };
const usage: UsageDto = {
  calls: 2,
  errors: 0,
  input: 10,
  output: 4,
  cache_read: 3,
  cache_write: 1,
  cache_write_1h: 0,
  reasoning: 2,
  total: 18,
  cache_hit_rate: 0.3,
  cost: 0.25,
  unpriced: 0,
  retried: 0,
  output_speed: null,
  speed_samples: 0,
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
  calls: 20,
  errors: 2,
  retried: 4,
  output_speed: 62.4,
  speed_samples: 3,
  mean_ttft_seconds: 7.6,
  ttft_samples: 2,
};
const summary: SummaryResponse = { data_revision: 1, usage, request_metrics: requestMetrics, session_count: 1, session_median: 14, session_p90: 14 };
const row: SessionListItemDto = {
  thread: {
    key: { agent: "claude", session_id: "session-1", thread_id: "thread-1" },
    type: "main",
    parent_thread_id: null,
    title: "Session Alpha",
    nickname: "",
    project: { kind: "project", path: "/work/Usagi", name: "Usagi" },
    first_usage_ms: 1_800_000_000_000,
    last_usage_ms: 1_800_000_000_100,
    model_effort: [{ model: "gpt-6", effort: "high", usage, compaction_tokens: 0 }],
    usage,
  },
  self_usage: usage,
  subagent_usage: { ...usage, total: 0 },
  unknown_usage: zeroUsage,
  inclusive_usage: usage,
  has_usage_rows: true,
  last_activity_ms: 1_800_000_000_100,
  subagent_count: 0,
};
const sessions: SessionListResponse = { data_revision: 1, summary, items: [row] };
const detail: SessionDetailResponse = {
  data_revision: 1,
  last_activity_ms: row.last_activity_ms,
  thread: row.thread,
  self_usage: usage,
  subagent_usage: { ...usage, total: 0 },
  unknown_usage: zeroUsage,
  inclusive_usage: usage,
  children: [],
};
const models: ModelDistributionResponse = { data_revision: 1, items: [{ model: "gpt-6", usage }] };
const projects: ProjectDistributionResponse = { data_revision: 1, items: [{ kind: "project", path: "/work/Usagi", name: "Usagi", usage }] };
const status: RuntimeStatusResponse = { status_revision: 1, runtime_state: "ready", last_error_code: null };

function clientWith(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    quota: vi.fn(async () => ({ reading: false, items: [] })),
    refreshQuota: vi.fn(async () => ({ reading: false, items: [] })),
    filterOptions: vi.fn(async () => ({ data_revision: 1, agents: [], models: [], projects: [] })),
    summary: vi.fn(async () => summary),
    modelDistribution: vi.fn(async () => models),
    projectDistribution: vi.fn(async () => projects),
    getSessions: vi.fn(async () => sessions),
    toolUsage: vi.fn(async (days) => ({ days, calls: 0, top: [] })),
    getSessionDetail: vi.fn(async () => detail),
    getStatus: vi.fn(async () => status),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    ...overrides,
  };
}

function renderDashboard(client?: UsagiClient) {
  const eventSourceFactory = vi.fn(() => ({ onerror: null, onmessage: null, close: vi.fn() }));
  const rendered = render(
    <ThemeProvider>
      <DashboardPage options={{
        client,
        eventSourceFactory,
      }} />
    </ThemeProvider>,
  );
  return { ...rendered, eventSourceFactory };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

describe("DashboardPage Spec05 shared sessions snapshot", () => {
  it("keeps the default Browser theme control working through ThemeProvider", () => {
    renderDashboard(clientWith());
    const initiallyDark = document.documentElement.classList.contains("dark");
    const toggle = screen.getByRole("button", {
      name: initiallyDark ? "Switch to light mode" : "Switch to dark mode",
    });
    expect(toggle).toBeVisible();

    fireEvent.click(toggle);
    expect(document.documentElement.classList.contains("dark")).toBe(!initiallyDark);
    fireEvent.click(screen.getByRole("button", {
      name: initiallyDark ? "Switch to dark mode" : "Switch to light mode",
    }));
    expect(document.documentElement.classList.contains("dark")).toBe(initiallyDark);
    expect(toggle).toHaveAccessibleName(initiallyDark ? "Switch to light mode" : "Switch to dark mode");
  });

  it("defaults to HTTP clients and SSE without calling the MCP App bridge", async () => {
    const responses: Record<string, unknown> = {
      "/api/quota": { reading: false, items: [] },
      "/api/usage/filter-options": { data_revision: 1, agents: [], models: [], projects: [] },
      "/api/usage/sessions": sessions,
      "/api/usage/model-distribution": models,
      "/api/usage/projects": projects,
      "/api/usage/tools": { days: 1, calls: 0, top: [] },
      "/api/status": status,
      "/api/revision": { data_revision: 1, status_revision: 1 },
      "/api/service": { state: "running" },
      "/api/service/stop": { state: "stopped" },
    };
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const path = new URL(String(input), "http://usagi.test").pathname;
      if (!(path in responses)) throw new Error(`Unexpected Browser request: ${path}`);
      return new Response(JSON.stringify(responses[path]), {
        headers: { "Content-Type": "application/json" },
      });
    });
    const getSessions = vi.spyOn(usagiClient, "getSessions");
    const getRevision = vi.spyOn(usagiClient, "getRevision");
    const getState = vi.spyOn(serviceClient, "getState");
    const stop = vi.spyOn(serviceClient, "stop");
    const createFeed = vi.spyOn(revisionFeedModule, "createRevisionFeed");
    const callServerTool = vi.spyOn(App.prototype, "callServerTool")
      .mockRejectedValue(new Error("Browser must not call the MCP App bridge"));
    const rendered = renderDashboard();

    expect(await screen.findByText("Session Alpha")).toBeInTheDocument();
    expect(getSessions).toHaveBeenCalled();
    expect(getRevision).toHaveBeenCalled();
    expect(fetchMock).toHaveBeenCalledWith(
      expect.stringMatching(/^\/api\/usage\/sessions\?/),
      expect.objectContaining({ method: "GET", credentials: "same-origin" }),
    );
    expect(fetchMock).toHaveBeenCalledWith("/api/revision", expect.objectContaining({ method: "GET" }));
    expect(createFeed).toHaveBeenCalledTimes(1);
    expect(createFeed).toHaveBeenCalledWith(expect.objectContaining({ client: undefined, mode: "sse" }));
    expect(rendered.eventSourceFactory).toHaveBeenCalledTimes(1);
    expect(rendered.eventSourceFactory).toHaveBeenCalledWith("/api/events");

    const stopButton = screen.getByRole("button", { name: "停止服务" });
    await waitFor(() => expect(stopButton).toBeEnabled());
    expect(getState).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledWith("/api/service", expect.objectContaining({ method: "GET" }));
    fireEvent.click(stopButton);
    expect(await screen.findByText("服务已停止")).toBeInTheDocument();
    expect(stopButton).toBeDisabled();
    expect(stop).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledWith("/api/service/stop", expect.objectContaining({
      method: "POST",
      headers: { Accept: "application/json", "X-Usagi-Request": "1" },
    }));
    expect(callServerTool).not.toHaveBeenCalled();
    rendered.unmount();
  });

  it("renders without ThemeProvider when the toggle is hidden and owns its poll feed", async () => {
    const createFeed = vi.spyOn(revisionFeedModule, "createRevisionFeed");
    const disposeFeed = vi.spyOn(revisionFeedModule.RevisionFeed.prototype, "dispose");
    const client = clientWith();
    const rendered = render(
      <DashboardPage options={{
        client,
        revisionMode: "poll",
        pollIntervalMs: 5000,
        showThemeToggle: false,
      }} />,
    );

    expect(await screen.findByText("Session Alpha")).toBeInTheDocument();
    expect(rendered.container.querySelector(".dashboard-shell")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Switch to (dark|light) mode$/ })).not.toBeInTheDocument();
    expect(createFeed).toHaveBeenCalledTimes(1);
    expect(createFeed).toHaveBeenCalledWith(expect.objectContaining({
      client,
      mode: "poll",
      pollIntervalMs: 5000,
    }));

    rendered.unmount();
    expect(disposeFeed).toHaveBeenCalledTimes(1);
  });

  it("defaults to sessions and preserves date, filters, and dashboard content when toggled", async () => {
    const client = clientWith({
      filterOptions: vi.fn(async () => ({
        data_revision: 1,
        agents: [{ agent: "claude", display_name: "Claude Code" }],
        models: [],
        projects: [],
      })),
    });
    renderDashboard(client);

    await waitFor(() => expect(screen.getByText("Session Alpha")).toBeInTheDocument());
    expect(screen.getByRole("tab", { name: "会话" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("会话数")).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "账户额度" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: "7d" }));
    await waitFor(() => expect(client.getSessions).toHaveBeenLastCalledWith(expect.objectContaining({ range: { key: "7d" } })));
    const dateAndFilterCallCount = vi.mocked(client.getSessions).mock.calls.length;

    fireEvent.click(screen.getByRole("button", { name: "Agent筛选，全部" }));
    const agentMenu = await screen.findByRole("dialog");
    fireEvent.click(within(agentMenu).getByRole("checkbox", { name: "Claude Code" }));
    await waitFor(() => expect(client.getSessions).toHaveBeenLastCalledWith(expect.objectContaining({
      range: { key: "7d" },
      filters: { ...emptyFilters, agents: ["claude"] },
    })));
    const filteredCallCount = vi.mocked(client.getSessions).mock.calls.length;

    fireEvent.click(screen.getByRole("tab", { name: "请求" }));
    expect(screen.getByRole("tab", { name: "请求" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("请求数")).toBeInTheDocument();
    expect(screen.getByText("输出速度")).toBeInTheDocument();
    expect(screen.getByTitle("20")).toHaveTextContent("20");
    expect(screen.queryByRole("group", { name: "账户额度" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Agent筛选，已选1项" })).toBeInTheDocument();
    expect(screen.getByText("Session Alpha")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Tool 使用分布" })).toBeInTheDocument();
    expect(vi.mocked(client.getSessions).mock.calls.length).toBe(filteredCallCount);
    expect(vi.mocked(client.getSessions).mock.calls[dateAndFilterCallCount][0]).toMatchObject({
      range: { key: "7d" },
      filters: { ...emptyFilters, agents: ["claude"] },
    });
  });

  it("keeps KPI and list skeletons until one sessions response replaces them together", async () => {
    const pending = deferred<SessionListResponse>();
    const client = clientWith({ getSessions: vi.fn(() => pending.promise) });
    const rendered = renderDashboard(client);

    await waitFor(() => expect(client.getSessions).toHaveBeenCalledTimes(1));
    expect(screen.getByLabelText("KPI 加载中").children).toHaveLength(5);
    expect(rendered.container.querySelectorAll("tbody tr")).toHaveLength(6);
    expect(screen.queryByText("Session Alpha")).not.toBeInTheDocument();
    expect(client.summary).not.toHaveBeenCalled();

    pending.resolve(sessions);
    await waitFor(() => expect(screen.getByText("Session Alpha")).toBeInTheDocument());
    expect(screen.getByLabelText("KPI 指标").children).toHaveLength(5);
    expect(screen.getByText("中位 14 · P90 14")).toBeInTheDocument();
    expect(rendered.container.querySelector("tbody tr[data-session-thread-key-id]")).toBeTruthy();
    expect(client.getSessions).toHaveBeenCalled();
  });

  it("uses one sessions request for the shared snapshot and reloads related views on manual refresh", async () => {
    const client = clientWith();
    const rendered = renderDashboard(client);
    await waitFor(() => expect(rendered.container.querySelector("tbody tr[data-session-thread-key-id]")).toBeTruthy());
    const sessionRow = rendered.container.querySelector<HTMLElement>("tbody tr[data-session-thread-key-id]");
    expect(sessionRow?.textContent).toContain("Session Alpha");
    await waitFor(() => {
      expect(client.filterOptions).toHaveBeenCalled();
      expect(client.modelDistribution).toHaveBeenCalled();
      expect(client.projectDistribution).toHaveBeenCalled();
      expect(client.toolUsage).toHaveBeenCalledWith(1, expect.any(AbortSignal));
      expect(client.quota).toHaveBeenCalledTimes(1);
      expect(screen.getByRole("region", { name: "使用分布图表" })).toHaveAttribute("aria-busy", "false");
    });
    expect(client.summary).not.toHaveBeenCalled();
    fireEvent.click(sessionRow!);
    await waitFor(() => expect(client.getSessionDetail).toHaveBeenCalledTimes(1));
    expect(client.getSessionDetail).toHaveBeenLastCalledWith(expect.objectContaining({
      key: row.thread.key,
      filters: emptyFilters,
      range: { key: "today" },
      signal: expect.any(AbortSignal),
    }));

    const baseline = {
      filterOptions: vi.mocked(client.filterOptions).mock.calls.length,
      models: vi.mocked(client.modelDistribution).mock.calls.length,
      projects: vi.mocked(client.projectDistribution).mock.calls.length,
      sessions: vi.mocked(client.getSessions).mock.calls.length,
      detail: vi.mocked(client.getSessionDetail).mock.calls.length,
    };
    fireEvent.click(screen.getByRole("button", { name: "刷新数据" }));

    await waitFor(() => {
      expect(client.filterOptions).toHaveBeenCalledTimes(baseline.filterOptions + 1);
      expect(client.modelDistribution).toHaveBeenCalledTimes(baseline.models + 1);
      expect(client.projectDistribution).toHaveBeenCalledTimes(baseline.projects + 1);
      expect(client.getSessions).toHaveBeenCalledTimes(baseline.sessions + 1);
      expect(client.getSessionDetail).toHaveBeenCalledTimes(baseline.detail + 1);
    });
    expect(client.summary).not.toHaveBeenCalled();
    expect(client.getStatus).toHaveBeenCalledTimes(1);
    expect(client.quota).toHaveBeenCalledTimes(1);
    expect(client.refreshQuota).not.toHaveBeenCalled();
  });

  it("keeps Tool usage on today across dashboard ranges, filters, and database revisions", async () => {
    let currentRevision = 1;
    const source = {
      onerror: null as ((event: Event) => void) | null,
      onmessage: null as ((event: MessageEvent<string>) => void) | null,
      close: vi.fn(),
    };
    const options = {
      data_revision: 1,
      agents: [{ agent: "claude", display_name: "Claude Code" }],
      models: [{ model: "gpt-6", provider_id: "openai", provider_name: "OpenAI" }],
      projects: [{ kind: "project" as const, path: "/work/Usagi", name: "Usagi" }],
    };
    const client = clientWith({
      filterOptions: vi.fn(async () => options),
      getSessions: vi.fn(async () => ({
        ...sessions,
        data_revision: currentRevision,
        summary: { ...summary, data_revision: currentRevision },
      })),
    });
    render(
      <ThemeProvider>
        <DashboardPage options={{ client, eventSourceFactory: () => source }} />
      </ThemeProvider>,
    );

    await waitFor(() => {
      expect(client.getSessions).toHaveBeenCalled();
      expect(client.modelDistribution).toHaveBeenCalled();
      expect(client.projectDistribution).toHaveBeenCalled();
      expect(client.filterOptions).toHaveBeenCalled();
      expect(client.toolUsage).toHaveBeenCalledTimes(1);
      expect(screen.getByRole("region", { name: "使用分布图表" })).toHaveAttribute("aria-busy", "false");
    });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
    const toolCard = screen.getByRole("heading", { name: "Tool 使用分布" }).closest("article");
    if (!toolCard) throw new Error("Tool usage card not found");
    expect(within(toolCard).getByRole("tab", { name: "今天" })).toHaveAttribute("aria-selected", "true");

    const requestCounts = () => ({
      sessions: vi.mocked(client.getSessions).mock.calls.length,
      models: vi.mocked(client.modelDistribution).mock.calls.length,
      projects: vi.mocked(client.projectDistribution).mock.calls.length,
    });
    const waitForScopeReload = async (
      previous: ReturnType<typeof requestCounts>,
      range: { key: string },
      filters: DashboardFilters,
    ) => {
      await waitFor(() => {
        expect(vi.mocked(client.getSessions).mock.calls.length).toBeGreaterThan(previous.sessions);
        expect(vi.mocked(client.modelDistribution).mock.calls.length).toBeGreaterThan(previous.models);
        expect(vi.mocked(client.projectDistribution).mock.calls.length).toBeGreaterThan(previous.projects);
        expect(vi.mocked(client.getSessions).mock.calls.at(-1)?.[0]).toMatchObject({ range, filters });
        expect(vi.mocked(client.modelDistribution).mock.calls.at(-1)?.slice(0, 2)).toEqual([range, filters]);
        expect(vi.mocked(client.projectDistribution).mock.calls.at(-1)?.slice(0, 2)).toEqual([range, filters]);
      });
      return requestCounts();
    };

    let previous = requestCounts();
    fireEvent.click(screen.getByRole("tab", { name: "7d" }));
    previous = await waitForScopeReload(previous, { key: "7d" }, emptyFilters);

    const selectFilter = async (trigger: string, checkbox: string) => {
      const before = previous;
      fireEvent.click(screen.getByRole("button", { name: trigger }));
      const dialog = await screen.findByRole("dialog");
      fireEvent.click(within(dialog).getByRole("checkbox", { name: checkbox }));
      fireEvent.keyDown(window, { key: "Escape" });
      await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
      previous = await waitForScopeReload(before, { key: "7d" }, currentFilters);
    };

    let currentFilters = emptyFilters;
    currentFilters = { ...currentFilters, agents: ["claude"] };
    await selectFilter("Agent筛选，全部", "Claude Code");
    currentFilters = { ...currentFilters, models: [{ provider_id: "openai", model: "gpt-6" }] };
    await selectFilter("模型筛选，全部", "gpt-6");
    currentFilters = { ...currentFilters, projects: [{ kind: "project", path: "/work/Usagi" }] };
    await selectFilter("项目筛选，全部", "Usagi");
    expect(vi.mocked(client.getSessions).mock.calls.at(-1)?.[0]).toMatchObject({
      range: { key: "7d" },
      filters: {
        agents: ["claude"],
        models: [{ provider_id: "openai", model: "gpt-6" }],
        projects: [{ kind: "project", path: "/work/Usagi" }],
      },
    });

    currentRevision = 2;
    const beforeRevision = previous;
    await act(async () => {
      source.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 1 }) } as MessageEvent<string>);
    });
    await waitForScopeReload(beforeRevision, { key: "7d" }, currentFilters);

    expect(client.toolUsage).toHaveBeenCalledTimes(1);
    expect(vi.mocked(client.toolUsage).mock.calls[0][0]).toBe(1);
    expect(within(toolCard).getByRole("tab", { name: "今天" })).toHaveAttribute("aria-selected", "true");
    expect(vi.mocked(client.modelDistribution).mock.calls.at(-1)?.slice(0, 2)).toEqual([
      { key: "7d" },
      {
        agents: ["claude"],
        models: [{ provider_id: "openai", model: "gpt-6" }],
        projects: [{ kind: "project", path: "/work/Usagi" }],
      },
    ]);
  });

  it("keeps chart A in its loading state when the database returns DATA_LOADING", async () => {
    const client = clientWith({
      modelDistribution: vi.fn().mockRejectedValue(new UsagiClientError("DATA_LOADING", 503)),
      projectDistribution: vi.fn().mockRejectedValue(new UsagiClientError("DATA_LOADING", 503)),
    });
    renderDashboard(client);
    await waitFor(() => expect(client.modelDistribution).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(client.projectDistribution).toHaveBeenCalledTimes(1));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(screen.getByRole("region", { name: "使用分布图表" })).toHaveAttribute("aria-busy", "true");
    expect(screen.queryByText("图表数据加载失败")).not.toBeInTheDocument();
    expect(screen.queryByRole("status", { name: "图表数据加载失败" })).not.toBeInTheDocument();
  });
});
