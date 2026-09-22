import { CircleAlert, CircleX } from "lucide-react";
import type { SessionItemDto, SessionSortField, SessionSortOrder } from "../../data/types";
import { Table, type SortState, type TableColumn } from "../../ui/beui/table";
import { Tooltip } from "../../ui/beui/tooltip";
import { formatCost, formatRatio } from "../format";
import {
  formatSessionModel,
  formatSessionProject,
  formatSessionTime,
  formatSessionTitle,
  formatSessionTokenInteger,
} from "./sessionFormat";

type SessionTableProps = {
  rows: SessionItemDto[];
  timezone: string;
  selectedRootSessionId?: string | null;
  onOpenSession?: (row: SessionItemDto) => void;
  loadState: "initial" | "loading" | "ready" | "refreshing" | "error";
  pageState: "idle" | "loading" | "error";
  sortBy: SessionSortField;
  sortOrder: SessionSortOrder;
  onSort: (sortBy: SessionSortField) => void;
  sourceDisplay?: (source: string) => string;
};

const TABLE_ROW_HEIGHT = 48;
const TABLE_PAGE_SIZE = 10;
const TABLE_EMPTY_HEIGHT = 192;
const TABLE_INITIAL_LOADING_HEIGHT = 480;

export const sortableColumns: Array<{ field: SessionSortField; label: string }> = [
  { field: "last_activity", label: "最后活动" },
  { field: "project", label: "项目" },
  { field: "model", label: "模型" },
  { field: "combined_total_tokens", label: "合计 Token" },
  { field: "cache_hit_rate", label: "缓存命中率" },
  { field: "combined_estimated_cost", label: "合计费用" },
];

export function SessionTable({
  rows,
  timezone,
  selectedRootSessionId = null,
  onOpenSession,
  loadState,
  pageState,
  sortBy,
  sortOrder,
  onSort,
  sourceDisplay,
}: SessionTableProps) {
  const loading =
    loadState === "initial" ||
    loadState === "loading" ||
    loadState === "refreshing" ||
    pageState === "loading";
  const tableHeight =
    rows.length === 0
      ? loading
        ? TABLE_INITIAL_LOADING_HEIGHT
        : TABLE_EMPTY_HEIGHT
      : TABLE_ROW_HEIGHT * (Math.min(rows.length, TABLE_PAGE_SIZE) + 1);
  const columns: TableColumn<SessionItemDto>[] = [
    {
      key: "last_activity",
      header: "最后活动",
      width: "128px",
      sortable: true,
      cell: (item) => {
        const value = formatSessionTime(item.last_activity_at_ms, timezone);
        return <span className="block truncate" title={value.title}>{value.text}</span>;
      },
    },
    {
      key: "source",
      header: "终端",
      width: "98pt",
      cell: (item) => {
        const name = sourceDisplay ? sourceDisplay(item.source) : item.source;
        return <span className="block truncate" title={name}>{name}</span>;
      },
    },
    {
      key: "title",
      header: "标题",
      cell: (item) => {
        const title = formatSessionTitle(item.title);
        const status =
          item.data_status === "incomplete"
            ? { label: "数据不完整", icon: <CircleAlert className="h-4 w-4 text-warning" /> }
            : item.data_status === "error"
              ? { label: "数据计算异常", icon: <CircleX className="h-4 w-4 text-destructive" /> }
              : null;
        return (
          <span className="flex min-w-0 items-center gap-1.5">
            {status ? (
              <Tooltip content={status.label} side="top">
                <span aria-label={status.label} className="inline-flex shrink-0">{status.icon}</span>
              </Tooltip>
            ) : null}
            <span className="min-w-0 truncate" title={title}>{title}</span>
          </span>
        );
      },
    },
    {
      key: "project",
      header: "项目",
      width: "150px",
      sortable: true,
      cell: (item) => {
        const project = formatSessionProject(item.project_name);
        return <span className="block truncate" title={item.project_path ?? project}>{project}</span>;
      },
    },
    {
      key: "model",
      header: "模型",
      width: "140pt",
      sortable: true,
      cell: (item) => {
        const model = formatSessionModel(item.models_used);
        return <span className="block truncate" title={model.title} aria-label={model.accessibleName}>{model.text}</span>;
      },
    },
    {
      key: "combined_total_tokens",
      header: "合计 Token",
      width: "150px",
      sortable: true,
      align: "right",
      cell: (item) => item.inclusive_usage
        ? <span className="tabular-nums" title={String(item.inclusive_usage.total_tokens)}>{formatSessionTokenInteger(item.inclusive_usage.total_tokens).text}</span>
        : <span className="tabular-nums">—</span>,
    },
    {
      key: "subagent_count",
      header: "sub数量",
      width: "128px",
      align: "right",
      cell: (item) => <span className="tabular-nums">{item.subagent_count}</span>,
    },
    {
      key: "cache_hit_rate",
      header: "缓存命中率",
      width: "80pt",
      sortable: true,
      align: "right",
      cell: (item) => item.inclusive_usage
        ? <span className="tabular-nums" title={formatRatio(item.inclusive_usage.cache_hit_rate).title}>{formatRatio(item.inclusive_usage.cache_hit_rate).text}</span>
        : <span className="tabular-nums">—</span>,
    },
    {
      key: "combined_estimated_cost",
      header: "合计费用",
      width: "120px",
      sortable: true,
      align: "right",
      cell: (item) => item.data_status === "error"
        ? <span className="tabular-nums">—</span>
        : <span className="tabular-nums" title={formatCost(item.inclusive_usage?.estimated_cost ?? null).title}>{formatCost(item.inclusive_usage?.estimated_cost ?? null).text}</span>,
    },
  ];

  const controlledSort: SortState = { key: sortBy, direction: sortOrder };

  return (
    <Table
      data={rows}
      columns={columns}
      getRowId={(row) => row.root_session_id}
      rowHeight={TABLE_ROW_HEIGHT}
      height={tableHeight}
      loading={loading}
      skeletonRows={10}
      emptyState="当前时间范围暂无 Session 记录"
      sort={controlledSort}
      onSortChange={(next) => onSort((next?.key ?? sortBy) as SessionSortField)}
      manualSort
      selectable={false}
      resizable={false}
      reorderable={false}
      className="rounded-2xl"
      getRowProps={(item) => {
        const error = item.data_status === "error";
        const activate = () => {
          if (!error) onOpenSession?.(item);
        };
        return {
          "data-session-root-id": item.root_session_id,
          tabIndex: onOpenSession && !error ? 0 : -1,
          "aria-disabled": error || undefined,
          "aria-selected": item.root_session_id === selectedRootSessionId,
          onClick: onOpenSession && !error ? activate : undefined,
          onKeyDown: onOpenSession && !error
            ? (event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  activate();
                }
              }
            : undefined,
          className: error ? "cursor-default" : onOpenSession ? "cursor-pointer" : undefined,
        };
      }}
    />
  );
}
