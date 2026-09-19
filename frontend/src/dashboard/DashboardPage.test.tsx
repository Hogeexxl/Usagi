import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { StrictMode, type ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../data/usagiClient";
import { UsagiClientError, type ApiErrorCode, type CodexQuotaResponse, type StatusResponse } from "../data/types";
import { ThemeProvider } from "../theme/ThemeProvider";
import { DashboardPage } from "./DashboardPage";
import { useCodexQuotaController } from "./useCodexQuotaController";

const summary = (range: "today" | "yesterday") => ({
  range: { key: range, start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" },
  data_revision: range === "today" ? 1 : 2,
  usage: {
    input_tokens: range === "today" ? 10 : 20,
    cached_tokens: 0,
    cache_write_tokens: 0,
    uncached_input_tokens: range === "today" ? 10 : 20,
    output_tokens: 2,
    reasoning_tokens: 0,
    other_output_tokens: 2,
    total_tokens: range === "today" ? 12 : 22,
    cache_hit_rate: 0.5,
    estimated_cost: null,
    estimated_cost_status: "unknown" as const,
    session_count: 1,
    cost_incomplete_session_count: 1,
    complete_session_cost_per_million_tokens: null,
    session_health: {
      total_sessions: 1,
      complete_sessions: 1,
      incomplete_sessions: 0,
      error_sessions: 0,
    },
  },
});

const status = {
  data_revision: 1,
  status_revision: 1,
  scan_state: "idle" as const,
  active_scan_id: null,
  last_finished_scan_id: null,
  last_finished_scan_result: null,
  followup: null,
  target_scan: null,
  last_scan_started_at_ms: null,
  last_scan_completed_at_ms: null,
  last_scan_failed_at_ms: null,
  last_scan_error_code: null,
  source_binding_status: "ready" as const,
};

const sessionSnapshot = {
  range: { key: "today" as const, start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" },
  data_revision: 1,
  total_items: 0,
  sort_index: [],
  items: [],
};

const quotaLoading: CodexQuotaResponse = {
  status: "loading",
  account_email: null,
  plan_type: null,
  session: null,
  weekly: null,
  reset_credits_available: null,
  fetched_at_ms: null,
};

const quotaReady: CodexQuotaResponse = {
  status: "ready",
  account_email: "hoge@example.com",
  plan_type: "prolite",
  session: null,
  weekly: {
    used_percent: 55,
    remaining_percent: 45,
    limit_window_seconds: 604800,
    reset_at_ms: 1_786_508_580_000,
  },
  reset_credits_available: 2,
  fetched_at_ms: 1_786_076_580_000,
};

const offsetHeightDescriptor = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");

function fakeClient(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    filterOptions: vi.fn(async () => ({ data_revision: 1, models: [], projects: [] })),
    summary: vi.fn(async (range) => (range.key === "today" ? summary("today") : summary("yesterday"))),
    modelDistribution: vi.fn(async (range) => ({
      range: { key: range.key, start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" },
      data_revision: 1,
      items: [],
    })),
    projectDistribution: vi.fn(async (range) => ({
      range: { key: range.key, start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" },
      data_revision: 1,
      items: [],
    })),
    skillsUsage: vi.fn(async () => ({
      range: { key: "7d" as const, start_ms: 1, end_ms: 8, timezone: "Asia/Shanghai" },
      data_revision: 1,
      data_status: "ready" as const,
      days: Array.from({ length: 7 }, (_, index) => ({
        date: `2026-08-${String(index + 1).padStart(2, "0")}`,
        start_ms: index + 1,
        end_ms: index + 2,
        total: 0,
        skills: [],
      })),
      })),
    codexQuota: vi.fn(async () => ({
      status: "unavailable" as const,
      account_email: null,
      plan_type: null,
      session: null,
      weekly: null,
      reset_credits_available: null,
      fetched_at_ms: null,
    })),
    getSessionSnapshot: vi.fn(async () => sessionSnapshot),
    getSessionRows: vi.fn(async ({ range }) => ({
      range: { ...sessionSnapshot.range, key: range.key },
      data_revision: 1,
      items: [],
    })),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(async () => status),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    refresh: vi.fn(async () => ({ http_status: 202 as const, disposition: "started" as const, scan_id: "scan", status_revision: 2 })),
    ...overrides,
  };
}

function fakeEvents() {
  return {
    onerror: null as ((event: Event) => void) | null,
    onmessage: null as ((event: MessageEvent<string>) => void) | null,
    close: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function renderWithTheme(node: ReactNode) {
  return render(<ThemeProvider>{node}</ThemeProvider>);
}

function QuotaProbe({
  client,
  range = "today",
  model = "",
  project = "",
  revision = 0,
}: {
  client: UsagiClient;
  range?: string;
  model?: string;
  project?: string;
  revision?: number;
}) {
  const quota = useCodexQuotaController({ client });
  return <output data-testid="quota-probe" data-range={range} data-model={model} data-project={project} data-revision={revision}>{quota.status}</output>;
}

async function flushQuotaPromises() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

function totalTokenCard() {
  return within(screen.getByLabelText("KPI 指标")).getByText("总 Token").parentElement;
}

describe("DashboardPage v0.2.1", () => {
  it("T-Q-006 controller reads immediately, retries loading after 1s, then polls every 300s", async () => {
    vi.useFakeTimers();
    try {
      const codexQuota = vi.fn()
        .mockResolvedValueOnce(quotaLoading)
        .mockResolvedValue(quotaReady);
      const client = fakeClient({ codexQuota });
      const rendered = render(<QuotaProbe client={client} />);

      expect(codexQuota).toHaveBeenCalledTimes(1);
      await flushQuotaPromises();
      await act(async () => { vi.advanceTimersByTime(999); });
      expect(codexQuota).toHaveBeenCalledTimes(1);
      await act(async () => { vi.advanceTimersByTime(1); });
      await flushQuotaPromises();
      expect(codexQuota).toHaveBeenCalledTimes(2);
      expect(screen.getByTestId("quota-probe")).toHaveTextContent("ready");

      await act(async () => { vi.advanceTimersByTime(299_999); });
      expect(codexQuota).toHaveBeenCalledTimes(2);
      await act(async () => { vi.advanceTimersByTime(1); });
      await flushQuotaPromises();
      expect(codexQuota).toHaveBeenCalledTimes(3);
      rendered.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("T-Q-006 controller aborts the active request and clears the loading retry on unmount", async () => {
    vi.useFakeTimers();
    try {
      const signals: AbortSignal[] = [];
      const codexQuota = vi.fn((signal?: AbortSignal) => {
        signals.push(signal!);
        if (signals.length === 1) return Promise.resolve(quotaLoading);
        return new Promise<CodexQuotaResponse>(() => undefined);
      });
      const client = fakeClient({ codexQuota });
      const rendered = render(<QuotaProbe client={client} />);
      await flushQuotaPromises();
      await act(async () => { vi.advanceTimersByTime(1_000); });
      expect(codexQuota).toHaveBeenCalledTimes(2);
      rendered.unmount();

      expect(signals[1]).toHaveProperty("aborted", true);
      await act(async () => { vi.advanceTimersByTime(300_000); });
      expect(codexQuota).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("T-Q-006 controller ignores dashboard range, filters, and revision changes", async () => {
    vi.useFakeTimers();
    try {
      const codexQuota = vi.fn(async () => quotaReady);
      const client = fakeClient({ codexQuota });
      const rendered = render(<QuotaProbe client={client} range="today" model="" project="" revision={1} />);
      await flushQuotaPromises();
      expect(codexQuota).toHaveBeenCalledTimes(1);
      rendered.rerender(<QuotaProbe client={client} range="30d" model="gpt-5" project="Usagi" revision={99} />);
      expect(codexQuota).toHaveBeenCalledTimes(1);
      rendered.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("T-Q-006 controller retains the ready view after a temporary local API failure", async () => {
    vi.useFakeTimers();
    try {
      const codexQuota = vi.fn()
        .mockResolvedValueOnce(quotaReady)
        .mockRejectedValueOnce(new Error("temporary local API failure"));
      const client = fakeClient({ codexQuota });
      const rendered = render(<QuotaProbe client={client} />);
      await flushQuotaPromises();
      expect(screen.getByTestId("quota-probe")).toHaveTextContent("ready");
      await act(async () => { vi.advanceTimersByTime(300_000); });
      await flushQuotaPromises();
      expect(codexQuota).toHaveBeenCalledTimes(2);
      expect(screen.getByTestId("quota-probe")).toHaveTextContent("ready");
      rendered.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows the last completed sync time with seconds", async () => {
    const completedAt = Date.UTC(2026, 0, 2, 3, 4, 5);
    const client = fakeClient({
      getStatus: vi.fn(async () => ({ ...status, last_scan_completed_at_ms: completedAt })),
    });
    renderWithTheme(<DashboardPage options={{ client, eventSourceFactory: () => fakeEvents() }} />);
    const date = new Date(completedAt);
    const expected = [date.getHours(), date.getMinutes(), date.getSeconds()]
      .map((part) => String(part).padStart(2, "0"))
      .join(":");
    await waitFor(() => expect(screen.getByText(/上次同步：/)).toHaveTextContent(expected));
  });

  it("does not stack ActionSwap layers when the initial sync timestamp arrives", async () => {
    vi.spyOn(window, "matchMedia").mockImplementation((query) => ({
      matches: !query.includes("prefers-reduced-motion"),
      media: query,
      onchange: null,
      addListener: () => undefined,
      removeListener: () => undefined,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
      dispatchEvent: () => false,
    }));
    const completedAt = Date.UTC(2026, 0, 2, 3, 4, 5);
    const statusRequest = deferred<StatusResponse>();
    const client = fakeClient({
      getStatus: vi.fn(() => statusRequest.promise),
    });
    renderWithTheme(<DashboardPage options={{ client, eventSourceFactory: () => fakeEvents() }} />);
    await waitFor(() => expect(screen.getByText(/上次同步：/)).toHaveTextContent("—"));
    const expected = new Date(completedAt);
    const syncTime = [expected.getHours(), expected.getMinutes(), expected.getSeconds()]
      .map((part) => String(part).padStart(2, "0"))
      .join(":");

    await act(async () => {
      statusRequest.resolve({ ...status, last_scan_completed_at_ms: completedAt });
      await Promise.resolve();
    });
    const syncLabel = screen.getByText(/上次同步：/);
    expect(syncLabel).toHaveTextContent(syncTime);
    expect(syncLabel.querySelectorAll("span.absolute")).toHaveLength(1);
  });

  it("uses an em dash before a sync has completed", async () => {
    renderWithTheme(<DashboardPage options={{ client: fakeClient(), eventSourceFactory: () => fakeEvents() }} />);
    await waitFor(() => expect(screen.getByText(/上次同步：/)).toHaveTextContent("—"));
  });

  it("keeps one shared revision transport active across StrictMode remounts", async () => {
    let activeSources = 0;
    let maxActiveSources = 0;
    const factory = vi.fn(() => {
      const source = fakeEvents();
      const close = source.close;
      activeSources += 1;
      maxActiveSources = Math.max(maxActiveSources, activeSources);
      source.close = vi.fn(() => {
        close();
        activeSources -= 1;
      });
      return source;
    });
    const client = fakeClient();
    const view = renderWithTheme(
      <StrictMode>
        <DashboardPage options={{ client, eventSourceFactory: factory }} />
      </StrictMode>,
    );
    await waitFor(() => expect(totalTokenCard()).toHaveTextContent("12"));
    expect(maxActiveSources).toBe(1);
    expect(activeSources).toBe(1);
    view.unmount();
    expect(activeSources).toBe(0);
  });

  it("keeps a range snapshot and renders the redesigned sections without navigation", async () => {
    renderWithTheme(<DashboardPage options={{ client: fakeClient(), eventSourceFactory: () => fakeEvents() }} />);
    await waitFor(() => expect(totalTokenCard()).toHaveTextContent("12"));
    expect(screen.getByLabelText("KPI 指标").children).toHaveLength(5);
    expect(screen.queryByRole("navigation")).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Session 记录" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Switch to light mode/ })).toBeInTheDocument();
  });

  it("loads the Session detail Drawer only after a row is selected", async () => {
    const sessionRow = {
      root_session_id: "root-1",
      title: "Session 1",
      project_name: null,
      project_path: null,
      last_activity_at_ms: 1,
      models_used: ["gpt-5"],
      subagent_count: 0,
      inclusive_usage: null,
      self_usage: null,
      subagent_usage: null,
      data_status: "complete" as const,
      error_code: null,
    };
    const client = fakeClient({
      getSessionSnapshot: vi.fn(async () => ({
        ...sessionSnapshot,
        total_items: 1,
        sort_index: [{
          root_session_id: sessionRow.root_session_id,
          last_activity_at_ms: sessionRow.last_activity_at_ms,
          project_sort_key: null,
          model_sort_key: "gpt-5",
          total_tokens: null,
          combined_total_tokens: null,
          combined_estimated_cost: null,
          cache_hit_rate: null,
          data_status: "complete" as const,
          error_code: null,
        }],
        items: [sessionRow],
      })),
      getSessionDetail: vi.fn(async () => {
        throw new Error("detail unavailable");
      }),
    });
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
      configurable: offsetHeightDescriptor?.configurable ?? true,
      enumerable: offsetHeightDescriptor?.enumerable ?? true,
      get() {
        const inline = Number.parseFloat(this.style.height);
        if (Number.isFinite(inline)) return inline;
        return offsetHeightDescriptor?.get?.call(this) ?? 0;
      },
    });
    try {
      renderWithTheme(<DashboardPage options={{ client, eventSourceFactory: () => fakeEvents() }} />);

      await waitFor(() => expect(document.querySelector('tbody tr[data-session-root-id="root-1"]')).toBeInTheDocument());
      expect(screen.queryByRole("dialog", { name: "Session 详情" })).not.toBeInTheDocument();

      const row = document.querySelector<HTMLElement>('tbody tr[data-session-root-id="root-1"]');
      if (!row) throw new Error("Session row not found");
      fireEvent.click(row);
      await waitFor(() => expect(screen.getByRole("dialog", { name: "Session 详情" })).toBeInTheDocument());
      expect(screen.getByRole("dialog", { name: "Session 详情" })).toHaveTextContent("Session 1");
    } finally {
      if (offsetHeightDescriptor) Object.defineProperty(HTMLElement.prototype, "offsetHeight", offsetHeightDescriptor);
    }
  });

  it("uses Spec01 BeUI header parameters and one gap-8 top-level stack", async () => {
    renderWithTheme(<DashboardPage options={{ client: fakeClient(), eventSourceFactory: () => fakeEvents() }} />);
    await waitFor(() => expect(totalTokenCard()).toHaveTextContent("12"));

    const main = document.querySelector("main.dashboard-content");
    expect(main).toBeInTheDocument();
    expect(main?.firstElementChild).toHaveClass("flex", "flex-col", "gap-8");
    expect(screen.getByRole("heading", { name: "Usagi" })).toHaveClass("text-foreground");

    const sync = screen.getByRole("button", { name: "同步数据" });
    const stop = screen.getByRole("button", { name: "停止服务" });
    for (const button of [sync, stop]) {
      expect(button).toHaveClass("h-8", "px-3", "text-xs", "gap-1.5", "rounded-full");
      expect(button).not.toHaveClass("h-10");
    }

    const theme = screen.getByRole("button", { name: "Switch to light mode" });
    expect(theme).toHaveClass("rounded-xl", "border", "border-border", "bg-background", "p-2.5");
    expect(theme.querySelector("svg")).toHaveClass("h-5", "w-5");
    expect(screen.getByText(/上次同步：/)).toHaveClass("text-muted-foreground");
  });

  it("maps a failed range load to fixed text without leaking the private error", async () => {
    const client = fakeClient({
      summary: vi.fn()
        .mockResolvedValueOnce(summary("today"))
        .mockRejectedValueOnce(new Error("private response body")),
    });
    renderWithTheme(<DashboardPage options={{ client, eventSourceFactory: () => fakeEvents() }} />);
    await waitFor(() => expect(totalTokenCard()).toHaveTextContent("12"));
    screen.getByRole("tab", { name: "昨天" }).click();
    await waitFor(() => expect(screen.getAllByRole("alert")[0]).toHaveTextContent("数据加载失败"));
    expect(screen.queryByText("private response body")).not.toBeInTheDocument();
    expect(screen.getByLabelText("KPI 加载中")).toBeInTheDocument();
  });

  it.each([
    [403, "FORBIDDEN", "无法发起同步"],
    [409, "SOURCE_CHANGED", "数据源已变化"],
    [500, "SCAN_START_FAILED", "同步失败"],
  ])("maps refresh HTTP errors %s/%s to %s", async (statusCode, code, message) => {
    const client = fakeClient({
      refresh: vi.fn(async () => {
        throw new UsagiClientError(code as ApiErrorCode, statusCode);
      }),
    });
    renderWithTheme(<DashboardPage options={{ client, eventSourceFactory: () => fakeEvents() }} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "同步数据" })).toBeEnabled());
    screen.getByRole("button", { name: "同步数据" }).click();
    await waitFor(() => expect(screen.getAllByText(message).length).toBeGreaterThan(0));
  });

  it("renders tracking failure separately and retries only status", async () => {
    let targetCalls = 0;
    const client = fakeClient({
      getStatus: vi.fn(async (target) => {
        if (!target) return status;
        targetCalls += 1;
        if (targetCalls === 1) throw new Error("raw response body");
        return {
          ...status,
          target_scan: {
            scan_id: target,
            state: "running" as const,
            started_status_revision: 2,
            terminal_status_revision: null,
            error_code: null,
          },
        };
      }),
    });
    renderWithTheme(<DashboardPage options={{ client, eventSourceFactory: () => fakeEvents() }} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "同步数据" })).toBeEnabled());
    screen.getByRole("button", { name: "同步数据" }).click();
    await waitFor(() => expect(screen.getAllByText("同步状态获取失败").length).toBeGreaterThan(0));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    screen.getByRole("button", { name: "重试" }).click();
    await waitFor(() => expect(screen.getAllByText("同步中…").length).toBeGreaterThan(0));
    expect(client.refresh).toHaveBeenCalledTimes(1);
  });

  it("disables sync when status failed while Summary remains available, then recovers via retry", async () => {
    let statusAttempts = 0;
    const client = fakeClient({
      getStatus: vi.fn(async () => {
        statusAttempts += 1;
        if (statusAttempts === 1) throw new Error("status unavailable");
        return status;
      }),
      getRevision: vi.fn(async () => ({ data_revision: 0, status_revision: 0 })),
    });
    renderWithTheme(<DashboardPage options={{ client, eventSourceFactory: () => fakeEvents() }} />);
    const syncButton = screen.getByRole("button", { name: "同步数据" });
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("数据加载失败"));
    expect(syncButton).toBeDisabled();
    expect(totalTokenCard()).toHaveTextContent("12");
    screen.getByRole("button", { name: "重试" }).click();
    await waitFor(() => expect(syncButton).toBeEnabled());
    expect(statusAttempts).toBe(2);
  });

  it.each([
    ["status then summary", ["status", "summary"] as const, false, true],
    ["summary then status", ["summary", "status"] as const, false, true],
    ["status then revision", ["status", "revision"] as const, true, false],
    ["revision then status", ["revision", "status"] as const, true, false],
  ])("keeps status-not-ready as the stable sync priority when dependencies fail (%s)", async (_name, failOrder, summarySucceeds, revisionSucceeds) => {
    const summaryRequest = deferred<ReturnType<typeof summary>>();
    const statusRequest = deferred<typeof status>();
    const revisionRequest = deferred<{ data_revision: number; status_revision: number }>();
    const client = fakeClient({
      summary: vi.fn(() => summaryRequest.promise),
      getStatus: vi.fn(() => statusRequest.promise),
      getRevision: vi.fn(() => revisionRequest.promise),
    });
    renderWithTheme(<DashboardPage options={{ client, eventSourceFactory: () => fakeEvents() }} />);

    await act(async () => {
      if (summarySucceeds) summaryRequest.resolve(summary("today"));
      if (revisionSucceeds) revisionRequest.resolve({ data_revision: 0, status_revision: 0 });
      await Promise.resolve();
    });
    for (const dependency of failOrder) {
      await act(async () => {
        if (dependency === "status") statusRequest.reject(new Error("status unavailable"));
        if (dependency === "summary") summaryRequest.reject(new Error("summary unavailable"));
        if (dependency === "revision") revisionRequest.reject(new Error("revision unavailable"));
        await Promise.resolve();
      });
    }

    const syncButton = screen.getByRole("button", { name: "同步数据" });
    await waitFor(() => expect(screen.getAllByRole("alert")[0]).toHaveTextContent("数据加载失败"));
    expect(syncButton).toBeDisabled();
    syncButton.click();
    expect(client.refresh).not.toHaveBeenCalled();
  });

  it("enables sync after status retry while a Summary failure remains visible", async () => {
    let statusAttempts = 0;
    const client = fakeClient({
      summary: vi.fn(async () => {
        throw new Error("summary unavailable");
      }),
      getStatus: vi.fn(async () => {
        statusAttempts += 1;
        if (statusAttempts === 1) throw new Error("status unavailable");
        return status;
      }),
      getRevision: vi.fn(async () => ({ data_revision: 0, status_revision: 0 })),
    });
    renderWithTheme(<DashboardPage options={{ client, eventSourceFactory: () => fakeEvents() }} />);
    const syncButton = screen.getByRole("button", { name: "同步数据" });
    await waitFor(() => expect(syncButton).toBeDisabled());
    expect(screen.getByRole("alert")).toHaveTextContent("数据加载失败");
    screen.getByRole("button", { name: "重试" }).click();
    await waitFor(() => expect(syncButton).toBeEnabled());
    expect(screen.getByRole("alert")).toHaveTextContent("数据加载失败");
    syncButton.click();
    await waitFor(() => expect(client.refresh).toHaveBeenCalledTimes(1));
  });
});
