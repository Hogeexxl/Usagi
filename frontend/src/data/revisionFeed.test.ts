import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "./usagiClient";
import type { RevisionTuple } from "./types";
import { createRevisionFeed, type RevisionEventSource } from "./revisionFeed";

function clientWith(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    quota: vi.fn(),
    refreshQuota: vi.fn(),
    filterOptions: vi.fn(),
    summary: vi.fn(),
    modelDistribution: vi.fn(),
    projectDistribution: vi.fn(),
    getSessions: vi.fn(),
    toolUsage: vi.fn(),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(),
    getRevision: vi.fn(async () => ({ data_revision: 0, status_revision: 0 })),
    ...overrides,
  };
}

function eventSource(): RevisionEventSource {
  return { onerror: null, onmessage: null, close: vi.fn() };
}

afterEach(() => vi.useRealTimers());

describe("RevisionFeed", () => {
  it("shares one SSE connection and retains the 60s authoritative fallback until an SSE frame arrives", async () => {
    vi.useFakeTimers();
    const source = eventSource();
    const factory = vi.fn(() => source);
    const client = clientWith({ getRevision: vi.fn(async () => ({ data_revision: 3, status_revision: 4 })) });
    const feed = createRevisionFeed({ client, eventSourceFactory: factory, pollIntervalMs: 60_000 });
    const first = vi.fn();
    const second = vi.fn();
    const unsubscribeFirst = feed.subscribe(first);
    const unsubscribeSecond = feed.subscribe(second);

    expect(factory).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(feed.get_snapshot()).toEqual({ data_revision: 3, status_revision: 4 }));
    expect(first).toHaveBeenCalledWith({ kind: "revision", tuple: { data_revision: 3, status_revision: 4 } });
    source.onerror?.(new Event("error"));
    await vi.advanceTimersByTimeAsync(60_000);
    expect(client.getRevision).toHaveBeenCalledTimes(2);
    expect(feed.get_snapshot()).toEqual({ data_revision: 3, status_revision: 4 });

    source.onmessage?.({ data: JSON.stringify({ data_revision: 5, status_revision: 4 }) } as MessageEvent<string>);
    expect(first).toHaveBeenLastCalledWith({ kind: "revision", tuple: { data_revision: 5, status_revision: 4 } });
    unsubscribeFirst();
    unsubscribeSecond();
    expect(source.close).toHaveBeenCalledTimes(1);
  });

  it("broadcasts an authoritative lower tuple as epoch_reset to every subscriber", async () => {
    const source = eventSource();
    let resolveRestart!: (value: { data_revision: number; status_revision: number }) => void;
    const client = clientWith({
      getRevision: vi.fn()
        .mockResolvedValueOnce({ data_revision: 80, status_revision: 12 })
        .mockImplementationOnce(() => new Promise((resolve) => { resolveRestart = resolve; })),
    });
    const feed = createRevisionFeed({ client, eventSourceFactory: () => source });
    const first = vi.fn();
    const second = vi.fn();
    feed.subscribe(first);
    feed.subscribe(second);
    await vi.waitFor(() => expect(feed.get_snapshot()).toEqual({ data_revision: 80, status_revision: 12 }));

    source.onmessage?.({ data: JSON.stringify({ data_revision: 0, status_revision: 0 }) } as MessageEvent<string>);
    await vi.waitFor(() => expect(client.getRevision).toHaveBeenCalledTimes(2));
    expect(feed.get_snapshot()).toEqual({ data_revision: 80, status_revision: 12 });
    expect(first).not.toHaveBeenCalledWith({ kind: "epoch_reset", tuple: { data_revision: 0, status_revision: 0 } });

    resolveRestart({ data_revision: 0, status_revision: 0 });
    await vi.waitFor(() => expect(feed.get_snapshot()).toEqual({ data_revision: 0, status_revision: 0 }));
    expect(first).toHaveBeenLastCalledWith({ kind: "epoch_reset", tuple: { data_revision: 0, status_revision: 0 } });
    expect(second).toHaveBeenLastCalledWith({ kind: "epoch_reset", tuple: { data_revision: 0, status_revision: 0 } });
    feed.dispose();
  });

  it("clears the fallback timer on a lower SSE hint, aborts a pending poll, and ignores duplicate tuples", async () => {
    vi.useFakeTimers();
    const source = eventSource();
    const signals: AbortSignal[] = [];
    const notifications = vi.fn();
    let resolveFallback!: (value: RevisionTuple) => void;
    let resolveAuthority!: (value: RevisionTuple) => void;
    let callCount = 0;
    const client = clientWith({
      getRevision: vi.fn((_signal?: AbortSignal): Promise<RevisionTuple> => {
        callCount += 1;
        signals.push(_signal!);
        if (callCount === 1) return Promise.resolve({ data_revision: 80, status_revision: 12 });
        if (callCount === 2) return new Promise<RevisionTuple>((resolve) => { resolveFallback = resolve; });
        if (callCount === 3) return new Promise<RevisionTuple>((resolve) => { resolveAuthority = resolve; });
        return Promise.resolve({ data_revision: 0, status_revision: 0 });
      }),
    });
    const feed = createRevisionFeed({ client, eventSourceFactory: () => source, pollIntervalMs: 60_000 });
    feed.subscribe(notifications);
    await vi.waitFor(() => expect(feed.get_snapshot()).toEqual({ data_revision: 80, status_revision: 12 }));

    source.onerror?.(new Event("error"));
    await vi.advanceTimersByTimeAsync(60_000);
    expect(client.getRevision).toHaveBeenCalledTimes(2);

    source.onmessage?.({ data: JSON.stringify({ data_revision: 0, status_revision: 0 }) } as MessageEvent<string>);
    expect(signals[1].aborted).toBe(true);
    await vi.waitFor(() => expect(client.getRevision).toHaveBeenCalledTimes(3));
    resolveAuthority({ data_revision: 0, status_revision: 0 });
    await vi.waitFor(() => expect(feed.get_snapshot()).toEqual({ data_revision: 0, status_revision: 0 }));
    const notificationCount = notifications.mock.calls.length;

    source.onmessage?.({ data: JSON.stringify({ data_revision: 0, status_revision: 0 }) } as MessageEvent<string>);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(client.getRevision).toHaveBeenCalledTimes(3);
    expect(notifications).toHaveBeenCalledTimes(notificationCount);
    expect(feed.get_snapshot()).toEqual({ data_revision: 0, status_revision: 0 });
    feed.dispose();
    resolveFallback({ data_revision: 80, status_revision: 12 });
  });
});
