import { afterEach, describe, expect, it, vi } from "vitest";

import { appendRangeParams, dashboardQueryKey, usagiClient } from "./usagiClient";
import { UsagiClientError, type DashboardFilters } from "./types";

const emptyFilters: DashboardFilters = { models: [], projects: [] };
const range = { key: "today", start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" };
const usage = {
  input_tokens: 10,
  cached_tokens: 4,
  cache_write_tokens: null,
  uncached_input_tokens: null,
  output_tokens: 20,
  reasoning_tokens: 0,
  other_output_tokens: 20,
  total_tokens: 30,
  cache_hit_rate: null,
  estimated_cost: null,
  estimated_cost_status: "unknown",
  session_count: 1,
  cost_incomplete_session_count: 1,
  complete_session_cost_per_million_tokens: null,
  session_health: {
    total_sessions: 1,
    complete_sessions: 0,
    incomplete_sessions: 1,
    error_sessions: 0,
  },
};

const sessionUsage = {
  input_tokens: 10,
  cached_tokens: 4,
  cache_write_tokens: null,
  uncached_input_tokens: 6,
  output_tokens: 20,
  reasoning_tokens: 2,
  other_output_tokens: 18,
  total_tokens: 30,
  cache_hit_rate: 0.4,
  estimated_cost: null,
  estimated_cost_status: "unknown",
};

const sessionItem = (root_session_id = "root-1") => ({
  root_session_id,
  title: "A session",
  project_name: "Usagi",
  project_path: "/work/Usagi",
  last_activity_at_ms: 1_700_000_000_000,
  models_used: ["gpt-5"],
  subagent_count: 1,
  inclusive_usage: sessionUsage,
  self_usage: sessionUsage,
  subagent_usage: sessionUsage,
  data_status: "incomplete",
  error_code: null,
});

afterEach(() => vi.restoreAllMocks());

describe("usagiClient DTO seam", () => {
  it("T-Q-SW-002 parses weekly-only and session-plus-weekly quota contracts", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(
        JSON.stringify({
          status: "ready",
          account_email: "hoge@example.com",
          plan_type: "prolite",
          session: null,
          weekly: {
            used_percent: 55,
            remaining_percent: 45,
            limit_window_seconds: 604800,
            reset_at_ms: 1_786_508_580_000,
          },
          reset_credits_available: 2,
          fetched_at_ms: 1_786_076_580_000,
        }),
        { status: 200 },
      ),
    );

    await expect(usagiClient.codexQuota()).resolves.toEqual({
      status: "ready",
      account_email: "hoge@example.com",
      plan_type: "prolite",
      session: null,
      weekly: {
        used_percent: 55,
        remaining_percent: 45,
        limit_window_seconds: 604800,
        reset_at_ms: 1_786_508_580_000,
      },
      reset_credits_available: 2,
      fetched_at_ms: 1_786_076_580_000,
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/codex/quota",
      expect.objectContaining({ method: "GET", credentials: "same-origin" }),
    );

    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          status: "ready",
          account_email: "hoge@example.com",
          plan_type: "prolite",
          session: {
            used_percent: 12,
            remaining_percent: 88,
            limit_window_seconds: 18000,
            reset_at_ms: 1_786_100_000_000,
          },
          weekly: {
            used_percent: 55,
            remaining_percent: 45,
            limit_window_seconds: 604800,
            reset_at_ms: 1_786_508_580_000,
          },
          reset_credits_available: 2,
          fetched_at_ms: 1_786_076_580_000,
        }),
        { status: 200 },
      ),
    );
    await expect(usagiClient.codexQuota()).resolves.toMatchObject({
      session: {
        used_percent: 12,
        remaining_percent: 88,
        limit_window_seconds: 18000,
        reset_at_ms: 1_786_100_000_000,
      },
      weekly: {
        used_percent: 55,
        remaining_percent: 45,
        limit_window_seconds: 604800,
        reset_at_ms: 1_786_508_580_000,
      },
    });

    for (const [session, weekly] of [
      [null, { used_percent: 101, remaining_percent: 0, limit_window_seconds: 604800, reset_at_ms: null }],
      [null, { used_percent: 55, remaining_percent: 45, limit_window_seconds: 18000, reset_at_ms: null }],
      [{ used_percent: 12, remaining_percent: 88, limit_window_seconds: 604800, reset_at_ms: null }, { used_percent: 55, remaining_percent: 45, limit_window_seconds: 604800, reset_at_ms: null }],
      [{ used_percent: 12, remaining_percent: 88, limit_window_seconds: 18000, reset_at_ms: null }, { used_percent: 55, remaining_percent: 45, limit_window_seconds: 18000, reset_at_ms: null }],
    ]) {
      fetchMock.mockResolvedValueOnce(
        new Response(JSON.stringify({
          status: "ready",
          account_email: null,
          plan_type: null,
          session,
          weekly,
          reset_credits_available: null,
          fetched_at_ms: null,
        }), { status: 200 }),
      );
      await expect(usagiClient.codexQuota()).rejects.toBeInstanceOf(UsagiClientError);
    }
  });

  it("t_s07_001 parses typed options and canonicalizes every summary filter shape", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");
    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          data_revision: 7,
          models: [
            { model: "gpt-5.6-sol", provider: "openai" },
            { model: "gpt-5.6", provider: "route-models" },
          ],
          projects: [
            { kind: "project", project_name: "Usagi", project_path: "/work/Usagi" },
            { kind: "projectless" },
            { kind: "unknown" },
          ],
        }),
        { status: 200 },
      ),
    );
    await expect(usagiClient.filterOptions()).resolves.toEqual({
      data_revision: 7,
      models: [
        { model: "gpt-5.6-sol", provider: "openai" },
        { model: "gpt-5.6", provider: "route-models" },
      ],
      projects: [
        { kind: "project", project_name: "Usagi", project_path: "/work/Usagi" },
        { kind: "projectless" },
        { kind: "unknown" },
      ],
    });

    for (const invalid of [
      { data_revision: 1, models: [], projects: [{ kind: "projectless", project_path: "/fake" }] },
      { data_revision: 1, models: [], projects: [{ kind: "project", project_name: "Usagi" }] },
      { data_revision: 1, models: [""], projects: [] },
      { data_revision: 1, models: [{ model: "gpt-a", provider: "unknown" }], projects: [] },
      { data_revision: 1, models: [{ model: "gpt-a" }], projects: [] },
      { data_revision: 1, models: [{ model: "gpt-a", provider: "openai", extra: true }], projects: [] },
    ]) {
      fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(invalid), { status: 200 }));
      await expect(usagiClient.filterOptions()).rejects.toBeInstanceOf(UsagiClientError);
    }

    const filters: DashboardFilters = {
      models: ["gpt-b", "gpt-a", "gpt-b"],
      projects: [
        { kind: "unknown" as const },
        { kind: "projectless" as const },
        { kind: "project", project_path: "/a & b" },
        { kind: "project", project_path: "/a & b" },
      ],
    };
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ range, data_revision: 1, usage }), { status: 200 }));
    await usagiClient.summary({ key: "today" }, filters);
    expect(fetchMock).toHaveBeenLastCalledWith(
      "/api/usage/summary?range=today&model=gpt-a&model=gpt-b&project_path=%2Fa+%26+b&include_projectless=1&include_unknown_project=1",
      expect.objectContaining({ method: "GET" }),
    );

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ range, data_revision: 1, usage }), { status: 200 }));
    await usagiClient.summary({ key: "today" }, { models: [], projects: [] });
    expect(fetchMock).toHaveBeenLastCalledWith(
      "/api/usage/summary?range=today",
      expect.objectContaining({ method: "GET" }),
    );
  });

  it("T-022-A2 serializes custom ranges into URLs and query identities", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ range: { ...range, key: "custom" }, data_revision: 1, usage }), { status: 200 }),
    );
    const custom = { key: "custom" as const, from: "2026-08-01", to: "2026-08-03" };
    await usagiClient.summary(custom, emptyFilters);
    expect(fetchMock).toHaveBeenLastCalledWith(
      "/api/usage/summary?range=custom&from=2026-08-01&to=2026-08-03",
      expect.objectContaining({ method: "GET" }),
    );
    expect(dashboardQueryKey(custom, emptyFilters)).not.toBe(
      dashboardQueryKey({ key: "custom", from: "2026-08-10", to: "2026-08-12" }, emptyFilters),
    );
    const params = appendRangeParams(new URLSearchParams({ from: "stale", to: "stale" }), { key: "7d" });
    expect(params.toString()).toBe("range=7d");
  });

  it("validates summary/status/revision and keeps exact nullable fields through the public client", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");
    fetchMock
      .mockResolvedValueOnce(new Response(JSON.stringify({ range, data_revision: 3, usage }), { status: 200 }))
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            data_revision: 3,
            status_revision: 4,
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
          }),
          { status: 200 },
        ),
      )
      .mockResolvedValueOnce(new Response(JSON.stringify({ data_revision: 3, status_revision: 4 }), { status: 200 }));
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).resolves.toEqual({
      range,
      data_revision: 3,
      usage,
    });
    const status = await usagiClient.getStatus();
    expect(status.source_binding_status).toBe("ready");
    await expect(usagiClient.getRevision()).resolves.toEqual({ data_revision: 3, status_revision: 4 });
  });

  it("T-S03-004 parser accepts cost-incomplete roots beyond healthy sessions only within health total", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");
    const acceptedUsage = {
      ...usage,
      session_count: 1,
      cost_incomplete_session_count: 2,
      session_health: {
        total_sessions: 2,
        complete_sessions: 0,
        incomplete_sessions: 1,
        error_sessions: 1,
      },
    };
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ range, data_revision: 0, usage: acceptedUsage }), { status: 200 }),
    );
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).resolves.toMatchObject({ usage: acceptedUsage });

    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          range,
          data_revision: 0,
          usage: { ...acceptedUsage, cost_incomplete_session_count: 3 },
        }),
        { status: 200 },
      ),
    );
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).rejects.toBeInstanceOf(UsagiClientError);
  });

  it("rejects unsafe integers, invalid ratios, and legacy-field-only responses", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");
    fetchMock
      .mockResolvedValueOnce(new Response(JSON.stringify({ range, data_revision: Number.MAX_SAFE_INTEGER + 1, usage }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ range, data_revision: 0, usage: { ...usage, cache_hit_rate: 1.1 } }), { status: 200 }))
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            range,
            data_revision: 0,
            usage: {
              input_tokens: 10,
              output_tokens: 20,
              total_tokens: 30,
              reasoning_output_tokens: 0,
              cached_input_tokens: 4,
              cache_write_input_tokens: null,
              cache_write_status: "unknown_missing",
              cache_tokens: null,
              cache_hit_rate: null,
              estimated_cost: null,
              session_count: 1,
            },
          }),
          { status: 200 },
        ),
      );
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).rejects.toBeInstanceOf(UsagiClientError);
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).rejects.toBeInstanceOf(UsagiClientError);
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).rejects.toBeInstanceOf(UsagiClientError);
  });

  it("preserves cache-write null and zero as distinct canonical values", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");
    fetchMock
      .mockResolvedValueOnce(new Response(JSON.stringify({ range, data_revision: 0, usage }), { status: 200 }))
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            range,
            data_revision: 0,
            usage: { ...usage, cache_write_tokens: 0, uncached_input_tokens: 6 },
          }),
          { status: 200 },
        ),
      );
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).resolves.toMatchObject({ usage });
    await expect(usagiClient.summary({ key: "today" }, emptyFilters)).resolves.toMatchObject({
      usage: { cache_write_tokens: 0, uncached_input_tokens: 6 },
    });
  });

  it("uses relative API URLs, validates refresh acknowledgement, and maps errors without body text", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ http_status: 202, disposition: "started", scan_id: "scan", status_revision: 2 }), {
        status: 202,
        headers: { "content-type": "application/json" },
      }),
    );
    await expect(usagiClient.refresh()).resolves.toMatchObject({ disposition: "started", http_status: 202 });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/refresh",
      expect.objectContaining({ method: "POST", headers: expect.objectContaining({ "X-Usagi-Request": "1" }) }),
    );

    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ error: { code: "SOURCE_CHANGED", message: "secret path" } }), { status: 409 }),
    );
    const error = await usagiClient.getRevision().catch((value: unknown) => value);
    expect(error).toBeInstanceOf(UsagiClientError);
    expect((error as UsagiClientError).code).toBe("SOURCE_CHANGED");
    expect(String(error)).not.toContain("secret path");
  });

  it("T-S04-001 parses snapshot/index, bounded repeated-ID rows, detail fields, and stale revision errors", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");
    const sortIndex = {
      root_session_id: "root-1",
      last_activity_at_ms: 1_700_000_000_000,
      project_sort_key: "/work/Usagi",
      model_sort_key: "gpt-5",
      total_tokens: 30,
      combined_total_tokens: 30,
      combined_estimated_cost: null,
      cache_hit_rate: 0.4,
      data_status: "incomplete",
      error_code: null,
    };
    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({ range, data_revision: 4, total_items: 1, sort_index: [sortIndex], items: [sessionItem()] }),
        { status: 200 },
      ),
    );
    await expect(usagiClient.getSessionSnapshot({ range: { key: "today" }, filters: emptyFilters })).resolves.toEqual({
      range,
      data_revision: 4,
      total_items: 1,
      sort_index: [sortIndex],
      items: [sessionItem()],
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/usage/sessions?range=today",
      expect.objectContaining({ method: "GET" }),
    );

    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ range, data_revision: 4, items: [sessionItem()] }), { status: 200 }),
    );
    await expect(
      usagiClient.getSessionRows({
        range: { key: "today" },
        filters: { models: ["gpt-b", "gpt-a", "gpt-b"], projects: [{ kind: "projectless" }] },
        root_session_ids: ["root-1", "root-1"],
        expected_data_revision: 4,
      }),
    ).resolves.toEqual({ range, data_revision: 4, items: [sessionItem()] });
    expect(fetchMock).toHaveBeenLastCalledWith(
      "/api/usage/session-rows?range=today&model=gpt-a&model=gpt-b&include_projectless=1&expected_data_revision=4&root_session_id=root-1",
      expect.objectContaining({ method: "GET" }),
    );

    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({
        range,
        data_revision: 4,
        root_session_id: "root-1",
        last_activity_at_ms: 1_700_000_000_000,
        main: {
          title: "A session",
          thread_id: "root-1",
          root_session_id: "root-1",
          models_used: ["gpt-5"],
          model_usage: [{ model: "gpt-5", reasoning_effort: "high", usage: sessionUsage }],
          self_usage: sessionUsage,
          subagent_count: 1,
          inclusive_usage: sessionUsage,
        },
        subagents: [{
          thread_id: "child-1",
          parent_thread_id: null,
          root_session_id: "root-1",
          title: null,
          last_activity_at_ms: 1_700_000_000_000,
          model_usage: [{
            model: "o4-mini",
            reasoning_effort: null,
            last_activity_at_ms: 1_700_000_000_000,
            usage: {
              ...sessionUsage,
              cache_write_tokens: 0,
              estimated_cost: 1.25,
              estimated_cost_status: "complete",
              reasoning_tokens: 9,
            },
          }],
        }],
      }), { status: 200 }),
    );
    await expect(usagiClient.getSessionDetail({ range: { key: "today" }, filters: emptyFilters, root_session_id: "root-1", expected_data_revision: 4 })).resolves.toMatchObject({
      last_activity_at_ms: 1_700_000_000_000,
      main: { model_usage: [{ model: "gpt-5", reasoning_effort: "high" }], self_usage: sessionUsage, inclusive_usage: sessionUsage },
      subagents: [{
        parent_thread_id: null,
        model_usage: [{
          model: "o4-mini",
          reasoning_effort: null,
          last_activity_at_ms: 1_700_000_000_000,
          usage: { reasoning_tokens: 9, cache_write_tokens: 0, estimated_cost: 1.25 },
        }],
      }],
    });
    expect(fetchMock).toHaveBeenLastCalledWith(
      "/api/usage/sessions/root-1/detail?range=today&expected_data_revision=4",
      expect.objectContaining({ method: "GET" }),
    );

    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ error: { code: "STALE_DATA_REVISION" } }), { status: 409 }),
    );
    await expect(usagiClient.getSessionRows({ range: { key: "today" }, filters: emptyFilters, root_session_ids: ["root-1"], expected_data_revision: 4 })).rejects.toMatchObject({ code: "STALE_DATA_REVISION" });
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ range, data_revision: 4, items: Array.from({ length: 61 }, () => sessionItem()) }), { status: 200 }));
    await expect(usagiClient.getSessionRows({ range: { key: "today" }, filters: emptyFilters, root_session_ids: ["root-1"] })).rejects.toBeInstanceOf(UsagiClientError);
    await expect(usagiClient.getSessionRows({ range: { key: "today" }, filters: emptyFilters, root_session_ids: Array.from({ length: 61 }, (_, index) => `root-${index}`) })).rejects.toMatchObject({ code: "INVALID_SESSION_IDS" });
  });

  it("T-MU04-C03 validates cost status combinations across summary, session, and detail DTOs", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");
    const validCosts = [
      { estimated_cost: 1.25, estimated_cost_status: "complete" },
      { estimated_cost: 1.25, estimated_cost_status: "partial" },
      { estimated_cost: null, estimated_cost_status: "unknown" },
    ] as const;
    for (const cost of validCosts) {
      fetchMock.mockResolvedValueOnce(
        new Response(JSON.stringify({ range, data_revision: 0, usage: { ...usage, ...cost } }), { status: 200 }),
      );
      await expect(usagiClient.summary({ key: "today" }, emptyFilters)).resolves.toMatchObject({ usage: cost });
    }

    for (const invalidUsage of [
      { ...usage, complete_session_cost_per_million_tokens: undefined },
      { ...usage, complete_session_cost_per_million_tokens: -1 },
      { ...usage, estimated_cost_status: undefined },
      { ...usage, estimated_cost_status: "invalid" },
      { ...usage, estimated_cost: null, estimated_cost_status: "complete" },
      { ...usage, estimated_cost: null, estimated_cost_status: "partial" },
      { ...usage, estimated_cost: 1.25, estimated_cost_status: "unknown" },
    ]) {
      fetchMock.mockResolvedValueOnce(
        new Response(JSON.stringify({ range, data_revision: 0, usage: invalidUsage }), { status: 200 }),
      );
      await expect(usagiClient.summary({ key: "today" }, emptyFilters)).rejects.toBeInstanceOf(UsagiClientError);
    }

    const sortIndex = {
      root_session_id: "root-1",
      last_activity_at_ms: 1_700_000_000_000,
      project_sort_key: "/work/Usagi",
      model_sort_key: "gpt-5",
      total_tokens: 30,
      combined_total_tokens: 30,
      combined_estimated_cost: null,
      cache_hit_rate: 0.4,
      data_status: "incomplete",
      error_code: null,
    };
    const partialSessionUsage = { ...sessionUsage, estimated_cost: 1.25, estimated_cost_status: "partial" };
    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          range,
          data_revision: 0,
          total_items: 1,
          sort_index: [sortIndex],
          items: [{
            ...sessionItem("root-1"),
            inclusive_usage: partialSessionUsage,
            self_usage: partialSessionUsage,
            subagent_usage: partialSessionUsage,
          }],
        }),
        { status: 200 },
      ),
    );
    await expect(usagiClient.getSessionSnapshot({ range: { key: "today" }, filters: emptyFilters })).resolves.toMatchObject({
      items: [{ inclusive_usage: partialSessionUsage, self_usage: partialSessionUsage, subagent_usage: partialSessionUsage }],
    });

    const detailUsage = { ...sessionUsage, estimated_cost: 1.25, estimated_cost_status: "partial" };
    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          range,
          data_revision: 0,
          root_session_id: "root-1",
          last_activity_at_ms: 1_700_000_000_000,
          main: {
            title: "A session",
            thread_id: "root-1",
            root_session_id: "root-1",
            models_used: ["gpt-5"],
            model_usage: [{ model: "gpt-5", reasoning_effort: "high", usage: detailUsage }],
            self_usage: detailUsage,
            subagent_count: 1,
            inclusive_usage: detailUsage,
          },
          subagents: [{
            thread_id: "child-1",
            parent_thread_id: null,
            root_session_id: "root-1",
            title: null,
            last_activity_at_ms: 1_700_000_000_000,
            model_usage: [{
              model: "o4-mini",
              reasoning_effort: null,
              last_activity_at_ms: 1_700_000_000_000,
              usage: detailUsage,
            }],
          }],
        }),
        { status: 200 },
      ),
    );
    await expect(
      usagiClient.getSessionDetail({ range: { key: "today" }, filters: emptyFilters, root_session_id: "root-1" }),
    ).resolves.toMatchObject({
      main: {
        model_usage: [{ usage: detailUsage }],
        self_usage: detailUsage,
        inclusive_usage: detailUsage,
      },
      subagents: [{ model_usage: [{ usage: detailUsage }] }],
    });
  });

});
