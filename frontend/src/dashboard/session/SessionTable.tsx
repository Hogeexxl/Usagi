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

const TABLE_ROW_HEIGHT = 48;
const TABLE_PAGE_SIZE = 10;
const TABLE_EMPTY_HEIGHT = 192;
const TABLE_INITIAL_LOADING_HEIGHT = 480;

export const sortableColumns: Array<{ field: SessionSortField; label: string }> = [
  { field: "last_activity", label: "最后活动" },
  { field: "project", label: "项目" },
  { field: "model", label: "模型" },
  { field: "total_tokens", label: "合计 Token" },
  { field: "cache_hit_rate", label: "缓存率" },
  { field: "cost", label: "合计费用" },
];

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
    loadState === "refreshing" ||
    pageState === "loading";
  const tableHeight =
    rows.length === 0
      ? loading
        ? TABLE_INITIAL_LOADING_HEIGHT
        : TABLE_EMPTY_HEIGHT
      : TABLE_ROW_HEIGHT * (Math.min(rows.length, TABLE_PAGE_SIZE) + 1);
  const columns: TableColumn<SessionListItemDto>[] = [
    {
      key: "last_activity",
      header: "最后活动",
      width: "128px",
      sortable: true,
      cell: (item) => {
        const value = formatSessionTime(item.thread.last_usage_ms, timezone);
        return <span className="block truncate" title={value.title}>{value.text}</span>;
      },
    },
    {
      key: "agent",
      header: "终端",
      width: "98pt",
      cell: (item) => {
        const agent = item.thread.key.agent;
        const name = agentDisplay ? agentDisplay(agent) : agent;
        return <span className="block truncate" title={name}>{name}</span>;
      },
    },
    {
      key: "title",
      header: "标题",
      cell: (item) => {
        const title = formatSessionTitle(item.thread.title);
        return (
          <span className="flex min-w-0 items-center gap-1.5">
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
        const project = item.thread.project.kind === "project" ? item.thread.project.name : projectDisplay(item.thread.project);
        const projectPath = item.thread.project.kind === "project" ? item.thread.project.path : project;
        return <span className="block truncate" title={projectPath}>{formatSessionProject(project)}</span>;
      },
    },
    {
      key: "model",
      header: "模型",
      width: "215px",
      sortable: true,
      cell: (item) => {
        const model = formatSessionModel(item.thread.model_effort);
        return <span className="block truncate" title={model.title} aria-label={model.accessibleName}>{model.text}</span>;
      },
    },
    {
      key: "total_tokens",
      header: "合计 Token",
      width: "150px",
      sortable: true,
      align: "right",
      cell: (item) => <span className="tabular-nums" title={String(item.inclusive_usage.total)}>{formatSessionTokenInteger(item.inclusive_usage.total).text}</span>,
    },
    {
      key: "subagent_count",
      header: "sub数",
      width: "80px",
      align: "right",
      cell: (item) => <span className="tabular-nums">{item.subagent_count}</span>,
    },
    {
      key: "cache_hit_rate",
      header: "缓存率",
      width: "127px",
      sortable: true,
      align: "right",
      cell: (item) => <span className="tabular-nums" title={formatRatio(item.inclusive_usage.cache_hit_rate).title}>{formatRatio(item.inclusive_usage.cache_hit_rate).text}</span>,
    },
    {
      key: "cost",
      header: "合计费用",
      width: "120px",
      sortable: true,
      align: "right",
      cell: (item) => <span className="tabular-nums" title={formatCost(item.inclusive_usage.cost).title}>{formatCost(item.inclusive_usage.cost).text}</span>,
    },
  ];

  const controlledSort: SortState = { key: sortBy, direction: sortOrder };

  return (
    <Table
      data={rows}
      columns={columns}
      getRowId={(row) => threadKeyId(row.thread.key)}
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
