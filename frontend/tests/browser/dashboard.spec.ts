import { expect, test, type Page, type Route } from "@playwright/test";

const REVISION = 100;

type RangeKey = "today" | "yesterday" | "7d" | "30d" | "year";

function rangeFrom(url: string, fallback: RangeKey = "today") {
  const value = new URL(url).searchParams.get("range");
  return (value === "today" || value === "yesterday" || value === "7d" || value === "30d" || value === "year") ? value : fallback;
}

function rangeDto(key: RangeKey) {
  return { key, start_ms: 1, end_ms: 2, timezone: "Asia/Shanghai" };
}

function usage(kind: "base" | "model" | "filtered" | "main" | "sub" = "base", cost?: number | null) {
  const values = {
    base: { input: 1200, cached: 400, output: 600, reasoning: 180, total: 2200 },
    model: { input: 800, cached: 250, output: 350, reasoning: 100, total: 1150 },
    filtered: { input: 500, cached: 150, output: 250, reasoning: 70, total: 750 },
    main: { input: 230, cached: 70, output: 120, reasoning: 30, total: 420 },
    sub: { input: 95, cached: 25, output: 60, reasoning: 15, total: 180 },
  }[kind];
  const estimatedCost = cost === undefined ? (kind === "base" ? 1.2 : kind === "model" ? 0.8 : kind === "filtered" ? 0.55 : 0.2) : cost;
  return {
    input_tokens: values.input,
    cached_tokens: values.cached,
    cache_write_tokens: 0,
    uncached_input_tokens: values.input - values.cached,
    output_tokens: values.output,
    reasoning_tokens: values.reasoning,
    other_output_tokens: values.output - values.reasoning,
    total_tokens: values.total,
    cache_hit_rate: values.input > 0 ? values.cached / values.input : null,
    estimated_cost: estimatedCost,
    estimated_cost_status: estimatedCost === null ? "unknown" : "complete",
  };
}

function summary(range: RangeKey, mode: "base" | "model" | "filtered") {
  return {
    range: rangeDto(range),
    data_revision: REVISION,
    usage: {
      ...usage(mode),
      session_count: 2,
      cost_incomplete_session_count: 0,
      session_health: {
        total_sessions: 2,
        complete_sessions: 2,
        incomplete_sessions: 0,
        error_sessions: 0,
      },
    },
  };
}

function session(id: string, title: string, lastActivity: number, combinedCost: number, totalTokens: number) {
  const inclusive = { ...usage("main", combinedCost), total_tokens: totalTokens };
  const self = { ...usage("main", combinedCost / 2), total_tokens: Math.max(1, totalTokens - 180) };
  return {
    root_session_id: id,
    title,
    project_name: "Usagi",
    project_path: "/work/Usagi",
    last_activity_at_ms: lastActivity,
    models_used: ["gpt-5", "o4-mini"],
    subagent_count: 2,
    inclusive_usage: inclusive,
    self_usage: self,
    subagent_usage: usage("sub", combinedCost / 2),
    data_status: "complete",
    error_code: null,
  };
}

const SESSIONS = [
  session("session-1", "First session", 200, 0.2, 600),
  session("session-2", "Second session", 100, 0.8, 800),
];

function snapshot(range: RangeKey) {
  return {
    range: rangeDto(range),
    data_revision: REVISION,
    total_items: SESSIONS.length,
    sort_index: SESSIONS.map((item) => ({
      root_session_id: item.root_session_id,
      last_activity_at_ms: item.last_activity_at_ms,
      project_sort_key: item.project_path,
      model_sort_key: item.models_used[0],
      total_tokens: item.self_usage.total_tokens,
      combined_total_tokens: item.inclusive_usage.total_tokens,
      combined_estimated_cost: item.inclusive_usage.estimated_cost,
      cache_hit_rate: item.inclusive_usage.cache_hit_rate,
      data_status: "complete",
      error_code: null,
    })),
    items: SESSIONS,
  };
}

function detail(range: RangeKey, rootId: string) {
  const mainUsage = usage("main", 0.3);
  const subUsage = usage("sub", 0.1);
  return {
    range: rangeDto(range),
    data_revision: REVISION,
    root_session_id: rootId,
    last_activity_at_ms: 200,
    main: {
      title: `Detail ${rootId}`,
      thread_id: rootId,
      root_session_id: rootId,
      models_used: ["gpt-5", "o4-mini"],
      model_usage: [
        { model: "gpt-5", reasoning_effort: "high", usage: { ...mainUsage, total_tokens: 250 } },
        { model: "o4-mini", reasoning_effort: null, usage: { ...mainUsage, total_tokens: 170 } },
      ],
      self_usage: mainUsage,
      subagent_count: 2,
      inclusive_usage: { ...mainUsage, total_tokens: 780, estimated_cost: 0.5 },
    },
    subagents: [
      {
        thread_id: `${rootId}-sub-recent`,
        parent_thread_id: rootId,
        root_session_id: rootId,
        title: "Recent subagent",
        last_activity_at_ms: 180,
        model_usage: [{
          model: "gpt-5",
          reasoning_effort: "high",
          last_activity_at_ms: 180,
          usage: subUsage,
        }],
      },
      {
        thread_id: `${rootId}-sub-old`,
        parent_thread_id: rootId,
        root_session_id: rootId,
        title: "Old subagent",
        last_activity_at_ms: 160,
        model_usage: [{
          model: "o4-mini",
          reasoning_effort: null,
          last_activity_at_ms: 160,
          usage: subUsage,
        }],
      },
    ],
  };
}

function modelDistribution(range: RangeKey) {
  return {
    range: rangeDto(range),
    data_revision: REVISION,
    items: [
      ["gpt-5", 900, 0.6, "complete"],
      ["o4-mini", 600, 0.3, "complete"],
      ["gpt-5-mini", 300, 0.15, "complete"],
      ["codex-auto-review", 200, null, "unknown"],
      ["gpt-4.1", 150, 0.08, "partial"],
      ["other-model", 50, 0.02, "complete"],
    ].map(([model, total_tokens, estimated_cost, estimated_cost_status]) => ({
      model,
      usage: { total_tokens, estimated_cost, estimated_cost_status },
    })),
  };
}

function projectDistribution(range: RangeKey) {
  return {
    range: rangeDto(range),
    data_revision: REVISION,
    items: [
      { kind: "project", project_name: "Usagi", project_path: "/work/Usagi", usage: { total_tokens: 1200, estimated_cost: 0.7, estimated_cost_status: "complete" } },
      { kind: "project", project_name: "Docs", project_path: "/work/Docs", usage: { total_tokens: 500, estimated_cost: 0.25, estimated_cost_status: "complete" } },
      { kind: "projectless", project_name: null, project_path: null, usage: { total_tokens: 200, estimated_cost: 0.1, estimated_cost_status: "partial" } },
      { kind: "unknown", project_name: null, project_path: null, usage: { total_tokens: 100, estimated_cost: null, estimated_cost_status: "unknown" } },
    ],
  };
}

function skills() {
  const dates = ["2026-08-13", "2026-08-14", "2026-08-15", "2026-08-16", "2026-08-17", "2026-08-18", "2026-08-19"];
  return {
    range: rangeDto("7d"),
    data_revision: REVISION,
    data_status: "ready",
    days: dates.map((date, index) => ({
      date,
      start_ms: index * 1000 + 1,
      end_ms: index * 1000 + 2,
      total: 3 + index,
      skills: [
        { skill_name: "github", count: 2 + index },
        { skill_name: "testing", count: 1 },
      ],
    })),
  };
}

async function json(route: Route, body: unknown) {
  await route.fulfill({ status: 200, contentType: "application/json", json: body });
}

async function routeStableDashboardData(page: Page) {
  await page.route("**/api/events*", (route) => route.abort());
  await page.route("**/api/codex/quota", (route) => json(route, {
    status: "ready",
    account_email: "hoge@example.com",
    plan_type: "prolite",
    session: null,
    weekly: {
      used_percent: 55,
      remaining_percent: 45,
      limit_window_seconds: 604800,
      reset_at_ms: Date.UTC(2026, 7, 12, 4, 23),
    },
    reset_credits_available: 2,
    fetched_at_ms: Date.UTC(2026, 7, 1),
  }));
  await page.route("**/api/revision*", (route) => json(route, { data_revision: REVISION, status_revision: 1 }));
  await page.route("**/api/status*", (route) => json(route, {
    data_revision: REVISION,
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
  }));
  await page.route("**/api/update/status*", (route) => json(route, {
    current_version: "0.2.0",
    latest_version: "0.2.0",
    update_available: false,
    release_url: null,
    last_checked_at_ms: null,
    checking: false,
  }));
  await page.route("**/api/service", (route) => json(route, { state: "running" }));
  await page.route("**/api/usage/filter-options*", (route) => json(route, {
    data_revision: REVISION,
    models: [
      { model: "gpt-5", provider: "openai" },
      { model: "o4-mini", provider: "route-models" },
    ],
    projects: [{ kind: "project", project_name: "Usagi", project_path: "/work/Usagi" }],
  }));
  await page.route("**/api/usage/summary*", (route) => {
    const url = new URL(route.request().url());
    const hasModel = url.searchParams.has("model");
    const hasProject = url.searchParams.has("project_path") || url.searchParams.has("include_projectless") || url.searchParams.has("include_unknown_project");
    const mode = hasModel && hasProject ? "filtered" : hasModel ? "model" : "base";
    return json(route, summary(rangeFrom(route.request().url()), mode));
  });
  await page.route("**/api/usage/model-distribution*", (route) => json(route, modelDistribution(rangeFrom(route.request().url()))));
  await page.route("**/api/usage/projects*", (route) => json(route, projectDistribution(rangeFrom(route.request().url()))));
  await page.route("**/api/usage/skills*", (route) => json(route, skills()));
  await page.route(/\/api\/usage\/sessions\?/, (route) => json(route, snapshot(rangeFrom(route.request().url()))));
  await page.route("**/api/usage/session-rows*", (route) => {
    const url = new URL(route.request().url());
    const ids = url.searchParams.getAll("root_session_id");
    return json(route, {
      range: rangeDto(rangeFrom(route.request().url())),
      data_revision: REVISION,
      items: SESSIONS.filter((item) => ids.includes(item.root_session_id)),
    });
  });
  await page.route("**/api/usage/sessions/*/detail*", (route) => {
    const url = new URL(route.request().url());
    const match = url.pathname.match(/\/api\/usage\/sessions\/([^/]+)\/detail$/);
    const rootId = decodeURIComponent(match?.[1] ?? "session-1");
    return json(route, detail(rangeFrom(route.request().url()), rootId));
  });
}

async function waitForDashboard(page: Page) {
  await expect(page.getByRole("heading", { name: "Usagi" })).toBeVisible();
  await expect(page.getByLabel("KPI 指标")).toBeVisible();
  await expect(page.getByRole("table")).toBeVisible();
}

async function illegalStorageWrites(page: Page) {
  return page.evaluate(() => (window as unknown as { __miniIllegalStorageWrites?: number }).__miniIllegalStorageWrites ?? 0);
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const originalSetItem = Storage.prototype.setItem;
    Object.defineProperty(window, "__miniIllegalStorageWrites", { value: 0, writable: true, configurable: true });
    Storage.prototype.setItem = function patchedSetItem(key: string, value: string) {
      if (key === "usagi.theme") {
        originalSetItem.call(this, key, value);
        return;
      }
      const target = window as unknown as { __miniIllegalStorageWrites: number };
      target.__miniIllegalStorageWrites += 1;
      throw new Error(`unexpected persistent browser state: ${key}`);
    };
  });
});

test("real Query API exposes the v0.2.0 Summary and Session contracts", async ({ page }) => {
  await page.goto("/");
  const result = await page.evaluate(async () => {
    const [healthResponse, revisionResponse, summaryResponse, sessionsResponse] = await Promise.all([
      fetch("/api/health"),
      fetch("/api/revision"),
      fetch("/api/usage/summary?range=year"),
      fetch("/api/usage/sessions?range=year"),
    ]);
    return {
      statuses: [healthResponse.status, revisionResponse.status, summaryResponse.status, sessionsResponse.status],
      summary: await summaryResponse.json(),
      sessions: await sessionsResponse.json(),
    };
  });
  expect(result.statuses).toEqual([204, 200, 200, 200]);
  expect(result.summary.usage).toHaveProperty("cost_incomplete_session_count");
  expect(result.summary.usage).toHaveProperty("session_health");
  expect(result.sessions.sort_index.length).toBeGreaterThan(0);
  expect(result.sessions.sort_index[0]).toHaveProperty("combined_estimated_cost");
});

test("C1 desktop 1512px matches the approved v0.2.0 dashboard geometry", async ({ page }) => {
  await page.setViewportSize({ width: 1512, height: 1000 });
  await routeStableDashboardData(page);
  await page.goto("/");
  await waitForDashboard(page);

  const themeTokens = await page.evaluate(() => {
    const styles = getComputedStyle(document.documentElement);
    return ["--color-primary", "--color-card", "--color-border"].map((token) => styles.getPropertyValue(token).trim());
  });
  for (const token of themeTokens) expect(token).not.toBe("");

  const rangeTabList = page.locator('[role="tablist"]').first();
  const selectedRangeTab = rangeTabList.locator('[role="tab"][aria-selected="true"]');
  await expect(selectedRangeTab).toHaveCount(1);
  const rangeIndicator = selectedRangeTab.locator("xpath=..").locator(":scope > span").first();
  await expect(rangeIndicator).toHaveCount(1);
  const indicatorStyle = await rangeIndicator.evaluate((node) => {
    const style = getComputedStyle(node);
    const rect = node.getBoundingClientRect();
    return { backgroundColor: style.backgroundColor, width: rect.width, height: rect.height };
  });
  expect(indicatorStyle.backgroundColor).not.toBe("transparent");
  expect(indicatorStyle.backgroundColor).not.toBe("rgba(0, 0, 0, 0)");
  expect(indicatorStyle.width).toBeGreaterThan(0);
  expect(indicatorStyle.height).toBeGreaterThan(0);
  expect(await selectedRangeTab.evaluate((node) => getComputedStyle(node).fontSize)).toBe("14px");

  const syncButton = page.getByRole("button", { name: "同步数据", exact: true });
  expect(await syncButton.evaluate((node) => getComputedStyle(node).fontSize)).toBe("12px");

  const filterTrigger = page.getByRole("button", { name: "模型筛选，全部", exact: true });
  const filterStyle = await filterTrigger.evaluate((node) => {
    const style = getComputedStyle(node);
    return {
      backgroundColor: style.backgroundColor,
      borderColor: style.borderTopColor,
      borderStyle: style.borderTopStyle,
      borderWidth: style.borderTopWidth,
    };
  });
  expect(filterStyle.backgroundColor).not.toBe("transparent");
  expect(filterStyle.backgroundColor).not.toBe("rgba(0, 0, 0, 0)");
  expect(filterStyle.borderStyle).not.toBe("none");
  expect(filterStyle.borderWidth).not.toBe("0px");
  expect(filterStyle.borderColor).not.toBe("rgba(0, 0, 0, 0)");

  const shell = page.locator(".dashboard-shell");
  const content = page.locator(".dashboard-content");
  const topLevelStack = content.locator(":scope > div.flex.flex-col.gap-8").first();
  const kpi = page.getByLabel("KPI 指标");
  const cards = kpi.locator(":scope > *");
  await expect(cards).toHaveCount(5);
  await expect(topLevelStack.locator(":scope > *")).toHaveCount(5);

  const topLevelBoxes = await topLevelStack.locator(":scope > *").evaluateAll((nodes) => nodes.map((node) => {
    const rect = node.getBoundingClientRect();
    return { top: rect.top, bottom: rect.bottom };
  }));
  const topLevelGaps = topLevelBoxes.slice(1).map((box, index) => box.top - topLevelBoxes[index].bottom);
  for (const gap of topLevelGaps) {
    expect(gap).toBeGreaterThanOrEqual(31);
    expect(gap).toBeLessThanOrEqual(33);
  }

  const shellBox = await shell.boundingBox();
  const contentBox = await content.boundingBox();
  const kpiBox = await kpi.boundingBox();
  expect(shellBox?.x).toBe(0);
  expect(contentBox?.x).toBe(84);
  expect(contentBox?.width).toBe(1344);
  expect(kpiBox?.x).toBe(100);
  expect(kpiBox?.width).toBe(1312);

  const cardBoxes = await cards.evaluateAll((nodes) => nodes.map((node) => {
    const rect = node.getBoundingClientRect();
    return { width: rect.width, height: rect.height, x: rect.x };
  }));
  expect(cardBoxes.map((box) => Math.round(box.width))).toEqual([304, 236, 236, 236, 236]);
  expect(cardBoxes.map((box) => Math.round(box.height))).toEqual([144, 144, 144, 144, 144]);
  expect(Math.round(cardBoxes[1].x - (cardBoxes[0].x + cardBoxes[0].width))).toBe(16);

  const chartSection = page.getByLabel("使用分布图表");
  const modelCard = chartSection.locator("article").filter({ has: page.getByRole("heading", { name: "模型分布" }) });
  const projectCard = chartSection.locator("article").filter({ has: page.getByRole("heading", { name: "项目分布" }) });
  const skillsCard = chartSection.locator("article").filter({ has: page.getByRole("heading", { name: "Skills Used" }) });
  const modelBox = await modelCard.boundingBox();
  const projectBox = await projectCard.boundingBox();
  const skillsBox = await skillsCard.boundingBox();
  expect(modelBox?.width).toBeCloseTo(projectBox?.width ?? 0, 0);
  expect(modelBox?.y).toBeCloseTo(projectBox?.y ?? 0, 0);
  expect(skillsBox?.width).toBeCloseTo(1312, 0);
  expect((skillsBox?.y ?? 0)).toBeGreaterThan((modelBox?.y ?? 0) + (modelBox?.height ?? 0));

  const sessionSection = page.locator('section[aria-labelledby="session-heading"]');
  const sessionHeadingBox = await sessionSection.getByRole("heading", { name: "Session 记录" }).boundingBox();
  const paginationBox = await sessionSection.getByText("1 / 1", { exact: true }).boundingBox();
  expect((paginationBox?.x ?? 0)).toBeGreaterThan((sessionHeadingBox?.x ?? 0));
  expect(paginationBox?.y).toBeCloseTo(sessionHeadingBox?.y ?? 0, 0);

  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(1512);
  expect(await page.evaluate(() => document.body.scrollWidth)).toBeLessThanOrEqual(1512);
  expect(await illegalStorageWrites(page)).toBe(0);
});

test("C2 covers the approved v0.2.0 core interaction flow", async ({ page }) => {
  await page.setViewportSize({ width: 1512, height: 1000 });
  await routeStableDashboardData(page);
  await page.goto("/");
  await waitForDashboard(page);

  await expect.poll(() => page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(true);
  const themeToggle = page.getByRole("button", { name: "Switch to light mode" });
  await expect(themeToggle).toHaveClass(/rounded-xl/);
  await expect(themeToggle).toHaveClass(/border-border/);
  await expect(themeToggle).toHaveClass(/bg-background/);
  await expect(themeToggle).toHaveClass(/p-2\.5/);
  const themeIconBox = await themeToggle.locator("svg").boundingBox();
  expect(themeIconBox?.width).toBeCloseTo(20, 0);
  expect(themeIconBox?.height).toBeCloseTo(20, 0);
  const supportsViewTransition = await page.evaluate(() => "startViewTransition" in document);
  await themeToggle.click();
  if (supportsViewTransition) {
    expect(await page.evaluate(() => document.documentElement.dataset.beuiVt)).toBe("circle-blur");
    expect(await page.evaluate(() => document.documentElement.style.getPropertyValue("--beui-vt-origin"))).toBe("50% 100%");
  }
  await expect.poll(() => page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(false);
  expect(await page.evaluate(() => localStorage.getItem("usagi.theme"))).toBe("light");
  await page.getByRole("button", { name: "Switch to dark mode" }).click();
  await expect.poll(() => page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(true);

  await page.getByRole("tab", { name: "7d" }).click();
  await expect(page.getByRole("tab", { name: "7d" })).toHaveAttribute("aria-selected", "true");

  const customRangeTab = page.getByRole("tab", { name: "自定义", exact: true });
  await customRangeTab.click();
  const customRangeDialog = page.getByRole("dialog", { name: "自定义日期范围" });
  await expect(customRangeDialog).toBeVisible();
  const customRangeBox = await customRangeDialog.boundingBox();
  expect(customRangeBox).not.toBeNull();
  expect(customRangeBox?.x ?? -1).toBeGreaterThanOrEqual(0);
  expect(customRangeBox?.y ?? -1).toBeGreaterThanOrEqual(0);
  expect(customRangeBox?.width ?? 0).toBeGreaterThan(0);
  expect(customRangeBox?.height ?? 0).toBeGreaterThan(0);
  expect((customRangeBox?.x ?? 0) + (customRangeBox?.width ?? 0)).toBeLessThanOrEqual(1512);
  expect((customRangeBox?.y ?? 0) + (customRangeBox?.height ?? 0)).toBeLessThanOrEqual(1000);
  await page.waitForTimeout(250);
  await expect(customRangeDialog).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(customRangeDialog).toBeHidden();

  const kpi = page.getByLabel("KPI 指标");
  await page.getByRole("button", { name: "模型筛选，全部" }).click();
  await page.getByRole("checkbox", { name: "gpt-5" }).click();
  await page.keyboard.press("Escape");
  await expect(kpi.locator(":scope > *")).toHaveCount(4);
  await expect(kpi.locator('span[title="1,150"]')).toHaveCount(1);

  await page.getByRole("button", { name: "项目筛选，全部" }).click();
  await page.getByRole("checkbox", { name: "Usagi" }).click();
  await page.keyboard.press("Escape");
  await expect(kpi.locator('span[title="750"]')).toHaveCount(1);

  const chartSection = page.getByLabel("使用分布图表");
  const modelCard = chartSection.locator("article").filter({ has: page.getByRole("heading", { name: "模型分布" }) });
  await modelCard.getByRole("tab", { name: "费用" }).click();
  await expect(modelCard.getByRole("img", { name: "模型分布费用分布" })).toBeVisible();
  const legendButtons = modelCard.getByRole("button");
  await expect.poll(() => legendButtons.count()).toBeGreaterThan(1);
  await legendButtons.nth(0).focus();
  await expect.poll(async () => Number(await legendButtons.nth(1).evaluate((node) => getComputedStyle(node).opacity))).toBeLessThan(1);

  const skillsCard = chartSection.locator("article").filter({ has: page.getByRole("heading", { name: "Skills Used" }) });
  const dateTriggers = skillsCard.getByRole("button", { name: /^2026-08-\d{2}$/ });
  await expect(dateTriggers).toHaveCount(7);
  await dateTriggers.nth(3).hover();
  await expect(page.getByRole("dialog").getByText("2026-08-16", { exact: true })).toBeVisible();

  const sessionRows = page.locator('tr[data-session-root-id]');
  await expect(sessionRows.first()).toHaveAttribute("data-session-root-id", "session-1");
  await page.getByRole("button", { name: "合计费用" }).click();
  await expect(sessionRows.first()).toHaveAttribute("data-session-root-id", "session-2");

  await sessionRows.first().click();
  const dialog = page.getByRole("dialog", { name: "Session 详情" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("heading", { name: "Detail session-2" })).toBeVisible();

  const mainFirst = dialog.getByRole("button", { name: "gpt-5 (high)" });
  const mainSecond = dialog.getByRole("button", { name: "o4-mini (—)" });
  await expect(mainFirst).toHaveAttribute("aria-expanded", "false");
  await expect(mainSecond).toHaveAttribute("aria-expanded", "false");
  await mainFirst.click();
  await expect(mainFirst).toHaveAttribute("aria-expanded", "true");
  await mainSecond.click();
  await expect(mainFirst).toHaveAttribute("aria-expanded", "false");
  await expect(mainSecond).toHaveAttribute("aria-expanded", "true");

  const subFirst = dialog.getByRole("button", { name: "Recent subagent" });
  const subSecond = dialog.getByRole("button", { name: "Old subagent" });
  await expect(subFirst).toHaveAttribute("aria-expanded", "false");
  await expect(subSecond).toHaveAttribute("aria-expanded", "false");
  await subFirst.click();
  await expect(subFirst).toHaveAttribute("aria-expanded", "true");
  await subSecond.click();
  await expect(subFirst).toHaveAttribute("aria-expanded", "false");
  await expect(subSecond).toHaveAttribute("aria-expanded", "true");

  await dialog.getByRole("button", { name: "关闭 Session 详情" }).click();
  await expect(dialog).toBeHidden();
  expect(await illegalStorageWrites(page)).toBe(0);
});

test("C3 narrow viewport wraps KPI and Donuts, scrolls only the Table, and makes Drawer full width", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await routeStableDashboardData(page);
  await page.goto("/");
  await waitForDashboard(page);

  const kpiCards = page.getByLabel("KPI 指标").locator(":scope > *");
  await expect(kpiCards).toHaveCount(5);
  const kpiColumns = await kpiCards.evaluateAll((nodes) => new Set(nodes.map((node) => Math.round(node.getBoundingClientRect().x))).size);
  expect(kpiColumns).toBe(1);

  const chartSection = page.getByLabel("使用分布图表");
  const modelCard = chartSection.locator("article").filter({ has: page.getByRole("heading", { name: "模型分布" }) });
  const projectCard = chartSection.locator("article").filter({ has: page.getByRole("heading", { name: "项目分布" }) });
  const modelBox = await modelCard.boundingBox();
  const projectBox = await projectCard.boundingBox();
  expect(modelBox?.x).toBeCloseTo(projectBox?.x ?? 0, 0);
  expect((projectBox?.y ?? 0)).toBeGreaterThan((modelBox?.y ?? 0) + (modelBox?.height ?? 0));

  const table = page.getByRole("table");
  const tableScroller = table.locator("xpath=..");
  const scrollState = await tableScroller.evaluate((node) => ({
    clientWidth: (node as HTMLElement).clientWidth,
    scrollWidth: (node as HTMLElement).scrollWidth,
    overflowX: getComputedStyle(node).overflowX,
  }));
  expect(scrollState.scrollWidth).toBeGreaterThan(scrollState.clientWidth);
  expect(["auto", "scroll"]).toContain(scrollState.overflowX);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);

  await page.locator('tr[data-session-root-id]').first().click();
  const dialog = page.getByRole("dialog", { name: "Session 详情" });
  await expect(dialog).toBeVisible();
  await expect.poll(async () => (await dialog.boundingBox())?.x ?? Number.POSITIVE_INFINITY).toBeCloseTo(0, 0);
  await expect.poll(async () => (await dialog.boundingBox())?.width ?? 0).toBeCloseTo(390, 0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  await dialog.getByRole("button", { name: "关闭 Session 详情" }).click();
  await expect(dialog).toBeHidden();
  expect(await illegalStorageWrites(page)).toBe(0);
});

test("T-Q-008 Codex Quota stays last, fixed-width, and overflow-free at required viewports", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  await routeStableDashboardData(page);
  await page.goto("/");
  await waitForDashboard(page);

  const kpi = page.getByLabel("KPI 指标");
  const cards = kpi.locator(":scope > *");
  await expect(cards).toHaveCount(5);
  await expect(cards.last().getByText("剩余配额")).toBeVisible();
  const cardWidths = await cards.evaluateAll((nodes) => nodes.map((node) => Math.round(node.getBoundingClientRect().width)));
  expect(cardWidths.slice(1)).toEqual([236, 236, 236, 236]);
  expect(cardWidths[0]).toBeLessThan(236);

  await cards.last().getByRole("button", { name: "Pro 5x", exact: true }).hover();
  await expect(page.getByRole("dialog")).toContainText("hoge@example.com");
  await expect(page.getByRole("dialog")).toContainText("重置卡：2 次");

  for (const width of [1280, 767]) {
    await page.setViewportSize({ width, height: 1000 });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    await expect.poll(() => page.evaluate(() => document.body.scrollWidth)).toBeLessThanOrEqual(width);
  }
});
