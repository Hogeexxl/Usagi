import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { ToolUsageResponse } from "../../data/types";
import { chartSeriesColor } from "./chartPalette";
import type { ToolUsageControllerView } from "./useToolUsageController";
import { ToolUsageCard } from "./ToolUsageCard";

const response: ToolUsageResponse = {
  days: 1,
  calls: 100,
  top: [20, 15, 10, 8, 7, 6, 5, 4, 3].map((calls, index) => ({ name: `Tool-${index + 1}`, calls })),
};

function view(overrides: Partial<ToolUsageControllerView> = {}): ToolUsageControllerView {
  return {
    range: "today",
    response,
    loading: false,
    error: false,
    select_range: () => undefined,
    ...overrides,
  };
}

function card(): HTMLElement {
  const element = screen.getByRole("article");
  return element as HTMLElement;
}

function legends(surface: HTMLElement): HTMLButtonElement[] {
  return Array.from(surface.querySelectorAll<HTMLButtonElement>("button")).filter(
    (button) => button.closest('[role="tablist"]') === null,
  );
}

describe("ToolUsageCard Spec05", () => {
  it("shows the top eight and Other with percentages based on all calls", () => {
    render(<ToolUsageCard view={view()} />);
    const surface = card();
    const rows = legends(surface);

    expect(surface).toHaveClass("h-[264px]");
    expect(within(surface).getByRole("heading", { name: "Tool 使用分布" })).toBeInTheDocument();
    expect(surface.querySelector("header")?.nextElementSibling).toHaveClass("mt-4");
    expect(within(surface).getAllByRole("tab").map((tab) => tab.textContent?.trim())).toEqual(["今天", "7 天", "30 天"]);
    expect(within(surface).queryByRole("tab", { name: "费用" })).not.toBeInTheDocument();
    expect(rows).toHaveLength(9);
    expect(rows.map((row) => row.getAttribute("aria-label"))).toEqual([
      "Tool-1，20 次，20.0%",
      "Tool-2，15 次，15.0%",
      "Tool-3，10 次，10.0%",
      "Tool-4，8 次，8.0%",
      "Tool-5，7 次，7.0%",
      "Tool-6，6 次，6.0%",
      "Tool-7，5 次，5.0%",
      "Tool-8，4 次，4.0%",
      "其他，25 次，25.0%",
    ]);
    expect(within(surface).queryByText("Tool-9")).not.toBeInTheDocument();
    expect(rows.every((row) => row.classList.contains("h-[18px]"))).toBe(true);
    const colors = rows.map((row) => (row.firstElementChild as HTMLElement).style.background);
    expect(colors).toEqual(Array.from({ length: 8 }, (_, index) => chartSeriesColor(index)).concat(chartSeriesColor(8, true)));
    expect(surface.querySelectorAll("svg circle")).toHaveLength(9);

    fireEvent.focus(rows[0]);
    expect(within(surface).getAllByText("20.0%")).toHaveLength(2);
    expect(rows[0]).toHaveAttribute("title", "Tool-1");
  });

  it("keeps all legend rows and the error message visible when refreshing fails", () => {
    render(<ToolUsageCard view={view({ loading: false, error: true })} />);
    const surface = card();
    const status = within(surface).getByRole("status");
    expect(status).toHaveTextContent("工具使用加载失败");
    expect(status.closest("header")).toBeInTheDocument();
    expect(status).toHaveClass("truncate");
    expect(status).toHaveAttribute("title", "工具使用加载失败");
    expect(surface.querySelector("header")?.nextElementSibling).toHaveClass("mt-4");
    expect(legends(surface)).toHaveLength(9);
    expect(within(surface).getByRole("button", { name: "其他，25 次，25.0%" })).toBeInTheDocument();
  });

  it("uses the existing empty and loading states when no Tool calls are available", async () => {
    const { rerender } = render(<ToolUsageCard view={view({ response: { days: 1, calls: 0, top: [] } })} />);
    expect(within(card()).getByRole("status")).toHaveTextContent("暂无工具使用数据");
    expect(card().querySelectorAll("svg circle")).toHaveLength(1);

    rerender(<ToolUsageCard view={view({ response: null, loading: true })} />);
    await waitFor(() => expect(within(card()).getByRole("status")).toHaveTextContent("工具使用加载中…"));
    expect(legends(card())).toHaveLength(0);
  });
});
