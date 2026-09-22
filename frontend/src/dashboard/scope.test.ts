import { describe, expect, it } from "vitest";
import { DASHBOARD_SCOPE_POLICIES, resolveDashboardScope } from "./scope";

const filters = {
  sources: ["source-b", "source-a", "source-b"],
  models: ["b", "a", "a"],
  projects: [{ kind: "project" as const, project_path: "/repo" }],
};

describe("Dashboard scope policy", () => {
  it("keeps Dashboard scope for KPI/distributions/sessions and fixes Skills to rolling 7d", () => {
    for (const key of ["kpi", "modelDistribution", "projectDistribution", "sessions"] as const) {
      expect(resolveDashboardScope(DASHBOARD_SCOPE_POLICIES[key], { key: "30d" }, filters)).toEqual({
        range: { key: "30d" },
        filters: {
          sources: ["source-a", "source-b"],
          models: ["a", "b"],
          projects: [{ kind: "project", project_path: "/repo" }],
        },
      });
    }
    const resolvedSkills = resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.skillsUsage, { key: "year" }, filters);
    expect(resolvedSkills.range).toEqual({ key: "7d" });
    expect(resolvedSkills.filters.sources).toEqual([]);
    expect(resolvedSkills.filters.models).toEqual(["a", "b"]);
    expect(resolvedSkills.filters.projects).toEqual([{ kind: "project", project_path: "/repo" }]);
  });

  it("TD-P5-SKILLS-01 ignores dashboard sources for skillsUsage even when antigravity is selected", () => {
    const customFilters = {
      sources: ["antigravity"],
      models: ["model-x"],
      projects: [{ kind: "project" as const, project_path: "/repo" }],
    };
    const resolved = resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.skillsUsage, { key: "30d" }, customFilters);
    expect(resolved.range).toEqual({ key: "7d" });
    expect(resolved.filters.sources).toEqual([]);
    expect(resolved.filters.models).toEqual(["model-x"]);
    expect(resolved.filters.projects).toEqual([{ kind: "project", project_path: "/repo" }]);
  });

  it("T-022-A4 keeps a complete custom range in Dashboard scope while Skills stays fixed", () => {
    const custom = { key: "custom" as const, from: "2026-08-01", to: "2026-08-03" };
    expect(resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.sessions, custom, filters)).toEqual({
      range: custom,
      filters: {
        sources: ["source-a", "source-b"],
        models: ["a", "b"],
        projects: [{ kind: "project", project_path: "/repo" }],
      },
    });
    expect(resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.skillsUsage, custom, filters).range).toEqual({ key: "7d" });
  });

  it("resolves sources to empty array when policy ignores sources", () => {
    const ignorePolicy = {
      range: { kind: "dashboard" as const },
      sources: "ignore" as const,
      models: "dashboard" as const,
      projects: "dashboard" as const,
    };
    expect(resolveDashboardScope(ignorePolicy, { key: "today" }, filters)).toEqual({
      range: { key: "today" },
      filters: {
        sources: [],
        models: ["a", "b"],
        projects: [{ kind: "project", project_path: "/repo" }],
      },
    });
  });
});
