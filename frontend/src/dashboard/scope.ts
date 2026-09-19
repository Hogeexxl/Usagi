import { canonicalDashboardFilters } from "../data/usagiClient";
import type { DashboardFilters, DashboardRange } from "../data/types";

export type RangePolicy = { kind: "dashboard" } | { kind: "fixed"; range: DashboardRange };
export type FilterPolicy = "dashboard" | "ignore";
export type DashboardScopePolicy = {
  range: RangePolicy;
  models: FilterPolicy;
  projects: FilterPolicy;
};
export type ResolvedDashboardScope = { range: DashboardRange; filters: DashboardFilters };

const FOLLOW_DASHBOARD: DashboardScopePolicy = {
  range: { kind: "dashboard" },
  models: "dashboard",
  projects: "dashboard",
};
const ROLLING_7D_FILTERED: DashboardScopePolicy = {
  range: { kind: "fixed", range: { key: "7d" } },
  models: "dashboard",
  projects: "dashboard",
};

export const DASHBOARD_SCOPE_POLICIES = {
  kpi: FOLLOW_DASHBOARD,
  modelDistribution: FOLLOW_DASHBOARD,
  projectDistribution: FOLLOW_DASHBOARD,
  sessions: FOLLOW_DASHBOARD,
  skillsUsage: ROLLING_7D_FILTERED,
} as const;

export function resolveDashboardScope(
  policy: DashboardScopePolicy,
  dashboardRange: DashboardRange,
  dashboardFilters: DashboardFilters,
): ResolvedDashboardScope {
  const canonical = canonicalDashboardFilters(dashboardFilters);
  return {
    range: policy.range.kind === "dashboard" ? dashboardRange : policy.range.range,
    filters: {
      models: policy.models === "dashboard" ? canonical.models : [],
      projects: policy.projects === "dashboard" ? canonical.projects : [],
    },
  };
}
