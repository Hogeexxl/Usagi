import { describe, expect, it } from "vitest";
import { DASHBOARD_SCOPE_POLICIES, resolveDashboardScope } from "./scope";

const filters = {
  agents: ["agent-b", "agent-a", "agent-b"],
  models: [
    { provider_id: "provider-b", model: "model-b" },
    { provider_id: "provider-a", model: "model-a" },
    { provider_id: "provider-a", model: "model-a" },
  ],
  projects: [
    { kind: "project" as const, path: "/repo" },
    { kind: "projectless" as const },
  ],
};

const canonicalFilters = {
  agents: ["agent-a", "agent-b"],
  models: [
    { provider_id: "provider-a", model: "model-a" },
    { provider_id: "provider-b", model: "model-b" },
  ],
  projects: [
    { kind: "project" as const, path: "/repo" },
    { kind: "projectless" as const },
  ],
};

describe("Dashboard scope policy", () => {
  it("keeps the selected range and all dynamic filter dimensions for Dashboard consumers", () => {
    for (const key of ["kpi", "modelDistribution", "projectDistribution", "sessions"] as const) {
      expect(resolveDashboardScope(DASHBOARD_SCOPE_POLICIES[key], { key: "30d" }, filters)).toEqual({
        range: { key: "30d" },
        filters: canonicalFilters,
      });
    }
  });

  it("keeps a complete custom range and provider/model pair selections", () => {
    const custom = { key: "custom" as const, from: "2026-08-01", to: "2026-08-03" };
    expect(resolveDashboardScope(DASHBOARD_SCOPE_POLICIES.sessions, custom, filters)).toEqual({
      range: custom,
      filters: canonicalFilters,
    });
  });

  it("applies explicit ignore policies without changing the other dimensions", () => {
    const ignoreAgents = {
      range: { kind: "dashboard" as const },
      agents: "ignore" as const,
      models: "dashboard" as const,
      projects: "dashboard" as const,
    };
    expect(resolveDashboardScope(ignoreAgents, { key: "today" }, filters)).toEqual({
      range: { key: "today" },
      filters: { ...canonicalFilters, agents: [] },
    });
  });
});
