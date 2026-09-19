import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";

import type { UsagiClient } from "../data/usagiClient";
import type { RevisionFeed as RevisionFeedType } from "../data/revisionFeed";
import type { CodexQuotaResponse, DashboardRange, RevisionTuple, StatusResponse, SummaryUsageDto } from "../data/types";
import { ThemeProvider } from "../theme/ThemeProvider";
import { TrayPanelPage, TrayPanelView, type TrayPanelViewModel } from "./TrayPanelPage";

const usage: SummaryUsageDto = {
  input_tokens: 1_500,
  cached_tokens: 600,
  cache_write_tokens: 0,
  uncached_input_tokens: 0,
  output_tokens: 500,
  reasoning_tokens: 125,
  other_output_tokens: 375,
  total_tokens: 2_000,
  cache_hit_rate: 0.4,
  estimated_cost: null,
  estimated_cost_status: "unknown",
  session_count: 1,
  cost_incomplete_session_count: 1,
  complete_session_cost_per_million_tokens: null,
  session_health: {
    total_sessions: 1,
    complete_sessions: 1,
    incomplete_sessions: 0,
    error_sessions: 0,
  },
};

const quota: CodexQuotaResponse = {
  status: "ready",
  account_email: "hoge@example.com",
  plan_type: "pro",
  session: null,
  weekly: {
    used_percent: 2,
    remaining_percent: 98,
    limit_window_seconds: 604_800,
    reset_at_ms: 1_800_000_000_000,
  },
  reset_credits_available: 999,
  fetched_at_ms: 1_800_000_000_000,
};

const status: StatusResponse = {
  data_revision: 1,
  status_revision: 1,
  scan_state: "idle",
  active_scan_id: null,
  last_finished_scan_id: null,
  last_finished_scan_result: null,
  followup: null,
  target_scan: null,
  last_scan_started_at_ms: null,
  last_scan_completed_at_ms: 1_800_000_000_000,
  last_scan_failed_at_ms: null,
  last_scan_error_code: null,
  source_binding_status: "ready",
};

const revisionFeed = {
  get_snapshot: () => null as RevisionTuple | null,
  subscribe: () => () => undefined,
  retry_now: () => undefined,
  dispose: () => undefined,
} as unknown as RevisionFeedType;

function fakeClient(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    codexQuota: vi.fn(async () => quota),
    filterOptions: vi.fn(async () => ({ data_revision: 1, models: [], projects: [] })),
    summary: vi.fn(async (range) => ({
      range: { key: range.key, start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" },
      data_revision: 1,
      usage,
    })),
    modelDistribution: vi.fn(async () => ({ range: { key: "today", start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" }, data_revision: 1, items: [] })),
    projectDistribution: vi.fn(async () => ({ range: { key: "today", start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" }, data_revision: 1, items: [] })),
    skillsUsage: vi.fn(async () => ({
      range: { key: "7d", start_ms: 1, end_ms: 8, timezone: "Asia/Shanghai" },
      data_revision: 1,
      data_status: "ready" as const,
      days: Array.from({ length: 7 }, (_, index) => ({ date: `2026-08-${index + 1}`, start_ms: index + 1, end_ms: index + 2, total: 0, skills: [] })),
    })),
    getSessionSnapshot: vi.fn(async () => ({ range: { key: "today", start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" }, data_revision: 1, total_items: 0, sort_index: [], items: [] })),
    getSessionRows: vi.fn(async () => ({ range: { key: "today", start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" }, data_revision: 1, items: [] })),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(async () => status),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    refresh: vi.fn(async () => ({ http_status: 202 as const, disposition: "started" as const, scan_id: "scan", status_revision: 2 })),
    ...overrides,
  } as UsagiClient;
}

function viewFor(overrides: Partial<TrayPanelViewModel> = {}): TrayPanelViewModel {
  return {
    range: { key: "today" },
    metrics: usage,
    load_state: "ready",
    last_scan_completed_at_ms: status.last_scan_completed_at_ms,
    refresh_state: "idle",
    error_code: undefined,
    select_range: vi.fn((_range: DashboardRange) => undefined),
    request_refresh: vi.fn(),
    retry_load: vi.fn(),
    retry_refresh_status: vi.fn(),
    ...overrides,
  };
}

function renderWithTheme(node: ReactNode) {
  return render(<ThemeProvider>{node}</ThemeProvider>);
}

function enableTiltEffects() {
  vi.spyOn(window, "matchMedia").mockImplementation((query) => ({
    matches: query.includes("(hover: hover) and (pointer: fine)"),
    media: query,
    onchange: null,
    addListener: () => undefined,
    removeListener: () => undefined,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    dispatchEvent: () => false,
  }));
}

function glareOverlayCount(card: HTMLElement): number {
  return Array.from(card.children).filter((child) =>
    child.classList.contains("pointer-events-none") &&
    child.classList.contains("absolute") &&
    child.classList.contains("inset-0") &&
    child.classList.contains("opacity-15"),
  ).length;
}

afterEach(() => {
  delete (window as Window & { ipc?: unknown }).ipc;
});

describe("TrayPanelPage", () => {
  it("disables glare on tray-only KPI cards while retaining it for total tokens", async () => {
    enableTiltEffects();
    renderWithTheme(
      <TrayPanelView
        view={viewFor()}
        quota={quota}
        stopping={false}
        onOpenDashboard={vi.fn()}
        onStop={vi.fn()}
      />,
    );

    const grid = screen.getByLabelText("KPI 指标");
    await waitFor(() => {
      expect(Array.from(grid.children).map((card) => glareOverlayCount(card as HTMLElement))).toEqual([1, 0, 0, 0]);
    });
  });

  it("renders the contracted tray controls and routes actions through injected clients", async () => {
    const client = fakeClient();
    const stop = vi.fn(async () => "stopped" as const);
    const ipc = vi.fn();
    (window as Window & { ipc?: unknown }).ipc = { postMessage: ipc };
    renderWithTheme(<TrayPanelPage options={{ client, revisionFeed, serviceClient: { getState: vi.fn(async () => "running" as const), stop } }} />);

    await waitFor(() => expect(screen.getByText("总 Token")).toBeInTheDocument());
    expect(screen.getAllByRole("tab")).toHaveLength(3);
    expect(screen.getByText("缓存命中")).toBeInTheDocument();
    expect(screen.getByText("预估费用")).toBeInTheDocument();
    expect(screen.getByText("剩余配额")).toBeInTheDocument();

    const dashboard = screen.getByRole("button", { name: "打开 Dashboard" });
    const syncTime = screen.getByText(/^上次同步：/);
    const refresh = screen.getByRole("button", { name: "刷新" });
    const themeToggle = screen.getByRole("button", { name: /Switch to (dark|light) mode/ });
    const stopButton = screen.getByRole("button", { name: "停止服务" });
    const toolbar = dashboard.parentElement as HTMLElement;
    const grid = screen.getByLabelText("KPI 指标");
    expect(grid).toHaveClass("grid", "grid-cols-[304px_304px]", "gap-4");
    expect(grid.children).toHaveLength(4);
    for (const [index, title] of ["总 Token", "预估费用", "缓存命中", "剩余配额"].entries()) {
      expect(within(grid.children[index] as HTMLElement).getByText(title)).toBeInTheDocument();
      expect(grid.children[index]).toHaveClass("h-36");
    }
    expect(dashboard).toHaveClass("border", "bg-card", "h-8");
    expect(refresh).toHaveClass("h-8", "w-8");
    expect(themeToggle).toHaveClass("rounded-xl", "border", "border-border", "bg-background", "p-2.5");
    expect(themeToggle.querySelector(".h-5.w-5")).toBeInTheDocument();
    expect(stopButton).toHaveClass("border-destructive/35", "text-destructive", "h-8", "w-8");
    expect(screen.getByRole("tablist").closest(".p-4")).toHaveClass("gap-4");
    expect(toolbar).toHaveClass("gap-4");
    expect(syncTime).toHaveClass("ml-auto");
    expect(Array.from(toolbar.children)).toEqual([dashboard, syncTime, refresh, themeToggle, stopButton]);
    expect(screen.getAllByText(/^上次同步：/)).toHaveLength(1);

    fireEvent.click(refresh);
    expect(client.refresh).toHaveBeenCalledTimes(1);
    fireEvent.click(dashboard);
    expect(ipc).toHaveBeenCalledWith("open-dashboard");
    fireEvent.click(stopButton);
    expect(stop).toHaveBeenCalledTimes(1);
    expect(stopButton).toBeDisabled();
  });

  it("restores the stop button and reports a toast when stopping fails", async () => {
    const client = fakeClient();
    const stop = vi.fn(async () => {
      throw new Error("stop failed");
    });
    renderWithTheme(<TrayPanelPage options={{ client, revisionFeed, serviceClient: { getState: vi.fn(async () => "running" as const), stop } }} />);
    await waitFor(() => expect(screen.getByText("总 Token")).toBeInTheDocument());
    const stopButton = screen.getByRole("button", { name: "停止服务" });
    fireEvent.click(stopButton);
    await waitFor(() => expect(screen.getByText("停止服务失败")).toBeInTheDocument());
    expect(stopButton).not.toBeDisabled();
  });

  it("recovers a first status failure through the load retry without advancing the revision", async () => {
    const getStatus = vi.fn()
      .mockRejectedValueOnce(new Error("status unavailable"))
      .mockResolvedValue(status);
    const client = fakeClient({ getStatus });
    renderWithTheme(<TrayPanelPage options={{ client, revisionFeed }} />);

    await waitFor(() => expect(screen.getByText("数据加载失败")).toBeInTheDocument());
    const refresh = screen.getByRole("button", { name: "刷新" });
    expect(refresh).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    await waitFor(() => expect(refresh).not.toBeDisabled());
    expect(screen.queryByText("数据加载失败")).not.toBeInTheDocument();
    expect(getStatus).toHaveBeenCalledTimes(2);
    expect(client.summary).toHaveBeenCalledTimes(1);
    expect(client.getRevision).not.toHaveBeenCalled();
  });

  it("recovers a tracking status failure through the status retry without a new data revision", async () => {
    const completedStatus: StatusResponse = {
      ...status,
      status_revision: 2,
      last_finished_scan_id: "scan",
      last_finished_scan_result: "completed",
      target_scan: {
        scan_id: "scan",
        state: "completed",
        started_status_revision: 2,
        terminal_status_revision: 2,
        error_code: null,
      },
    };
    const getStatus = vi.fn()
      .mockResolvedValueOnce(status)
      .mockRejectedValueOnce(new Error("tracking unavailable"))
      .mockResolvedValueOnce(completedStatus);
    const client = fakeClient({ getStatus });
    renderWithTheme(<TrayPanelPage options={{ client, revisionFeed }} />);

    await waitFor(() => expect(screen.getByText("总 Token")).toBeInTheDocument());
    const refresh = screen.getByRole("button", { name: "刷新" });
    expect(refresh).not.toBeDisabled();
    fireEvent.click(refresh);

    await waitFor(() => expect(screen.getByText("同步状态获取失败")).toBeInTheDocument());
    expect(refresh).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    await waitFor(() => expect(refresh).not.toBeDisabled());
    expect(screen.queryByText("同步状态获取失败")).not.toBeInTheDocument();
    expect(getStatus).toHaveBeenCalledTimes(3);
    expect(client.summary).toHaveBeenCalledTimes(1);
    expect(client.getRevision).not.toHaveBeenCalled();
  });

  it("keeps refresh availability aligned with Dashboard state", () => {
    const cases = [
      ["idle", undefined, false],
      ["failed", undefined, false],
      ["requesting", undefined, true],
      ["running", undefined, true],
      ["source_changed", "SOURCE_CHANGED", true],
      ["tracking_error", "HTTP_ERROR", true],
      ["idle", "STATUS_NOT_READY", true],
    ] as const;
    for (const [refreshState, errorCode, disabled] of cases) {
      const { unmount } = renderWithTheme(
        <TrayPanelView
          view={viewFor({ refresh_state: refreshState, error_code: errorCode })}
          quota={quota}
          stopping={false}
          onOpenDashboard={vi.fn()}
          onStop={vi.fn()}
        />,
      );
      expect(screen.getByRole("button", { name: "刷新" })).toHaveProperty("disabled", disabled);
      unmount();
    }
  });

  it("renders independent error recovery actions without turning them into refresh", () => {
    const retryLoad = vi.fn();
    const retryStatus = vi.fn();
    const requestRefresh = vi.fn();
    renderWithTheme(
      <TrayPanelView
        view={viewFor({ load_state: "error", refresh_state: "tracking_error", error_code: "HTTP_ERROR", retry_load: retryLoad, retry_refresh_status: retryStatus, request_refresh: requestRefresh })}
        quota={quota}
        stopping={false}
        onOpenDashboard={vi.fn()}
        onStop={vi.fn()}
      />,
    );
    expect(screen.getByText("数据加载失败")).toBeInTheDocument();
    expect(screen.getByText("同步状态获取失败")).toBeInTheDocument();
    const retries = screen.getAllByRole("button", { name: "重试" });
    expect(retries).toHaveLength(2);
    fireEvent.click(retries[0]);
    fireEvent.click(retries[1]);
    expect(retryLoad).toHaveBeenCalledTimes(1);
    expect(retryStatus).toHaveBeenCalledTimes(1);
    expect(requestRefresh).not.toHaveBeenCalled();
  });

  it("does not crash while metrics are null", () => {
    renderWithTheme(
      <TrayPanelView
        view={viewFor({ metrics: null, load_state: "loading" })}
        quota={{ ...quota, status: "loading", weekly: null }}
        stopping={false}
        onOpenDashboard={vi.fn()}
        onStop={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "刷新" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "停止服务" })).toBeInTheDocument();
    expect(screen.queryByText("总 Token")).not.toBeInTheDocument();
    const grid = screen.getByLabelText("KPI 加载中");
    expect(grid.children).toHaveLength(4);
    for (const card of Array.from(grid.children)) expect(card).toHaveClass("h-36");
  });
});
