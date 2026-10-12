import { memo } from "react";

import { projectDisplay } from "../shared/projectDisplay";
import { DistributionDonutCard } from "./DistributionDonutCard";
import type { DashboardChartsView } from "./useDashboardChartsController";

export const ChartSection = memo(function ChartSection({ view }: { view: DashboardChartsView }) {
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
        <DistributionDonutCard title="模型分布" items={modelItems} />
        <DistributionDonutCard title="项目分布" items={projectItems} />
      </div>
    </section>
  );
});
