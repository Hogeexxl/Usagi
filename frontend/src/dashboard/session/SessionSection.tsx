import { Button } from "../../ui/beui/button";
import type { AgentDisplayLookup } from "../shared/agentDisplay";
import { SessionTable } from "./SessionTable";
import { SessionTableFooter } from "./SessionTableFooter";
import type { SessionTableViewModel } from "./sessionTypes";
import type { SessionDetailControllerViewModel } from "./useSessionDetailController";

export function SessionSection({
  view,
  detail,
  agentDisplay,
}: {
  view: SessionTableViewModel;
  detail?: SessionDetailControllerViewModel;
  agentDisplay?: AgentDisplayLookup;
}) {
  const refreshing = view.load_state === "refreshing";
  return (
    <section aria-labelledby="session-heading">
      <div className="mb-3 flex min-w-0 flex-nowrap items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <h2 id="session-heading" className="m-0 text-sm font-medium leading-4 text-foreground">Session 记录</h2>
          {refreshing ? <span className="text-xs leading-4 text-muted-foreground" aria-live="polite">更新中…</span> : null}
        </div>
        <SessionTableFooter
          page={view.page}
          totalItems={view.total_items}
          totalPages={view.total_pages}
          pageState={view.page_state}
          onPrevious={view.previous_page}
          onNext={view.next_page}
          onGoToPage={view.go_to_page}
          onRetry={view.retry_page}
        />
      </div>
      <SessionTable
        rows={view.rows}
        timezone={view.timezone}
        selectedThreadKeyId={detail?.selected_thread_key_id}
        onOpenSession={detail?.open_detail}
        loadState={view.load_state}
        pageState={view.page_state}
        sortBy={view.sort_by}
        sortOrder={view.sort_order}
        onSort={view.select_sort}
        agentDisplay={agentDisplay}
        filtersActive={view.filters.agents.length > 0 || view.filters.models.length > 0 || view.filters.projects.length > 0}
      />
      {view.load_state === "error" ? <div className="mt-2 flex items-center gap-2 text-xs text-destructive" role="alert" aria-live="polite"><span>{view.error_code === "UPDATE_FAILED" || view.error_code === "REVISION_FAILED" ? "Session 记录更新失败" : "Session 记录加载失败"}</span><Button variant="ghost" size="sm" onClick={view.retry_load}>重试</Button></div> : null}
    </section>
  );
}
