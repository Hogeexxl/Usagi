import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { DashboardChartsView } from "./useDashboardChartsController";
import { ChartSection } from "./ChartSection";

const usage = {
  calls: 1,
  errors: 0,
  input: 50,
  output: 30,
  cache_read: 10,
  cache_write: 10,
  cache_write_1h: 0,
  reasoning: 5,
  total: 100,
  cache_hit_rate: 0.143,
  cost: 1.25,
  unpriced: 0,
  retried: 0, output_speed: null, speed_samples: 0,
};

const view: DashboardChartsView = {
  models: {
    data_revision: 1,
    items: [{ model: "gpt-5", usage }],
  },
  projects: {
    data_revision: 1,
    items: [{ kind: "project", name: "Usagi", path: "/tmp/Usagi", usage }],
  },
  loading: false,
  error: false,
};

describe("ChartSection", () => {
  it("[T-S06-001] keeps model and project distributions in one two-column grid", () => {
    render(<ChartSection view={view} />);

    const section = screen.getByRole("region", { name: "使用分布图表" });
    expect(section).toHaveClass("[content-visibility:auto]", "[contain-intrinsic-size:520px]");
    const directGrids = Array.from(section.children).filter(
      (child): child is HTMLElement => child instanceof HTMLElement && child.classList.contains("grid"),
    );
    expect(directGrids).toHaveLength(1);

    const grid = directGrids[0];
    expect(grid).toHaveClass("grid", "grid-cols-2", "gap-4", "max-[1279px]:grid-cols-1");
    expect(grid.children).toHaveLength(2);
    expect(screen.getByRole("heading", { name: "模型分布" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "项目分布" })).toBeInTheDocument();
  });

  it("[T-S06-001] routes both distributions through the same fixed-height ChartSurface", () => {
    render(<ChartSection view={view} />);

    const surfaces = screen.getAllByRole("article");
    expect(surfaces).toHaveLength(2);
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
  });
});
