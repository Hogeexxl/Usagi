import { useEffect, useMemo, useState } from "react";
import { usagiClient, dashboardQueryKey, type UsagiClient } from "../../data/usagiClient";
import type { DashboardFilters, DashboardRange, ModelDistributionResponse, ProjectDistributionResponse, SkillsUsageResponse } from "../../data/types";
import { DASHBOARD_SCOPE_POLICIES, resolveDashboardScope } from "../scope";

export type DashboardChartsView = {
  models: ModelDistributionResponse | null;
  projects: ProjectDistributionResponse | null;
  skills: SkillsUsageResponse | null;
  loading: boolean;
  error: boolean;
};

export function useDashboardChartsController(args: {
  range: DashboardRange;
  filters: DashboardFilters;
  dataRevision: number;
  client?: UsagiClient;
}): DashboardChartsView {
  const client = args.client ?? usagiClient;
  const filterKey = useMemo(() => dashboardQueryKey(args.range, args.filters), [args.range, args.filters]);
  const [view, setView] = useState<DashboardChartsView>({ models: null, projects: null, skills: null, loading: true, error: false });
  useEffect(() => {
    const controller = new AbortController();
    const modelScope = resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.modelDistribution, args.range, args.filters);
    const projectScope = resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.projectDistribution, args.range, args.filters);
    const skillsScope = resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.skillsUsage, args.range, args.filters);
    setView((current) => ({ ...current, loading: true, error: false }));
    void Promise.all([
      client.modelDistribution(modelScope.range, modelScope.filters, controller.signal),
      client.projectDistribution(projectScope.range, projectScope.filters, controller.signal),
      client.skillsUsage(skillsScope.range, skillsScope.filters, controller.signal),
    ]).then(
      ([models, projects, skills]) => {
        if (!controller.signal.aborted) setView({ models, projects, skills, loading: false, error: false });
      },
      (error: unknown) => {
        if (!controller.signal.aborted && !(error instanceof DOMException && error.name === "AbortError")) {
          setView((current) => ({ ...current, loading: false, error: true }));
        }
      },
    );
    return () => controller.abort();
  }, [client, args.dataRevision, args.range, filterKey]);
  return view;
}
