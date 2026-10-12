# Spec04 v0.4 Phase 1C Gate Record

Scope: §4.9 Dashboard contract migration and §6.5 Phase 1C tasks and gates, against §4.3/§4.4/§4.5.1/§8.8. No Phase 2, Phase 3, or Phase 4 work was started.

## Frontend migration

- `UsageDto` now follows the Spec04 fields, with `session_count` at the top level of `SummaryResponse`. The Dashboard consumes dynamic agents and provider/model pairs throughout filter options, UI selection, request serialization, and query identity.
- Session list/detail use the complete `ThreadKey` and `SessionListItemView` contract. Session rollups and existing sort columns use inclusive usage and `subagent_count`; Detail displays only returned children and their DTO values.
- Quota consumers share `GET /api/quota`, cached for 60 seconds, and use `POST /api/quota/refresh` only for the quota card action. The Dashboard reload button GETs Summary, Filter Options, Models, Projects, Sessions, and an open Detail without refreshing quota.
- Revision transport keeps SSE hints and the 60-second authoritative fallback. An authoritative lower tuple emits `epoch_reset`; Dashboard, Session, Detail, and Distribution clear old-epoch state and reload. Session Detail scope changes are keyed by the canonical query identity so equal range/filter objects do not restart requests.
- Rust scanner refresh/tracking types and calls are removed from the frontend. Skills API loading is out of the main Dashboard chain; reusable Skills presentation code remains.

## Gates

| Gate | Result | Evidence |
| --- | --- | --- |
| `GATE-P1C-01` No retired estimated cost status dependency | PASS | `src/data/usagiClient.test.ts` verifies the new Usage parser and rejects malformed Usage; a source scan found no `estimated_cost_status` or `EstimatedCostStatus` reference in `frontend/src`. `MetricGrid.test.tsx` checks `cost / total` and the `unpriced` count. |
| `GATE-P1C-02` Dynamic agent options | PASS | `src/data/usagiClient.test.ts` and `src/dashboard/FilterControls.test.tsx` cover arbitrary agent IDs/display names and selected filter state. |
| `GATE-P1C-03` Dynamic provider/model pairs | PASS | `src/data/usagiClient.test.ts` covers distinct same-name provider/model selections, query identity, and separate `model_at` values. `src/dashboard/FilterControls.test.tsx` covers backend provider order and dynamic groups. |
| `GATE-P1C-04` Full ThreadKey Session Detail | PASS | `src/data/usagiClient.test.ts`, `src/dashboard/session/useSessionDetailController.test.tsx`, and `src/dashboard/session/SessionDetailDrawer.test.tsx` cover all key fields, on-demand Detail, distinct child identities, and returned child DTOs. |
| `GATE-P1C-05` Unified Quota | PASS | `src/dashboard/useDashboardQuotaController.test.tsx` and `src/tray/TrayPanelPage.test.tsx` cover the shared snapshot and manual POST; source scan found no retired provider-specific quota endpoint. |
| `GATE-P1C-06` Epoch reset across Dashboard consumers | PASS | `useDashboardController.test.tsx` covers authoritative `80/12 -> 0/0` and new Summary/Filter Options/Status; Session and Detail controller tests verify new revision-0 responses after reset. |
| `GATE-P1C-07` Pure Dashboard reload | PASS | `src/dashboard/DashboardPage.test.tsx` opens a Session Detail, clicks the Dashboard refresh button, and asserts one additional GET for Summary, Filter Options, Models, Projects, Sessions, and that Detail. It asserts no additional Status or Quota request and no quota POST. Source scan found no scanner refresh/tracking symbols or `/api/refresh` call. |
| `GATE-P1C-08` Starting to ready recovery | PASS | `src/dashboard/useDashboardController.test.tsx` verifies a starting Summary failure recovers on the ready revision event without a manual refresh. |
| `GATE-P1C-09` Session accepts revision 0 | PASS | `useSessionTableController.test.tsx` seeds revision 80, delivers `epoch_reset` at 0, and asserts the new Session response at revision 0 replaces the old snapshot. |
| `GATE-P1C-10` Open Detail accepts revision 0 | PASS | `useSessionDetailController.test.tsx` holds an epoch-0 Detail request while the parent still reports revision 80, resolves the real request with Detail revision 0, and asserts revision 0 is accepted. |
| §8.8 quota timing and interaction | PASS | `useDashboardQuotaController.test.tsx` starts with `reading: true`, proves no 1-second retry, checks the 59,999/60,000 ms GET boundary, and checks manual refresh uses POST. |

## Commands

Run from `frontend/`:

```text
npm test -- --maxWorkers=1 --reporter=dot
Test Files  31 passed (31)
Tests       159 passed (159)
Duration    28.55s

npm run check -- --pretty false
PASS (tsc --noEmit)

npm run build
PASS (tsc --noEmit; Vite transformed 3,347 modules and built production assets)
```

The frontend suite was run serially because an earlier unconstrained parallel run exhausted the local Node worker heap. The final single-worker suite passed. No browser, Playwright, or manual UI check was run; these are outside the Phase 1C gate.
