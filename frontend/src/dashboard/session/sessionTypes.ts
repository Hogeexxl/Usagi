import type {
  DashboardFilters,
  DashboardRange,
  SessionListItemDto,
  SessionSortField,
  SessionSortOrder,
} from "../../data/types";

export type SessionLoadState = "initial" | "loading" | "ready" | "refreshing" | "error";
export type SessionPageState = "idle" | "loading" | "error";

export type SessionTableViewModel = {
  range: DashboardRange;
  filters: DashboardFilters;
  rows: SessionListItemDto[];
  timezone: string;
  load_state: SessionLoadState;
  page_state: SessionPageState;
  page: number;
  /** Revision of the snapshot currently backing the visible rows. */
  data_revision?: number;
  total_items: number;
  total_pages: number;
  sort_by: SessionSortField;
  sort_order: SessionSortOrder;
  error_code?: string;
  page_error_code?: string;
  retry_load: () => void;
  go_to_page: (page: number) => void;
  previous_page: () => void;
  next_page: () => void;
  select_sort: (sortBy: SessionSortField) => void;
  retry_page: () => void;
};
