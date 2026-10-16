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
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

async function settle() {
  await act(async () => { await Promise.resolve(); await Promise.resolve(); });
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
    expect(sevenSignal?.aborted).toBe(false);
    expect(api.toolUsage).toHaveBeenNthCalledWith(3, 30, expect.any(AbortSignal));
    thirty.resolve({ days: 30, calls: 30, top: [{ name: "Monthly", calls: 30 }] });
    await waitFor(() => expect(result.current.response?.days).toBe(30));
  });

  it("polls selected days 60 seconds after a completed successful request", async () => {
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

  it("retries a first MAGPIE_UNAVAILABLE response after five seconds and polls every minute after recovery", async () => {
    vi.useFakeTimers();
    const retry = deferred<ToolUsageResponse>();
    const api = clientWith({
      toolUsage: vi.fn()
        .mockRejectedValueOnce(new UsagiClientError("MAGPIE_UNAVAILABLE", 503))
        .mockReturnValueOnce(retry.promise)
        .mockResolvedValue({ days: 1, calls: 3, top: [{ name: "Read", calls: 3 }] }),
    });
    const { result, unmount } = renderHook(() => useToolUsageController({ client: api }));

    await settle();
    expect(result.current.error).toBe(true);
    expect(result.current.loading).toBe(false);
    expect(result.current.response).toBeNull();

    await act(async () => { await vi.advanceTimersByTimeAsync(4_999); });
    expect(api.toolUsage).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(api.toolUsage).toHaveBeenCalledTimes(2);
    expect(result.current.error).toBe(true);
    expect(result.current.loading).toBe(false);
    await act(async () => { retry.resolve({ days: 1, calls: 2, top: [{ name: "Read", calls: 2 }] }); await retry.promise; });
    await settle();
    expect(result.current.response).toEqual({ days: 1, calls: 2, top: [{ name: "Read", calls: 2 }] });
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBe(false);

    await act(async () => { await vi.advanceTimersByTimeAsync(59_999); });
    expect(api.toolUsage).toHaveBeenCalledTimes(2);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    await settle();
    expect(api.toolUsage).toHaveBeenCalledTimes(3);
    expect(result.current.response?.calls).toBe(3);
    unmount();
  });

  it("does not abort a slow request and schedules the next poll only after it completes", async () => {
    vi.useFakeTimers();
    const slow = deferred<ToolUsageResponse>();
    const api = clientWith({
      toolUsage: vi.fn()
        .mockReturnValueOnce(slow.promise)
        .mockResolvedValue({ days: 1, calls: 2, top: [] }),
    });
    const { unmount } = renderHook(() => useToolUsageController({ client: api }));
    const signal = vi.mocked(api.toolUsage).mock.calls[0][1];

    await act(async () => { await vi.advanceTimersByTimeAsync(120_000); });
    expect(api.toolUsage).toHaveBeenCalledTimes(1);
    expect(signal?.aborted).toBe(false);

    await act(async () => { slow.resolve({ days: 1, calls: 1, top: [] }); await slow.promise; });
    await settle();
    await act(async () => { await vi.advanceTimersByTimeAsync(59_999); });
    expect(api.toolUsage).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    await settle();
    expect(api.toolUsage).toHaveBeenCalledTimes(2);
    unmount();
  });

  it("clears retry timers on range changes and aborts pending requests on unmount", async () => {
    vi.useFakeTimers();
    const pending = deferred<ToolUsageResponse>();
    const signals: AbortSignal[] = [];
    const api = clientWith({
      toolUsage: vi.fn((days, signal?: AbortSignal) => {
        if (signal) signals.push(signal);
        return days === 1
          ? Promise.reject(new UsagiClientError("MAGPIE_UNAVAILABLE", 503))
          : pending.promise;
      }),
    });
    const { result, unmount } = renderHook(() => useToolUsageController({ client: api }));

    await settle();
    expect(result.current.error).toBe(true);
    act(() => result.current.select_range("7d"));
    expect(api.toolUsage).toHaveBeenCalledTimes(2);
    expect(api.toolUsage).toHaveBeenLastCalledWith(7, expect.any(AbortSignal));
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    expect(api.toolUsage).toHaveBeenCalledTimes(2);

    const pendingSignal = signals[1];
    unmount();
    expect(pendingSignal.aborted).toBe(true);
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    expect(api.toolUsage).toHaveBeenCalledTimes(2);
    pending.reject(new DOMException("Aborted", "AbortError"));
    await settle();
  });
});
