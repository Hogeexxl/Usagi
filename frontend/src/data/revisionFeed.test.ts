import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "./usagiClient";
import { createRevisionFeed, type RevisionEventSource } from "./revisionFeed";

function clientWithRevision(revision: number): UsagiClient {
  return {
    filterOptions: vi.fn(),
    codexQuota: vi.fn(),
    summary: vi.fn(),
    modelDistribution: vi.fn(),
    projectDistribution: vi.fn(),
    skillsUsage: vi.fn(),
    getSessionSnapshot: vi.fn(),
    getSessionRows: vi.fn(),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(),
    getRevision: vi.fn(async () => ({ data_revision: revision, status_revision: revision })),
    refresh: vi.fn(),
  };
}

afterEach(() => vi.useRealTimers());

describe("RevisionFeed", () => {
  it("shares one EventSource, falls back to one poll timer, and closes after the last subscriber", async () => {
    vi.useFakeTimers();
    const source: RevisionEventSource = {
      onerror: null,
      onmessage: null,
      close: vi.fn(),
    };
    const factory = vi.fn(() => source);
    const client = clientWithRevision(3);
    const feed = createRevisionFeed({ client, eventSourceFactory: factory, pollIntervalMs: 100 });
    const first = vi.fn();
    const second = vi.fn();
    const unsubscribeFirst = feed.subscribe(first);
    const unsubscribeSecond = feed.subscribe(second);
    expect(factory).toHaveBeenCalledTimes(1);

    source.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 1 }) } as MessageEvent<string>);
    expect(first).toHaveBeenCalledWith({ data_revision: 2, status_revision: 1 });
    expect(second).toHaveBeenCalledWith({ data_revision: 2, status_revision: 1 });

    source.onerror?.(new Event("error"));
    await vi.advanceTimersByTimeAsync(100);
    expect(client.getRevision).toHaveBeenCalledTimes(2); // initial snapshot + fallback poll
    expect(feed.get_snapshot()).toEqual({ data_revision: 3, status_revision: 3 });

    source.onmessage?.({ data: JSON.stringify({ data_revision: 1, status_revision: 1 }) } as MessageEvent<string>);
    expect(first).toHaveBeenCalledTimes(3); // initial event, initial snapshot, equal poll success; no old-tuple event
    unsubscribeFirst();
    unsubscribeSecond();
    expect(source.close).toHaveBeenCalledTimes(1);
  });

  it("rejects a cross-old tuple instead of inventing a max/max revision", () => {
    const source: RevisionEventSource = { onerror: null, onmessage: null, close: vi.fn() };
    const client = clientWithRevision(0);
    const feed = createRevisionFeed({ client, eventSourceFactory: () => source });
    const listener = vi.fn();
    feed.subscribe(listener);
    source.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 2 }) } as MessageEvent<string>);
    source.onmessage?.({ data: JSON.stringify({ data_revision: 3, status_revision: 1 }) } as MessageEvent<string>);
    expect(feed.get_snapshot()).toEqual({ data_revision: 2, status_revision: 2 });
    expect(listener).toHaveBeenCalledTimes(1);
    feed.dispose();
  });
});
