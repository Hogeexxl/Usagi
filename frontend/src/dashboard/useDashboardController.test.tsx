import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../data/usagiClient";
import { createRevisionFeed } from "../data/revisionFeed";
import type { DashboardFilters, DashboardRange, FilterOptionsResponse, RuntimeStatusResponse, SessionListItemDto, SessionListResponse, UsageDto } from "../data/types";
import { UsagiClientError } from "../data/types";
import { useDashboardController } from "./useDashboardController";

const emptyFilters: DashboardFilters = { agents: [], models: [], projects: [] };

const usage = (input = 10): UsageDto => ({
  calls: 2,
  errors: 0,
  input,
  output: 4,
  cache_read: 3,
  cache_write: 1,
  cache_write_1h: 0,
  reasoning: 2,
  total: input + 8,
  cache_hit_rate: 0.3,
  cost: 0.25,
  unpriced: 0,
  retried: 0,
  output_speed: null,
  speed_samples: 0,
});

const summary = (revision = 1, input = 10, sessionCount = 3) => ({
  data_revision: revision,
  usage: usage(input),
  session_count: sessionCount,
  session_median: sessionCount === 0 ? null : input,
  session_p90: sessionCount === 0 ? null : input * 2,
});

function row(title: string, revision = 1, input = 10): SessionListItemDto {
  const rowUsage = usage(input);
  return {
    thread: {
      key: { agent: "claude", session_id: `session-${revision}`, thread_id: `thread-${revision}` },
      type: "main",
      parent_thread_id: null,
      title,
      project: { kind: "project", path: "/work/Usagi", name: "Usagi" },
      first_usage_ms: 100,
      last_usage_ms: 200,
      model_effort: [{ model: "gpt-x", effort: "high", usage: rowUsage, compaction_tokens: null }],
      usage: rowUsage,
    },
    self_usage: rowUsage,
    subagent_usage: usage(0),
    inclusive_usage: rowUsage,
    has_usage_rows: true,
    last_activity_ms: 200,
    subagent_count: 0,
  };
}

const sessions = (revision = 1, input = 10, title = `Session ${revision}`): SessionListResponse => ({
  data_revision: revision,
  summary: summary(revision, input),
  items: [row(title, revision, input)],
});

const options = (revision = 1): FilterOptionsResponse => ({
  data_revision: revision,
  agents: [{ agent: "claude", display_name: "Claude Code" }],
  models: [{ provider_id: "openai", provider_name: "OpenAI", model: "gpt-x" }],
  projects: [{ kind: "project", path: "/work/Usagi", name: "Usagi" }],
});

const status = (overrides: Partial<RuntimeStatusResponse> = {}): RuntimeStatusResponse => ({
  status_revision: 1,
  runtime_state: "ready",
  last_error_code: null,
  ...overrides,
});

function fakeEvents() {
  return {
    onerror: null as ((event: Event) => void) | null,
    onmessage: null as ((event: MessageEvent<string>) => void) | null,
    close: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };
}

function clientWith(overrides: Partial<UsagiClient> = {}): UsagiClient {
  return {
    quota: vi.fn(async () => ({ reading: false, items: [] })),
    refreshQuota: vi.fn(async () => ({ reading: false, items: [] })),
    filterOptions: vi.fn(async () => options()),
    summary: vi.fn(),
    modelDistribution: vi.fn(async () => ({ data_revision: 1, items: [] })),
    projectDistribution: vi.fn(async () => ({ data_revision: 1, items: [] })),
    getSessions: vi.fn(async () => sessions()),
    toolUsage: vi.fn(async (days) => ({ days, calls: 0, top: [] })),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(async () => status()),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

afterEach(() => vi.useRealTimers());

describe("useDashboardController sessions snapshot", () => {
  it("derives metrics and session count from the same initial sessions response", async () => {
    const source = fakeEvents();
    const client = clientWith();
    const { result, unmount } = renderHook(() => useDashboardController({ client, eventSourceFactory: () => source }));

    await waitFor(() => expect(result.current.load_state).toBe("ready"));
    expect(result.current.metrics).toEqual(usage());
    expect(result.current.session_count).toBe(3);
    expect(result.current.session_median).toBe(10);
    expect(result.current.session_p90).toBe(20);
    expect(result.current.sessions?.items[0].thread.title).toBe("Session 1");
    expect(result.current.range).toEqual({ key: "today" });
    expect(client.getSessions).toHaveBeenCalledWith({ range: { key: "today" }, filters: emptyFilters, signal: expect.any(AbortSignal) });
    expect(client.summary).not.toHaveBeenCalled();
    expect(client.filterOptions).toHaveBeenCalledTimes(1);
    expect(client.getStatus).toHaveBeenCalledTimes(1);
    expect(client.getRevision).toHaveBeenCalledTimes(1);
    unmount();
  });

  it("keeps range and filters in the shared sessions query", async () => {
    const client = clientWith({
      getSessions: vi.fn(async ({ range, filters }) => {
        const input = filters.models.length ? 20 : filters.agents.length ? 30 : filters.projects.length ? 40 : range.key === "today" ? 10 : 50;
        return sessions(1, input);
      }),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.metrics?.input).toBe(10));

    const pairFilters: DashboardFilters = { agents: ["claude"], models: [{ provider_id: "openai", model: "gpt-x" }], projects: [] };
    await act(async () => result.current.select_filters(pairFilters));
    await waitFor(() => expect(result.current.metrics?.input).toBe(20));
    expect(result.current.agentFilterActive).toBe(true);
    expect(result.current.modelFilterActive).toBe(true);
    expect(result.current.projectFilterActive).toBe(false);

    await act(async () => result.current.select_range({ key: "yesterday" }));
    await waitFor(() => expect(result.current.range).toEqual({ key: "yesterday" }));
    expect(result.current.filters).toEqual(pairFilters);
    await act(async () => result.current.select_filters({ ...emptyFilters, projects: [{ kind: "projectless" }] }));
    await waitFor(() => expect(result.current.metrics?.input).toBe(40));
    expect(result.current.projectFilterActive).toBe(true);
    await act(async () => result.current.clear_filters());
    await waitFor(() => expect(result.current.metrics?.input).toBe(50));
    expect(result.current.anyFilterActive).toBe(false);
    expect(client.getSessions).toHaveBeenLastCalledWith(expect.objectContaining({ range: { key: "yesterday" }, filters: emptyFilters }));
  });

  it("does not let a late response for an old range replace the current snapshot", async () => {
    let resolveToday!: (value: SessionListResponse) => void;
    let resolveYesterday!: (value: SessionListResponse) => void;
    const today = new Promise<SessionListResponse>((resolve) => { resolveToday = resolve; });
    const yesterday = new Promise<SessionListResponse>((resolve) => { resolveYesterday = resolve; });
    const client = clientWith({ getSessions: vi.fn(({ range }) => range.key === "today" ? today : yesterday) });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));

    await waitFor(() => expect(client.getSessions).toHaveBeenCalledTimes(1));
    await act(async () => result.current.select_range({ key: "yesterday" }));
    resolveYesterday(sessions(2, 20, "Yesterday"));
    await waitFor(() => expect(result.current.metrics?.input).toBe(20));
    resolveToday(sessions(1, 10, "Today"));
    await act(async () => undefined);
    expect(result.current.range).toEqual({ key: "yesterday" });
    expect(result.current.metrics?.input).toBe(20);
    expect(result.current.sessions?.items[0].thread.title).toBe("Yesterday");
  });

  it("keeps the prior complete KPI and table snapshot while refreshing, then replaces both together", async () => {
    const pending = deferred<SessionListResponse>();
    const client = clientWith({ getSessions: vi.fn().mockResolvedValueOnce(sessions()).mockReturnValueOnce(pending.promise) });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.load_state).toBe("ready"));

    act(() => result.current.request_refresh());
    expect(result.current.load_state).toBe("refreshing");
    expect(result.current.metrics?.input).toBe(10);
    expect(result.current.sessions?.items[0].thread.title).toBe("Session 1");
    pending.resolve(sessions(2, 99, "Updated Session"));
    await waitFor(() => expect(result.current.sessions?.items[0].thread.title).toBe("Updated Session"));
    expect(result.current.metrics?.input).toBe(99);
    expect(result.current.session_median).toBe(99);
    expect(result.current.session_p90).toBe(198);
    expect(result.current.session_count).toBe(3);
  });

  it("keeps DATA_LOADING sessions in loading state and treats filter options DATA_LOADING as loading", async () => {
    const client = clientWith({
      getSessions: vi.fn().mockRejectedValueOnce(new UsagiClientError("DATA_LOADING", 503)).mockResolvedValueOnce(sessions()),
      filterOptions: vi.fn().mockRejectedValueOnce(new UsagiClientError("DATA_LOADING", 503)).mockResolvedValueOnce(options(2)),
      getStatus: vi.fn()
        .mockResolvedValueOnce(status({ status_revision: 0, runtime_state: "starting" }))
        .mockResolvedValueOnce(status({ status_revision: 1, runtime_state: "ready" })),
      getRevision: vi.fn(async () => ({ data_revision: 0, status_revision: 0 })),
    });
    const source = fakeEvents();
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: () => source }));
    await waitFor(() => expect(client.getSessions).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(result.current.load_state).toBe("loading"));
    expect(result.current.error_code).toBeUndefined();
    expect(result.current.metrics).toBeNull();
    expect(result.current.sessions).toBeNull();
    expect(result.current.filter_options_loading).toBe(true);
    expect(result.current.filter_options_error_code).toBeUndefined();

    source.onmessage?.({ data: JSON.stringify({ data_revision: 0, status_revision: 1 }) } as MessageEvent<string>);
    await waitFor(() => expect(result.current.load_state).toBe("ready"));
    expect(result.current.filter_options).toEqual(options(2));
    expect(client.getSessions).toHaveBeenCalledTimes(2);
    expect(client.filterOptions).toHaveBeenCalledTimes(2);
  });

  it("reloads Filter Options in the background on data revisions and keeps them on failure", async () => {
    const source = fakeEvents();
    let optionCalls = 0;
    const client = clientWith({
      filterOptions: vi.fn(async () => {
        optionCalls += 1;
        if (optionCalls === 2) throw new Error("options unavailable");
        return optionCalls === 1 ? options(1) : options(3);
      }),
      getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: () => source }));
    await waitFor(() => expect(result.current.filter_options).toEqual(options(1)));

    await act(async () => source.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 1 }) } as MessageEvent<string>));
    await waitFor(() => expect(optionCalls).toBe(2));
    expect(result.current.filter_options).toEqual(options(1));
    expect(result.current.filter_options_loading).toBe(false);
    expect(result.current.filter_options_error_code).toBeUndefined();

    await act(async () => source.onmessage?.({ data: JSON.stringify({ data_revision: 3, status_revision: 1 }) } as MessageEvent<string>));
    await waitFor(() => expect(result.current.filter_options).toEqual(options(3)));
    expect(result.current.filter_options_loading).toBe(false);
  });

  it("loads Filter Options for a selected range and reuses cached options when switching back", async () => {
    const client = clientWith({ filterOptions: vi.fn(async (range: DashboardRange) => range.key === "year" ? options(2) : options(1)) });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.filter_options).toEqual(options(1)));
    expect(vi.mocked(client.filterOptions).mock.calls[0][0]).toEqual({ key: "today" });

    act(() => result.current.select_range({ key: "year" }));
    expect(result.current.filter_options).toBeNull();
    expect(result.current.filter_options_loading).toBe(true);
    await waitFor(() => expect(result.current.filter_options).toEqual(options(2)));
    expect(vi.mocked(client.filterOptions).mock.calls[1][0]).toEqual({ key: "year" });

    act(() => result.current.select_range({ key: "today" }));
    expect(result.current.filter_options).toEqual(options(1));
    expect(result.current.filter_options_loading).toBe(false);
  });

  it("uses the refresh icon for sessions and filter options only", async () => {
    const client = clientWith();
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.load_state).toBe("ready"));
    expect(client.getSessions).toHaveBeenCalledTimes(1);
    expect(client.filterOptions).toHaveBeenCalledTimes(1);

    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refreshing).toBe(false));
    expect(result.current.reload_key).toBe(1);
    expect(client.getSessions).toHaveBeenCalledTimes(2);
    expect(client.filterOptions).toHaveBeenCalledTimes(2);
    expect(client.getStatus).toHaveBeenCalledTimes(1);
    expect(client.quota).not.toHaveBeenCalled();
    expect(client.refreshQuota).not.toHaveBeenCalled();
    expect(client.summary).not.toHaveBeenCalled();
  });

  it("bootstraps fresh low revisions after an authoritative epoch reset", async () => {
    const client = clientWith({
      getSessions: vi.fn()
        .mockResolvedValueOnce(sessions(80, 80))
        .mockResolvedValueOnce({ ...sessions(0, 0), summary: { ...summary(0, 0, 0), session_median: null, session_p90: null } }),
      filterOptions: vi.fn()
        .mockResolvedValueOnce(options(80))
        .mockResolvedValueOnce({ data_revision: 0, agents: [], models: [], projects: [] }),
      getStatus: vi.fn()
        .mockResolvedValueOnce(status({ status_revision: 12 }))
        .mockResolvedValueOnce(status({ status_revision: 0 })),
      getRevision: vi.fn()
        .mockResolvedValueOnce({ data_revision: 80, status_revision: 12 })
        .mockResolvedValueOnce({ data_revision: 0, status_revision: 0 }),
    });
    const feed = createRevisionFeed({ client, eventSourceFactory: fakeEvents });
    const { result, unmount } = renderHook(() => useDashboardController({ client, revisionFeed: feed }));
    await waitFor(() => expect(result.current.data_revision).toBe(80));
    expect(result.current.filter_options).toEqual(options(80));

    await act(async () => feed.retry_now());
    await waitFor(() => expect(result.current.data_revision).toBe(0));
    await waitFor(() => expect(result.current.filter_options).toEqual({ data_revision: 0, agents: [], models: [], projects: [] }));
    expect(result.current.metrics?.input).toBe(0);
    expect(result.current.session_count).toBe(0);
    expect(result.current.session_median).toBeNull();
    expect(result.current.epoch_reset_key).toBe(1);
    expect(feed.get_snapshot()).toEqual({ data_revision: 0, status_revision: 0 });
    expect(client.getSessions).toHaveBeenCalledTimes(2);
    expect(client.filterOptions).toHaveBeenCalledTimes(2);
    expect(client.getStatus).toHaveBeenCalledTimes(2);
    unmount();
  });
});
