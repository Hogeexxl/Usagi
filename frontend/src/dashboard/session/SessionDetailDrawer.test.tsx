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

const zeroUsage: UsageDto = {
  ...usage(0, 0),
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
};

function thread(overrides: Partial<SessionThreadDto> = {}): SessionThreadDto {
  return {
    key: { agent: "codex", session_id: "root-session-full-id", thread_id: "root-session-full-id" },
    type: "main",
    parent_thread_id: null,
    title: "A long Session title",
    nickname: "",
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
  last_activity_ms: Date.UTC(2026, 7, 12, 8),
  thread: thread(),
  self_usage: usage(1_801, 0.6),
  subagent_usage: usage(1_800, 0.6),
  unknown_usage: zeroUsage,
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
  unknown_usage: detail.unknown_usage,
  inclusive_usage: detail.inclusive_usage,
  has_usage_rows: true,
  last_activity_ms: detail.last_activity_ms,
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
  it("opens the unpriced CostValue prompt on pointer hover", () => {
    const inclusiveUsage = usage(3_601, 1.2, 1);
    render(
      <SessionDetailDrawer
        view={view({
          detail: { ...detail, inclusive_usage: inclusiveUsage },
          selected_row: { ...row, inclusive_usage: inclusiveUsage },
        })}
        timezone={timezone}
      />,
    );

    const trigger = screen.getByRole("button", { name: "未计价使用记录提示" });
    fireEvent.pointerEnter(trigger, { pointerId: 1, pointerType: "mouse", buttons: 0 });
    expect(trigger).toHaveAttribute("aria-expanded", "true");
  });

  it("renders the existing receipt shell from the new inclusive DTO", () => {
    const rendered = render(<SessionDetailDrawer view={view()} timezone={timezone} />);

    const dialog = screen.getByRole("dialog", { name: "Session 详情" });
    expect(dialog).toHaveClass("w-[480px]", "max-[480px]:w-screen", "[contain:layout_paint]");
    expect(screen.getByRole("heading", { name: "A long Session title" })).toBeInTheDocument();
    expect(rendered.container.querySelector('[aria-label="Session nickname"]')).toBeNull();
    expect(screen.getByText("root-session-full-id")).toBeInTheDocument();

    const summary = screen.getByRole("region", { name: "Session 合计" });
    expect(summary).toHaveTextContent("Agent");
    expect(summary).toHaveTextContent("Project");
    expect(summary).toHaveTextContent("Main Tokens");
    expect(summary).toHaveTextContent("Subagent Tokens");
    expect(summary).toHaveTextContent("Total Tokens");
    expect(summary).toHaveTextContent("1.8K");
    const totalTokens = summary.querySelector('dd[aria-label="Total Tokens：3,601"]');
    expect(totalTokens).toHaveTextContent("3.6K");
    expect(totalTokens?.querySelector("span")).toHaveAttribute("title", "3,601");
    expect(summary).toHaveTextContent("$1.20");
    expect(screen.queryByText(/复制/)).not.toBeInTheDocument();
  });

  it("keeps the parent title primary and uses child nickname and model groups in its header", () => {
    const namedThread = thread({ title: "Task title", nickname: "Task nickname" });
    const namedChild = child({
      title: "Child task title",
      nickname: "Child nickname",
      model_effort: [
        { model: "Sol", effort: "high", compaction_tokens: 11, usage: usage(101, 0.11) },
        { model: "o4-mini", effort: "low", compaction_tokens: 4, usage: usage(20, 0.02) },
      ],
    });
    const unnamedChild = child({
      key: { agent: "codex", session_id: "root-session-full-id", thread_id: "child-no-name" },
      title: "Title is not a nickname",
      nickname: "",
      model_effort: [],
    });
    const namedDetail = { ...detail, thread: namedThread, children: [namedChild, unnamedChild] };
    const rendered = render(
      <SessionDetailDrawer
        view={view({ detail: namedDetail, selected_row: { ...row, thread: namedThread } })}
        timezone={timezone}
      />,
    );

    expect(screen.getByRole("heading", { name: "Task title" })).toBeInTheDocument();
    expect(rendered.container.querySelector('[aria-label="Session nickname"]')).toHaveTextContent("Task nickname");
    const childButton = screen.getByRole("button", { name: /Child nickname.*Sol \(high\), o4-mini \(low\)/ });
    expect(childButton).not.toHaveTextContent("Child task title");
    fireEvent.click(childButton);
    const childRegion = screen.getByRole("region", { name: /Child nickname/ });
    expect(within(childRegion).getByText("Child task title")).toBeInTheDocument();
    expect(within(childRegion).getAllByRole("term").slice(0, 3).map((term) => term.textContent)).toEqual([
      "id",
      "Title",
      "Last Active",
    ]);
    expect(within(childRegion).getAllByText("Model")).toHaveLength(2);

    const unnamedButton = screen.getByRole("button", { name: /未命名子代理.*—/ });
    expect(unnamedButton).not.toHaveTextContent("Title is not a nickname");
  });

  it("uses detail activity for metadata-only headers and updates after refresh", () => {
    const metadataThread = thread({ last_usage_ms: 0, model_effort: [], usage: zeroUsage });
    const firstActivity = Date.UTC(2026, 7, 12, 7, 45);
    const metadataDetail: SessionDetailResponse = {
      ...detail,
      thread: metadataThread,
      self_usage: zeroUsage,
      subagent_usage: zeroUsage,
      unknown_usage: zeroUsage,
      inclusive_usage: zeroUsage,
      children: [],
      last_activity_ms: firstActivity,
    };
    const metadataRow: SessionListItemDto = { ...row, thread: metadataThread, last_activity_ms: Date.UTC(2026, 7, 12, 6) };
    const rendered = render(
      <SessionDetailDrawer
        view={view({ detail: metadataDetail, selected_row: metadataRow })}
        timezone={timezone}
      />,
    );

    const headerTime = rendered.container.querySelector("time");
    expect(headerTime).toHaveTextContent("08-12 15:45");
    expect(headerTime).toHaveAttribute("datetime", new Date(firstActivity).toISOString());

    const refreshedActivity = Date.UTC(2026, 7, 12, 9, 30);
    rendered.rerender(
      <SessionDetailDrawer
        view={view({ detail: { ...metadataDetail, last_activity_ms: refreshedActivity }, selected_row: metadataRow, load_state: "ready" })}
        timezone={timezone}
      />,
    );
    expect(headerTime).toHaveTextContent("08-12 17:30");
    expect(headerTime).toHaveAttribute("datetime", new Date(refreshedActivity).toISOString());
  });

  it("renders missing header and child activity without Unix epoch dates", () => {
    const noActivityThread = thread({ last_usage_ms: 0, model_effort: [] });
    const noActivityChild = child({ title: "Unknown activity", nickname: "Time helper", last_usage_ms: 0 });
    const noActivityDetail: SessionDetailResponse = {
      ...detail,
      thread: noActivityThread,
      children: [noActivityChild],
      last_activity_ms: null,
    };
    const rendered = render(
      <SessionDetailDrawer
        view={view({ detail: noActivityDetail, selected_row: { ...row, thread: noActivityThread, last_activity_ms: null } })}
        timezone={timezone}
      />,
    );

    const headerTime = rendered.container.querySelector("time");
    expect(headerTime).toHaveTextContent("—");
    expect(headerTime).not.toHaveAttribute("datetime");
    fireEvent.click(screen.getByRole("button", { name: /Time helper.*Sol \(high\)/ }));
    const childRegion = screen.getByRole("region", { name: /Time helper/ });
    const lastActive = within(childRegion).getByText("Last Active");
    expect(lastActive.nextElementSibling).toHaveTextContent("—");
    expect(lastActive.nextElementSibling).toHaveAttribute("title", "未知");
    expect(childRegion).not.toHaveTextContent("1970");
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
    expect(screen.getByText("未知会话")).toBeInTheDocument();
    const visibleId = screen.getByText("a".repeat(96));
    expect(visibleId).toHaveClass("min-w-0", "break-all");
    fireEvent.click(screen.getByRole("button", { name: /未命名子代理.*Sol \(high\)/ }));
    const childRegion = screen.getByRole("region", { name: /未命名子代理.*Sol \(high\)/ });
    expect(childRegion).toHaveTextContent("a".repeat(96));
    expect(childRegion).not.toHaveTextContent("antigravity:");
  });

  it("shows one workspace label and keeps the original path on the project value", () => {
    const path = "/private/tmp/claude-501/child";
    const workspaceThread = thread({ project: { kind: "workspace", path } });
    const workspaceDetail = { ...detail, thread: workspaceThread };
    const workspaceRow: SessionListItemDto = { ...row, thread: workspaceThread };
    render(
      <SessionDetailDrawer
        view={view({ detail: workspaceDetail, selected_row: workspaceRow, selected_thread_key_id: threadKeyId(workspaceThread.key) })}
        timezone={timezone}
      />,
    );

    expect(screen.getByText("临时工作区")).toHaveAttribute("title", path);
  });

  it("starts accordions collapsed, opens actual child DTOs, and preserves distinct ThreadKeys", () => {
    const duplicated = detail.children.map((item) => ({ ...item, title: "Same title", nickname: "Same nickname" }));
    render(<SessionDetailDrawer view={view({ detail: { ...detail, children: duplicated } })} timezone={timezone} />);

    const mainButtons = screen.getAllByRole("button", { name: /gpt-5|o4-mini/ });
    expect(mainButtons).toHaveLength(2);
    expect(screen.queryByText("Cache Write 1h")).not.toBeInTheDocument();
    fireEvent.click(mainButtons[0]);
    expect(screen.getAllByText("Cache Write 1h").length).toBeGreaterThan(0);
    fireEvent.click(mainButtons[1]);
    expect(screen.getAllByText("Cache Write 1h").length).toBeGreaterThan(0);

    const childButtons = screen.getAllByRole("button", { name: /Same nickname/ });
    expect(childButtons).toHaveLength(2);
    fireEvent.click(childButtons[0]);
    expect(screen.getByText("child-thread-1")).toBeInTheDocument();
    fireEvent.click(childButtons[1]);
    expect(screen.getByText("child-thread-2")).toBeInTheDocument();
  });

  it("does not repeat a parent ID in the Codex child receipt", () => {
    const mainId = "01a135e4-9b71-7fa1-8bb0-2dd2bdaf5497";
    const childId = "01a138ab-909f-7990-a365-9555077faea2";
    const codexMain = thread({
      key: { agent: "codex", session_id: mainId, thread_id: mainId },
      title: "Codex main",
    });
    const codexChild = child({
      key: { agent: "codex", session_id: mainId, thread_id: childId },
      parent_thread_id: mainId,
      title: "Codex child",
      nickname: "Codex helper",
    });
    const codexDetail: SessionDetailResponse = {
      ...detail,
      thread: codexMain,
      children: [codexChild],
    };
    const codexRow: SessionListItemDto = { ...row, thread: codexMain };
    const rendered = render(
      <SessionDetailDrawer
        view={view({ detail: codexDetail, selected_row: codexRow, selected_thread_key_id: threadKeyId(codexMain.key) })}
        timezone={timezone}
      />,
    );

    expect(screen.getByRole("heading", { name: "Main (2)" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Codex helper.*Sol \(high\)/ }));
    const childRegion = screen.getByRole("region", { name: /Codex helper.*Sol \(high\)/ });
    expect(codexChild.parent_thread_id).toBe(mainId);
    expect(within(childRegion).queryByText("Parent Thread ID")).not.toBeInTheDocument();
    expect(within(childRegion).queryByText(mainId)).not.toBeInTheDocument();
    expect(within(childRegion).getByText(childId)).toBeInTheDocument();
    expect(rendered.container).toHaveTextContent("Main Tokens");
  });

  it("shows unknown usage totals without attributing a main model", () => {
    const unknownUsage = { ...zeroUsage, calls: 1 };
    const unknownThread = thread({
      key: { agent: "codex", session_id: "01a135e4-9b71-7fa1-8bb0-2dd2bdaf5497", thread_id: "unknown-calls" },
      type: "unknown",
      title: "Unconfirmed calls",
      model_effort: [{ model: "unconfirmed-model", effort: "high", usage: unknownUsage, compaction_tokens: null }],
      usage: zeroUsage,
    });
    const unknownDetail: SessionDetailResponse = {
      ...detail,
      thread: unknownThread,
      self_usage: zeroUsage,
      subagent_usage: zeroUsage,
      unknown_usage: unknownUsage,
      inclusive_usage: unknownUsage,
      children: [],
    };
    const unknownRow: SessionListItemDto = { ...row, thread: unknownThread, unknown_usage: unknownUsage };
    const rendered = render(
      <SessionDetailDrawer
        view={view({ detail: unknownDetail, selected_row: unknownRow, selected_thread_key_id: threadKeyId(unknownThread.key) })}
        timezone={timezone}
      />,
    );

    const summary = screen.getByRole("region", { name: "Session 合计" });
    expect(summary).not.toHaveTextContent("Unknown Tokens");
    expect(summary).not.toHaveTextContent("Unknown Cost");
    expect(summary).not.toHaveTextContent("Main Tokens");
    expect(screen.queryByRole("heading", { name: /Main \(/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "unconfirmed-model (high)" })).not.toBeInTheDocument();

    rendered.rerender(
      <SessionDetailDrawer
        view={view({
          detail: {
            ...unknownDetail,
            unknown_usage: { ...unknownUsage, input: 4, total: 4 },
            inclusive_usage: { ...unknownUsage, input: 4, total: 4 },
          },
          selected_row: unknownRow,
          selected_thread_key_id: threadKeyId(unknownThread.key),
        })}
        timezone={timezone}
      />,
    );
    expect(screen.getByRole("region", { name: "Session 合计" })).toHaveTextContent("Unknown Tokens");
    expect(screen.queryByText("Unknown Cost")).not.toBeInTheDocument();

    rendered.rerender(
      <SessionDetailDrawer
        view={view({
          detail: {
            ...unknownDetail,
            unknown_usage: { ...unknownUsage, cost: 0.02 },
            inclusive_usage: { ...unknownUsage, cost: 0.02 },
          },
          selected_row: unknownRow,
          selected_thread_key_id: threadKeyId(unknownThread.key),
        })}
        timezone={timezone}
      />,
    );
    expect(screen.queryByText("Unknown Tokens")).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Session 合计" })).toHaveTextContent("Unknown Cost");
  });

  it("labels an orphaned known subagent's own model group as Self", () => {
    const orphan = child({ title: "Orphaned subagent" });
    const orphanDetail: SessionDetailResponse = { ...detail, thread: orphan, children: [] };
    const orphanRow: SessionListItemDto = { ...row, thread: orphan };
    render(
      <SessionDetailDrawer
        view={view({ detail: orphanDetail, selected_row: orphanRow, selected_thread_key_id: threadKeyId(orphan.key) })}
        timezone={timezone}
      />,
    );

    expect(screen.getByRole("heading", { name: "Self (1)" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Session 合计" })).toHaveTextContent("Self Tokens");
    expect(screen.getByRole("region", { name: "Session 合计" })).not.toHaveTextContent("Main Tokens");
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
    fireEvent.click(screen.getByRole("button", { name: /未命名子代理.*model-y \(low\)/ }));
    expect(screen.queryByText("Compaction")).not.toBeInTheDocument();
  });

  it("uses compact KPI formatting for every token value and retains exact values", () => {
    const largeUsage: UsageDto = {
      ...usage(2_345_678),
      input: 1_801,
      output: 1_000_000,
      reasoning: 2_000_000,
      cache_read: 3_000_000,
      cache_write: 4_000_000,
      cache_write_1h: 5_000_000,
    };
    const largeThread = thread({
      model_effort: [{ model: "gpt-5", effort: "high", compaction_tokens: 6_000_000, usage: largeUsage }],
    });
    const largeChild = child({
      nickname: "Large child",
      model_effort: [{ model: "Sol", effort: "high", compaction_tokens: 6_000_000, usage: largeUsage }],
    });
    const largeDetail: SessionDetailResponse = {
      ...detail,
      thread: largeThread,
      self_usage: usage(1_801),
      subagent_usage: usage(2_000_000),
      unknown_usage: usage(1_000_000_000),
      inclusive_usage: largeUsage,
      children: [largeChild],
    };
    render(<SessionDetailDrawer view={view({ detail: largeDetail, selected_row: { ...row, thread: largeThread } })} timezone={timezone} />);

    const summary = screen.getByRole("region", { name: "Session 合计" });
    for (const [label, full, compact] of [
      ["Main Tokens", "1,801", "1.8K"],
      ["Subagent Tokens", "2,000,000", "2M"],
      ["Unknown Tokens", "1,000,000,000", "1B"],
      ["Total Tokens", "2,345,678", "2.3M"],
    ]) {
      const value = summary.querySelector(`dd[aria-label="${label}：${full}"]`);
      expect(value).toHaveTextContent(compact);
      expect(value?.querySelector("span")).toHaveAttribute("title", full);
    }

    fireEvent.click(screen.getByRole("button", { name: "gpt-5 (high)" }));
    const mainRegion = screen.getByRole("region", { name: "gpt-5 (high)" });
    for (const [label, full, compact] of [
      ["Total Tokens", "2,345,678", "2.3M"],
      ["Input", "1,801", "1.8K"],
      ["Output", "1,000,000", "1M"],
      ["Reasoning", "2,000,000", "2M"],
      ["Cache Read", "3,000,000", "3M"],
      ["Cache Write", "4,000,000", "4M"],
      ["Cache Write 1h", "5,000,000", "5M"],
      ["Compaction", "6,000,000", "6M"],
    ]) {
      const value = within(mainRegion).getByLabelText(`${label}：${full}`);
      expect(value).toHaveTextContent(compact);
      expect(value).toHaveAttribute("title", full);
    }

    fireEvent.click(screen.getByRole("button", { name: /Large child/ }));
    const childRegion = screen.getByRole("region", { name: /Large child/ });
    expect(within(childRegion).getByLabelText("Input：1,801")).toHaveTextContent("1.8K");
  });

  it("hides zero cache write rows for main and child usage receipts", () => {
    const noCacheWrites = { ...usage(), cache_write: 0, cache_write_1h: 0 };
    const noCacheWriteMain = thread({
      model_effort: [{ model: "gpt-5", effort: "high", compaction_tokens: 0, usage: noCacheWrites }],
    });
    const noCacheWriteChild = child({
      model_effort: [{ model: "o4-mini", effort: "low", compaction_tokens: 37, usage: noCacheWrites }],
    });
    render(
      <SessionDetailDrawer
        view={view({ detail: { ...detail, thread: noCacheWriteMain, children: [noCacheWriteChild] } })}
        timezone={timezone}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "gpt-5 (high)" }));
    const mainRegion = screen.getByRole("region", { name: "gpt-5 (high)" });
    expect(within(mainRegion).queryByText("Cache Write")).not.toBeInTheDocument();
    expect(within(mainRegion).queryByText("Cache Write 1h")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /未命名子代理.*o4-mini \(low\)/ }));
    const childRegion = screen.getByRole("region", { name: /未命名子代理.*o4-mini \(low\)/ });
    expect(within(childRegion).queryByText("Cache Write")).not.toBeInTheDocument();
    expect(within(childRegion).queryByText("Cache Write 1h")).not.toBeInTheDocument();
  });

  it("renders null compaction as unknown and preserves explicit zero", () => {
    const compactionThread = thread({
      model_effort: [
        { model: "gpt-null", effort: "high", compaction_tokens: null, usage: usage() },
        { model: "gpt-zero", effort: "low", compaction_tokens: 0, usage: usage() },
      ],
    });
    const compactionDetail = { ...detail, thread: compactionThread };
    const rendered = render(
      <SessionDetailDrawer
        view={view({ detail: compactionDetail, selected_row: { ...row, thread: compactionThread } })}
        timezone={timezone}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "gpt-null (high)" }));
    fireEvent.click(screen.getByRole("button", { name: "gpt-zero (low)" }));

    expect(rendered.container.querySelector('[aria-label="Compaction：未知"]')).toHaveTextContent("—");
    expect(rendered.container.querySelector('[aria-label="Compaction：0"]')).toHaveTextContent("0");
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
    expect(loading.container.querySelector("time")).toHaveAttribute("datetime", new Date(row.last_activity_ms!).toISOString());
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
    fireEvent.click(screen.getByRole("button", { name: /未命名子代理.*Sol \(high\)/ }));
    const region = screen.getByRole("region", { name: /未命名子代理.*Sol \(high\)/ });
    expect(within(region).getByText("child-thread-1")).toBeInTheDocument();
    expect(within(region).getByText("Recent subagent")).toBeInTheDocument();
    expect(within(region).getByText("Sol (high)")).toBeInTheDocument();
    expect(within(region).getByText("Cache Write 1h")).toBeInTheDocument();
    expect(within(region).getByText("Compaction")).toBeInTheDocument();
  });
});
