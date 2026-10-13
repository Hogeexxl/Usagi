import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { DashboardFilters, DashboardRange, SessionListItemDto, SessionListResponse, UsageDto } from "../../data/types";
import { useSessionTableController } from "./useSessionTableController";

const filters: DashboardFilters = { agents: [], models: [], projects: [] };

function usage(total: number, cost = total / 100): UsageDto {
  return {
    calls: 1,
    errors: 0,
    input: Math.max(total - 2, 0),
    output: Math.min(total, 2),
    cache_read: 0,
    cache_write: 0,
    cache_write_1h: 0,
    reasoning: 0,
    total,
    cache_hit_rate: 0,
    cost,
    unpriced: 0,
    retried: 0,
    output_speed: null,
    speed_samples: 0,
  };
}

function item(index: number, options: { total?: number; cost?: number; activity?: number | null; hasUsage?: boolean; project?: string | null } = {}): SessionListItemDto {
  const rowUsage = usage(options.total ?? index, options.cost ?? index / 100);
  const project = options.project === null
    ? { kind: "projectless" as const }
    : { kind: "project" as const, path: `/project/${options.project ?? index}`, name: options.project ?? `Project ${index}` };
  return {
    thread: {
      key: { agent: "codex", session_id: `session-${index}`, thread_id: `thread-${index}` },
      type: "main",
      parent_thread_id: null,
      title: `Session ${index}`,
      project,
      first_usage_ms: index,
      last_usage_ms: index,
      model_effort: [{ model: `model-${index % 2}`, effort: "high", usage: rowUsage, compaction_tokens: null }],
      usage: rowUsage,
    },
    self_usage: rowUsage,
    subagent_usage: usage(0, 0),
    inclusive_usage: rowUsage,
    has_usage_rows: options.hasUsage ?? true,
    last_activity_ms: options.activity === undefined ? index : options.activity,
    subagent_count: index % 3,
  };
}

function snapshot(items: SessionListItemDto[], dataRevision = 4): SessionListResponse {
  return {
    data_revision: dataRevision,
    summary: { data_revision: dataRevision, usage: usage(items.length), session_count: items.length, session_median: null, session_p90: null },
    items,
  };
}

describe("useSessionTableController shared snapshot", () => {
  it("sorts and pages all snapshot items locally at ten per page", () => {
    const items = Array.from({ length: 25 }, (_, index) => item(index + 1));
    const result = renderHook(() => useSessionTableController({ key: "today" }, filters, snapshot(items), "ready", undefined, vi.fn())).result;

    expect(result.current.sort_by).toBe("last_activity");
    expect(result.current.sort_order).toBe("desc");
    expect(result.current.rows).toHaveLength(10);
    expect(result.current.rows[0].thread.title).toBe("Session 25");
    expect(result.current.total_items).toBe(25);
    expect(result.current.total_pages).toBe(3);

    act(() => result.current.select_sort("cost"));
    expect(result.current.sort_by).toBe("cost");
    expect(result.current.sort_order).toBe("desc");
    expect(result.current.rows[0].inclusive_usage.cost).toBe(0.25);
    act(() => result.current.next_page());
    expect(result.current.page).toBe(2);
    expect(result.current.rows).toHaveLength(10);
    expect(result.current.total_items).toBe(25);
  });

  it("keeps null last for ascending and descending sorts, using ThreadKey for ties", () => {
    const rows = [
      item(3, { total: 8, activity: null, hasUsage: false }),
      item(2, { total: 20, activity: 100 }),
      item(1, { total: 10, activity: 200 }),
      item(4, { total: 99, activity: null, hasUsage: false }),
    ];
    const result = renderHook(() => useSessionTableController({ key: "today" }, filters, snapshot(rows), "ready", undefined, vi.fn())).result;

    expect(result.current.rows.map((row) => row.thread.key.session_id)).toEqual(["session-1", "session-2", "session-3", "session-4"]);
    act(() => result.current.select_sort("total_tokens"));
    expect(result.current.rows.map((row) => row.thread.key.session_id)).toEqual(["session-2", "session-1", "session-3", "session-4"]);
    act(() => result.current.select_sort("total_tokens"));
    expect(result.current.sort_order).toBe("asc");
    expect(result.current.rows.map((row) => row.thread.key.session_id)).toEqual(["session-1", "session-2", "session-3", "session-4"]);
    expect(result.current.rows.slice(-2).every((row) => !row.has_usage_rows)).toBe(true);
  });

  it("uses project ascending by default and passes parent load and retry state through", () => {
    const retryLoad = vi.fn();
    const rows = [item(1, { project: null }), item(2, { project: "Bravo" }), item(3, { project: "Alpha" })];
    const range: DashboardRange = { key: "yesterday" };
    const result = renderHook(() => useSessionTableController(range, filters, snapshot(rows), "refreshing", "DATA_LOADING", retryLoad)).result;

    act(() => result.current.select_sort("project"));
    expect(result.current.sort_order).toBe("asc");
    expect(result.current.rows.map((row) => row.thread.key.session_id)).toEqual(["session-3", "session-2", "session-1"]);
    expect(result.current.load_state).toBe("refreshing");
    expect(result.current.error_code).toBe("DATA_LOADING");
    act(() => result.current.retry_load());
    expect(retryLoad).toHaveBeenCalledTimes(1);
  });
});
