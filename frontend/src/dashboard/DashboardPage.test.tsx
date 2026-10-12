import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../data/usagiClient";
import type {
  DashboardFilters,
  ModelDistributionResponse,
  ProjectDistributionResponse,
  RuntimeStatusResponse,
  SessionDetailResponse,
  SessionListItemDto,
  SummaryResponse,
  UsageDto,
} from "../data/types";
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
};

const row: SessionListItemDto = {
  thread: {
    key: { agent: "claude", session_id: "session-1", thread_id: "thread-1" },
    type: "main",
    parent_thread_id: null,
    title: "Session Alpha",
    project: { kind: "project", path: "/work/Usagi", name: "Usagi" },
    first_usage_ms: 1_800_000_000_000,
    last_usage_ms: 1_800_000_000_100,
    model_effort: [{ model: "gpt-6", effort: "high", usage, compaction_tokens: 0 }],
    usage,
  },
  self_usage: usage,
  subagent_usage: { ...usage, total: 0 },
  inclusive_usage: usage,
  subagent_count: 0,
};

const detail: SessionDetailResponse = {
  data_revision: 1,
  thread: row.thread,
  self_usage: usage,
  subagent_usage: { ...usage, total: 0 },
  inclusive_usage: usage,
  children: [],
};

const models: ModelDistributionResponse = {
  data_revision: 1,
  items: [{ model: "gpt-6", usage }],
};
const projects: ProjectDistributionResponse = {
  data_revision: 1,
  items: [{ kind: "project", path: "/work/Usagi", name: "Usagi", usage }],
};
const status: RuntimeStatusResponse = { status_revision: 1, runtime_state: "ready", last_error_code: null };

function clientWith(overrides: Partial<UsagiClient> = {}): UsagiClient {
  const summary: SummaryResponse = { data_revision: 1, usage, session_count: 1 };
  return {
    quota: vi.fn(async () => ({ reading: false, items: [] })),
    refreshQuota: vi.fn(async () => ({ reading: false, items: [] })),
    filterOptions: vi.fn(async () => ({ data_revision: 1, agents: [], models: [], projects: [] })),
    summary: vi.fn(async () => summary),
    modelDistribution: vi.fn(async () => models),
    projectDistribution: vi.fn(async () => projects),
    getSessions: vi.fn(async () => ({ data_revision: 1, items: [row] })),
    getSessionDetail: vi.fn(async () => detail),
    getStatus: vi.fn(async () => status),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    ...overrides,
  };
}

function renderDashboard(client: UsagiClient) {
  return render(
    <ThemeProvider>
      <DashboardPage options={{
        client,
        eventSourceFactory: () => ({
          onerror: null,
          onmessage: null,
          close: vi.fn(),
        }),
      }} />
    </ThemeProvider>,
  );
}

describe("DashboardPage Spec04 refresh collection", () => {
  it("GETs Summary, Filter Options, Models, Projects, Sessions, and an open Detail on manual refresh", async () => {
    const client = clientWith();
    const rendered = renderDashboard(client);
    await waitFor(() => expect(client.getSessions).toHaveBeenCalled());
    await waitFor(() => expect(rendered.container.querySelector("tbody tr[data-session-thread-key-id]")).toBeTruthy());
    const sessionRow = rendered.container.querySelector<HTMLElement>("tbody tr[data-session-thread-key-id]");
    expect(sessionRow?.textContent).toContain("Session Alpha");
    await waitFor(() => {
      expect(client.summary).toHaveBeenCalled();
      expect(client.filterOptions).toHaveBeenCalled();
      expect(client.modelDistribution).toHaveBeenCalled();
      expect(client.projectDistribution).toHaveBeenCalled();
      expect(client.getSessions).toHaveBeenCalled();
      expect(client.quota).toHaveBeenCalledTimes(1);
    });
    fireEvent.click(sessionRow!);
    await waitFor(() => expect(client.getSessionDetail).toHaveBeenCalledTimes(1));
    expect(client.getSessionDetail).toHaveBeenLastCalledWith(expect.objectContaining({
      key: row.thread.key,
      filters: emptyFilters,
      range: { key: "today" },
      signal: expect.any(AbortSignal),
    }));

    const baseline = {
      summary: vi.mocked(client.summary).mock.calls.length,
      filterOptions: vi.mocked(client.filterOptions).mock.calls.length,
      models: vi.mocked(client.modelDistribution).mock.calls.length,
      projects: vi.mocked(client.projectDistribution).mock.calls.length,
      sessions: vi.mocked(client.getSessions).mock.calls.length,
      detail: vi.mocked(client.getSessionDetail).mock.calls.length,
    };
    fireEvent.click(screen.getByRole("button", { name: "刷新数据" }));

    await waitFor(() => {
      expect(client.summary).toHaveBeenCalledTimes(baseline.summary + 1);
      expect(client.filterOptions).toHaveBeenCalledTimes(baseline.filterOptions + 1);
      expect(client.modelDistribution).toHaveBeenCalledTimes(baseline.models + 1);
      expect(client.projectDistribution).toHaveBeenCalledTimes(baseline.projects + 1);
      expect(client.getSessions).toHaveBeenCalledTimes(baseline.sessions + 1);
      expect(client.getSessionDetail).toHaveBeenCalledTimes(baseline.detail + 1);
    });
    expect(client.getStatus).toHaveBeenCalledTimes(1);
    expect(client.quota).toHaveBeenCalledTimes(1);
    expect(client.refreshQuota).not.toHaveBeenCalled();
  });
});
