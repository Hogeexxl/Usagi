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
    toolUsage: vi.fn(),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(),
    getRevision: vi.fn(),
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function settle() {
  await act(async () => { await Promise.resolve(); });
}

afterEach(() => vi.useRealTimers());

describe("useDashboardQuotaController cached quota contract", () => {
  it("polls quota at a 60 second interval while reading", async () => {
    vi.useFakeTimers();
    const first = deferred<QuotaResponse>();
    const second = deferred<QuotaResponse>();
    const api = client({
      quota: vi.fn()
        .mockReturnValueOnce(first.promise)
        .mockReturnValueOnce(second.promise)
        .mockResolvedValue(snapshot),
    });
    const { result, unmount } = renderHook(() => useDashboardQuotaController({ client: api, codexOnly: true }));

    expect(api.quota).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    expect(api.quota).toHaveBeenCalledTimes(1);

    await act(async () => { first.resolve(readingSnapshot); await first.promise; });
    expect(result.current.quota).toEqual(readingSnapshot);
    expect(api.refreshQuota).not.toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(59_999); });
    expect(api.quota).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(api.quota).toHaveBeenCalledTimes(2);

    await act(async () => { await vi.advanceTimersByTimeAsync(120_000); });
    expect(api.quota).toHaveBeenCalledTimes(2);
    await act(async () => { second.resolve(snapshot); await second.promise; });
    expect(result.current.quota.items.map((item) => item.provider)).toEqual(["codex"]);
    await act(async () => { await vi.advanceTimersByTimeAsync(59_999); });
    expect(api.quota).toHaveBeenCalledTimes(2);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(api.quota).toHaveBeenCalledTimes(3);
    expect(api.refreshQuota).not.toHaveBeenCalled();
    unmount();
  });

  it("retries a failed first read on the 60 second interval", async () => {
    vi.useFakeTimers();
    const api = client({
      quota: vi.fn().mockRejectedValueOnce(new Error("temporary failure")).mockResolvedValue(snapshot),
    });
    const { result, unmount } = renderHook(() => useDashboardQuotaController({ client: api }));

    await settle();
    expect(result.current.quota).toEqual(readingSnapshot);
    expect(result.current.refresh_error).toBe(true);
    await act(async () => { await vi.advanceTimersByTimeAsync(59_999); });
    expect(api.quota).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(api.quota).toHaveBeenCalledTimes(2);
    await settle();
    expect(result.current.quota).toEqual(snapshot);
    expect(result.current.refresh_error).toBe(false);
    expect(api.refreshQuota).not.toHaveBeenCalled();
    unmount();
  });

  it("follows a manual refresh that returns reading=true with cached GETs", async () => {
    vi.useFakeTimers();
    const pendingGet = deferred<QuotaResponse>();
    const signals: AbortSignal[] = [];
    const api = client({
      quota: vi.fn((signal?: AbortSignal) => {
        if (signal) signals.push(signal);
        return pendingGet.promise;
      }),
      refreshQuota: vi.fn(async () => readingSnapshot),
    });
    const { result, unmount } = renderHook(() => useDashboardQuotaController({ client: api }));

    act(() => { result.current.refresh(); });
    expect(result.current.refreshing).toBe(true);
    expect(signals[0].aborted).toBe(true);
    await settle();
    expect(api.refreshQuota).toHaveBeenCalledTimes(1);
    expect(result.current.refreshing).toBe(false);
    expect(result.current.quota).toEqual(readingSnapshot);

    await act(async () => { pendingGet.resolve(snapshot); await pendingGet.promise; });
    expect(result.current.quota).toEqual(readingSnapshot);
    await act(async () => { await vi.advanceTimersByTimeAsync(59_999); });
    expect(api.quota).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(api.quota).toHaveBeenCalledTimes(2);
    await settle();
    expect(result.current.quota).toEqual(snapshot);
    expect(api.refreshQuota).toHaveBeenCalledTimes(1);
    unmount();
  });

  it("cleans up a scheduled poll and aborts an in-flight GET on unmount", async () => {
    vi.useFakeTimers();
    const inFlight = deferred<QuotaResponse>();
    const signals: AbortSignal[] = [];
    const api = client({
      quota: vi.fn((signal?: AbortSignal) => {
        if (signal) signals.push(signal);
        return signals.length === 1 ? Promise.resolve(readingSnapshot) : inFlight.promise;
      }),
    });
    const first = renderHook(() => useDashboardQuotaController({ client: api }));

    await settle();
    first.unmount();
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(api.quota).toHaveBeenCalledTimes(1);

    const secondApi = client({ quota: vi.fn(() => inFlight.promise) });
    const second = renderHook(() => useDashboardQuotaController({ client: secondApi }));
    const signal = (secondApi.quota as ReturnType<typeof vi.fn>).mock.calls[0]?.[0] as AbortSignal | undefined;
    second.unmount();
    expect(signal?.aborted).toBe(true);
  });

  it("aborts the previous client request and ignores its late response after a client switch", async () => {
    vi.useFakeTimers();
    const oldGet = deferred<QuotaResponse>();
    const oldSignals: AbortSignal[] = [];
    const oldClient = client({
      quota: vi.fn((signal?: AbortSignal) => {
        if (signal) oldSignals.push(signal);
        return oldGet.promise;
      }),
    });
    const newClient = client({ quota: vi.fn(async () => readingSnapshot) });
    const { result, rerender, unmount } = renderHook(
      ({ currentClient }: { currentClient: UsagiClient }) => useDashboardQuotaController({ client: currentClient }),
      { initialProps: { currentClient: oldClient } },
    );

    rerender({ currentClient: newClient });
    expect(oldSignals[0].aborted).toBe(true);
    await settle();
    expect(newClient.quota).toHaveBeenCalledTimes(1);
    expect(result.current.quota).toEqual(readingSnapshot);

    await act(async () => { oldGet.resolve(snapshot); await oldGet.promise; });
    expect(result.current.quota).toEqual(readingSnapshot);
    expect(newClient.refreshQuota).not.toHaveBeenCalled();
    unmount();
  });
});
