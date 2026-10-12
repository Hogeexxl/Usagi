import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../../data/usagiClient";
import type { DashboardFilters, ModelDistributionResponse, ProjectDistributionResponse } from "../../data/types";
import { useDashboardChartsController } from "./useDashboardChartsController";

const filters: DashboardFilters = { agents: [], models: [], projects: [] };
const oldModels: ModelDistributionResponse = { data_revision: 80, items: [{ model: "OldEpochModel", usage: usage() }] };
const oldProjects: ProjectDistributionResponse = { data_revision: 80, items: [{ kind: "project", name: "OldEpochProject", path: "/old", usage: usage() }] };

function usage() {
  return {
    calls: 1,
    errors: 0,
    input: 2,
    output: 3,
    cache_read: 0,
    cache_write: 0,
    cache_write_1h: 0,
    reasoning: 1,
    total: 5,
    cache_hit_rate: 0,
    cost: 0.1,
    unpriced: 0,
  };
}

describe("useDashboardChartsController epoch reset", () => {
  it("clears old Distribution state when epoch changes while data_revision stays 0", async () => {
    let resolveModels!: (response: ModelDistributionResponse) => void;
    let resolveProjects!: (response: ProjectDistributionResponse) => void;
    const client: UsagiClient = {
      quota: vi.fn(),
      refreshQuota: vi.fn(),
      filterOptions: vi.fn(),
      summary: vi.fn(),
      modelDistribution: vi.fn()
        .mockResolvedValueOnce(oldModels)
        .mockImplementationOnce(() => new Promise((resolve) => { resolveModels = resolve; })),
      projectDistribution: vi.fn()
        .mockResolvedValueOnce(oldProjects)
        .mockImplementationOnce(() => new Promise((resolve) => { resolveProjects = resolve; })),
      getSessions: vi.fn(),
      getSessionDetail: vi.fn(),
      getStatus: vi.fn(),
      getRevision: vi.fn(),
    };
    const { result, rerender } = renderHook(
      ({ epochResetKey }: { epochResetKey: number }) => useDashboardChartsController({
        range: { key: "today" },
        filters,
        dataRevision: 0,
        epochResetKey,
        client,
      }),
      { initialProps: { epochResetKey: 0 } },
    );
    await waitFor(() => expect(result.current.models?.items[0].model).toBe("OldEpochModel"));
    expect(result.current.projects?.items[0]).toMatchObject({ path: "/old" });

    rerender({ epochResetKey: 1 });
    expect(result.current).toMatchObject({ models: null, projects: null, loading: true, error: false });
    await waitFor(() => {
      expect(client.modelDistribution).toHaveBeenCalledTimes(2);
      expect(client.projectDistribution).toHaveBeenCalledTimes(2);
    });
    resolveModels({ data_revision: 0, items: [] });
    resolveProjects({ data_revision: 0, items: [] });
    await waitFor(() => expect(result.current).toMatchObject({ models: { data_revision: 0, items: [] }, projects: { data_revision: 0, items: [] }, loading: false }));
  });
});
