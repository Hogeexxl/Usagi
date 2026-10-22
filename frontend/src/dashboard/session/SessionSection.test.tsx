import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

import type { SessionListItemDto, UsageDto } from "../../data/types";
import type { SessionTableViewModel } from "./sessionTypes";
import type { SessionDetailControllerViewModel } from "./useSessionDetailController";
import { SessionSection } from "./SessionSection";

const offsetHeightDescriptor = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  "offsetHeight",
);
const offsetWidthDescriptor = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  "offsetWidth",
);

function inlinePixels(element: HTMLElement, property: "height" | "width"): number | null {
  const value = element.style.getPropertyValue(property).trim();
  if (!value.endsWith("px")) return null;
  const pixels = Number.parseFloat(value);
  return Number.isFinite(pixels) ? pixels : null;
}

function descriptorPixels(element: HTMLElement, descriptor: PropertyDescriptor | undefined): number {
  if (descriptor?.get) return Number(descriptor.get.call(element)) || 0;
  return typeof descriptor?.value === "number" ? descriptor.value : 0;
}

function restoreDimension(name: "offsetHeight" | "offsetWidth", descriptor: PropertyDescriptor | undefined) {
  if (descriptor) {
    Object.defineProperty(HTMLElement.prototype, name, descriptor);
  } else {
    Reflect.deleteProperty(HTMLElement.prototype, name);
  }
}

const usage: UsageDto = {
  calls: 2,
  errors: 0,
  input: 1_234,
  output: 567,
  cache_read: 100,
  cache_write: 20,
  cache_write_1h: 10,
  reasoning: 12,
  total: 1_921,
  cache_hit_rate: 100 / 1354,
  cost: 1.25,
  unpriced: 0,
  retried: 0, output_speed: null, speed_samples: 0,
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

const item: SessionListItemDto = {
  thread: {
    key: { agent: "codex", session_id: "session-1", thread_id: "root-1" },
    type: "main",
    parent_thread_id: null,
    title: null,
    nickname: "",
    project: { kind: "project", name: "Usagi", path: "/work/Usagi" },
    first_usage_ms: Date.UTC(2026, 7, 10, 8, 0),
    last_usage_ms: Date.UTC(2026, 7, 10, 8, 9),
    model_effort: [
      { model: "gpt-5", effort: "high", usage, compaction_tokens: null },
      { model: "o4-mini", effort: "low", usage, compaction_tokens: null },
    ],
    usage,
  },
  subagent_count: 2,
  inclusive_usage: { ...usage, total: 5_678 },
  self_usage: { ...usage, total: 1_234 },
  subagent_usage: usage,
  unknown_usage: zeroUsage,
  has_usage_rows: true,
  last_activity_ms: Date.UTC(2026, 7, 10, 8, 9),
};

function view(overrides: Partial<SessionTableViewModel> = {}): SessionTableViewModel {
  return {
    range: { key: "today" },
    rows: [item],
    timezone: "Asia/Shanghai",
    load_state: "ready",
    page_state: "idle",
    filters: { agents: [], models: [], projects: [] },
    page: 1,
    total_items: 1,
    total_pages: 1,
    sort_by: "last_activity",
    sort_order: "desc",
    retry_load: vi.fn(),
    go_to_page: vi.fn(),
    previous_page: vi.fn(),
    next_page: vi.fn(),
    select_sort: vi.fn(),
    retry_page: vi.fn(),
    ...overrides,
  };
}

function row(index: number, overrides: Partial<SessionListItemDto> = {}): SessionListItemDto {
  return {
    ...item,
    thread: {
      ...item.thread,
      key: { ...item.thread.key, session_id: `session-${index}`, thread_id: `root-${index}` },
      title: `Session ${index}`,
      last_usage_ms: item.thread.last_usage_ms + index,
    },
    ...overrides,
  };
}

function detailView(openDetail: (row: SessionListItemDto) => void): SessionDetailControllerViewModel {
  return {
    open: false,
    selected_thread_key_id: null,
    selected_row: null,
    detail: null,
    load_state: "closed",
    open_detail: openDetail,
    select_session: vi.fn(),
    close_detail: vi.fn(),
    retry_detail: vi.fn(),
  };
}

function tableViewport(container: HTMLElement): HTMLElement {
  const table = container.querySelector("table");
  if (!(table?.parentElement instanceof HTMLElement)) throw new Error("Session table viewport not found");
  return table.parentElement;
}

function tableRoot(container: HTMLElement): HTMLElement {
  const viewport = tableViewport(container);
  if (!(viewport.parentElement instanceof HTMLElement)) throw new Error("Session table root not found");
  return viewport.parentElement;
}

function tableRow(container: HTMLElement, threadKeyId: string): HTMLTableRowElement {
  const tableRow = container.querySelector<HTMLTableRowElement>(
    `tbody tr[data-session-thread-key-id='${threadKeyId}']`,
  );
  if (!tableRow) throw new Error(`Session row ${threadKeyId} not found`);
  return tableRow;
}

describe("SessionSection v0.2.0", () => {
  beforeAll(() => {
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
      configurable: offsetHeightDescriptor?.configurable ?? true,
      enumerable: offsetHeightDescriptor?.enumerable ?? true,
      get() {
        return inlinePixels(this, "height") ?? descriptorPixels(this, offsetHeightDescriptor);
      },
    });
    Object.defineProperty(HTMLElement.prototype, "offsetWidth", {
      configurable: offsetWidthDescriptor?.configurable ?? true,
      enumerable: offsetWidthDescriptor?.enumerable ?? true,
      get() {
        return inlinePixels(this, "width") ?? descriptorPixels(this, offsetWidthDescriptor);
      },
    });
  });

  afterAll(() => {
    restoreDimension("offsetHeight", offsetHeightDescriptor);
    restoreDimension("offsetWidth", offsetWidthDescriptor);
  });

  it("T-S04-001 preserves manual-sort row order while sort and row props stay active", () => {
    const rows = [row(2), row(1)];
    const selectSort = vi.fn();
    const openSession = vi.fn();
    const rendered = render(
      <SessionSection
        view={view({ rows, select_sort: selectSort })}
        detail={detailView(openSession)}
      />,
    );

    expect(
      Array.from(
        rendered.container.querySelectorAll("tbody tr[data-session-thread-key-id]"),
        (tableRow) => tableRow.getAttribute("data-session-thread-key-id"),
      ),
    ).toEqual([JSON.stringify(["codex", "session-2", "root-2"]), JSON.stringify(["codex", "session-1", "root-1"])]);

    fireEvent.click(screen.getByRole("button", { name: "最后活动" }));
    expect(selectSort).toHaveBeenCalledWith("last_activity");

    const firstRow = tableRow(rendered.container, JSON.stringify(["codex", "session-2", "root-2"]));
    expect(firstRow).toHaveAttribute("data-session-thread-key-id", JSON.stringify(["codex", "session-2", "root-2"]));
    expect(firstRow).toHaveAttribute("tabindex", "0");
    expect(firstRow).toHaveAttribute("aria-selected", "false");
    fireEvent.click(firstRow);
    expect(openSession).toHaveBeenCalledWith(rows[0]);
  });

  it("T17 renders the six columns, combined session details, and numeric alignment", () => {
    const rendered = render(<SessionSection view={view()} />);
    const headers = screen.getAllByRole("columnheader");

    expect(screen.getByRole("heading", { name: "Session 记录" })).toBeInTheDocument();
    expect(headers.map((header) => header.textContent?.trim())).toEqual([
      "最后活动",
      "会话",
      "项目",
      "Token",
      "缓存率",
      "合计费用",
    ]);

    expect(headers[1].querySelector("button")).toBeNull();
    expect(headers.filter((header) => header.querySelector("button")).length).toBe(5);
    for (const header of headers) {
      const label = header.querySelector("button, span");
      expect(label).toHaveClass("px-4");
    }

    for (const label of ["Token", "缓存率", "合计费用"]) {
      expect(screen.getByRole("button", { name: label })).toHaveClass("justify-end");
      expect(screen.getByRole("button", { name: label }).lastElementChild).toHaveTextContent(label);
    }

    const cells = rendered.container.querySelectorAll(
      `tbody tr[data-session-thread-key-id='${JSON.stringify(["codex", "session-1", "root-1"])}'] td`,
    );
    for (const index of [3, 4, 5]) {
      expect(cells[index]).toHaveClass("text-right");
    }
    expect(cells[1]).toHaveTextContent("未命名会话");
    expect(cells[1]).toHaveTextContent("codex · gpt-5 (high), o4-mini (low) · 2 subagent");
    expect(cells[1].querySelectorAll("span")[0]).toHaveAttribute("title", "未命名会话");
    expect(cells[1].querySelectorAll("span")[1]).toHaveAttribute("title", "codex · gpt-5 (high), o4-mini (low) · 2 subagent");
    expect(cells[3]).toHaveTextContent("5.7K");
    expect(cells[3].querySelector(".tabular-nums")).toHaveAttribute("title", "5,678");
    expect(cells[3].querySelector(".tabular-nums")).toHaveAttribute("aria-label", "5,678");
    expect(cells[4]).toHaveTextContent("7.4%");
    expect(cells[5]).toHaveTextContent("$1.25");
    expect(cells[3].querySelector(".tabular-nums")).toBeTruthy();

    const table = rendered.container.querySelector("table");
    if (!table) throw new Error("Session table not found");
    expect(table.querySelector("thead tr")).toHaveStyle({ height: "48px" });
    expect(table.querySelector("tbody tr")).toHaveStyle({ height: "64px" });
    expect(Array.from(table.querySelectorAll("col"), (column) => column.style.width)).toEqual([
      "100px",
      "",
      "150px",
      "100px",
      "100px",
      "100px",
      "0px",
    ]);
  });

  it("shows the shared workspace label while retaining its original path as a tooltip", () => {
    const path = "/private/tmp/claude-501/child";
    const workspaceRow = row(1, { thread: { ...item.thread, project: { kind: "workspace", path } } });
    const rendered = render(<SessionSection view={view({ rows: [workspaceRow] })} />);
    const projectCell = tableRow(rendered.container, JSON.stringify(["codex", "session-1", "root-1"])).cells[2];

    expect(projectCell).toHaveTextContent("临时工作区");
    expect(projectCell.firstElementChild).toHaveAttribute("title", path);
  });

  it("shows non-today activity time above the lower hierarchy date and keeps today time-only", () => {
    const now = Date.UTC(2026, 7, 10, 8, 15);
    const dateNow = vi.spyOn(Date, "now").mockReturnValue(now);
    try {
      const rendered = render(<SessionSection view={view()} />);
      const currentActivity = tableRow(rendered.container, JSON.stringify(["codex", "session-1", "root-1"])).cells[0];
      expect(currentActivity).toHaveTextContent("16:09");
      expect(currentActivity.firstElementChild).toHaveClass("block", "truncate");
      expect(currentActivity.firstElementChild).toHaveAttribute("title", "2026-08-10 16:09:00");

      rendered.rerender(
        <SessionSection
          view={view({ rows: [row(1, { last_activity_ms: Date.UTC(2025, 11, 1, 7, 8, 3) })] })}
        />,
      );
      const datedActivity = tableRow(rendered.container, JSON.stringify(["codex", "session-1", "root-1"])).cells[0];
      const lines = datedActivity.firstElementChild as HTMLElement;
      expect(lines).toHaveClass("flex", "flex-col", "leading-5");
      expect(lines).toHaveAttribute("title", "2025-12-01 15:08:03");
      expect(Array.from(lines.children, (line) => line.textContent)).toEqual(["15:08", "2025-12-01"]);
      expect(lines.children[0]).toHaveClass("block", "min-w-0", "truncate", "whitespace-nowrap");
      expect(lines.children[1]).toHaveClass("text-xs", "leading-4", "text-muted-foreground");
    } finally {
      dateNow.mockRestore();
    }
  });

  it("T17 includes mapped Agent names in the combined session details", () => {
    const mixedRows: SessionListItemDto[] = [
      row(1, { thread: { ...row(1).thread, key: { ...row(1).thread.key, agent: "codex" } } }),
      row(2, { thread: { ...row(2).thread, key: { ...row(2).thread.key, agent: "antigravity" } } }),
    ];
    const agentDisplay = (s: string) => (s === "antigravity" ? "Antigravity" : "Codex");
    const rendered = render(
      <SessionSection view={view({ rows: mixedRows })} agentDisplay={agentDisplay} />,
    );
    const headers = screen.getAllByRole("columnheader");
    expect(headers.map((h) => h.textContent?.trim())).toEqual([
      "最后活动",
      "会话",
      "项目",
      "Token",
      "缓存率",
      "合计费用",
    ]);

    const table = rendered.container.querySelector("table");
    if (!table) throw new Error("Session table not found");
    expect(Array.from(table.querySelectorAll("col"), (col) => col.style.width)).toEqual([
      "100px",
      "",
      "150px",
      "100px",
      "100px",
      "100px",
      "0px",
    ]);

    const row1Cells = rendered.container.querySelectorAll(`tbody tr[data-session-thread-key-id='${JSON.stringify(["codex", "session-1", "root-1"])}'] td`);
    expect(row1Cells[1]).toHaveTextContent("Codex");
    const row2Cells = rendered.container.querySelectorAll(`tbody tr[data-session-thread-key-id='${JSON.stringify(["antigravity", "session-2", "root-2"])}'] td`);
    expect(row2Cells[1]).toHaveTextContent("Antigravity");
  });

  it("T-S04-002 keeps table width styles stable when sorting changes", () => {
    const rows = [row(1), row(2)];
    const rendered = render(<SessionSection view={view({ rows })} />);
    const table = rendered.container.querySelector("table");
    if (!table) throw new Error("Session table not found");
    const columns = Array.from(table.querySelectorAll("col"));
    const filler = columns.at(-1);
    if (!filler) throw new Error("Session table filler column not found");

    const before = {
      fillerWidth: filler.style.width,
      columnWidths: columns.map((column) => column.style.width),
    };
    expect(table).not.toHaveClass("w-full");
    expect(before.fillerWidth).toBe("0px");

    fireEvent.click(screen.getByRole("button", { name: "项目" }));
    rendered.rerender(
      <SessionSection
        view={view({
          rows: [...rows].reverse(),
          sort_by: "project",
          sort_order: "asc",
        })}
      />,
    );

    const sortedTable = rendered.container.querySelector("table");
    if (!sortedTable) throw new Error("Session table not found after sorting");
    const sortedColumns = Array.from(sortedTable.querySelectorAll("col"));
    expect(sortedTable).not.toHaveClass("w-full");
    expect({
      fillerWidth: sortedColumns.at(-1)?.style.width,
      columnWidths: sortedColumns.map((column) => column.style.width),
    }).toEqual(before);
  });

  it("T-S04-004 computes the table height from ready rows", () => {
    for (const [count, height] of [
      [6, 432],
      [3, 240],
    ] as const) {
      const rendered = render(
        <SessionSection
          view={view({
            rows: Array.from({ length: count }, (_, index) => row(index)),
            total_items: count,
            total_pages: 1,
          })}
        />,
      );
      expect(tableViewport(rendered.container)).toHaveStyle({ height: `${height}px` });
      expect(tableRoot(rendered.container).classList.contains("[&>div:first-child]:overflow-x-auto")).toBe(true);
      expect(tableRoot(rendered.container).classList.contains("[&>div:first-child]:overflow-y-hidden")).toBe(true);
      rendered.unmount();
    }

    const readyEmpty = render(<SessionSection view={view({ rows: [], total_items: 0, total_pages: 0 })} />);
    expect(tableViewport(readyEmpty.container)).toHaveStyle({ height: "192px" });
    expect(tableRoot(readyEmpty.container).classList.contains("[&>div:first-child]:overflow-y-hidden")).toBe(true);
    readyEmpty.unmount();
  });

  it("T-S04-005 activates every returned Session row with its full ThreadKey", () => {
    const openSession = vi.fn();
    const rendered = render(
      <SessionSection view={view()} detail={detailView(openSession)} />,
    );
    const usableRow = tableRow(rendered.container, JSON.stringify(["codex", "session-1", "root-1"]));
    fireEvent.click(usableRow);
    fireEvent.keyDown(usableRow, { key: "Enter" });
    fireEvent.keyDown(usableRow, { key: " " });
    expect(openSession).toHaveBeenCalledTimes(3);
    expect(openSession.mock.calls.map(([selected]) => selected.thread.key)).toEqual([
      { agent: "codex", session_id: "session-1", thread_id: "root-1" },
      { agent: "codex", session_id: "session-1", thread_id: "root-1" },
      { agent: "codex", session_id: "session-1", thread_id: "root-1" },
    ]);
    rendered.unmount();
  });

  it("keeps inclusive rollups and subagent counts in the Session cell", () => {
    const rendered = render(<SessionSection view={view({ rows: [row(1)] })} />);
    const cells = rendered.container.querySelectorAll(`tbody tr[data-session-thread-key-id='${JSON.stringify(["codex", "session-1", "root-1"])}'] td`);
    expect(cells[1]).toHaveTextContent("2 subagent");
    expect(cells[3]).toHaveTextContent("5.7K");
    expect(cells[3].querySelector(".tabular-nums")).toHaveAttribute("title", "5,678");
    expect(cells[4]).toHaveTextContent("7.4%");
    expect(cells[5]).toHaveTextContent("$1.25");
  });

  it("warns on an inclusive cost only when the session has unpriced usage", () => {
    const rendered = render(<SessionSection view={view({ rows: [row(1, { inclusive_usage: { ...item.inclusive_usage, unpriced: 1 } }), row(2)] })} />);
    const unpricedCost = tableRow(rendered.container, JSON.stringify(["codex", "session-1", "root-1"])).cells[5].firstElementChild;
    const pricedCost = tableRow(rendered.container, JSON.stringify(["codex", "session-2", "root-2"])).cells[5].firstElementChild;

    expect(unpricedCost).toHaveClass("text-warning");
    expect(pricedCost).not.toHaveClass("text-warning");
  });

  it("does not attribute a model to unknown thread rows", () => {
    const unknown = row(3, {
      thread: {
        ...row(3).thread,
        type: "unknown",
        model_effort: [{ model: "unconfirmed-model", effort: "high", usage, compaction_tokens: null }],
      },
    });
    const rendered = render(<SessionSection view={view({ rows: [unknown] })} />);
    const cells = rendered.container.querySelectorAll(`tbody tr[data-session-thread-key-id='${JSON.stringify(["codex", "session-3", "root-3"])}'] td`);

    expect(cells[1]).not.toHaveTextContent("unconfirmed-model");
  });

  it("shows em dashes when a session has no usage rows and preserves subagent count semantics", () => {
    const codexNoRows = row(1, {
      has_usage_rows: false,
      last_activity_ms: null,
      subagent_count: null,
      thread: { ...row(1).thread, model_effort: [] },
    });
    const claudeZero = row(2, {
      thread: { ...row(2).thread, key: { ...row(2).thread.key, agent: "claude" } },
      subagent_count: 0,
    });
    const rendered = render(<SessionSection view={view({ rows: [codexNoRows, claudeZero] })} />);
    const codexCells = rendered.container.querySelectorAll(`tbody tr[data-session-thread-key-id='${JSON.stringify(["codex", "session-1", "root-1"])}'] td`);
    expect(codexCells[0]).toHaveTextContent("—");
    expect(codexCells[1].querySelectorAll("span")[1]).toHaveTextContent("codex");
    expect(codexCells[1].querySelectorAll("span")[1]).not.toHaveTextContent("—");
    for (const index of [3, 4, 5]) expect(codexCells[index]).toHaveTextContent("—");
    const claudeSession = rendered.container.querySelector(`tbody tr[data-session-thread-key-id='${JSON.stringify(["claude", "session-2", "root-2"])}'] td:nth-child(2)`);
    expect(claudeSession).toHaveTextContent("claude · gpt-5 (high), o4-mini (low)");
    expect(claudeSession).not.toHaveTextContent("0 subagent");
  });

  it("T-S04-006 keeps pagination in the Session header and commits the real page-input flow", () => {
    const nextPage = vi.fn();
    const previousPage = vi.fn();
    const goToPage = vi.fn();
    const initialView = view({
      page: 2,
      total_items: 5,
      total_pages: 5,
      next_page: nextPage,
      previous_page: previousPage,
      go_to_page: goToPage,
    });
    const rendered = render(<SessionSection view={initialView} />);

    expect(screen.getByText("共 5 条")).toBeInTheDocument();
    expect(screen.getByText("2 / 5")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    fireEvent.click(screen.getByRole("button", { name: "上一页" }));
    expect(nextPage).toHaveBeenCalledTimes(1);
    expect(previousPage).toHaveBeenCalledTimes(1);

    const pageInput = screen.getByRole("textbox", { name: "跳转页码" });
    fireEvent.change(pageInput, { target: { value: "3" } });
    fireEvent.keyDown(pageInput, { key: "Enter" });
    expect(goToPage).toHaveBeenCalledTimes(1);
    expect(goToPage).toHaveBeenCalledWith(3);

    rendered.rerender(<SessionSection view={{ ...initialView, page: 3 }} />);
    expect(screen.getByRole("textbox", { name: "跳转页码" })).toHaveValue("3");
    rendered.unmount();

    const invalidGoToPage = vi.fn();
    const invalid = render(
      <SessionSection
        view={view({ page: 2, total_items: 5, total_pages: 5, go_to_page: invalidGoToPage })}
      />,
    );
    const invalidInput = screen.getByRole("textbox", { name: "跳转页码" });
    fireEvent.change(invalidInput, { target: { value: "99" } });
    fireEvent.blur(invalidInput);
    expect(invalidInput).toHaveValue("2");
    expect(invalidGoToPage).not.toHaveBeenCalled();
    invalid.unmount();
  });

  it("T-S04-004 uses BeUI structural initial loading rows and the approved empty state", () => {
    const loading = render(<SessionSection view={view({ rows: [], load_state: "initial" })} />);
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.queryByText("当前时间范围暂无 Session 记录")).not.toBeInTheDocument();
    expect(tableViewport(loading.container)).toHaveStyle({ height: "384px" });
    expect(loading.container.querySelectorAll("tbody tr")).toHaveLength(6);
    expect(tableRoot(loading.container).classList.contains("[&>div:first-child]:overflow-y-hidden")).toBe(true);
    loading.unmount();

    render(<SessionSection view={view({ rows: [], total_items: 0, total_pages: 0 })} />);
    expect(screen.getByText("当前时间范围暂无 Session 记录")).toBeInTheDocument();
  });

  it("explains when active filters leave the current range empty", () => {
    render(<SessionSection view={view({
      rows: [],
      total_items: 0,
      total_pages: 0,
      filters: { agents: ["codex"], models: [], projects: [] },
    })} />);

    expect(screen.getByText("当前筛选在当前时间范围暂无 Session 记录")).toBeInTheDocument();
  });

  it("keeps bounded refresh and page errors with retry controls", () => {
    const retryLoad = vi.fn();
    const retryPage = vi.fn();
    const rendered = render(
      <SessionSection
        view={view({ load_state: "error", error_code: "UPDATE_FAILED", retry_load: retryLoad })}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("Session 记录更新失败");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(retryLoad).toHaveBeenCalledTimes(1);
    rendered.unmount();

    render(
      <SessionSection
        view={view({ page_state: "error", total_items: 16, total_pages: 2, retry_page: retryPage })}
      />,
    );
    expect(screen.getByText("加载页面失败")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(retryPage).toHaveBeenCalledTimes(1);
  });
});
