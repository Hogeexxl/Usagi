import type { SessionListItemDto, SessionSortField, SessionSortOrder } from "../../data/types";
import { threadKeyId } from "../../data/usagiClient";
import { Table, type SortState, type TableColumn } from "../../ui/beui/table";
import { formatCost, formatRatio } from "../format";
import { projectDisplay } from "../shared/projectDisplay";
import {
  formatSessionModel,
  formatSessionProject,
  formatSessionTime,
  formatSessionTitle,
  formatSessionTokenInteger,
} from "./sessionFormat";

type SessionTableProps = {
  rows: SessionListItemDto[];
  timezone: string;
  selectedThreadKeyId?: string | null;
  onOpenSession?: (row: SessionListItemDto) => void;
  loadState: "initial" | "loading" | "ready" | "refreshing" | "error";
  pageState: "idle" | "loading" | "error";
  sortBy: SessionSortField;
  sortOrder: SessionSortOrder;
  onSort: (sortBy: SessionSortField) => void;
  agentDisplay?: (agent: string) => string;
};

const TABLE_ROW_HEIGHT = 64;
const TABLE_HEADER_HEIGHT = 48;
const TABLE_PAGE_SIZE = 10;
const TABLE_EMPTY_HEIGHT = 192;
const TABLE_INITIAL_LOADING_HEIGHT = 480;

export const sortableColumns: Array<{ field: SessionSortField; label: string }> = [
  { field: "last_activity", label: "最后活动" },
  { field: "project", label: "项目" },
  { field: "total_tokens", label: "合计 Token" },
  { field: "cache_hit_rate", label: "缓存率" },
  { field: "cost", label: "合计费用" },
];

function subagentText(item: SessionListItemDto): string {
  return item.subagent_count === null ? "—" : `${item.subagent_count} subagent`;
}

export function SessionTable({
  rows,
  timezone,
  selectedThreadKeyId = null,
  onOpenSession,
  loadState,
  pageState,
  sortBy,
  sortOrder,
  onSort,
  agentDisplay,
}: SessionTableProps) {
  const loading =
    loadState === "initial" ||
    loadState === "loading" ||
    pageState === "loading";
  const tableHeight = rows.length === 0
    ? loading ? TABLE_INITIAL_LOADING_HEIGHT : TABLE_EMPTY_HEIGHT
    : TABLE_HEADER_HEIGHT + TABLE_ROW_HEIGHT * Math.min(rows.length, TABLE_PAGE_SIZE);
  const columns: TableColumn<SessionListItemDto>[] = [
    {
      key: "last_activity",
      header: "最后活动",
      width: "128px",
      sortable: true,
      cell: (item) => {
        const value = formatSessionTime(item.last_activity_ms, timezone);
        const separator = value.text.indexOf(" ");
        if (separator === -1) return <span className="block truncate" title={value.title}>{value.text}</span>;
        return (
          <div className="flex min-w-0 flex-col leading-5" title={value.title}>
            <span className="block min-w-0 truncate whitespace-nowrap">{value.text.slice(separator + 1)}</span>
            <span className="block min-w-0 truncate whitespace-nowrap text-xs leading-4 text-muted-foreground">{value.text.slice(0, separator)}</span>
          </div>
        );
      },
    },
    {
      key: "session",
      header: "会话",
      cell: (item) => {
        const title = formatSessionTitle(item.thread.title);
        const agent = item.thread.key.agent;
        const agentName = agentDisplay ? agentDisplay(agent) : agent;
        const model = formatSessionModel(item.thread.model_effort);
        const details = [agentName, model.text, subagentText(item)].filter(Boolean).join(" · ");
        return (
          <div className="flex min-w-0 flex-col leading-5">
            <span className="block min-w-0 truncate whitespace-nowrap" title={title} aria-label={title}>{title}</span>
            <span className="block min-w-0 truncate whitespace-nowrap text-xs leading-4 text-muted-foreground" title={details} aria-label={details}>{details}</span>
          </div>
        );
      },
    },
    {
      key: "project",
      header: "项目",
      width: "150px",
      sortable: true,
      cell: (item) => {
        const project = projectDisplay(item.thread.project);
        const projectPath = (item.thread.project.kind === "project" || item.thread.project.kind === "workspace") ? item.thread.project.path : project;
        return <span className="block truncate" title={projectPath}>{formatSessionProject(project)}</span>;
      },
    },
    {
      key: "total_tokens",
      header: "合计 Token",
      width: "150px",
      sortable: true,
      align: "right",
      cell: (item) => item.has_usage_rows
        ? <span className="tabular-nums" title={String(item.inclusive_usage.total)}>{formatSessionTokenInteger(item.inclusive_usage.total).text}</span>
        : <span>—</span>,
    },
    {
      key: "cache_hit_rate",
      header: "缓存率",
      width: "127px",
      sortable: true,
      align: "right",
      cell: (item) => item.has_usage_rows
        ? <span className="tabular-nums" title={formatRatio(item.inclusive_usage.cache_hit_rate).title}>{formatRatio(item.inclusive_usage.cache_hit_rate).text}</span>
        : <span>—</span>,
    },
    {
      key: "cost",
      header: "合计费用",
      width: "120px",
      sortable: true,
      align: "right",
      cell: (item) => item.has_usage_rows
        ? <span className="tabular-nums" title={formatCost(item.inclusive_usage.cost).title}>{formatCost(item.inclusive_usage.cost).text}</span>
        : <span>—</span>,
    },
  ];

  const controlledSort: SortState = { key: sortBy, direction: sortOrder };

  return (
    <Table
      data={rows}
      columns={columns}
      getRowId={(row) => threadKeyId(row.thread.key)}
      rowHeight={TABLE_ROW_HEIGHT}
      headerHeight={TABLE_HEADER_HEIGHT}
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
        const keyId = threadKeyId(item.thread.key);
        const activate = () => onOpenSession?.(item);
        return {
          "data-session-thread-key-id": keyId,
          tabIndex: onOpenSession ? 0 : -1,
          "aria-selected": keyId === selectedThreadKeyId,
          onClick: onOpenSession ? activate : undefined,
          onKeyDown: onOpenSession
            ? (event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  activate();
                }
              }
            : undefined,
          className: onOpenSession ? "cursor-pointer" : undefined,
        };
      }}
    />
  );
}
