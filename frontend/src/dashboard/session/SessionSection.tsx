import { Button } from "../../ui/beui/button";
import { SessionTable } from "./SessionTable";
import { SessionTableFooter } from "./SessionTableFooter";
import type { SessionTableViewModel } from "./sessionTypes";
import type { SessionDetailControllerViewModel } from "./useSessionDetailController";

export function SessionSection({
  view,
  detail,
  sourceDisplay,
}: {
  view: SessionTableViewModel;
  detail?: SessionDetailControllerViewModel;
  sourceDisplay?: (source: string) => string;
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
        selectedRootSessionId={detail?.selected_root_session_id}
        onOpenSession={detail?.open_detail}
        loadState={view.load_state}
        pageState={view.page_state}
        sortBy={view.sort_by}
        sortOrder={view.sort_order}
        onSort={view.select_sort}
        sourceDisplay={sourceDisplay}
      />
      {view.load_state === "error" ? <div className="mt-2 flex items-center gap-2 text-xs text-destructive" role="alert" aria-live="polite"><span>{view.error_code === "UPDATE_FAILED" || view.error_code === "REVISION_FAILED" ? "Session 记录更新失败" : "Session 记录加载失败"}</span><Button variant="ghost" size="sm" onClick={view.retry_load}>重试</Button></div> : null}
    </section>
  );
}
