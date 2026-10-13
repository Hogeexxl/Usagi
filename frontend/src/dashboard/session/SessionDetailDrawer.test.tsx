import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { SessionDetailResponse, SessionListItemDto, SessionThreadDto, UsageDto } from "../../data/types";
import { threadKeyId } from "../../data/usagiClient";
import type { SessionDetailControllerViewModel } from "./useSessionDetailController";
import { SessionDetailDrawer } from "./SessionDetailDrawer";

function usage(total = 1_801, cost = 0.5, unpriced = 0): UsageDto {
  return {
    calls: 2,
    errors: 0,
    input: 1_234,
    output: 567,
    cache_read: 100,
    cache_write: 20,
    cache_write_1h: 10,
    reasoning: 8,
    total,
    cache_hit_rate: 100 / 1_354,
    cost,
    unpriced,
    retried: 0, output_speed: null, speed_samples: 0,
  };
}

function thread(overrides: Partial<SessionThreadDto> = {}): SessionThreadDto {
  return {
    key: { agent: "codex", session_id: "root-session-full-id", thread_id: "root-session-full-id" },
    type: "main",
    parent_thread_id: null,
    title: "A long Session title",
    project: { kind: "project", name: "Usagi", path: "/work/Usagi" },
    first_usage_ms: Date.UTC(2026, 7, 12, 7),
    last_usage_ms: Date.UTC(2026, 7, 12, 8),
    model_effort: [
      { model: "gpt-5", effort: "high", compaction_tokens: 0, usage: usage() },
      { model: "o4-mini", effort: "low", compaction_tokens: 37, usage: usage(200, 0.1) },
    ],
    usage: usage(),
    ...overrides,
  };
}

function child(overrides: Partial<SessionThreadDto> = {}): SessionThreadDto {
  return thread({
    key: { agent: "codex", session_id: "root-session-full-id", thread_id: "child-thread-1" },
    type: "subagent",
    parent_thread_id: "root-session-full-id",
    title: "Recent subagent",
    last_usage_ms: Date.UTC(2026, 7, 12, 7),
    model_effort: [{ model: "Sol", effort: "high", compaction_tokens: 11, usage: usage(101, 0.11) }],
    ...overrides,
  });
}

const detail: SessionDetailResponse = {
  data_revision: 3,
  thread: thread(),
  self_usage: usage(1_801, 0.6),
  subagent_usage: usage(1_800, 0.6),
  inclusive_usage: usage(3_601, 1.2),
  children: [
    child(),
    child({ key: { agent: "codex", session_id: "root-session-full-id", thread_id: "child-thread-2" }, title: "Old subagent", last_usage_ms: Date.UTC(2026, 7, 11, 7), model_effort: [] }),
  ],
};

const row: SessionListItemDto = {
  thread: detail.thread,
  self_usage: detail.self_usage,
  subagent_usage: detail.subagent_usage,
  inclusive_usage: detail.inclusive_usage,
  subagent_count: 2,
};

function view(overrides: Partial<SessionDetailControllerViewModel> = {}): SessionDetailControllerViewModel {
  return {
    open: true,
    selected_thread_key_id: threadKeyId(row.thread.key),
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

const timezone = "Asia/Shanghai";

describe("SessionDetailDrawer Spec04 SessionListItemView", () => {
  it("renders the existing receipt shell from the new inclusive DTO", () => {
    render(<SessionDetailDrawer view={view()} timezone={timezone} />);

    const dialog = screen.getByRole("dialog", { name: "Session 详情" });
    expect(dialog).toHaveClass("w-[480px]", "max-[480px]:w-screen", "[contain:layout_paint]");
    expect(screen.getByRole("heading", { name: "A long Session title" })).toBeInTheDocument();
    expect(screen.getByText("root-session-full-id")).toBeInTheDocument();

    const summary = screen.getByRole("region", { name: "Session 合计" });
    expect(summary).toHaveTextContent("Agent");
    expect(summary).toHaveTextContent("Project");
    expect(summary).toHaveTextContent("Main Tokens");
    expect(summary).toHaveTextContent("Subagent Tokens");
    expect(summary).toHaveTextContent("Total Tokens");
    expect(summary).toHaveTextContent("3,601");
    expect(summary).toHaveTextContent("$1.20");
    expect(screen.queryByText(/复制/)).not.toBeInTheDocument();
  });

  it("uses dynamic agent and project values and keeps long IDs readable", () => {
    const longId = `antigravity:${"a".repeat(96)}`;
    const antigravityDetail: SessionDetailResponse = {
      ...detail,
      thread: thread({
        key: { agent: "antigravity", session_id: longId, thread_id: longId },
        project: { kind: "unknown" },
      }),
      children: [child({ key: { agent: "antigravity", session_id: longId, thread_id: longId } })],
    };
    const agentRow: SessionListItemDto = { ...row, thread: antigravityDetail.thread };
    render(
      <SessionDetailDrawer
        view={view({ detail: antigravityDetail, selected_row: agentRow, selected_thread_key_id: threadKeyId(antigravityDetail.thread.key) })}
        timezone={timezone}
        agentDisplay={(agent) => agent === "antigravity" ? "Antigravity" : agent}
      />,
    );

    expect(screen.getByText("Antigravity")).toBeInTheDocument();
    expect(screen.getByText("未识别项目")).toBeInTheDocument();
    const visibleId = screen.getByText("a".repeat(96));
    expect(visibleId).toHaveClass("min-w-0", "break-all");
    fireEvent.click(screen.getByRole("button", { name: "Recent subagent" }));
    const childRegion = screen.getByRole("region", { name: "Recent subagent" });
    expect(childRegion).toHaveTextContent("a".repeat(96));
    expect(childRegion).not.toHaveTextContent("antigravity:");
  });

  it("starts accordions collapsed, opens actual child DTOs, and preserves distinct ThreadKeys", () => {
    const duplicated = detail.children.map((item) => ({ ...item, title: "Same title" }));
    render(<SessionDetailDrawer view={view({ detail: { ...detail, children: duplicated } })} timezone={timezone} />);

    const mainButtons = screen.getAllByRole("button", { name: /gpt-5|o4-mini/ });
    expect(mainButtons).toHaveLength(2);
    expect(screen.queryByText("Cache Write 1h")).not.toBeInTheDocument();
    fireEvent.click(mainButtons[0]);
    expect(screen.getAllByText("Cache Write 1h").length).toBeGreaterThan(0);
    fireEvent.click(mainButtons[1]);
    expect(screen.getAllByText("Cache Write 1h").length).toBeGreaterThan(0);

    const childButtons = screen.getAllByRole("button", { name: "Same title" });
    expect(childButtons).toHaveLength(2);
    fireEvent.click(childButtons[0]);
    expect(screen.getByText("child-thread-1")).toBeInTheDocument();
    fireEvent.click(childButtons[1]);
    expect(screen.getByText("child-thread-2")).toBeInTheDocument();
  });

  it("renders Usage DTO fields and only shows Codex compaction values", () => {
    const nonCodexDetail: SessionDetailResponse = {
      ...detail,
      thread: thread({
        key: { agent: "aider", session_id: "session-a", thread_id: "thread-a" },
        model_effort: [{ model: "model-x", effort: "custom", compaction_tokens: 99, usage: usage(20, 0.2, 1) }],
      }),
      children: [child({ key: { agent: "aider", session_id: "session-a", thread_id: "child-a" }, model_effort: [{ model: "model-y", effort: "low", compaction_tokens: 88, usage: usage(10, 0.1, 1) }] })],
    };
    render(<SessionDetailDrawer view={view({ detail: nonCodexDetail, selected_row: { ...row, thread: nonCodexDetail.thread } })} timezone={timezone} />);

    fireEvent.click(screen.getByRole("button", { name: "model-x (custom)" }));
    expect(screen.getByText("Cache Read")).toBeInTheDocument();
    expect(screen.getByText("Cache Write")).toBeInTheDocument();
    expect(screen.queryByText("Compaction")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "未计价使用记录提示" }));
    expect(screen.getByText("1 条使用记录未计价")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Recent subagent" }));
    expect(screen.queryByText("Compaction")).not.toBeInTheDocument();
  });

  it("resets opened receipts when the complete selected ThreadKey changes", () => {
    const rendered = render(<SessionDetailDrawer view={view()} timezone={timezone} />);
    fireEvent.click(screen.getByRole("button", { name: "gpt-5 (high)" }));
    expect(screen.getByText("Compaction")).toBeInTheDocument();

    const nextThread = thread({ key: { agent: "codex", session_id: "other-session", thread_id: "same-thread-id" } });
    const nextRow: SessionListItemDto = { ...row, thread: nextThread };
    rendered.rerender(
      <SessionDetailDrawer
        view={view({ selected_row: nextRow, selected_thread_key_id: threadKeyId(nextThread.key), detail: { ...detail, thread: nextThread } })}
        timezone={timezone}
      />,
    );
    expect(screen.queryByText("Compaction")).not.toBeInTheDocument();
  });

  it("keeps loading and retry fallbacks inside the open drawer and wires close", () => {
    const retryDetail = vi.fn();
    const closeDetail = vi.fn();
    const loading = render(<SessionDetailDrawer view={view({ detail: null, load_state: "loading" })} timezone={timezone} />);
    expect(screen.getByRole("status", { name: "Session 详情加载中" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Session 合计加载中" })).toBeInTheDocument();
    loading.rerender(<SessionDetailDrawer view={view({ detail: null, load_state: "error", retry_detail: retryDetail, close_detail: closeDetail })} timezone={timezone} />);
    expect(screen.getByRole("alert")).toHaveTextContent("Session 详情加载失败");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    fireEvent.click(screen.getByRole("button", { name: "关闭 Session 详情" }));
    expect(retryDetail).toHaveBeenCalledTimes(1);
    expect(closeDetail).toHaveBeenCalledTimes(1);
  });

  it("preserves detail during a failed refresh and reports the failure", () => {
    const rendered = render(<SessionDetailDrawer view={view({ load_state: "refreshing" })} timezone={timezone} />);
    expect(screen.getByRole("heading", { name: "A long Session title" })).toBeInTheDocument();
    rendered.rerender(<SessionDetailDrawer view={view({ load_state: "ready", refresh_error_code: "UPSTREAM_FAILED" })} timezone={timezone} />);
    expect(screen.getByRole("heading", { name: "A long Session title" })).toBeInTheDocument();
    expect(screen.getByText("详情更新失败")).toBeInTheDocument();
  });

  it("renders a child receipt from child identity, time, and per-model usage", () => {
    render(<SessionDetailDrawer view={view()} timezone={timezone} />);
    fireEvent.click(screen.getByRole("button", { name: "Recent subagent" }));
    const region = screen.getByRole("region", { name: "Recent subagent" });
    expect(within(region).getByText("child-thread-1")).toBeInTheDocument();
    expect(within(region).getByText("Sol (high)")).toBeInTheDocument();
    expect(within(region).getByText("Cache Write 1h")).toBeInTheDocument();
    expect(within(region).getByText("Compaction")).toBeInTheDocument();
  });
});
