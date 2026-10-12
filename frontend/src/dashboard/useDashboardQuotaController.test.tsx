import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../data/usagiClient";
import type { QuotaResponse } from "../data/types";
import { useDashboardQuotaController } from "./useDashboardQuotaController";

const snapshot: QuotaResponse = {
  reading: false,
  items: [
    { provider: "codex", name: "Codex", plan: "Pro", user: "user@example.test", windows: [], balance: "", error: "", as_of_ms: 1, read_at_ms: 2 },
    { provider: "other", name: "Other", plan: "", user: "", windows: [], balance: "", error: "", as_of_ms: 1, read_at_ms: 2 },
  ],
};
const readingSnapshot: QuotaResponse = { reading: true, items: [] };

function client(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    quota: vi.fn(async () => snapshot),
    refreshQuota: vi.fn(async () => snapshot),
    filterOptions: vi.fn(),
    summary: vi.fn(),
    modelDistribution: vi.fn(),
    projectDistribution: vi.fn(),
    getSessions: vi.fn(),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(),
    getRevision: vi.fn(),
    ...overrides,
  };
}

afterEach(() => vi.useRealTimers());

describe("useDashboardQuotaController cached quota contract", () => {
  it("GETs the shared snapshot every 60 seconds and POSTs only for manual refresh", async () => {
    vi.useFakeTimers();
    const api = client({ quota: vi.fn().mockResolvedValueOnce(readingSnapshot).mockResolvedValue(snapshot) });
    const { result, unmount } = renderHook(() => useDashboardQuotaController({ client: api, codexOnly: true }));

    await act(async () => { await Promise.resolve(); });
    expect(api.quota).toHaveBeenCalledTimes(1);
    expect(result.current.quota).toEqual(readingSnapshot);
    expect(api.refreshQuota).not.toHaveBeenCalled();

    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
    expect(api.quota).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(58_999); });
    expect(api.quota).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(api.quota).toHaveBeenCalledTimes(2);
    expect(result.current.quota.items.map((item) => item.provider)).toEqual(["codex"]);
    expect(api.refreshQuota).not.toHaveBeenCalled();

    await act(async () => {
      result.current.refresh();
      await Promise.resolve();
    });
    expect(api.refreshQuota).toHaveBeenCalledTimes(1);
    expect(api.quota).toHaveBeenCalledTimes(2);
    unmount();
  });
});
