import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../data/usagiClient";
import { createRevisionFeed } from "../data/revisionFeed";
import type { DashboardFilters, DashboardRange, FilterOptionsResponse, RuntimeStatusResponse, SummaryResponse } from "../data/types";
import { UsagiClientError } from "../data/types";
import { useDashboardController } from "./useDashboardController";

const emptyFilters: DashboardFilters = { agents: [], models: [], projects: [] };

const usage = (input = 10) => ({
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
  retried: 0, output_speed: null, speed_samples: 0,
});

const summary = (range: DashboardRange, revision = 1, input = 10): SummaryResponse => ({
  data_revision: revision,
  usage: usage(input),
  session_count: 3,
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
    summary: vi.fn(async (range) => summary(range)),
    modelDistribution: vi.fn(async () => ({ data_revision: 1, items: [] })),
    projectDistribution: vi.fn(async () => ({ data_revision: 1, items: [] })),
    getSessions: vi.fn(async () => ({ data_revision: 1, items: [] })),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(async () => status()),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    ...overrides,
  };
}

afterEach(() => vi.useRealTimers());

describe("useDashboardController", () => {
  it("loads Summary, Filter Options, and runtime Status for the initial range", async () => {
    const source = fakeEvents();
    const client = clientWith();
    const { result, unmount } = renderHook(() => useDashboardController({ client, eventSourceFactory: () => source }));

    await waitFor(() => expect(result.current.load_state).toBe("ready"));
    expect(result.current.metrics).toEqual(usage());
    expect(result.current.session_count).toBe(3);
    expect(result.current.range).toEqual({ key: "today" });
    expect(client.summary).toHaveBeenCalledWith({ key: "today" }, emptyFilters, expect.any(AbortSignal));
    expect(client.filterOptions).toHaveBeenCalledTimes(1);
    expect(client.getStatus).toHaveBeenCalledTimes(1);
    expect(client.getRevision).toHaveBeenCalledTimes(1);
    unmount();
  });

  it("keeps range, agent, provider/model pair, and project filters in the active query", async () => {
    const client = clientWith({
      summary: vi.fn(async (range, filters) => {
        const input = filters.models.length ? 20 : filters.agents.length ? 30 : filters.projects.length ? 40 : range.key === "today" ? 10 : 50;
        return summary(range, 1, input);
      }),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.metrics?.input).toBe(10));

    const pairFilters: DashboardFilters = {
      agents: ["claude"],
      models: [{ provider_id: "openai", model: "gpt-x" }],
      projects: [],
    };
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
    expect(client.summary).toHaveBeenLastCalledWith({ key: "yesterday" }, emptyFilters, expect.any(AbortSignal));
  });

  it("does not let a late response for an old range replace the current Summary", async () => {
    let resolveToday!: (value: SummaryResponse) => void;
    let resolveYesterday!: (value: SummaryResponse) => void;
    const today = new Promise<SummaryResponse>((resolve) => { resolveToday = resolve; });
    const yesterday = new Promise<SummaryResponse>((resolve) => { resolveYesterday = resolve; });
    const client = clientWith({ summary: vi.fn((range) => range.key === "today" ? today : yesterday) });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));

    await waitFor(() => expect(client.summary).toHaveBeenCalledTimes(1));
    await act(async () => result.current.select_range({ key: "yesterday" }));
    resolveYesterday(summary({ key: "yesterday" }, 2, 20));
    await waitFor(() => expect(result.current.metrics?.input).toBe(20));
    resolveToday(summary({ key: "today" }, 1, 10));
    await act(async () => undefined);
    expect(result.current.range).toEqual({ key: "yesterday" });
    expect(result.current.metrics?.input).toBe(20);
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

    await act(async () => {
      source.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 1 }) } as MessageEvent<string>);
    });
    await waitFor(() => expect(optionCalls).toBe(2));
    // The failed background reload keeps the options on screen without an error.
    expect(result.current.filter_options).toEqual(options(1));
    expect(result.current.filter_options_loading).toBe(false);
    expect(result.current.filter_options_error_code).toBeUndefined();

    await act(async () => {
      source.onmessage?.({ data: JSON.stringify({ data_revision: 3, status_revision: 1 }) } as MessageEvent<string>);
    });
    await waitFor(() => expect(result.current.filter_options).toEqual(options(3)));
    expect(result.current.filter_options_loading).toBe(false);
  });

  it("loads Filter Options for the selected range and reuses them when switching back", async () => {
    const client = clientWith({
      filterOptions: vi.fn(async (range: DashboardRange) => range.key === "year" ? options(2) : options(1)),
    });
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

  it("retries failed Filter Options once Magpie becomes ready", async () => {
    const source = fakeEvents();
    const client = clientWith({
      filterOptions: vi.fn()
        .mockRejectedValueOnce(new UsagiClientError("MAGPIE_UNAVAILABLE", 503))
        .mockResolvedValueOnce(options(1)),
      getStatus: vi.fn()
        .mockResolvedValueOnce(status({ status_revision: 0, runtime_state: "starting" }))
        .mockResolvedValueOnce(status({ status_revision: 1, runtime_state: "ready" })),
      getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 0 })),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: () => source }));
    await waitFor(() => expect(result.current.filter_options_error_code).toBe("MAGPIE_UNAVAILABLE"));
    await waitFor(() => expect(source.onmessage).toBeTypeOf("function"));

    // Only the status moves, so the reload comes from the runtime turning ready.
    source.onmessage?.({ data: JSON.stringify({ data_revision: 1, status_revision: 1 }) } as MessageEvent<string>);
    await waitFor(() => expect(result.current.filter_options).toEqual(options(1)));
    expect(result.current.filter_options_error_code).toBeUndefined();
  });


  it("uses the refresh icon for GET reloads of Summary and Filter Options only", async () => {
    const client = clientWith();
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.load_state).toBe("ready"));
    expect(client.summary).toHaveBeenCalledTimes(1);
    expect(client.filterOptions).toHaveBeenCalledTimes(1);

    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refreshing).toBe(false));
    expect(result.current.reload_key).toBe(1);
    expect(client.summary).toHaveBeenCalledTimes(2);
    expect(client.filterOptions).toHaveBeenCalledTimes(2);
    expect(client.getStatus).toHaveBeenCalledTimes(1);
    expect(client.quota).not.toHaveBeenCalled();
    expect(client.refreshQuota).not.toHaveBeenCalled();
  });

  it("recovers a starting Summary after the ready revision event", async () => {
    const source = fakeEvents();
    const readySummary = summary({ key: "today" }, 1, 90);
    const client = clientWith({
      summary: vi.fn()
        .mockRejectedValueOnce(new Error("starting"))
        .mockResolvedValueOnce(readySummary),
      getStatus: vi.fn()
        .mockResolvedValueOnce(status({ status_revision: 0, runtime_state: "starting" }))
        .mockResolvedValueOnce(status({ status_revision: 1, runtime_state: "ready" })),
      getRevision: vi.fn(async () => ({ data_revision: 0, status_revision: 0 })),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: () => source }));
    await waitFor(() => expect(result.current.load_state).toBe("error"));
    expect(client.summary).toHaveBeenCalledTimes(1);

    source.onmessage?.({ data: JSON.stringify({ data_revision: 1, status_revision: 1 }) } as MessageEvent<string>);
    await waitFor(() => expect(result.current.metrics?.input).toBe(90));
    expect(result.current.load_state).toBe("ready");
    expect(client.summary).toHaveBeenCalledTimes(2);
  });

  it("bootstraps fresh low revisions after an authoritative epoch reset", async () => {
    const client = clientWith({
      summary: vi.fn()
        .mockResolvedValueOnce(summary({ key: "today" }, 80, 80))
        .mockResolvedValueOnce(summary({ key: "today" }, 0, 0)),
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
    expect(result.current.epoch_reset_key).toBe(1);
    expect(feed.get_snapshot()).toEqual({ data_revision: 0, status_revision: 0 });
    expect(client.summary).toHaveBeenCalledTimes(2);
    expect(client.filterOptions).toHaveBeenCalledTimes(2);
    expect(client.getStatus).toHaveBeenCalledTimes(2);
    unmount();
  });
});
