import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { DashboardChartsView } from "./useDashboardChartsController";
import { ChartSection } from "./ChartSection";

const range = {
  key: "7d" as const,
  start_ms: 0,
  end_ms: 7,
  timezone: "UTC",
};

const usage = {
  total_tokens: 100,
  estimated_cost: 1.25,
  estimated_cost_status: "complete" as const,
};

const view: DashboardChartsView = {
  models: {
    range,
    data_revision: 1,
    items: [{ model: "gpt-5", usage }],
  },
  projects: {
    range,
    data_revision: 1,
    items: [{ kind: "project", project_name: "Usagi", project_path: "/tmp/Usagi", usage }],
  },
  skills: null,
  loading: false,
  error: false,
};

describe("ChartSection", () => {
  it("[T-S06-001] uses one grid for two Donuts and a desktop full-span Skills surface", () => {
    render(<ChartSection view={view} />);

    const section = screen.getByRole("region", { name: "使用分布图表" });
    expect(section).toHaveClass("[content-visibility:auto]", "[contain-intrinsic-size:520px]");
    const directGrids = Array.from(section.children).filter(
      (child): child is HTMLElement => child instanceof HTMLElement && child.classList.contains("grid"),
    );
    expect(directGrids).toHaveLength(1);

    const grid = directGrids[0];
    expect(grid).toHaveClass("grid", "grid-cols-2", "gap-4", "max-[1279px]:grid-cols-1");
    expect(grid.children).toHaveLength(3);
    expect(screen.getByRole("heading", { name: "模型分布" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "项目分布" })).toBeInTheDocument();
    expect(grid.children[2]).toHaveClass("col-span-2", "max-[1279px]:col-span-1");
  });

  it("[T-S06-001] routes every chart through the same ChartSurface and fixes only Donut heights", () => {
    render(<ChartSection view={view} />);

    const surfaces = screen.getAllByRole("article");
    expect(surfaces).toHaveLength(3);
    for (const surface of surfaces) {
      expect(surface).toHaveClass(
        "min-w-0",
        "rounded-[28px]",
        "border",
        "border-border",
        "bg-[#fcfcfc]",
        "dark:bg-[#151515]",
        "p-5",
        "text-card-foreground",
      );
    }
    expect(surfaces[0]).toHaveClass("h-[264px]");
    expect(surfaces[1]).toHaveClass("h-[264px]");
    expect(surfaces[2]).not.toHaveClass("h-[264px]");
  });
});
