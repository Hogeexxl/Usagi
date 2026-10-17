import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { App } from "@modelcontextprotocol/ext-apps";
import { afterEach, describe, expect, it, vi } from "vitest";

import packageJson from "../../package.json";

const mocks = vi.hoisted(() => ({
  useApp: vi.fn(),
  useHostStyles: vi.fn(),
  dashboardPage: vi.fn(),
  createRevisionFeed: vi.fn(),
}));

vi.mock("@modelcontextprotocol/ext-apps/react", () => ({
  useApp: mocks.useApp,
  useHostStyles: mocks.useHostStyles,
}));

vi.mock("../dashboard/DashboardPage", () => ({
  DashboardPage: (props: { options: Record<string, unknown> }) => {
    mocks.dashboardPage(props);
    return <div data-testid="shared-dashboard">Dashboard</div>;
  },
}));

vi.mock("../data/revisionFeed", () => ({ createRevisionFeed: mocks.createRevisionFeed }));

import { McpDashboardApp } from "./McpDashboardApp";

type ToolParams = { name: string; arguments?: Record<string, unknown> };
type HostContext = { theme?: string; styles?: { variables?: Record<string, string> } };
type HostContextListener = (context: HostContext) => void;

function readyResult(payload: unknown = null) {
  return { structuredContent: { status: "ready", payload } };
}

function notRunningResult() {
  return { structuredContent: { status: "not_running" } };
}

function fakeApp(
  call: (params: ToolParams) => Promise<unknown> | unknown,
  theme = "light",
) {
  let context: HostContext = { theme };
  const listeners = new Set<HostContextListener>();
  const app = {
    callServerTool: vi.fn((params: ToolParams) => call(params)),
    getHostContext: vi.fn(() => context),
    addEventListener: vi.fn((_type: string, listener: HostContextListener) => listeners.add(listener)),
    removeEventListener: vi.fn((_type: string, listener: HostContextListener) => listeners.delete(listener)),
  } as unknown as App;

  return {
    app,
    emitHostContext(next: HostContext) {
      context = next;
      for (const listener of listeners) listener(next);
    },
  };
}

function useConnectedApp(app: App) {
  mocks.useApp.mockReturnValue({ app, isConnected: true, error: null });
}

function dashboardOptions() {
  const call = mocks.dashboardPage.mock.calls.at(-1)?.[0] as { options: Record<string, unknown> } | undefined;
  if (!call) throw new Error("DashboardPage has not rendered");
  return call.options;
}

afterEach(() => {
  document.documentElement.classList.remove("dark");
  vi.resetAllMocks();
});

describe("McpDashboardApp shell", () => {
  it("probes after bridge connection and injects the shared DashboardPage options", async () => {
    let resolveProbe!: (result: unknown) => void;
    const probe = new Promise<unknown>((resolve) => { resolveProbe = resolve; });
    const host = fakeApp(() => probe);
    useConnectedApp(host.app);

    render(<McpDashboardApp />);
    await waitFor(() => expect(host.app.callServerTool).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("status")).toHaveTextContent("正在检测 Usagi…");
    expect(host.app.callServerTool).toHaveBeenCalledWith({
      name: "usagi.dashboard.read",
      arguments: { operation: "probe" },
    });

    resolveProbe(readyResult());
    expect(await screen.findByTestId("shared-dashboard")).toBeInTheDocument();
    expect(mocks.useApp).toHaveBeenCalledWith({
      appInfo: { name: "usagi-dashboard", version: packageJson.version },
      capabilities: { availableDisplayModes: ["fullscreen"] },
    });
    expect(mocks.useHostStyles).toHaveBeenCalledWith(host.app, { theme: "light" });
    expect(dashboardOptions()).toMatchObject({
      client: expect.any(Object),
      serviceClient: expect.any(Object),
      revisionMode: "poll",
      pollIntervalMs: 5000,
      showThemeToggle: false,
    });
    expect(dashboardOptions()).not.toHaveProperty("revisionFeed");
    expect(mocks.createRevisionFeed).not.toHaveBeenCalled();
  });

  it("shows the fixed not-running message and retries the probe", async () => {
    const host = fakeApp(vi.fn()
      .mockResolvedValueOnce(notRunningResult())
      .mockResolvedValueOnce(readyResult()));
    useConnectedApp(host.app);

    render(<McpDashboardApp />);
    expect(await screen.findByText("Usagi 未启动")).toBeInTheDocument();
    expect(screen.getByText("请先启动 Usagi，然后重新检测。")).toBeInTheDocument();
    expect(screen.queryByTestId("shared-dashboard")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "重新检测" }));
    expect(await screen.findByTestId("shared-dashboard")).toBeInTheDocument();
    expect(host.app.callServerTool).toHaveBeenCalledTimes(2);
  });

  it("returns from bridge_error to probing when the user retries", async () => {
    const host = fakeApp(vi.fn()
      .mockRejectedValueOnce(new Error("bridge unavailable"))
      .mockResolvedValueOnce(readyResult()));
    useConnectedApp(host.app);

    render(<McpDashboardApp />);
    fireEvent.click(await screen.findByRole("button", { name: "重试" }));
    expect(await screen.findByTestId("shared-dashboard")).toBeInTheDocument();
    expect(host.app.callServerTool).toHaveBeenCalledTimes(2);
  });

  it("recreates the official bridge hook after a connection initialization error", async () => {
    const host = fakeApp(async () => readyResult());
    let connection: { app: App | null; isConnected: boolean; error: Error | null } = {
      app: null,
      isConnected: false,
      error: new Error("initialize failed"),
    };
    mocks.useApp.mockImplementation(() => connection);

    render(<McpDashboardApp />);
    const retry = await screen.findByRole("button", { name: "重试" });
    connection = { app: host.app, isConnected: true, error: null };
    fireEvent.click(retry);
    expect(await screen.findByTestId("shared-dashboard")).toBeInTheDocument();
    expect(host.app.callServerTool).toHaveBeenCalledTimes(1);
  });

  it("returns to not_running when a ready client reports that runtime disappeared", async () => {
    const host = fakeApp(vi.fn()
      .mockResolvedValueOnce(readyResult())
      .mockResolvedValueOnce(notRunningResult()));
    useConnectedApp(host.app);

    render(<McpDashboardApp />);
    await screen.findByTestId("shared-dashboard");
    await act(async () => {
      await expect((dashboardOptions().client as { getRevision: () => Promise<unknown> }).getRevision())
        .rejects.toMatchObject({ code: "HTTP_ERROR", status: 0 });
    });
    expect(await screen.findByText("Usagi 未启动")).toBeInTheDocument();
  });

  it("syncs initial and changed host themes and enters not_running after service stop", async () => {
    const host = fakeApp((params) => {
      if (params.arguments?.operation === "probe") return readyResult();
      return readyResult({ state: "stopped" });
    }, "dark");
    useConnectedApp(host.app);

    render(<McpDashboardApp />);
    await screen.findByTestId("shared-dashboard");
    expect(document.documentElement).toHaveClass("dark");

  act(() => host.emitHostContext({ styles: { variables: {} } }));
  expect(document.documentElement).toHaveClass("dark");

    act(() => host.emitHostContext({ theme: "light" }));
    expect(document.documentElement).not.toHaveClass("dark");

    await act(async () => {
      await (dashboardOptions().serviceClient as { stop: () => Promise<unknown> }).stop();
    });
    expect(host.app.callServerTool).toHaveBeenLastCalledWith({
      name: "usagi.dashboard.stop_service",
      arguments: {},
    });
    expect(await screen.findByText("Usagi 未启动")).toBeInTheDocument();
  });
});
