import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../../data/usagiClient";
import { createRevisionFeed, type RevisionEventSource } from "../../data/revisionFeed";
import type {
  DashboardFilters,
  DashboardRange,
  SessionItemDto,
  SessionSnapshotResponse,
  SessionSortField,
  SessionSortIndexItem,
} from "../../data/types";
import { useSessionTableController } from "./useSessionTableController";

const emptyFilters: DashboardFilters = { models: [], projects: [] };
const range = (key: DashboardRange = { key: "today" }) => ({ key: key.key, start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" });
const usage = {
  input_tokens: 1,
  cached_tokens: 0,
  cache_write_tokens: null,
  uncached_input_tokens: null,
  output_tokens: 2,
  reasoning_tokens: 0,
  other_output_tokens: 2,
  total_tokens: 3,
  cache_hit_rate: null,
  estimated_cost: null,
  estimated_cost_status: "unknown" as const,
};

function item(id: string, total = 3): SessionItemDto {
  return {
    root_session_id: id,
    title: id,
    project_name: "Usagi",
    project_path: "/work/Usagi",
    last_activity_at_ms: 1_700_000_000_000,
    models_used: ["gpt-5"],
    subagent_count: 0,
    inclusive_usage: { ...usage, total_tokens: total },
    self_usage: { ...usage, total_tokens: total },
    subagent_usage: usage,
    data_status: "complete",
    error_code: null,
  };
}

function snapshot(count: number, seedCount = 40, key: DashboardRange = { key: "today" }): SessionSnapshotResponse {
  const sort_index: SessionSortIndexItem[] = Array.from({ length: count }, (_, index) => ({
    root_session_id: `root-${index + 1}`,
    last_activity_at_ms: count - index,
    project_sort_key: index % 4 === 0 ? null : `/project/${index % 3}`,
    model_sort_key: index % 5 === 0 ? null : `model-${index % 2}`,
    total_tokens: count - index,
    combined_total_tokens: count - index,
    combined_estimated_cost: index % 7 === 0 ? null : (count - index) / 100,
    cache_hit_rate: index % 6 === 0 ? null : (index % 10) / 10,
    data_status: "complete",
    error_code: null,
  }));
  return {
    range: range(key),
    data_revision: 1,
    total_items: count,
    sort_index,
    items: sort_index.slice(0, seedCount).map(({ root_session_id }, index) => item(root_session_id, count - index)),
  };
}

function sourceAndFeed(client: UsagiClient) {
  let source: RevisionEventSource | null = null;
  const feed = createRevisionFeed({
    client,
    eventSourceFactory: () => {
      const created: RevisionEventSource = { onerror: null, onmessage: null, close: vi.fn() };
      source = created;
      return created;
    },
  });
  return { feed, source: () => source };
}

function clientWith(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    filterOptions: vi.fn(),
    codexQuota: vi.fn(),
    summary: vi.fn(),
    modelDistribution: vi.fn(),
    projectDistribution: vi.fn(),
    skillsUsage: vi.fn(),
    getSessionSnapshot: vi.fn(async ({ range: key }) => snapshot(0, 0, key)),
    getSessionRows: vi.fn(async ({ range: key, root_session_ids }) => ({
      range: range(key),
      data_revision: 1,
      items: root_session_ids.map((id: string) => item(id)),
    })),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    refresh: vi.fn(),
    ...overrides,
  };
}

afterEach(() => vi.useRealTimers());

describe("useSessionTableController", () => {
  it("T-S05-001 keeps a full index, shows 10/page, jumps windows, and isolates QueryKey caches", async () => {
    const first = snapshot(200);
    const getSessionSnapshot = vi.fn(async ({ range: key }: Parameters<UsagiClient["getSessionSnapshot"]>[0]) => ({ ...first, range: range(key) }));
    const getSessionRows = vi.fn(async ({ range: key, root_session_ids }: Parameters<UsagiClient["getSessionRows"]>[0]) => ({
      range: range(key),
      data_revision: 1,
      items: root_session_ids.map((id) => item(id)),
    }));
    const client = clientWith({ getSessionSnapshot, getSessionRows });
    const { feed } = sourceAndFeed(client);
    const { result, rerender } = renderHook(
      ({ key, selectedFilters }: { key: DashboardRange; selectedFilters: DashboardFilters }) => useSessionTableController(key, selectedFilters, { client, revisionFeed: feed }),
      { initialProps: { key: { key: "today" }, selectedFilters: emptyFilters } },
    );

    await waitFor(() => expect(result.current.rows).toHaveLength(10));
    expect(result.current.total_items).toBe(200);
    expect(result.current.total_pages).toBe(20);
    expect(getSessionSnapshot).toHaveBeenCalledTimes(1);

    await act(async () => result.current.next_page());
    await waitFor(() => expect(result.current.page).toBe(2));
    expect(getSessionRows).not.toHaveBeenCalled();

    await act(async () => result.current.go_to_page(6));
    await waitFor(() => expect(result.current.page).toBe(6));
    await waitFor(() => expect(getSessionRows).toHaveBeenCalledTimes(1));
    expect(getSessionRows.mock.calls[0][0].root_session_ids).toHaveLength(40);
    expect(getSessionRows.mock.calls[0][0].root_session_ids[0]).toBe("root-41");
    expect(getSessionRows.mock.calls[0][0].root_session_ids[39]).toBe("root-80");

    await act(async () => result.current.select_sort("project"));
    expect(result.current.page).toBe(6);
    rerender({ key: { key: "yesterday" }, selectedFilters: { models: ["gpt-5"], projects: [] } });
    await waitFor(() => expect(result.current.page).toBe(1));
    expect(result.current.sort_by).toBe("project");
    expect(result.current.filters.models).toEqual(["gpt-5"]);
    expect(getSessionSnapshot).toHaveBeenCalledTimes(2);
    feed.dispose();
  });

  it("T-S04-003 covers full-index sorting, fixed pagination windows, null-last ties, and bounded prefetch", async () => {
    const sortIndex: SessionSortIndexItem[] = Array.from({ length: 200 }, (_, index) => ({
      root_session_id: index === 0 ? "root-199" : index === 198 ? "root-001" : `root-${String(index + 1).padStart(3, "0")}`,
      last_activity_at_ms: 200 - index,
      project_sort_key: index === 198 ? null : index === 199 ? "" : `project-${index % 4}`,
      model_sort_key: index === 198 ? null : index === 199 ? "" : `model-${index % 3}`,
      total_tokens: index < 2 ? 200 : 200 - index,
      combined_total_tokens: index < 2 ? 400 : 400 - index,
      combined_estimated_cost: index === 198 ? null : index < 2 ? 20 : (200 - index) / 10,
      cache_hit_rate: index === 198 ? null : (index % 10) / 10,
      data_status: "complete",
      error_code: null,
    }));
    const full: SessionSnapshotResponse = {
      range: range(),
      data_revision: 1,
      total_items: sortIndex.length,
      sort_index: sortIndex,
      items: sortIndex.slice(0, 40).map(({ root_session_id }) => item(root_session_id)),
    };
    const getSessionSnapshot = vi.fn(async ({ range: key }: Parameters<UsagiClient["getSessionSnapshot"]>[0]) => ({ ...full, range: range(key) }));
    const getSessionRows = vi.fn(async ({ range: key, root_session_ids }: Parameters<UsagiClient["getSessionRows"]>[0]) => ({
      range: range(key),
      data_revision: 1,
      items: root_session_ids.map((id) => item(id)),
    }));
    const client = clientWith({ getSessionSnapshot, getSessionRows });
    const { feed } = sourceAndFeed(client);
    const { result, unmount } = renderHook(() => useSessionTableController({ key: "today" }, emptyFilters, { client, revisionFeed: feed }));
    await waitFor(() => expect(result.current.rows).toHaveLength(10));

    const validText = (value: string | null) => value !== null && value.length > 0;
    const valueFor = (entry: SessionSortIndexItem, field: SessionSortField): string | number | null => {
      switch (field) {
        case "project": return entry.project_sort_key;
        case "model": return entry.model_sort_key;
        case "last_activity": return entry.last_activity_at_ms;
        case "total_tokens": return entry.total_tokens;
        case "combined_total_tokens": return entry.combined_total_tokens;
        case "combined_estimated_cost": return entry.combined_estimated_cost;
        case "cache_hit_rate": return entry.cache_hit_rate;
      }
    };
    const compare = (left: SessionSortIndexItem, right: SessionSortIndexItem, field: SessionSortField, order: "asc" | "desc") => {
      const leftValue = valueFor(left, field);
      const rightValue = valueFor(right, field);
      if (field === "project" || field === "model") {
        const leftPresent = validText(leftValue as string | null);
        const rightPresent = validText(rightValue as string | null);
        if (leftPresent !== rightPresent) return leftPresent ? -1 : 1;
      } else if (leftValue === null || rightValue === null) {
        if (leftValue !== rightValue) return leftValue === null ? 1 : -1;
      }
      const valueComparison = leftValue === null || rightValue === null ? 0 : leftValue < rightValue ? -1 : leftValue > rightValue ? 1 : 0;
      return valueComparison === 0
        ? (left.root_session_id < right.root_session_id ? -1 : left.root_session_id > right.root_session_id ? 1 : 0)
        : order === "asc" ? valueComparison : -valueComparison;
    };
    const expectedPage = (field: SessionSortField, order: "asc" | "desc", page: number) => [...sortIndex]
      .sort((left, right) => compare(left, right, field, order))
      .slice((page - 1) * 10, page * 10)
      .map((entry) => entry.root_session_id);
    const setSort = async (field: SessionSortField, order: "asc" | "desc") => {
      if (result.current.sort_by !== field) await act(async () => result.current.select_sort(field));
      if (result.current.sort_order !== order) await act(async () => result.current.select_sort(field));
      expect(result.current.page).toBe(2);
    };

    await act(async () => result.current.go_to_page(2));
    await waitFor(() => expect(result.current.page).toBe(2));
    for (const field of ["last_activity", "project", "model", "total_tokens", "combined_total_tokens", "cache_hit_rate", "combined_estimated_cost"] as const) {
      await setSort(field, "asc");
      await waitFor(() => expect(result.current.rows.map((row) => row.root_session_id)).toEqual(expectedPage(field, "asc", 2)));
      await setSort(field, "desc");
      await waitFor(() => expect(result.current.rows.map((row) => row.root_session_id)).toEqual(expectedPage(field, "desc", 2)));
    }

    await setSort("combined_estimated_cost", "desc");
    await act(async () => result.current.go_to_page(20));
    await waitFor(() => expect(result.current.rows.map((row) => row.root_session_id)).toEqual(expectedPage("combined_estimated_cost", "desc", 20)));
    expect(result.current.rows.at(-1)?.root_session_id).toBe(
      sortIndex.find((entry) => entry.combined_estimated_cost === null)?.root_session_id,
    );

    unmount();
    feed.dispose();

    const prefetchRows = vi.fn(async ({ range: key, root_session_ids }: Parameters<UsagiClient["getSessionRows"]>[0]) => ({
      range: range(key),
      data_revision: 1,
      items: root_session_ids.map((id) => item(id)),
    }));
    const prefetchClient = clientWith({
      getSessionSnapshot: vi.fn(async ({ range: key }: Parameters<UsagiClient["getSessionSnapshot"]>[0]) => ({ ...full, range: range(key) })),
      getSessionRows: prefetchRows,
    });
    const { feed: prefetchFeed } = sourceAndFeed(prefetchClient);
    const { result: prefetchResult } = renderHook(() => useSessionTableController({ key: "today" }, emptyFilters, { client: prefetchClient, revisionFeed: prefetchFeed }));
    await waitFor(() => expect(prefetchResult.current.rows).toHaveLength(10));
    const requestCountBeforePrefetch = prefetchRows.mock.calls.length;
    await act(async () => prefetchResult.current.go_to_page(3));
    await waitFor(() => expect(prefetchRows.mock.calls.length).toBeGreaterThan(requestCountBeforePrefetch));
    const page3Request = prefetchRows.mock.calls.at(-1)?.[0].root_session_ids ?? [];
    expect(page3Request).toHaveLength(40);
    expect(page3Request[0]).toBe("root-041");
    expect(page3Request.at(-1)).toBe("root-080");
    const countAfterPage3 = prefetchRows.mock.calls.length;
    await waitFor(() => expect(prefetchRows.mock.calls.length).toBe(countAfterPage3));
    await act(async () => prefetchResult.current.go_to_page(7));
    await waitFor(() => expect(prefetchRows.mock.calls.length).toBeGreaterThan(countAfterPage3));
    const page7Request = prefetchRows.mock.calls.at(-1)?.[0].root_session_ids ?? [];
    expect(page7Request).toHaveLength(40);
    expect(page7Request[0]).toBe("root-081");
    expect(page7Request.at(-1)).toBe("root-120");
    const countAfterPage7 = prefetchRows.mock.calls.length;
    await waitFor(() => expect(prefetchRows.mock.calls.length).toBe(countAfterPage7));
    await act(async () => prefetchResult.current.select_sort("last_activity"));
    expect(prefetchRows.mock.calls.length).toBe(countAfterPage7 + 1);
    prefetchFeed.dispose();
  });

  it("T-S06-001 upgrades a pending prefetch when the foreground jumps into its window", async () => {
    let resolveBatch!: () => void;
    const getSessionRows = vi.fn(({ range: key, root_session_ids }: Parameters<UsagiClient["getSessionRows"]>[0]) =>
      new Promise<Awaited<ReturnType<UsagiClient["getSessionRows"]>>>((resolve) => {
        resolveBatch = () => resolve({ range: range(key), data_revision: 1, items: root_session_ids.map((id) => item(id)) });
      }),
    );
    const full = snapshot(200);
    const client = clientWith({
      getSessionSnapshot: vi.fn(async ({ range: key }: Parameters<UsagiClient["getSessionSnapshot"]>[0]) => ({ ...full, range: range(key) })),
      getSessionRows,
    });
    const { feed } = sourceAndFeed(client);
    const { result } = renderHook(() => useSessionTableController({ key: "today" }, emptyFilters, { client, revisionFeed: feed }));
    await waitFor(() => expect(result.current.rows).toHaveLength(10));

    await act(async () => result.current.go_to_page(3));
    await waitFor(() => expect(getSessionRows).toHaveBeenCalledTimes(1));
    expect(getSessionRows.mock.calls[0][0].root_session_ids).toHaveLength(40);
    expect(result.current.page_state).toBe("idle");

    await act(async () => {
      result.current.go_to_page(5);
      result.current.go_to_page(6);
    });
    await waitFor(() => expect(result.current.page_state).toBe("loading"));
    expect(getSessionRows).toHaveBeenCalledTimes(1);

    await act(async () => resolveBatch());
    await waitFor(() => expect(result.current.page_state).toBe("idle"));
    await waitFor(() => expect(result.current.rows).toHaveLength(10));
    expect(result.current.page).toBe(6);
    expect(getSessionRows).toHaveBeenCalledTimes(1);
    feed.dispose();
  });

  it("does not let a late row response cross scope or revision", async () => {
    let resolveRows!: (value: Awaited<ReturnType<UsagiClient["getSessionRows"]>>) => void;
    const client = clientWith({
      getSessionSnapshot: vi.fn(async ({ range: key }) => ({ ...snapshot(80), range: range(key) })),
      getSessionRows: vi.fn(() => new Promise<Awaited<ReturnType<UsagiClient["getSessionRows"]>>>((resolve) => { resolveRows = resolve; })),
    });
    const { feed } = sourceAndFeed(client);
    const { result, rerender } = renderHook(({ key }: { key: DashboardRange }) => useSessionTableController(key, emptyFilters, { client, revisionFeed: feed }), { initialProps: { key: { key: "today" } } });
    await waitFor(() => expect(result.current.rows).toHaveLength(10));
    await act(async () => result.current.go_to_page(6));
    rerender({ key: { key: "yesterday" } });
    resolveRows({ range: range({ key: "today" }), data_revision: 1, items: [item("root-61")] });
    await act(async () => undefined);
    expect(result.current.range).toEqual({ key: "yesterday" });
    expect(result.current.rows.every((row) => row.root_session_id !== "root-61")).toBe(true);
    feed.dispose();
  });

  it("T-S05-001 settles a revision refresh after bounded stale snapshot responses", async () => {
    let snapshotCalls = 0;
    let resolveStale!: (value: SessionSnapshotResponse) => void;
    const fresh = snapshot(1);
    const getSessionSnapshot = vi.fn(async ({ range: key }: Parameters<UsagiClient["getSessionSnapshot"]>[0]) => {
      snapshotCalls += 1;
      if (snapshotCalls === 1) return { ...fresh, range: range(key) };
      if (snapshotCalls === 2) return await new Promise<SessionSnapshotResponse>((resolve) => { resolveStale = resolve; });
      return {
        ...fresh,
        range: range(key),
        data_revision: 2,
        items: [item("root-fresh", 99)],
        sort_index: [{
          ...fresh.sort_index[0],
          root_session_id: "root-fresh",
          total_tokens: 99,
          combined_total_tokens: 99,
          combined_estimated_cost: 0.99,
        }],
      };
    });
    const client = clientWith({ getSessionSnapshot });
    const { feed, source } = sourceAndFeed(client);
    const { result } = renderHook(() => useSessionTableController({ key: "today" }, emptyFilters, { client, revisionFeed: feed }));
    await waitFor(() => expect(result.current.rows.map((row) => row.root_session_id)).toEqual(["root-1"]));
    await act(async () => source()?.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 2 }) } as MessageEvent<string>));
    await waitFor(() => expect(snapshotCalls).toBe(2));
    resolveStale({ ...fresh, data_revision: 1 });
    await waitFor(() => expect(snapshotCalls).toBe(3));
    await waitFor(() => expect(result.current.load_state).toBe("ready"));
    expect(result.current.rows.map((row) => row.root_session_id)).toEqual(["root-fresh"]);
    expect(result.current.load_state).not.toBe("refreshing");
    feed.dispose();
  });
});
