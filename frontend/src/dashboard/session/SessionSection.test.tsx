import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

import type { SessionItemDto, UsageDto } from "../../data/types";
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
  input_tokens: 1_234,
  cached_tokens: 100,
  cache_write_tokens: null,
  uncached_input_tokens: 1_134,
  output_tokens: 567,
  reasoning_tokens: 12,
  other_output_tokens: 555,
  total_tokens: 1_801,
  cache_hit_rate: 100 / 1234,
  estimated_cost: 1.25,
  estimated_cost_status: "complete",
};

const item: SessionItemDto = {
  root_session_id: "root-1",
  title: null,
  project_name: "Usagi",
  project_path: "/work/Usagi",
  last_activity_at_ms: Date.UTC(2026, 7, 10, 8, 9),
  models_used: ["gpt-5", "o4-mini"],
  subagent_count: 2,
  inclusive_usage: { ...usage, total_tokens: 5_678 },
  self_usage: { ...usage, total_tokens: 1_234 },
  subagent_usage: usage,
  data_status: "complete",
  error_code: null,
};

function view(overrides: Partial<SessionTableViewModel> = {}): SessionTableViewModel {
  return {
    range: { key: "today" },
    rows: [item],
    timezone: "Asia/Shanghai",
    load_state: "ready",
    page_state: "idle",
    filters: { models: [], projects: [] },
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

function row(index: number, overrides: Partial<SessionItemDto> = {}): SessionItemDto {
  return {
    ...item,
    root_session_id: `root-${index}`,
    title: `Session ${index}`,
    last_activity_at_ms: item.last_activity_at_ms + index,
    ...overrides,
  };
}

function detailView(openDetail: (row: SessionItemDto) => void): SessionDetailControllerViewModel {
  return {
    open: false,
    selected_root_session_id: null,
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

function tableRow(container: HTMLElement, rootSessionId: string): HTMLTableRowElement {
  const tableRow = container.querySelector<HTMLTableRowElement>(
    `tbody tr[data-session-root-id="${rootSessionId}"]`,
  );
  if (!tableRow) throw new Error(`Session row ${rootSessionId} not found`);
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
        rendered.container.querySelectorAll("tbody tr[data-session-root-id]"),
        (tableRow) => tableRow.getAttribute("data-session-root-id"),
      ),
    ).toEqual(["root-2", "root-1"]);

    fireEvent.click(screen.getByRole("button", { name: "最后活动" }));
    expect(selectSort).toHaveBeenCalledWith("last_activity");

    const firstRow = tableRow(rendered.container, "root-2");
    expect(firstRow).toHaveAttribute("data-session-root-id", "root-2");
    expect(firstRow).toHaveAttribute("tabindex", "0");
    expect(firstRow).toHaveAttribute("aria-selected", "false");
    fireEvent.click(firstRow);
    expect(openSession).toHaveBeenCalledWith(rows[0]);
  });

  it("T-S04-002 keeps the fixed column order, sortable rules, and numeric alignment", () => {
    const rendered = render(<SessionSection view={view()} />);
    const headers = screen.getAllByRole("columnheader");

    expect(screen.getByRole("heading", { name: "Session 记录" })).toBeInTheDocument();
    expect(headers.map((header) => header.textContent?.trim())).toEqual([
      "最后活动",
      "标题",
      "项目",
      "模型",
      "合计 Token",
      "sub数量",
      "缓存命中率",
      "合计费用",
    ]);

    expect(headers[1].querySelector("button")).toBeNull();
    expect(headers[5].querySelector("button")).toBeNull();
    expect(headers.filter((header) => header.querySelector("button")).length).toBe(6);

    for (const label of ["合计 Token", "缓存命中率", "合计费用"]) {
      expect(screen.getByRole("button", { name: label })).toHaveClass("justify-end");
    }

    const cells = rendered.container.querySelectorAll(
      'tbody tr[data-session-root-id="root-1"] td',
    );
    for (const index of [4, 5, 6, 7]) {
      expect(cells[index]).toHaveClass("text-right");
    }
    expect(cells[4]).toHaveTextContent("5,678");
    expect(cells[5]).toHaveTextContent("2");
    expect(cells[4].querySelector(".tabular-nums")).toBeTruthy();
    expect(cells[5].querySelector(".tabular-nums")).toBeTruthy();

    const table = rendered.container.querySelector("table");
    if (!table) throw new Error("Session table not found");
    expect(Array.from(table.querySelectorAll("col"), (column) => column.style.width)).toEqual([
      "128px",
      "",
      "150px",
      "168px",
      "150px",
      "128px",
      "150px",
      "120px",
      "0px",
    ]);
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

    fireEvent.click(screen.getByRole("button", { name: "模型" }));
    rendered.rerender(
      <SessionSection
        view={view({
          rows: [...rows].reverse(),
          sort_by: "model",
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
      [10, 528],
      [3, 192],
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
      rendered.unmount();
    }

    const readyEmpty = render(<SessionSection view={view({ rows: [], total_items: 0, total_pages: 0 })} />);
    expect(tableViewport(readyEmpty.container)).toHaveStyle({ height: "192px" });
    readyEmpty.unmount();
  });

  it("T-S04-005 activates usable rows and keeps error rows disabled", () => {
    const openSession = vi.fn();
    const rendered = render(
      <SessionSection view={view()} detail={detailView(openSession)} />,
    );
    const usableRow = tableRow(rendered.container, "root-1");
    fireEvent.click(usableRow);
    fireEvent.keyDown(usableRow, { key: "Enter" });
    fireEvent.keyDown(usableRow, { key: " " });
    expect(openSession).toHaveBeenCalledTimes(3);
    rendered.unmount();

    const errorOpen = vi.fn();
    const errorRendered = render(
      <SessionSection
        view={view({ rows: [row(1, { data_status: "error" })] })}
        detail={detailView(errorOpen)}
      />,
    );
    const errorRow = tableRow(errorRendered.container, "root-1");
    expect(errorRow).toHaveAttribute("aria-disabled", "true");
    expect(errorRow).toHaveAttribute("tabindex", "-1");
    fireEvent.click(errorRow);
    fireEvent.keyDown(errorRow, { key: "Enter" });
    fireEvent.keyDown(errorRow, { key: " " });
    expect(errorOpen).not.toHaveBeenCalled();
  });

  it("T-S04-005 exposes the incomplete and error tooltip copy", () => {
    for (const [dataStatus, label] of [
      ["incomplete", "数据不完整"],
      ["error", "数据计算异常"],
    ] as const) {
      vi.useFakeTimers();
      const rendered = render(
        <SessionSection view={view({ rows: [row(1, { data_status: dataStatus })] })} />,
      );
      try {
        const trigger = screen.getByLabelText(label);
        expect(trigger).toHaveAttribute("aria-describedby");
        if (!(trigger.parentElement instanceof HTMLElement)) {
          throw new Error("Tooltip wrapper not found");
        }
        fireEvent.pointerEnter(trigger.parentElement, {
          pointerId: 1,
          pointerType: "mouse",
          buttons: 0,
        });
        act(() => {
          vi.advanceTimersByTime(120);
        });
        const tooltip = screen.getByRole("tooltip", { hidden: true });
        expect(tooltip).toHaveTextContent(label);
        expect(trigger).toHaveAttribute("aria-describedby", tooltip.id);
      } finally {
        rendered.unmount();
        vi.useRealTimers();
      }
    }
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
    expect(tableViewport(loading.container)).toHaveStyle({ height: "480px" });
    expect(loading.container.querySelectorAll("tbody tr")).toHaveLength(10);
    loading.unmount();

    render(<SessionSection view={view({ rows: [], total_items: 0, total_pages: 0 })} />);
    expect(screen.getByText("当前时间范围暂无 Session 记录")).toBeInTheDocument();
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
