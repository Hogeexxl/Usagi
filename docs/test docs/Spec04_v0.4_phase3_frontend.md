# Spec04 v0.4 Phase 3 Frontend Gate Record

Scope: frontend integration evidence for §6.7 against §4.5.1, §4.6, and §8.8. The tests use the existing Vitest/jsdom setup, the real `RevisionFeed`, and `usagiClient` HTTP parsing. No browser, full frontend suite/build, Go check, or static server smoke was run in this frontend task.

## Frontend integration behavior

- A valid SSE frame immediately stops the 60-second revision fallback before tuple comparison. A lower frame still triggers an authoritative `/api/revision` GET. The regression test keeps a fallback GET pending, verifies that the lower frame aborts it, and verifies the authoritative `0/0` response broadcasts one `epoch_reset` without duplicate tuple notifications or another fallback GET.
- The mounted Dashboard, charts, Session table, and open Detail consume JSON `Response` bodies through the real `usagiClient` parser. The epoch test starts at `80/12`, holds Summary, Filter Options, Models, Projects, Sessions, and Detail responses, then sends the `0/0` SSE hint. The authoritative `0/0` read clears the old state and reloads Summary, Filter Options, Status, Models, Projects, Sessions, and Detail. Every held request signal is aborted; resolving their captured revision-80 responses afterward leaves all displayed data at the new epoch. Detail returns revision 0 with its real children and stays on revision 0.
- A starting Status plus Summary `503 MAGPIE_UNAVAILABLE` recovers when the ready `1/1` data event arrives. Summary is fetched again and the integrated Dashboard, distributions, and Sessions reach the ready revision without manual refresh.
- After SSE disconnect, the 60-second authoritative fallback advances the mounted consumers and an open Detail to revision 81. An equal successful SSE frame stops fallback polling and produces no duplicate consumer reads.
- Quota begins with `reading: true` and performs one cached GET at the 60-second boundary. Manual refresh sends one POST and does not fetch Summary. After closing Detail, two more 60-second advances produce no Detail request.

## Gates

| Gate | Result | Evidence |
| --- | --- | --- |
| `GATE-P3-03` SSE disconnect recovery | PASS | `spec04RevisionIntegration.test.tsx` disconnects SSE, advances to the 60-second fallback, observes revision 81 in Summary/charts/Sessions/open Detail, then confirms an equal SSE frame cancels later fallback GETs without duplicate reads. `revisionFeed.test.ts` covers a lower SSE hint during a pending fallback GET. |
| `GATE-P3-04` Quota change does not create a Summary fetch storm | PASS | The real-client integration advances Quota's 60-second GET, performs manual Quota POST, and confirms Summary request count remains unchanged. Existing quota controller tests also check the `reading: true` 59,999/60,000 ms boundary. |
| `GATE-P3-05` Closed Detail has no polling | PASS | The integration closes a loaded Detail and advances the clock through two Quota intervals; the Detail request count remains unchanged. |
| Restart epoch `80/12 -> 0/0` frontend bootstrap | PASS | The integration confirms the lower SSE frame is only a hint, authoritative revision GET emits reset, all Dashboard/Distribution/Session/open Detail consumers reload revision 0, and delayed aborted revision-80 bodies cannot overwrite them. |
| Starting Summary 503 -> ready data recovery | PASS | The real wire integration returns `503 MAGPIE_UNAVAILABLE` while starting, then sends ready revision `1/1`; Summary and other mounted data consumers recover without a manual refresh. |

## Command and result

Run from `frontend/`:

```text
npm test -- --maxWorkers=1 src/data/revisionFeed.test.ts src/data/usagiClient.test.ts src/dashboard/spec04RevisionIntegration.test.tsx src/dashboard/DashboardPage.test.tsx src/dashboard/useDashboardController.test.tsx src/dashboard/charts/useDashboardChartsController.test.tsx src/dashboard/session/useSessionTableController.test.tsx src/dashboard/session/useSessionDetailController.test.tsx src/dashboard/useDashboardQuotaController.test.tsx
Test Files  9 passed (9)
Tests       31 passed (31)
Duration    8.00s
```

No frontend Phase 3 test failure remains. Backend SSE/static smoke and the broader final frontend/build gates are outside this frontend phase record.
