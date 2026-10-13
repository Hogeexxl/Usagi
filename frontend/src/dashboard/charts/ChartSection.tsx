import { memo } from "react";

import { projectDisplay } from "../shared/projectDisplay";
import { ToolUsageCard } from "./ToolUsageCard";
import { DistributionDonutCard } from "./DistributionDonutCard";
import type { DashboardChartsView } from "./useDashboardChartsController";
import type { ToolUsageControllerView } from "./useToolUsageController";

export const ChartSection = memo(function ChartSection({ view, tools }: { view: DashboardChartsView; tools: ToolUsageControllerView }) {
  const modelItems = (view.models?.items ?? []).map((item) => ({
    id: item.model,
    label: item.model,
    totalTokens: item.usage.total,
    cost: item.usage.cost,
    unpriced: item.usage.unpriced,
  }));
  const projectItems = (view.projects?.items ?? []).map((item) => {
    const project = item.kind === "project"
      ? { kind: "project" as const, name: item.name, path: item.path }
      : { kind: item.kind as "projectless" | "unknown" };
    const label = projectDisplay(project);
    return {
      id: item.kind === "project" ? item.path : item.kind,
      label,
      title: item.kind === "project" ? item.path : undefined,
      totalTokens: item.usage.total,
      cost: item.usage.cost,
      unpriced: item.usage.unpriced,
    };
  });

  return (
    <section
      aria-label="使用分布图表"
      aria-busy={view.loading}
      className="[content-visibility:auto] [contain-intrinsic-size:520px]"
    >
      {view.error ? <div className="mb-3 text-xs text-destructive" role="status">图表数据加载失败</div> : null}
      <div className="grid grid-cols-2 gap-4 max-[1279px]:grid-cols-1">
        <DistributionDonutCard modelItems={modelItems} projectItems={projectItems} />
        <ToolUsageCard view={tools} />
      </div>
    </section>
  );
});
