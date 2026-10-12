import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../../data/usagiClient";
import { createRevisionFeed, type RevisionEventSource } from "../../data/revisionFeed";
import type { DashboardFilters, SessionListItemDto, UsageDto } from "../../data/types";
import { useSessionTableController } from "./useSessionTableController";

const filters: DashboardFilters = { agents: [], models: [], projects: [] };

function usage(total: number, cost = total / 100): UsageDto {
  return {
    calls: 1,
    errors: 0,
    input: total - 2,
    output: 2,
    cache_read: 0,
    cache_write: 0,
    cache_write_1h: 0,
    reasoning: 0,
    total,
    cache_hit_rate: 0,
    cost,
    unpriced: 0,
  };
}

function item(index: number, total = index, cost = total / 100): SessionListItemDto {
  const rowUsage = usage(total, cost);
  return {
    thread: {
      key: { agent: index % 2 ? "codex" : "aider", session_id: `session-${index}`, thread_id: `thread-${index}` },
      type: "main",
      parent_thread_id: null,
      title: `Session ${index}`,
      project: { kind: "project", path: `/project/${index % 3}`, name: `Project ${index % 3}` },
      first_usage_ms: index,
      last_usage_ms: index,
      model_effort: [{ model: `model-${index % 2}`, effort: "high", usage: rowUsage, compaction_tokens: null }],
      usage: rowUsage,
    },
    self_usage: rowUsage,
    subagent_usage: usage(0, 0),
    inclusive_usage: rowUsage,
    subagent_count: index % 3,
  };
}

function source(): RevisionEventSource {
  return { onerror: null, onmessage: null, close: vi.fn() };
}

function clientWith(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    quota: vi.fn(),
    refreshQuota: vi.fn(),
    filterOptions: vi.fn(),
    summary: vi.fn(),
    modelDistribution: vi.fn(),
    projectDistribution: vi.fn(),
    getSessions: vi.fn(async () => ({ data_revision: 1, items: [] })),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    ...overrides,
  };
}

describe("useSessionTableController SessionListItemView", () => {
  it("keeps the full Session list, pages locally, and sorts by inclusive cost", async () => {
    const items = Array.from({ length: 25 }, (_, index) => item(index + 1, 100 - index, index / 10));
    const client = clientWith({ getSessions: vi.fn(async () => ({ data_revision: 4, items })) });
    const { result } = renderHook(() => useSessionTableController({ key: "today" }, filters, { client }));

    await waitFor(() => expect(result.current.rows).toHaveLength(10));
    expect(result.current.total_items).toBe(25);
    expect(result.current.total_pages).toBe(3);
    expect(client.getSessions).toHaveBeenCalledTimes(1);
    expect(client.getSessions).toHaveBeenCalledWith(expect.objectContaining({ range: { key: "today" }, filters }));

    await act(async () => result.current.select_sort("cost"));
    expect(result.current.rows[0].inclusive_usage.cost).toBe(2.4);
    await act(async () => result.current.next_page());
    expect(result.current.page).toBe(2);
    expect(result.current.rows).toHaveLength(10);
    expect(client.getSessions).toHaveBeenCalledTimes(1);
  });

  it("refetches the complete list for a new scope and a manual reload key", async () => {
    const client = clientWith({ getSessions: vi.fn(async ({ range }) => ({
      data_revision: 4,
      items: [item(range.key === "today" ? 1 : 2)],
    })) });
    const { result, rerender } = renderHook(
      ({ range, reloadKey }: { range: { key: "today" | "yesterday" }; reloadKey: number }) => useSessionTableController(range, filters, { client, reloadKey }),
      { initialProps: { range: { key: "today" as "today" | "yesterday" }, reloadKey: 0 } },
    );
    await waitFor(() => expect(result.current.rows[0]?.thread.title).toBe("Session 1"));
    rerender({ range: { key: "yesterday" }, reloadKey: 0 });
    await waitFor(() => expect(result.current.rows[0]?.thread.title).toBe("Session 2"));
    rerender({ range: { key: "yesterday" }, reloadKey: 1 });
    await waitFor(() => expect(client.getSessions).toHaveBeenCalledTimes(3));
    expect(client.getSessions).toHaveBeenLastCalledWith(expect.objectContaining({ range: { key: "yesterday" }, filters }));
  });

  it("clears the old snapshot and accepts revision 0 after authoritative epoch reset", async () => {
    const eventSource: RevisionEventSource = source();
    const client = clientWith({
      getRevision: vi.fn()
        .mockResolvedValueOnce({ data_revision: 80, status_revision: 12 })
        .mockResolvedValueOnce({ data_revision: 0, status_revision: 0 }),
      getSessions: vi.fn()
        .mockResolvedValueOnce({ data_revision: 80, items: [item(80, 80)] })
        .mockResolvedValueOnce({ data_revision: 0, items: [item(0, 0)] }),
    });
    const feed = createRevisionFeed({ client, eventSourceFactory: () => eventSource });
    const { result } = renderHook(() => useSessionTableController({ key: "today" }, filters, { client, revisionFeed: feed }));
    await waitFor(() => expect(result.current.rows[0]?.thread.title).toBe("Session 80"));

    await act(async () => eventSource.onmessage?.({ data: JSON.stringify({ data_revision: 0, status_revision: 0 }) } as MessageEvent<string>));
    await waitFor(() => expect(result.current.rows[0]?.thread.title).toBe("Session 0"));
    expect(result.current.data_revision).toBe(0);
    expect(client.getSessions).toHaveBeenCalledTimes(2);
    feed.dispose();
  });

  it("keeps the current rows and page while a data revision refetches in place", async () => {
    const eventSource: RevisionEventSource = source();
    let resolveRefresh: (value: { data_revision: number; items: SessionListItemDto[] }) => void = () => {};
    const first = Array.from({ length: 15 }, (_, index) => item(index + 1));
    const client = clientWith({
      getRevision: vi.fn(async () => ({ data_revision: 5, status_revision: 1 })),
      getSessions: vi.fn()
        .mockResolvedValueOnce({ data_revision: 5, items: first })
        .mockImplementationOnce(() => new Promise((resolve) => { resolveRefresh = resolve; })),
    });
    const feed = createRevisionFeed({ client, eventSourceFactory: () => eventSource });
    const { result } = renderHook(() => useSessionTableController({ key: "today" }, filters, { client, revisionFeed: feed }));
    await waitFor(() => expect(result.current.total_items).toBe(15));
    await act(async () => result.current.next_page());

    await act(async () => eventSource.onmessage?.({ data: JSON.stringify({ data_revision: 6, status_revision: 1 }) } as MessageEvent<string>));
    await waitFor(() => expect(client.getSessions).toHaveBeenCalledTimes(2));
    expect(result.current.load_state).toBe("refreshing");
    expect(result.current.total_items).toBe(15);
    expect(result.current.page).toBe(2);

    await act(async () => resolveRefresh({ data_revision: 6, items: [...first, item(16)] }));
    expect(result.current.load_state).toBe("ready");
    expect(result.current.total_items).toBe(16);
    expect(result.current.page).toBe(2);
    feed.dispose();
  });
});
