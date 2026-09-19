import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { SessionDetailResponse, SessionItemDto, UsageDto } from "../../data/types";
import type { SessionDetailControllerViewModel } from "./useSessionDetailController";
import { SessionDetailDrawer } from "./SessionDetailDrawer";

const usage: UsageDto = {
  input_tokens: 1_234,
  cached_tokens: 12,
  cache_write_tokens: null,
  uncached_input_tokens: 1_222,
  output_tokens: 567,
  reasoning_tokens: 8,
  other_output_tokens: 559,
  total_tokens: 1_801,
  cache_hit_rate: 0.01,
  estimated_cost: 0.5,
  estimated_cost_status: "complete",
};

const subagentModelUsage = [
  {
    model: "Sol",
    reasoning_effort: "high",
    last_activity_at_ms: Date.UTC(2026, 7, 12, 7, 40),
    usage: { ...usage, total_tokens: 101, input_tokens: 70, output_tokens: 31, estimated_cost: 0.11 },
  },
  {
    model: "Sol",
    reasoning_effort: "medium",
    last_activity_at_ms: Date.UTC(2026, 7, 12, 7, 30),
    usage: { ...usage, total_tokens: 202, input_tokens: 140, output_tokens: 62, estimated_cost: 0.22 },
  },
  {
    model: "Luna",
    reasoning_effort: "high",
    last_activity_at_ms: Date.UTC(2026, 7, 12, 7, 20),
    usage: { ...usage, total_tokens: 303, input_tokens: 210, output_tokens: 93, estimated_cost: 0.33 },
  },
  {
    model: "Luna",
    reasoning_effort: "max",
    last_activity_at_ms: Date.UTC(2026, 7, 12, 7, 10),
    usage: { ...usage, total_tokens: 404, input_tokens: 280, output_tokens: 124, estimated_cost: 0.44 },
  },
];

const detail: SessionDetailResponse = {
  range: { key: "today", start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" },
  data_revision: 3,
  root_session_id: "root-session-full-id",
  last_activity_at_ms: Date.UTC(2026, 7, 12, 8),
  main: {
    title: "A long Session title",
    thread_id: "root-session-full-id",
    root_session_id: "root-session-full-id",
    models_used: ["gpt-5", "o4-mini"],
    model_usage: [
      { model: "gpt-5", reasoning_effort: "high", usage },
      { model: "o4-mini", reasoning_effort: null, usage: { ...usage, total_tokens: 200, estimated_cost: 0.1 } },
    ],
    self_usage: { ...usage, total_tokens: 1_801, estimated_cost: 0.6 },
    subagent_count: 2,
    inclusive_usage: { ...usage, total_tokens: 3_601, estimated_cost: 1.2, estimated_cost_status: "partial" },
  },
  subagents: [
    {
      thread_id: "123e4567-e89b-12d3-a456-426614174000",
      parent_thread_id: "root-session-full-id",
      root_session_id: "root-session-full-id",
      title: "Recent subagent",
      last_activity_at_ms: Date.UTC(2026, 7, 12, 7),
      model_usage: subagentModelUsage,
    },
    {
      thread_id: "subagent-old-full-id",
      parent_thread_id: "root-session-full-id",
      root_session_id: "root-session-full-id",
      title: "Old subagent",
      last_activity_at_ms: Date.UTC(2026, 7, 11, 7),
      model_usage: [],
    },
  ],
};

const row: SessionItemDto = {
  root_session_id: detail.root_session_id,
  title: detail.main.title,
  project_name: "Usagi",
  project_path: "/work/Usagi",
  last_activity_at_ms: detail.last_activity_at_ms,
  models_used: detail.main.models_used,
  subagent_count: 2,
  inclusive_usage: detail.main.inclusive_usage,
  self_usage: detail.main.self_usage,
  subagent_usage: usage,
  data_status: "complete",
  error_code: null,
};

function view(overrides: Partial<SessionDetailControllerViewModel> = {}): SessionDetailControllerViewModel {
  return {
    open: true,
    selected_root_session_id: row.root_session_id,
    selected_row: row,
    detail,
    data_revision: 3,
    load_state: "ready",
    error_code: undefined,
    refresh_error_code: undefined,
    open_detail: vi.fn(),
    select_session: vi.fn(),
    close_detail: vi.fn(),
    retry_detail: vi.fn(),
    ...overrides,
  };
}

describe("SessionDetailDrawer v0.2.0", () => {
  it("renders the 480px receipt shell with exactly four summary rows", () => {
    render(<SessionDetailDrawer view={view()} timezone="Asia/Shanghai" />);

    const dialog = screen.getByRole("dialog", { name: "Session 详情" });
    expect(dialog).toHaveClass("w-[480px]", "max-[480px]:w-screen", "[contain:layout_paint]");
    expect(screen.getByRole("heading", { name: "A long Session title" })).toBeInTheDocument();
    const rootSessionId = screen.getByText("root-session-full-id");
    expect(rootSessionId).toBeInTheDocument();
    expect(rootSessionId).not.toHaveAttribute("aria-describedby");
    expect(rootSessionId).not.toHaveAttribute("title");
    expect(screen.queryByRole("button", { name: /刷新当前详情/ })).not.toBeInTheDocument();

    const summary = screen.getByRole("region", { name: "Session 合计" });
    const rows = summary.querySelectorAll("dl > div");
    expect(rows).toHaveLength(4);
    expect(summary).toHaveTextContent("Main Tokens");
    expect(summary).toHaveTextContent("Subagent Tokens");
    expect(summary).toHaveTextContent("Total Tokens");
    expect(summary).toHaveTextContent("Estimated Cost");
    expect(summary).toHaveTextContent("1,801");
    expect(summary).toHaveTextContent("1,800");
    expect(summary).toHaveTextContent("3,601");
    expect(summary).toHaveTextContent("$1.20");
    expect(screen.queryByText(/复制/)).not.toBeInTheDocument();
  });

  it("keeps the backdrop surface free of border and outline layers", () => {
    render(<SessionDetailDrawer view={view()} timezone="Asia/Shanghai" />);

    const backdrop = screen.getByRole("button", { name: "Close" });
    expect(backdrop).toHaveClass("fixed", "inset-0", "bg-black/40", "backdrop-blur-none");
    expect(backdrop).not.toHaveClass("backdrop-blur-sm");
    expect(backdrop.className).not.toMatch(/(?:^|\s)(?:border|outline)(?:[-\s]|$)/);
  });

  it("starts both accordion groups collapsed and enforces single-open within each group", () => {
    render(<SessionDetailDrawer view={view()} timezone="Asia/Shanghai" />);

    const mainFirst = screen.getByRole("button", { name: "gpt-5 (high)" });
    const mainSecond = screen.getByRole("button", { name: "o4-mini (—)" });
    const subFirst = screen.getByRole("button", { name: "Recent subagent" });
    const subSecond = screen.getByRole("button", { name: "Old subagent" });
    const mainFirstContent = document.getElementById(mainFirst.getAttribute("aria-controls") ?? "");
    const subFirstContent = document.getElementById(subFirst.getAttribute("aria-controls") ?? "");

    for (const trigger of [mainFirst, mainSecond, subFirst, subSecond]) {
      expect(trigger).toHaveAttribute("aria-expanded", "false");
    }
    expect(mainFirstContent?.querySelector("dl")).toBeNull();
    expect(subFirstContent?.querySelector("dl")).toBeNull();

    fireEvent.click(mainFirst);
    expect(mainFirst).toHaveAttribute("aria-expanded", "true");
    expect(mainSecond).toHaveAttribute("aria-expanded", "false");
    expect(mainFirstContent?.querySelector("dl")).toBeInTheDocument();
    fireEvent.click(mainSecond);
    expect(mainFirst).toHaveAttribute("aria-expanded", "false");
    expect(mainSecond).toHaveAttribute("aria-expanded", "true");
    expect(mainFirstContent?.querySelector("dl")).toBeInTheDocument();

    fireEvent.click(subFirst);
    expect(subFirst).toHaveAttribute("aria-expanded", "true");
    expect(subSecond).toHaveAttribute("aria-expanded", "false");
    expect(mainSecond).toHaveAttribute("aria-expanded", "true");
    expect(subFirstContent?.querySelector("dl")).toBeInTheDocument();
    fireEvent.click(subSecond);
    expect(subFirst).toHaveAttribute("aria-expanded", "false");
    expect(subSecond).toHaveAttribute("aria-expanded", "true");
    expect(mainSecond).toHaveAttribute("aria-expanded", "true");
    expect(subFirstContent?.querySelector("dl")).toBeInTheDocument();
  });

  it("resets both accordion groups when the selected root Session changes", () => {
    const rendered = render(<SessionDetailDrawer view={view()} timezone="Asia/Shanghai" />);
    fireEvent.click(screen.getByRole("button", { name: "gpt-5 (high)" }));
    fireEvent.click(screen.getByRole("button", { name: "Recent subagent" }));
    expect(screen.getByRole("button", { name: "gpt-5 (high)" })).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("button", { name: "Recent subagent" })).toHaveAttribute("aria-expanded", "true");

    const nextRootSessionId = "next-root-session-id";
    const nextDetail: SessionDetailResponse = {
      ...detail,
      root_session_id: nextRootSessionId,
      main: {
        ...detail.main,
        title: "Next Session title",
        thread_id: nextRootSessionId,
        root_session_id: nextRootSessionId,
      },
      subagents: detail.subagents.map((item) => ({
        ...item,
        parent_thread_id: nextRootSessionId,
        root_session_id: nextRootSessionId,
      })),
    };
    const nextRow: SessionItemDto = {
      ...row,
      root_session_id: nextRootSessionId,
      title: "Next Session title",
    };

    rendered.rerender(
      <SessionDetailDrawer
        view={view({
          selected_root_session_id: nextRootSessionId,
          selected_row: nextRow,
          detail: nextDetail,
        })}
        timezone="Asia/Shanghai"
      />,
    );

    expect(screen.getByRole("heading", { name: "Next Session title" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "gpt-5 (high)" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByRole("button", { name: "Recent subagent" })).toHaveAttribute("aria-expanded", "false");
  });

  it("renders Main model detail through the shared ordered usage receipt", () => {
    render(<SessionDetailDrawer view={view()} timezone="Asia/Shanghai" />);

    fireEvent.click(screen.getByRole("button", { name: "gpt-5 (high)" }));
    const region = screen.getByRole("region", { name: "gpt-5 (high)" });
    const usageReceipt = region.querySelector("dl") as HTMLElement;
    const labels = Array.from(usageReceipt.querySelectorAll("dt"), (node) => node.textContent);
    expect(labels).toEqual([
      "Total Tokens",
      "Input",
      "Output",
      "Reasoning",
      "Cache Read",
      "Cache Write",
      "Cache Hit Rate",
      "Estimated Cost",
    ]);
    expect(usageReceipt).toHaveTextContent("1,801");
    expect(usageReceipt).toHaveTextContent("1,234");
    expect(usageReceipt).toHaveTextContent("567");
    expect(usageReceipt).toHaveTextContent("8");
    expect(usageReceipt).toHaveTextContent("12");
    expect(usageReceipt).toHaveTextContent("—");
    expect(usageReceipt).toHaveTextContent("1.0%");
    expect(usageReceipt).toHaveTextContent("$0.50");
    expect(usageReceipt).not.toHaveTextContent("1234");
  });

  it("renders ordered Subagent model usage blocks with basic identity metadata", () => {
    render(<SessionDetailDrawer view={view()} timezone="Asia/Shanghai" />);

    const trigger = screen.getByRole("button", { name: "Recent subagent" });
    expect(trigger).not.toHaveTextContent("subagent-recent-full-id");
    expect(trigger).not.toHaveTextContent("Sol (high)");

    fireEvent.click(trigger);
    const region = screen.getByRole("region", { name: "Recent subagent" });
    expect(region).toHaveTextContent("Thread ID");
    const metadata = region.querySelector("dl") as HTMLElement;
    const threadId = metadata.querySelector("dt")?.nextElementSibling as HTMLElement;
    expect(threadId.textContent).toBe("123e4567-e89b-12d3-a456-426614174000");
    expect(threadId).toHaveClass("whitespace-nowrap");
    expect(threadId).not.toHaveClass("truncate");
    expect(threadId).not.toHaveAttribute("aria-describedby");
    expect(threadId).not.toHaveAttribute("title");
    expect(region).toHaveTextContent("Last Active");
    expect(Array.from(metadata.querySelectorAll("dt"), (node) => node.textContent)).toEqual(["Thread ID", "Last Active"]);
    expect(metadata).not.toHaveTextContent("Model");

    const lastActive = Array.from(metadata.querySelectorAll("dt"))
      .find((node) => node.textContent === "Last Active")?.nextElementSibling as HTMLElement;
    expect(lastActive).toHaveTextContent(/\d{2}:\d{2}:\d{2}/);
    expect(lastActive).toHaveAttribute("title", expect.stringMatching(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/));

    const modelLabels = Array.from(region.querySelectorAll("dt"))
      .filter((node) => node.textContent === "Model")
      .map((node) => node.nextElementSibling?.textContent);
    expect(modelLabels).toEqual(["Sol (high)", "Sol (medium)", "Luna (high)", "Luna (max)"]);

    const usageReceipts = Array.from(region.querySelectorAll("dl"))
      .filter((node) => node.querySelector("dt")?.textContent === "Total Tokens") as HTMLElement[];
    expect(usageReceipts).toHaveLength(4);
    expect(usageReceipts.map((receipt) => receipt.querySelector("dt")?.nextElementSibling?.textContent)).toEqual([
      "101",
      "202",
      "303",
      "404",
    ]);
  });

  it("keeps duplicate Subagent titles distinct by thread ID", () => {
    const duplicateTitleDetail: SessionDetailResponse = {
      ...detail,
      subagents: detail.subagents.map((item) => ({ ...item, title: "Same subagent title" })),
    };
    render(
      <SessionDetailDrawer
        view={view({ detail: duplicateTitleDetail })}
        timezone="Asia/Shanghai"
      />,
    );

    const triggers = screen.getAllByRole("button", { name: "Same subagent title" });
    expect(triggers).toHaveLength(2);
    expect(triggers[0]).toHaveAttribute("aria-controls");
    expect(triggers[1]).toHaveAttribute("aria-controls");
    expect(triggers[0].getAttribute("aria-controls")).not.toBe(triggers[1].getAttribute("aria-controls"));

    fireEvent.click(triggers[0]);
    expect(triggers[0]).toHaveAttribute("aria-expanded", "true");
    expect(triggers[1]).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(triggers[1]);
    expect(triggers[0]).toHaveAttribute("aria-expanded", "false");
    expect(triggers[1]).toHaveAttribute("aria-expanded", "true");
  });

  it("preserves rendered detail during refresh and reports refresh failure through toast", async () => {
    const rendered = render(
      <SessionDetailDrawer
        view={view({ load_state: "refreshing" })}
        timezone="Asia/Shanghai"
      />,
    );

    expect(screen.getByRole("heading", { name: "A long Session title" })).toBeInTheDocument();
    expect(screen.queryByText("Session 详情加载失败")).not.toBeInTheDocument();
    expect(screen.getByText("3,601")).toBeInTheDocument();
    expect(document.querySelector('[aria-busy="true"]')).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /刷新当前详情/ })).not.toBeInTheDocument();

    rendered.rerender(
      <SessionDetailDrawer
        view={view({ refresh_error_code: "HTTP_ERROR" })}
        timezone="Asia/Shanghai"
      />,
    );
    expect(screen.getByRole("heading", { name: "A long Session title" })).toBeInTheDocument();
    expect(screen.getByText("3,601")).toBeInTheDocument();
    expect(await screen.findByText("详情更新失败")).toBeInTheDocument();
  });

  it("renders the first-load skeleton with Summary, Main, and Subagent sections", () => {
    render(
      <SessionDetailDrawer
        view={view({ detail: null, load_state: "loading" })}
        timezone="Asia/Shanghai"
      />,
    );

    const status = screen.getByRole("status", { name: "Session 详情加载中" });
    const summary = within(status).getByRole("region", { name: "Session 合计加载中" });
    const main = within(status).getByRole("region", { name: "Main 加载中" });
    const subagent = within(status).getByRole("region", { name: "Subagent 加载中" });
    expect(summary.querySelectorAll(".animate-pulse")).toHaveLength(8);
    expect(Array.from(main.querySelectorAll("div.animate-pulse")).filter((node) => node.className.includes("h-[54px]")).length).toBe(2);
    expect(Array.from(subagent.querySelectorAll("div.animate-pulse")).filter((node) => node.className.includes("h-[54px]")).length).toBe(2);
  });

  it("keeps loading/error fallbacks inside the open Drawer and wires retry/close", () => {
    const retry = vi.fn();
    const close = vi.fn();
    const loading = render(
      <SessionDetailDrawer view={view({ detail: null, load_state: "loading", close_detail: close })} timezone="Asia/Shanghai" />,
    );
    expect(screen.getByRole("dialog", { name: "Session 详情" })).toBeInTheDocument();
    expect(screen.getByRole("status", { name: "Session 详情加载中" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "关闭 Session 详情" }));
    expect(close).toHaveBeenCalledTimes(1);
    loading.unmount();

    render(
      <SessionDetailDrawer
        view={view({ detail: null, load_state: "error", error_code: "HTTP_ERROR", retry_detail: retry })}
        timezone="Asia/Shanghai"
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("Session 详情加载失败");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(retry).toHaveBeenCalledTimes(1);
  });

  it("does not open the Close tooltip when the Drawer content scrolls", () => {
    vi.useFakeTimers();
    const requestAnimationFrame = vi
      .spyOn(window, "requestAnimationFrame")
      .mockImplementation((callback) => {
        callback(0);
        return 0;
      });
    try {
      render(<SessionDetailDrawer view={view()} timezone="Asia/Shanghai" />);
      const dialog = screen.getByRole("dialog", { name: "Session 详情" });
      const scrollViewport = dialog.querySelector(".overflow-y-auto");
      if (!(scrollViewport instanceof HTMLElement)) throw new Error("Drawer scroll viewport not found");

      fireEvent.scroll(scrollViewport);
      act(() => {
        vi.advanceTimersByTime(120);
      });

      expect(screen.queryByRole("tooltip", { name: "关闭", hidden: true })).not.toBeInTheDocument();
    } finally {
      requestAnimationFrame.mockRestore();
      vi.useRealTimers();
    }
  });

  it("does not show the Close tooltip when a Subagent accordion opens", () => {
    vi.useFakeTimers();
    const requestAnimationFrame = vi
      .spyOn(window, "requestAnimationFrame")
      .mockImplementation((callback) => {
        callback(0);
        return 0;
      });
    try {
      render(<SessionDetailDrawer view={view()} timezone="Asia/Shanghai" />);
      const subagent = screen.getByRole("button", { name: "Recent subagent" });
      subagent.focus();
      fireEvent.click(subagent);
      act(() => {
        vi.advanceTimersByTime(240);
      });
      expect(screen.queryByRole("tooltip", { hidden: true })).not.toBeInTheDocument();
    } finally {
      requestAnimationFrame.mockRestore();
      vi.useRealTimers();
    }
  });
});
