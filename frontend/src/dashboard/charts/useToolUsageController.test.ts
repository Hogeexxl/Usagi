import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../../data/usagiClient";
import type { ToolUsageResponse } from "../../data/types";
import { UsagiClientError } from "../../data/types";
import { useToolUsageController } from "./useToolUsageController";

function clientWith(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    quota: vi.fn(),
    refreshQuota: vi.fn(),
    filterOptions: vi.fn(),
    summary: vi.fn(),
    modelDistribution: vi.fn(),
    projectDistribution: vi.fn(),
    getSessions: vi.fn(),
    toolUsage: vi.fn(async (days) => ({ days, calls: 0, top: [] })),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(),
    getRevision: vi.fn(),
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

afterEach(() => vi.useRealTimers());

describe("useToolUsageController Spec05", () => {
  it("defaults to today and cancels stale requests when selecting seven and thirty days", async () => {
    const today = deferred<ToolUsageResponse>();
    const seven = deferred<ToolUsageResponse>();
    const thirty = deferred<ToolUsageResponse>();
    const api = clientWith({ toolUsage: vi.fn()
      .mockReturnValueOnce(today.promise)
      .mockReturnValueOnce(seven.promise)
      .mockReturnValueOnce(thirty.promise) });
    const { result } = renderHook(() => useToolUsageController({ client: api }));

    expect(result.current.range).toBe("today");
    await waitFor(() => expect(api.toolUsage).toHaveBeenCalledTimes(1));
    expect(api.toolUsage).toHaveBeenNthCalledWith(1, 1, expect.any(AbortSignal));
    const todaySignal = vi.mocked(api.toolUsage).mock.calls[0][1];

    act(() => result.current.select_range("7d"));
    await waitFor(() => expect(api.toolUsage).toHaveBeenCalledTimes(2));
    expect(todaySignal?.aborted).toBe(true);
    expect(api.toolUsage).toHaveBeenNthCalledWith(2, 7, expect.any(AbortSignal));
    const sevenSignal = vi.mocked(api.toolUsage).mock.calls[1][1];
    today.resolve({ days: 1, calls: 100, top: [{ name: "Late", calls: 100 }] });
    await act(async () => { await today.promise; });
    expect(result.current.response).toBeNull();

    seven.resolve({ days: 7, calls: 7, top: [{ name: "Weekly", calls: 7 }] });
    await waitFor(() => expect(result.current.response?.days).toBe(7));
    expect(result.current.loading).toBe(false);
    act(() => result.current.select_range("30d"));
    await waitFor(() => expect(api.toolUsage).toHaveBeenCalledTimes(3));
    expect(sevenSignal?.aborted).toBe(true);
    expect(api.toolUsage).toHaveBeenNthCalledWith(3, 30, expect.any(AbortSignal));
    thirty.resolve({ days: 30, calls: 30, top: [{ name: "Monthly", calls: 30 }] });
    await waitFor(() => expect(result.current.response?.days).toBe(30));
  });

  it("polls the selected days every 60 seconds", async () => {
    vi.useFakeTimers();
    const api = clientWith({ toolUsage: vi.fn(async (days) => ({ days, calls: 2, top: [{ name: "Read", calls: 2 }] })) });
    const { result, unmount } = renderHook(() => useToolUsageController({ client: api }));
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    expect(result.current.response?.days).toBe(1);
    expect(api.toolUsage).toHaveBeenCalledTimes(1);

    await act(async () => { await vi.advanceTimersByTimeAsync(59_999); });
    expect(api.toolUsage).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(api.toolUsage).toHaveBeenCalledTimes(2);
    expect(api.toolUsage).toHaveBeenLastCalledWith(1, expect.any(AbortSignal));
    unmount();
  });

  it("keeps Tool upstream errors in the failed state", async () => {
    const api = clientWith({ toolUsage: vi.fn().mockRejectedValue(new UsagiClientError("DATA_LOADING", 503)) });
    const { result } = renderHook(() => useToolUsageController({ client: api }));
    await waitFor(() => expect(result.current.error).toBe(true));
    expect(result.current.loading).toBe(false);
    expect(result.current.response).toBeNull();
  });
});
