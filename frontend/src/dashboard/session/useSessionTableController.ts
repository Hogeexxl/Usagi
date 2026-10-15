import { useMemo, useState } from "react";

import { dashboardQueryKey, threadKeyId } from "../../data/usagiClient";
import type {
  DashboardFilters,
  DashboardRange,
  SessionListItemDto,
  SessionListResponse,
  SessionSortField,
  SessionSortOrder,
} from "../../data/types";
import type { LoadState } from "../useDashboardController";
import type { SessionTableViewModel } from "./sessionTypes";

export const FRONTEND_PAGE_SIZE = 6;

const DEFAULT_SORT_BY: SessionSortField = "last_activity";
const DEFAULT_SORT_ORDER: SessionSortOrder = "desc";
const SORT_DEFAULTS: Record<SessionSortField, SessionSortOrder> = {
  last_activity: "desc",
  project: "asc",
  total_tokens: "desc",
  cost: "desc",
  cache_hit_rate: "desc",
};

function currentTimezone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "Asia/Shanghai";
}

function compareNullableText(left: string | null, right: string | null, order: SessionSortOrder): number {
  if (left === null || left === "" || right === null || right === "") {
    if (left === right || !left && !right) return 0;
    return !left ? 1 : -1;
  }
  const comparison = left.localeCompare(right, "en", { numeric: true });
  return order === "asc" ? comparison : -comparison;
}

function compareNullableNumber(left: number | null, right: number | null, order: SessionSortOrder): number {
  if (left === null || right === null) {
    if (left === right) return 0;
    return left === null ? 1 : -1;
  }
  const comparison = left < right ? -1 : left > right ? 1 : 0;
  return order === "asc" ? comparison : -comparison;
}

function compareSessions(left: SessionListItemDto, right: SessionListItemDto, sortBy: SessionSortField, order: SessionSortOrder): number {
  const projectName = (item: SessionListItemDto) => item.thread.project.kind === "project" ? item.thread.project.name : null;
  const usageValue = (item: SessionListItemDto, value: (row: SessionListItemDto) => number | null) =>
    item.has_usage_rows ? value(item) : null;
  let comparison = 0;
  if (sortBy === "last_activity") comparison = compareNullableNumber(left.last_activity_ms, right.last_activity_ms, order);
  if (sortBy === "project") comparison = compareNullableText(projectName(left), projectName(right), order);
  if (sortBy === "total_tokens") comparison = compareNullableNumber(
    usageValue(left, (item) => item.inclusive_usage.total),
    usageValue(right, (item) => item.inclusive_usage.total),
    order,
  );
  if (sortBy === "cost") comparison = compareNullableNumber(
    usageValue(left, (item) => item.inclusive_usage.cost),
    usageValue(right, (item) => item.inclusive_usage.cost),
    order,
  );
  if (sortBy === "cache_hit_rate") comparison = compareNullableNumber(
    usageValue(left, (item) => item.inclusive_usage.cache_hit_rate),
    usageValue(right, (item) => item.inclusive_usage.cache_hit_rate),
    order,
  );
  return comparison || threadKeyId(left.thread.key).localeCompare(threadKeyId(right.thread.key));
}

function totalPages(totalItems: number): number {
  return Math.ceil(totalItems / FRONTEND_PAGE_SIZE);
}

export function useSessionTableController(
  range: DashboardRange,
  filters: DashboardFilters,
  snapshot: SessionListResponse | null,
  loadState: LoadState,
  errorCode: string | undefined,
  retryLoad: () => void,
): SessionTableViewModel {
  const scopeKey = dashboardQueryKey(range, filters);
  const [pageState, setPageState] = useState({ scopeKey, page: 1 });
  const [sortBy, setSortBy] = useState<SessionSortField>(DEFAULT_SORT_BY);
  const [sortOrder, setSortOrder] = useState<SessionSortOrder>(DEFAULT_SORT_ORDER);
  const items = snapshot?.items ?? [];
  const pages = totalPages(items.length);
  const page = pageState.scopeKey === scopeKey ? pageState.page : 1;
  const visiblePage = pages === 0 ? 1 : Math.min(page, pages);
  const rows = useMemo(
    () => [...items]
      .sort((left, right) => compareSessions(left, right, sortBy, sortOrder))
      .slice((visiblePage - 1) * FRONTEND_PAGE_SIZE, visiblePage * FRONTEND_PAGE_SIZE),
    [items, sortBy, sortOrder, visiblePage],
  );

  const goToPage = (targetPage: number) => {
    if (!Number.isSafeInteger(targetPage) || targetPage < 1 || targetPage > pages || targetPage === visiblePage) return;
    setPageState({ scopeKey, page: targetPage });
  };
  const selectSort = (field: SessionSortField) => {
    if (field === sortBy) {
      setSortOrder((current) => current === "asc" ? "desc" : "asc");
      return;
    }
    setSortBy(field);
    setSortOrder(SORT_DEFAULTS[field]);
  };

  return {
    range,
    filters,
    rows,
    timezone: currentTimezone(),
    load_state: loadState,
    page_state: "idle",
    page: visiblePage,
    data_revision: snapshot?.data_revision,
    total_items: items.length,
    total_pages: pages,
    sort_by: sortBy,
    sort_order: sortOrder,
    error_code: errorCode,
    page_error_code: undefined,
    retry_load: retryLoad,
    go_to_page: goToPage,
    previous_page: () => goToPage(visiblePage - 1),
    next_page: () => goToPage(visiblePage + 1),
    select_sort: selectSort,
    retry_page: retryLoad,
  };
}
