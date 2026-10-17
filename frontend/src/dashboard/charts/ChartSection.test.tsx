import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { DashboardChartsView } from "./useDashboardChartsController";
import type { ToolUsageControllerView } from "./useToolUsageController";
import { ChartSection } from "./ChartSection";

const usage = (index: number) => ({
  calls: 1,
  errors: 0,
  input: index * 50,
  output: index * 30,
  cache_read: index * 10,
  cache_write: index * 10,
  cache_write_1h: 0,
  reasoning: index * 5,
  total: index * 100,
  cache_hit_rate: 0.143,
  cost: index * 1.25,
  unpriced: 0,
  retried: 0,
  output_speed: null,
  speed_samples: 0,
});

const view: DashboardChartsView = {
  models: {
    data_revision: 1,
    items: Array.from({ length: 6 }, (_, index) => ({ model: `model-${index + 1}`, usage: usage(index + 1) })),
  },
  projects: {
    data_revision: 1,
    items: [
      { kind: "project", name: "Usagi", path: "/tmp/Usagi", usage: usage(2) },
      { kind: "project", name: "Other", path: "/tmp/Other", usage: usage(1) },
    ],
  },
  loading: false,
  error: false,
};

const tools: ToolUsageControllerView = {
  range: "today",
  response: { days: 1, calls: 10, top: [{ name: "Read", calls: 8 }] },
  loading: false,
  error: false,
  select_range: () => undefined,
};

describe("ChartSection Spec05", () => {
  it("renders one distribution card and one Tool card in the two-column grid", () => {
    render(<ChartSection view={view} tools={tools} />);

    const section = screen.getByRole("region", { name: "使用分布图表" });
    expect(section).toHaveClass("[content-visibility:auto]", "[contain-intrinsic-size:520px]");
    const grids = Array.from(section.children).filter(
      (child): child is HTMLElement => child instanceof HTMLElement && child.classList.contains("grid"),
    );
    expect(grids).toHaveLength(1);
    expect(grids[0]).toHaveClass("grid", "grid-cols-2", "gap-4", "max-[1024px]:grid-cols-1");
    expect(grids[0].children).toHaveLength(2);
    expect(screen.getByRole("heading", { name: "Tool 使用分布" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "模型分布" })).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "项目分布" })).not.toBeInTheDocument();

    const surfaces = screen.getAllByRole("article");
    expect(surfaces).toHaveLength(2);
    for (const surface of surfaces) {
      expect(surface).toHaveClass("min-w-0", "rounded-[28px]", "border", "border-border", "bg-[#fcfcfc]", "dark:bg-[#151515]", "p-5", "text-card-foreground", "h-[264px]");
    }
    const distribution = surfaces[0];
    expect(within(distribution).getByRole("tab", { name: "模型" })).toHaveAttribute("aria-selected", "true");
    expect(within(distribution).getByRole("tab", { name: "Token" })).toHaveAttribute("aria-selected", "true");
    expect(within(distribution).getByRole("img", { name: "模型分布Token分布" })).toBeInTheDocument();
    expect(within(distribution).getAllByRole("button").filter((button) => button.closest('[role="tablist"]') === null)).toHaveLength(6);
    expect(within(distribution).getByText("其他")).toBeInTheDocument();
  });

  it("preserves the selected metric when switching from models to projects", () => {
    render(<ChartSection view={view} tools={tools} />);
    const distribution = screen.getAllByRole("article")[0];

    fireEvent.click(within(distribution).getByRole("tab", { name: "费用" }));
    expect(within(distribution).getByRole("tab", { name: "费用" })).toHaveAttribute("aria-selected", "true");
    expect(within(distribution).getByRole("img", { name: "模型分布费用分布" })).toBeInTheDocument();
    fireEvent.click(within(distribution).getByRole("tab", { name: "项目" }));
    expect(within(distribution).getByRole("tab", { name: "项目" })).toHaveAttribute("aria-selected", "true");
    expect(within(distribution).getByRole("tab", { name: "费用" })).toHaveAttribute("aria-selected", "true");
    expect(within(distribution).getByRole("img", { name: "项目分布费用分布" })).toBeInTheDocument();
    expect(within(distribution).getAllByRole("button").filter((button) => button.closest('[role="tablist"]') === null)).toHaveLength(2);
  });

  it("maps workspace, projectless, and unknown project kinds to their shared chart labels", () => {
    const projectView: DashboardChartsView = {
      ...view,
      projects: {
        data_revision: 1,
        items: [
          { kind: "workspace", usage: usage(3) },
          { kind: "projectless", usage: usage(2) },
          { kind: "unknown", usage: usage(1) },
        ],
      },
    };
    render(<ChartSection view={projectView} tools={tools} />);
    const distribution = screen.getAllByRole("article")[0];
    fireEvent.click(within(distribution).getByRole("tab", { name: "项目" }));

    expect(within(distribution).getByText("临时工作区")).toBeInTheDocument();
    expect(within(distribution).getByText("无项目会话")).toBeInTheDocument();
    expect(within(distribution).getByText("未知会话")).toBeInTheDocument();
    expect(within(distribution).getAllByRole("button").filter((button) => button.closest('[role="tablist"]') === null)).toHaveLength(3);
    expect(within(distribution).getByText("临时工作区").closest("button")).not.toHaveAttribute("title");
  });
});
