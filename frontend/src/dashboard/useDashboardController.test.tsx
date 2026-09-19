import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UsagiClient } from "../data/usagiClient";
import { createRevisionFeed } from "../data/revisionFeed";
import type { DashboardFilters, DashboardRange, StatusResponse, SummaryResponse } from "../data/types";
import { useDashboardController } from "./useDashboardController";

const status = (overrides: Partial<StatusResponse> = {}): StatusResponse => ({
  data_revision: 1,
  status_revision: 1,
  scan_state: "idle",
  active_scan_id: null,
  last_finished_scan_id: null,
  last_finished_scan_result: null,
  followup: null,
  target_scan: null,
  last_scan_started_at_ms: null,
  last_scan_completed_at_ms: null,
  last_scan_failed_at_ms: null,
  last_scan_error_code: null,
  source_binding_status: "ready",
  ...overrides,
});

const summary = (range: DashboardRange, revision = 1, input = 10): SummaryResponse => ({
  range: { key: range.key, start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" },
  data_revision: revision,
  usage: {
    input_tokens: input,
    cached_tokens: 0,
    cache_write_tokens: null,
    uncached_input_tokens: null,
    output_tokens: 2,
    reasoning_tokens: 0,
    other_output_tokens: 2,
    total_tokens: input + 2,
    cache_hit_rate: null,
    estimated_cost: null,
    estimated_cost_status: "unknown",
    session_count: 1,
    cost_incomplete_session_count: 1,
    complete_session_cost_per_million_tokens: null,
    session_health: {
      total_sessions: 1,
      complete_sessions: 1,
      incomplete_sessions: 0,
      error_sessions: 0,
    },
  },
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
    filterOptions: vi.fn(async () => ({ data_revision: 1, models: [], projects: [] })),
    codexQuota: vi.fn(async () => ({
      status: "unavailable" as const,
      account_email: null,
      plan_type: null,
      session: null,
      weekly: null,
      reset_credits_available: null,
      fetched_at_ms: null,
    })),
    summary: vi.fn(async (range) => summary(range)),
    modelDistribution: vi.fn(),
    projectDistribution: vi.fn(),
    skillsUsage: vi.fn(),
    getSessionSnapshot: vi.fn(async () => ({
      range: { key: "today" as const, start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" },
      data_revision: 1,
      total_items: 0,
      sort_index: [],
      items: [],
    })),
    getSessionRows: vi.fn(async ({ range }) => ({
      range: { key: range.key, start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" },
      data_revision: 1,
      items: [],
    })),
    getSessionDetail: vi.fn(),
    getStatus: vi.fn(async () => status()),
    getRevision: vi.fn(async () => ({ data_revision: 1, status_revision: 1 })),
    refresh: vi.fn(async () => ({ http_status: 202 as const, disposition: "started" as const, scan_id: "scan-1", status_revision: 2 })),
    ...overrides,
  };
}

afterEach(() => vi.useRealTimers());

describe("useDashboardController", () => {
  it("t_s07_002 keeps range/filters independent and isolates query snapshots", async () => {
    const source = fakeEvents();
    const client = clientWith({
      summary: vi.fn(async (range, filters) => {
        const input = filters.models.length > 0 ? 20 : filters.projects.length > 0 ? 30 : range.key === "today" ? 10 : 40;
        return summary(range, 1, input);
      }),
    });
    const { result, unmount } = renderHook(() => useDashboardController({ client, eventSourceFactory: () => source }));
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(10));

    const modelFilters: DashboardFilters = { models: ["gpt-a"], projects: [] };
    await act(async () => result.current.select_filters(modelFilters));
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(20));
    expect(result.current.range).toEqual({ key: "today" });
    expect(result.current.modelFilterActive).toBe(true);
    expect(result.current.projectFilterActive).toBe(false);
    expect(result.current.anyFilterActive).toBe(true);

    await act(async () => result.current.select_range({ key: "yesterday" }));
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(20));
    expect(result.current.filters).toEqual(modelFilters);
    expect(result.current.range).toEqual({ key: "yesterday" });

    await act(async () => result.current.select_filters({ models: [], projects: [{ kind: "projectless" }] }));
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(30));
    expect(result.current.range).toEqual({ key: "yesterday" });
    expect(result.current.projectFilterActive).toBe(true);
    expect(result.current.modelFilterActive).toBe(false);

    await act(async () => result.current.clear_filters());
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(40));
    expect(result.current.range).toEqual({ key: "yesterday" });
    expect(result.current.anyFilterActive).toBe(false);
    expect(client.summary).toHaveBeenLastCalledWith({ key: "yesterday" }, { models: [], projects: [] }, expect.any(AbortSignal));

    unmount();
    const raceSource = fakeEvents();
    type SummaryRequest = {
      range: DashboardRange;
      filters: DashboardFilters;
      signal: AbortSignal | undefined;
      resolve: (value: SummaryResponse) => void;
    };
    const summaryRequests: SummaryRequest[] = [];
    const raceClient = clientWith({
      summary: vi.fn((range: DashboardRange, filters: DashboardFilters, signal?: AbortSignal) => {
        let resolve!: (value: SummaryResponse) => void;
        const promise = new Promise<SummaryResponse>((resolvePromise) => {
          resolve = resolvePromise;
        });
        summaryRequests.push({ range, filters, signal, resolve });
        return promise;
      }),
    });
    const raceHook = renderHook(() => useDashboardController({ client: raceClient, eventSourceFactory: () => raceSource }));
    await waitFor(() => expect(summaryRequests).toHaveLength(1));
    summaryRequests[0].resolve(summary({ key: "today" }, 1, 10));
    await waitFor(() => expect(raceHook.result.current.metrics?.input_tokens).toBe(10));

    await act(async () => raceHook.result.current.select_filters({ models: ["gpt-a"], projects: [] }));
    await waitFor(() => expect(summaryRequests).toHaveLength(2));
    expect(summaryRequests[0].signal?.aborted).toBe(true);

    await act(async () => raceHook.result.current.select_range({ key: "yesterday" }));
    await waitFor(() => expect(summaryRequests).toHaveLength(3));
    expect(summaryRequests[1].signal?.aborted).toBe(true);
    summaryRequests[1].resolve(summary({ key: "today" }, 1, 20));
    summaryRequests[2].resolve(summary({ key: "yesterday" }, 1, 30));
    await waitFor(() => expect(raceHook.result.current.metrics?.input_tokens).toBe(30));

    raceSource.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 1 }) } as MessageEvent<string>);
    await waitFor(() => expect(summaryRequests).toHaveLength(4));
    expect(summaryRequests[3].range).toEqual({ key: "yesterday" });
    expect(summaryRequests[3].filters).toEqual({ models: ["gpt-a"], projects: [] });
    summaryRequests[3].resolve(summary({ key: "yesterday" }, 2, 40));
    await waitFor(() => expect(raceHook.result.current.metrics?.input_tokens).toBe(40));
    expect(raceClient.getSessionSnapshot).not.toHaveBeenCalled();
    raceHook.unmount();
  });

  it("t_s07_003 loads options once, refreshes once at a terminal revision, and keeps stale selections", async () => {
    const source = fakeEvents();
    let optionCalls = 0;
    const client = clientWith({
      filterOptions: vi.fn(async () => {
        optionCalls += 1;
        if (optionCalls === 2) throw new Error("options unavailable");
        return optionCalls === 1
          ? { data_revision: 1, models: [{ model: "gpt-a", provider: "openai" as const }], projects: [{ kind: "projectless" as const }] }
          : { data_revision: 2, models: [], projects: [] };
      }),
      refresh: vi.fn(async () => ({ http_status: 202 as const, disposition: "started" as const, scan_id: "scan-options", status_revision: 2 })),
      getStatus: vi.fn(async (target) =>
        target
          ? status({
              data_revision: 2,
              status_revision: 3,
              target_scan: {
                scan_id: target,
                state: "completed",
                started_status_revision: 2,
                terminal_status_revision: 3,
                error_code: null,
              },
            })
          : status(),
      ),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: () => source }));
    await waitFor(() => expect(result.current.filter_options).not.toBeNull());
    expect(optionCalls).toBe(1);

    await act(async () => result.current.select_range({ key: "7d" }));
    await act(async () => result.current.select_filters({ models: ["gpt-a"], projects: [{ kind: "projectless" }] }));
    await act(async () => result.current.clear_filters());
    expect(optionCalls).toBe(1);

    await act(async () => result.current.select_filters({ models: ["gpt-a"], projects: [{ kind: "projectless" }] }));

    source.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 1 }) } as MessageEvent<string>);
    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refresh_state).toBe("idle"));
    expect(optionCalls).toBe(2);
    expect(result.current.filters).toEqual({ models: ["gpt-a"], projects: [{ kind: "projectless" }] });
    expect(result.current.filter_options).toEqual({
      data_revision: 1,
      models: [{ model: "gpt-a", provider: "openai" }],
      projects: [{ kind: "projectless" }],
    });
    expect(result.current.filter_options_stale).toBe(true);
    await act(async () => result.current.retry_filter_options());
    expect(optionCalls).toBe(3);
    await waitFor(() => expect(result.current.filter_options).toEqual({ data_revision: 2, models: [], projects: [] }));
    expect(result.current.filters).toEqual({ models: ["gpt-a"], projects: [{ kind: "projectless" }] });
  });

  it("loads today in parallel and isolates snapshots when ranges change", async () => {
    const client = clientWith();
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(10));
    expect(result.current.range).toEqual({ key: "today" });
    expect(client.summary).toHaveBeenCalledWith({ key: "today" }, { models: [], projects: [] }, expect.any(AbortSignal));
    expect(client.getStatus).toHaveBeenCalled();
    expect(client.getRevision).toHaveBeenCalled();

    await act(async () => result.current.select_range({ key: "yesterday" }));
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(10));
    expect(result.current.range).toEqual({ key: "yesterday" });
    expect(client.summary).toHaveBeenCalledWith({ key: "yesterday" }, { models: [], projects: [] }, expect.any(AbortSignal));
  });

  it("T-022-A6 sends complete custom ranges while retaining active filters", async () => {
    const client = clientWith();
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(10));
    await act(async () => result.current.select_filters({ models: ["gpt-5"], projects: [] }));
    await waitFor(() => expect(client.summary).toHaveBeenLastCalledWith({ key: "today" }, { models: ["gpt-5"], projects: [] }, expect.any(AbortSignal)));

    const custom = { key: "custom" as const, from: "2026-08-01", to: "2026-08-03" };
    await act(async () => result.current.select_range(custom));
    await waitFor(() => expect(result.current.range).toEqual(custom));
    expect(client.summary).toHaveBeenLastCalledWith(custom, { models: ["gpt-5"], projects: [] }, expect.any(AbortSignal));
  });

  it("does not let a late old range response overwrite the current range", async () => {
    let resolveToday!: (value: SummaryResponse) => void;
    let resolveYesterday!: (value: SummaryResponse) => void;
    const today = new Promise<SummaryResponse>((resolve) => {
      resolveToday = resolve;
    });
    const yesterday = new Promise<SummaryResponse>((resolve) => {
      resolveYesterday = resolve;
    });
    const client = clientWith({
      summary: vi.fn((range) => (range.key === "today" ? today : yesterday)),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await act(async () => result.current.select_range({ key: "yesterday" }));
    resolveYesterday(summary({ key: "yesterday" }, 2, 20));
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(20));
    resolveToday(summary({ key: "today" }, 1, 10));
    await act(async () => undefined);
    expect(result.current.range).toEqual({ key: "yesterday" });
    expect(result.current.metrics?.input_tokens).toBe(20);
  });

  it("accepts started target IDs and never posts a second refresh while tracking", async () => {
    const targetStatus = status({
      data_revision: 1,
      status_revision: 2,
      target_scan: {
        scan_id: "scan-1",
        state: "running",
        started_status_revision: 2,
        terminal_status_revision: null,
        error_code: null,
      },
    });
    const client = clientWith({ getStatus: vi.fn(async (target) => (target ? targetStatus : status())) });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.error_code).not.toBe("STATUS_NOT_READY"));
    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refresh_state).toBe("running"));
    await act(async () => result.current.request_refresh());
    expect(client.refresh).toHaveBeenCalledTimes(1);
    expect(client.getStatus).toHaveBeenCalledWith("scan-1", expect.any(AbortSignal));
  });

  it("tracks the durable follow-up ID for a coalesced refresh", async () => {
    const followupStatus = status({
      status_revision: 2,
      followup: {
        scan_id: "followup-1",
        state: "queued",
        enqueued_status_revision: 2,
        requested_at_ms: 1,
        error_code: null,
      },
    });
    const client = clientWith({
      refresh: vi.fn(async () => ({ http_status: 200 as const, disposition: "coalesced" as const, scan_id: "followup-1", status_revision: 2 })),
      getStatus: vi.fn(async (target) => (target ? { ...followupStatus, target_scan: { scan_id: target, state: "queued" as const, started_status_revision: null, terminal_status_revision: null, error_code: null } } : status())),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.error_code).not.toBe("STATUS_NOT_READY"));
    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refresh_state).toBe("running"));
    expect(client.getStatus).toHaveBeenCalledWith("followup-1", expect.any(AbortSignal));
  });

  it.each([
    ["started", { http_status: 202 as const, disposition: "started" as const, scan_id: "terminal-1", status_revision: 2 }],
    ["coalesced", { http_status: 200 as const, disposition: "coalesced" as const, scan_id: "terminal-1", status_revision: 2 }],
  ])("reduces a cached terminal status immediately after %s acknowledgement", async (_name, accepted) => {
    const terminalStatus = status({
      target_scan: {
        scan_id: accepted.scan_id,
        state: "completed",
        started_status_revision: 2,
        terminal_status_revision: 3,
        error_code: null,
      },
    });
    const client = clientWith({
      getStatus: vi.fn(async () => terminalStatus),
      refresh: vi.fn(async () => accepted),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.error_code).not.toBe("STATUS_NOT_READY"));
    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refresh_state).toBe("idle"));
    expect(client.refresh).toHaveBeenCalledTimes(1);
    expect(client.getStatus).toHaveBeenCalledTimes(1);
  });

  it("recovers a queued follow-up target after a page reload", async () => {
    const client = clientWith({
      getStatus: vi.fn(async (target) =>
        target
          ? status({
              status_revision: 3,
              followup: null,
              target_scan: {
                scan_id: target,
                state: "queued",
                started_status_revision: null,
                terminal_status_revision: null,
                error_code: null,
              },
            })
          : status({
              status_revision: 2,
              followup: {
                scan_id: "persisted-followup",
                state: "queued",
                enqueued_status_revision: 2,
                requested_at_ms: 1,
                error_code: null,
              },
            }),
      ),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.error_code).toBe("TARGET_QUEUED"));
    expect(client.getStatus).toHaveBeenCalledWith("persisted-followup", expect.any(AbortSignal));
    expect(result.current.refresh_state).toBe("running");
  });

  it("recovers an active target after a page reload", async () => {
    const client = clientWith({
      getStatus: vi.fn(async (target) =>
        target
          ? status({
              status_revision: 3,
              active_scan_id: target,
              target_scan: {
                scan_id: target,
                state: "running",
                started_status_revision: 3,
                terminal_status_revision: null,
                error_code: null,
              },
            })
          : status({ active_scan_id: "active-mount" }),
      ),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.refresh_state).toBe("running"));
    expect(client.getStatus).toHaveBeenCalledWith("active-mount", expect.any(AbortSignal));
    expect(client.refresh).not.toHaveBeenCalled();
  });

  it("recovers a start_failed follow-up and exposes a terminal failure", async () => {
    const client = clientWith({
      getStatus: vi.fn(async (target) =>
        target
          ? status({
              status_revision: 3,
              followup: null,
              target_scan: {
                scan_id: target,
                state: "start_failed",
                started_status_revision: null,
                terminal_status_revision: 3,
                error_code: "SCAN_START_FAILED",
              },
            })
          : status({
              followup: {
                scan_id: "start-failed-mount",
                state: "start_failed",
                enqueued_status_revision: 2,
                requested_at_ms: 1,
                error_code: "SCAN_START_FAILED",
              },
            }),
      ),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.refresh_state).toBe("failed"));
    expect(result.current.error_code).toBe("SCAN_START_FAILED");
    expect(client.refresh).not.toHaveBeenCalled();
  });

  it("keeps a busy queued target queued across repeated status polls", async () => {
    const client = clientWith({
      getStatus: vi.fn(async (target) =>
        target
          ? status({
              target_scan: {
                scan_id: target,
                state: "queued",
                started_status_revision: null,
                terminal_status_revision: null,
                error_code: null,
              },
            })
          : status({
              followup: {
                scan_id: "busy-queued",
                state: "queued",
                enqueued_status_revision: 2,
                requested_at_ms: 1,
                error_code: null,
              },
            }),
      ),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.error_code).toBe("TARGET_QUEUED"));
    await act(async () => result.current.retry_refresh_status());
    await act(async () => result.current.retry_refresh_status());
    await waitFor(() => expect(result.current.error_code).toBe("TARGET_QUEUED"));
    expect(result.current.refresh_state).toBe("running");
    expect(client.refresh).not.toHaveBeenCalled();
    expect(vi.mocked(client.getStatus).mock.calls.filter(([target]) => target === "busy-queued").length).toBeGreaterThanOrEqual(3);
  });

  it("moves a completed target into a newly queued follow-up without guessing completion", async () => {
    let targetCalls = 0;
    const client = clientWith({
      refresh: vi.fn(async () => ({ http_status: 202 as const, disposition: "started" as const, scan_id: "first", status_revision: 2 })),
      getStatus: vi.fn(async (target) => {
        if (!target) return status();
        targetCalls += 1;
        if (target === "first") {
          return status({
            target_scan: {
              scan_id: target,
              state: "completed",
              started_status_revision: 2,
              terminal_status_revision: 3,
              error_code: null,
            },
            followup: {
              scan_id: "followup-after-terminal",
              state: "queued",
              enqueued_status_revision: 3,
              requested_at_ms: 1,
              error_code: null,
            },
          });
        }
        return status({
          target_scan: {
            scan_id: target,
            state: "running",
            started_status_revision: 3,
            terminal_status_revision: null,
            error_code: null,
          },
        });
      }),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.error_code).not.toBe("STATUS_NOT_READY"));
    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refresh_state).toBe("running"));
    expect(client.getStatus).toHaveBeenCalledWith("followup-after-terminal", expect.any(AbortSignal));
    expect(targetCalls).toBe(2);
  });

  it("tracks repeated coalesced acknowledgements under one durable ID", async () => {
    const client = clientWith({
      getStatus: vi.fn(async (target) =>
        target
          ? status({
              target_scan: {
                scan_id: target,
                state: "queued",
                started_status_revision: null,
                terminal_status_revision: null,
                error_code: null,
              },
            })
          : status({
              followup: {
                scan_id: "same-coalesced",
                state: "queued",
                enqueued_status_revision: 2,
                requested_at_ms: 1,
                error_code: null,
              },
            }),
      ),
      refresh: vi.fn(async () => ({ http_status: 200 as const, disposition: "coalesced" as const, scan_id: "same-coalesced", status_revision: 2 })),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.error_code).toBe("TARGET_QUEUED"));
    await act(async () => result.current.retry_refresh_status());
    await act(async () => result.current.retry_refresh_status());
    expect(vi.mocked(client.getStatus).mock.calls.filter(([target]) => target === "same-coalesced").every(([target]) => target === "same-coalesced")).toBe(true);
    expect(result.current.refresh_state).toBe("running");
    expect(client.refresh).not.toHaveBeenCalled();
  });

  it("accepts multiple coalesced acknowledgements for the same terminal ID", async () => {
    const client = clientWith({
      getStatus: vi.fn(async (target) =>
        target
          ? status({
              target_scan: {
                scan_id: target,
                state: "completed",
                started_status_revision: 2,
                terminal_status_revision: 3,
                error_code: null,
              },
            })
          : status(),
      ),
      refresh: vi.fn(async () => ({ http_status: 200 as const, disposition: "coalesced" as const, scan_id: "coalesced-same", status_revision: 2 })),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.error_code).not.toBe("STATUS_NOT_READY"));
    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refresh_state).toBe("idle"));
    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refresh_state).toBe("idle"));
    expect(client.refresh).toHaveBeenCalledTimes(2);
  });

  it.each(["failed", "start_failed"] as const)("maps only non-retryable terminal state %s to refresh failure", async (targetState) => {
    const client = clientWith({
      getStatus: vi.fn(async (target) =>
        target
          ? status({
              target_scan: {
                scan_id: target,
                state: targetState,
                started_status_revision: null,
                terminal_status_revision: 3,
                error_code: "SCAN_FAILED",
              },
            })
          : status(),
      ),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.error_code).not.toBe("STATUS_NOT_READY"));
    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refresh_state).toBe("failed"));
    expect(result.current.error_code).toBe("SCAN_FAILED");
  });

  it("keeps a target on tracking error and retry only retries status", async () => {
    let targetAttempts = 0;
    const targetStatus = status({
      data_revision: 1,
      status_revision: 2,
      target_scan: {
        scan_id: "scan-1",
        state: "running",
        started_status_revision: 2,
        terminal_status_revision: null,
        error_code: null,
      },
    });
    const client = clientWith({
      getStatus: vi.fn(async (target) => {
        if (!target) return status();
        targetAttempts += 1;
        if (targetAttempts === 1) throw new Error("offline");
        return targetStatus;
      }),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.error_code).not.toBe("STATUS_NOT_READY"));
    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refresh_state).toBe("tracking_error"));
    expect(result.current.load_state).not.toBe("error");
    expect(result.current.metrics?.input_tokens).toBe(10);
    await act(async () => result.current.retry_refresh_status());
    await waitFor(() => expect(result.current.refresh_state).toBe("running"));
    expect(client.refresh).toHaveBeenCalledTimes(1);
    expect(client.getStatus).toHaveBeenCalledTimes(3); // mount, target tracking, retry
  });

  it("keeps stable KPI values while a target runs and refreshes only after completion", async () => {
    let summaryCalls = 0;
    let targetCalls = 0;
    const client = clientWith({
      summary: vi.fn(async (range) => {
        summaryCalls += 1;
        return summary(range, summaryCalls > 1 ? 2 : 1, summaryCalls > 1 ? 20 : 10);
      }),
      getStatus: vi.fn(async (target) => {
        if (!target) return status();
        targetCalls += 1;
        return status({
          data_revision: targetCalls > 1 ? 2 : 1,
          status_revision: targetCalls + 1,
          target_scan: {
            scan_id: target,
            state: targetCalls > 1 ? "completed" : "running",
            started_status_revision: 2,
            terminal_status_revision: targetCalls > 1 ? 3 : null,
            error_code: null,
          },
        });
      }),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(10));
    await act(async () => result.current.request_refresh());
    await waitFor(() => expect(result.current.refresh_state).toBe("running"));
    expect(result.current.metrics?.input_tokens).toBe(10);
    await act(async () => result.current.retry_refresh_status());
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(20));
    expect(result.current.refresh_state).toBe("idle");
  });

  it("finishes loading when a stale summary response is discarded", async () => {
    let summaryCalls = 0;
    let resolveStale!: (value: SummaryResponse) => void;
    const stale = new Promise<SummaryResponse>((resolve) => {
      resolveStale = resolve;
    });
    const source = fakeEvents();
    const client = clientWith({
      summary: vi.fn((range) => {
        summaryCalls += 1;
        return summaryCalls === 1 ? Promise.resolve(summary(range, 2, 20)) : stale;
      }),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: () => source }));
    await waitFor(() => expect(result.current.metrics?.input_tokens).toBe(20));
    source.onmessage?.({ data: JSON.stringify({ data_revision: 3, status_revision: 1 }) } as MessageEvent<string>);
    await waitFor(() => expect(result.current.load_state).toBe("loading"));
    resolveStale(summary({ key: "today" }, 1, 1));
    await waitFor(() => expect(result.current.load_state).toBe("ready"));
    expect(result.current.metrics?.input_tokens).toBe(20);
  });

  it("starts one fallback poll after SSE error and stops it only after a valid event", async () => {
    vi.useFakeTimers();
    const source = fakeEvents();
    const client = clientWith();
    renderHook(() => useDashboardController({ client, eventSourceFactory: () => source, pollIntervalMs: 100 }));
    await act(async () => undefined);
    source.onerror?.(new Event("error"));
    expect(vi.getTimerCount()).toBe(1);
    await act(async () => vi.advanceTimersByTime(100));
    expect(client.getRevision).toHaveBeenCalledTimes(2); // initial + fallback
    source.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 1 }) } as MessageEvent<string>);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("surfaces shared revision fetch failure and retries it through retry_load", async () => {
    const source = fakeEvents();
    let attempts = 0;
    const client = clientWith({
      getRevision: vi.fn(async () => {
        attempts += 1;
        if (attempts === 1) throw new Error("offline");
        return { data_revision: 2, status_revision: 2 };
      }),
    });
    const feed = createRevisionFeed({ client, eventSourceFactory: () => source, pollIntervalMs: 60_000 });
    const { result } = renderHook(() => useDashboardController({ client, revisionFeed: feed }));
    await waitFor(() => expect(result.current.load_state).toBe("error"));
    await act(async () => result.current.retry_load());
    await waitFor(() => expect(client.getRevision).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(result.current.load_state).not.toBe("error"));
    feed.dispose();
  });

  it("rejects a cross-old SSE tuple instead of synthesizing a revision", async () => {
    vi.useFakeTimers();
    const source = fakeEvents();
    const client = clientWith();
    renderHook(() => useDashboardController({ client, eventSourceFactory: () => source, pollIntervalMs: 100 }));
    await act(async () => undefined);
    const summaryCalls = vi.mocked(client.summary).mock.calls.length;
    const statusCalls = vi.mocked(client.getStatus).mock.calls.length;
    source.onerror?.(new Event("error"));
    expect(vi.getTimerCount()).toBe(1);

    source.onmessage?.({ data: JSON.stringify({ data_revision: 1, status_revision: 1 }) } as MessageEvent<string>);
    expect(vi.getTimerCount()).toBe(0);
    expect(client.summary).toHaveBeenCalledTimes(summaryCalls);
    expect(client.getStatus).toHaveBeenCalledTimes(statusCalls);

    source.onmessage?.({ data: JSON.stringify({ data_revision: 1, status_revision: 2 }) } as MessageEvent<string>);
    await act(async () => undefined);
    expect(client.getStatus).toHaveBeenCalledTimes(statusCalls + 1);
    source.onerror?.(new Event("error"));
    source.onmessage?.({ data: JSON.stringify({ data_revision: 2, status_revision: 1 }) } as MessageEvent<string>);
    await act(async () => undefined);
    expect(client.summary).toHaveBeenCalledTimes(summaryCalls);
    expect(vi.getTimerCount()).toBe(1);
  });

  it.each([
    ["idle", status({ scan_state: "idle" }), true],
    ["failed", status({ scan_state: "failed" }), true],
    ["startup", status({ scan_state: "startup" }), false],
    ["running", status({ scan_state: "running" }), false],
    ["source_changed scan", status({ scan_state: "source_changed" }), false],
    ["source_changed binding", status({ source_binding_status: "source_changed" }), false],
    ["unbound", status({ source_binding_status: "unbound" }), false],
    [
      "active target",
      status({ active_scan_id: "active-1" }),
      false,
    ],
    [
      "queued followup",
      status({
        followup: {
          scan_id: "queued-1",
          state: "queued",
          enqueued_status_revision: 2,
          requested_at_ms: 1,
          error_code: null,
        },
      }),
      false,
    ],
  ])("posts refresh only for an explicitly refreshable status: %s", async (_name, initial, canPost) => {
    const client = clientWith({
      getStatus: vi.fn(async (target) => {
        if (target === "active-1") {
          return status({
            target_scan: {
              scan_id: target,
              state: "running",
              started_status_revision: 2,
              terminal_status_revision: null,
              error_code: null,
            },
          });
        }
        if (target === "queued-1") {
          return status({
            target_scan: {
              scan_id: target,
              state: "queued",
              started_status_revision: null,
              terminal_status_revision: null,
              error_code: null,
            },
          });
        }
        return initial;
      }),
    });
    const { result } = renderHook(() => useDashboardController({ client, eventSourceFactory: fakeEvents }));
    await waitFor(() => expect(client.getStatus).toHaveBeenCalled());
    await waitFor(() => expect(result.current.refresh_state).not.toBe("requesting"));
    await act(async () => result.current.request_refresh());
    if (canPost) {
      expect(client.refresh).toHaveBeenCalledTimes(1);
    } else {
      expect(client.refresh).not.toHaveBeenCalled();
    }
  });
});
