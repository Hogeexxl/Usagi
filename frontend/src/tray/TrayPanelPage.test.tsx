import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";

import type { UsagiClient } from "../data/usagiClient";
import type { RevisionFeed as RevisionFeedType } from "../data/revisionFeed";
import type { DashboardRange, QuotaProviderDto, QuotaResponse, RevisionTuple, RuntimeStatusResponse, UsageDto } from "../data/types";
import { ThemeProvider } from "../theme/ThemeProvider";
import type { DashboardQuotaControllerView } from "../dashboard/useDashboardQuotaController";
import { TrayPanelPage, TrayPanelView, type TrayPanelViewModel } from "./TrayPanelPage";

const usage: UsageDto = {
  calls: 3,
  errors: 0,
  input: 1_500,
  output: 500,
  cache_read: 600,
  cache_write: 100,
  cache_write_1h: 0,
  reasoning: 125,
  total: 2_700,
  cache_hit_rate: 600 / 2_200,
  cost: 1.25,
  unpriced: 0,
  retried: 0, output_speed: null, speed_samples: 0,
};

function provider(providerId: string, name: string, display: string): QuotaProviderDto {
  return {
    provider: providerId,
    name,
    plan: "Pro",
    user: `${providerId}@example.test`,
    windows: [{
      unlimited: false,
      name: "5H",
      used: 0.2,
      resets_at_ms: 1_800_000_000_000,
      reset_secs: 18_000,
      display,
      amount: 2,
      limit: 10,
      unit: "calls",
      family: "usage",
      pool: "primary",
    }],
    balance: "",
    error: "",
    as_of_ms: 1_800_000_000_000,
    read_at_ms: 1_800_000_000_000,
  };
}

const allQuota: QuotaResponse = {
  reading: false,
  items: [provider("codex", "Codex", "Codex Pro 20x"), provider("gemini", "Gemini", "Gemini Pro")],
};
const codexQuota: QuotaResponse = { ...allQuota, items: allQuota.items.slice(0, 1) };
const status: RuntimeStatusResponse = { status_revision: 1, runtime_state: "ready", last_error_code: null };

function quotaView(overrides: Partial<DashboardQuotaControllerView> = {}): DashboardQuotaControllerView {
  return {
    quota: codexQuota,
    refreshing: false,
    refresh_error: false,
    refresh_available: true,
    refresh: vi.fn(),
    ...overrides,
  };
}

const revisionFeed = {
  get_snapshot: () => null as RevisionTuple | null,
  subscribe: () => () => undefined,
  retry_now: () => undefined,
  dispose: () => undefined,
} as unknown as RevisionFeedType;

function fakeClient(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    quota: vi.fn(async () => allQuota),
    refreshQuota: vi.fn(async () => codexQuota),
    filterOptions: vi.fn(async () => ({ data_revision: 1, agents: [], models: [], projects: [] })),
    summary: vi.fn(async () => ({ data_revision: 1, usage, request_metrics: { calls: 0, errors: 0, retried: 0, output_speed: null, speed_samples: 0, mean_ttft_seconds: null, ttft_samples: 0 }, session_count: 1, session_median: null, session_p90: null })),
    modelDistribution: vi.fn(async () => ({ data_revision: 1, items: [] })),
    projectDistribution: vi.fn(async () => ({ data_revision: 1, items: [] })),
    getSessions: vi.fn(async () => ({ data_revision: 1, summary: { data_revision: 1, usage, request_metrics: { calls: 0, errors: 0, retried: 0, output_speed: null, speed_samples: 0, mean_ttft_seconds: null, ttft_samples: 0 }, session_count: 1, session_median: null, session_p90: null }, items: [] })),
    toolUsage: vi.fn(async (days) => ({ days, calls: 0, top: [] })),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(async () => status),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    ...overrides,
  };
}

function viewFor(overrides: Partial<TrayPanelViewModel> = {}): TrayPanelViewModel {
  return {
    range: { key: "today" },
    metrics: usage,
    session_count: 1,
    session_median: null,
    session_p90: null,
    load_state: "ready",
    refreshing: false,
    error_code: undefined,
    select_range: vi.fn((_range: DashboardRange) => undefined),
    request_refresh: vi.fn(),
    retry_load: vi.fn(),
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
  vi.restoreAllMocks();
});

describe("TrayPanelPage Spec04", () => {
  it("keeps the existing KPI layout and shared quota card", () => {
    enableTiltEffects();
    const quota = quotaView();
    renderWithTheme(<TrayPanelView view={viewFor()} quota={quota} stopping={false} onOpenDashboard={vi.fn()} onStop={vi.fn()} />);
    const grid = screen.getByLabelText("KPI 指标");
    expect(grid).toHaveClass("grid", "grid-cols-[304px_304px]", "gap-4");
    expect(grid.children).toHaveLength(4);
    expect(Array.from(grid.children, (card) => glareOverlayCount(card as HTMLElement))).toEqual([1, 0, 0, 0]);
    expect(within(grid.children[0] as HTMLElement).getByText("总 Token")).toBeInTheDocument();
    expect(within(grid.children[1] as HTMLElement).getByText("账户额度")).toBeInTheDocument();
    expect(within(grid.children[2] as HTMLElement).getByText("缓存命中")).toBeInTheDocument();
    expect(within(grid.children[3] as HTMLElement).getByText("等价费用")).toBeInTheDocument();
    expect(grid.children[1]).toHaveClass("h-[144px]");
  });

  it("uses one provider-agnostic quota snapshot and POSTs only on the manual quota action", async () => {
    const client = fakeClient();
    const stop = vi.fn(async () => "stopped" as const);
    const ipc = vi.fn();
    (window as Window & { ipc?: unknown }).ipc = { postMessage: ipc };
    renderWithTheme(<TrayPanelPage options={{ client, revisionFeed, serviceClient: { getState: vi.fn(async () => "running" as const), stop } }} />);

    await waitFor(() => expect(screen.getByText("总 Token")).toBeInTheDocument());
    await waitFor(() => expect(client.quota).toHaveBeenCalledTimes(1));
    expect(screen.queryByText(/Gemini·/)).not.toBeInTheDocument();
    expect(screen.getAllByRole("tab").map((tab) => tab.textContent?.trim())).toEqual(["今天", "昨天", "7d"]);
    expect(screen.getByText("缓存命中")).toBeInTheDocument();
    expect(screen.getByText("等价费用")).toBeInTheDocument();

    const refreshData = screen.getByRole("button", { name: "刷新数据" });
    const refreshQuota = screen.getByRole("button", { name: "刷新账户额度" });
    fireEvent.click(refreshData);
    await waitFor(() => expect(client.getSessions).toHaveBeenCalledTimes(2));
    expect(client.refreshQuota).not.toHaveBeenCalled();
    fireEvent.click(refreshQuota);
    await waitFor(() => expect(client.refreshQuota).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "打开 Dashboard" }));
    expect(ipc).toHaveBeenCalledWith("open-dashboard");
    const stopButton = screen.getByRole("button", { name: "停止服务" });
    fireEvent.click(stopButton);
    expect(stop).toHaveBeenCalledTimes(1);
    expect(stopButton).toBeDisabled();
  });

  it("restores the stop button and reports a toast when stopping fails", async () => {
    const stop = vi.fn(async () => { throw new Error("stop failed"); });
    renderWithTheme(<TrayPanelPage options={{ client: fakeClient(), revisionFeed, serviceClient: { getState: vi.fn(async () => "running" as const), stop } }} />);
    await waitFor(() => expect(screen.getByText("总 Token")).toBeInTheDocument());
    const stopButton = screen.getByRole("button", { name: "停止服务" });
    fireEvent.click(stopButton);
    await waitFor(() => expect(screen.getByText("停止服务失败")).toBeInTheDocument());
    expect(stopButton).not.toBeDisabled();
  });

  it("recovers a failed data read through the retry control", async () => {
    const sessions = vi.fn()
      .mockRejectedValueOnce(new Error("Sessions unavailable"))
      .mockResolvedValue({ data_revision: 1, summary: { data_revision: 1, usage, request_metrics: { calls: 0, errors: 0, retried: 0, output_speed: null, speed_samples: 0, mean_ttft_seconds: null, ttft_samples: 0 }, session_count: 1, session_median: null, session_p90: null }, items: [] });
    const client = fakeClient({
      getSessions: sessions,
      getStatus: vi.fn(async () => ({ status_revision: 1, runtime_state: "starting" as const, last_error_code: null })),
    });
    renderWithTheme(<TrayPanelPage options={{ client, revisionFeed }} />);
    await waitFor(() => expect(screen.getByText("数据加载失败")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(screen.getByText("总 Token")).toBeInTheDocument());
    expect(sessions).toHaveBeenCalledTimes(2);
  });

  it("renders skeleton cards while metrics are unavailable", () => {
    renderWithTheme(
      <TrayPanelView
        view={viewFor({ metrics: null, load_state: "loading" })}
        quota={quotaView({ quota: { reading: true, items: [] } })}
        stopping={false}
        onOpenDashboard={vi.fn()}
        onStop={vi.fn()}
      />,
    );
    const grid = screen.getByLabelText("KPI 加载中");
    expect(grid.children).toHaveLength(4);
    expect(grid.children[0]).toHaveClass("h-36");
    expect(grid.children[1]).toHaveClass("h-[144px]");
    expect(screen.getByText("正在读取账户额度…")).toBeInTheDocument();
  });
});
