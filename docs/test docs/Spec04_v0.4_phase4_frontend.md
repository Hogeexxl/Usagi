# Spec04 v0.4 Phase 4 Frontend Gate Record

Scope: §6.8 final frontend verification and §9.5 forbidden search gate. This record covers only the current frontend. No production implementation, cleanup, dependency, browser/UI, native Windows, Spec05, or Git changes were made for this gate. After the first build exposed three test-only type errors, the root task authorized narrow type fixes in the affected tests; the full suite was not rerun because those edits do not change runtime behavior.

## Full frontend tests

Command:

```text
npm --prefix frontend test -- --maxWorkers=1
```

Result:

```text
Test Files  32 passed (32)
Tests       164 passed (164)
Duration    28.59s
exit_code=0
```

## TypeScript check and production build

Command:

```text
npm --prefix frontend run build
```

The initial run returned `exit_code=2`. The script stopped at `tsc --noEmit`, so Vite did not run. TypeScript reported these three errors:

| Location | Diagnostic |
| --- | --- |
| `frontend/src/dashboard/spec04RevisionIntegration.test.tsx:86` | `TS2322`: `thread()` returned `project.kind` widened to `string`, not the `SessionProjectDto` discriminated union. |
| `frontend/src/dashboard/spec04RevisionIntegration.test.tsx:225` | `TS2339`: `name` was read from `ProjectDistributionItemDto` without narrowing `kind`. |
| `frontend/src/data/revisionFeed.test.ts:90` | `TS2322`: the test mock was inferred as returning `Promise<unknown>` rather than `Promise<RevisionResponse>`. |

The root task authorized only test typing corrections: `thread()` now returns `SessionThreadDto`, the project distribution assertion narrows `kind`, and the RevisionFeed mock and deferred values explicitly return `RevisionTuple`. No production code changed.

Affected test command after those corrections:

```text
npm --prefix frontend test -- --maxWorkers=1 src/data/revisionFeed.test.ts src/dashboard/spec04RevisionIntegration.test.tsx
Test Files  2 passed (2)
Tests       7 passed (7)
Duration    1.36s
exit_code=0
```

Final build command and result:

```text
npm --prefix frontend run build
exit_code=0
tsc --noEmit passed
Vite transformed 3,347 modules and built production assets
Vite build duration: 2.28s
```

## §9.5 source search

The implementation search command was:

```text
rg -n -i 'ModelFilterProvider|ACTIVE_MODEL_GROUPS|MODEL_RELEASE_DATES|estimated_cost_status|estimated_cost|session[-_]rows|CodexQuota|AntigravityQuota|codex.*antigravity.*quota|quota.*codex.*antigravity|/api/refresh|scanner|target.?thread|follow.?up' frontend/src --glob '!**/*.test.*'
```

It returned six legacy `estimated_cost` fields, all in `frontend/src/test-fixtures/t_mu03_f01_real_structure.json`, a checked-in historical test fixture. No application-code hits were found for the forbidden model/provider unions and release tables, retired Usage fields, old session-rows naming, Codex/Antigravity quota request combinations, `/api/refresh`, or scanner tracking/target/follow-up state. The fixture was left unchanged.

Skills main-path search command:

```text
rg -n -i 'skill' frontend/src/dashboard/DashboardPage.tsx frontend/src/dashboard/useDashboardController.ts frontend/src/dashboard/charts/useDashboardChartsController.ts frontend/src/dashboard/charts/ChartSection.tsx frontend/src/data/usagiClient.ts
```

Result: no matches. `SkillsUsageResponse` and the standalone Skills chart/tests remain outside that path. The active Dashboard quota client uses `GET /api/quota` and manual `POST /api/quota/refresh`.

Dashboard timer search command:

```text
rg -n 'setInterval|setTimeout|clearInterval|clearTimeout' frontend/src/dashboard --glob '!**/*.test.*'
```

It found the 60-second quota timer in `useDashboardQuotaController` and the separate update-status timer in `useUpdateController`; it found no timer in Session Detail or Filter Options.

## Tray/native boundary and manual UI

The only tray paths modified in this workspace are `frontend/src/tray/TrayPanelMeasurementPage.tsx`, `frontend/src/tray/TrayPanelPage.tsx`, and its test. Their diff adapts the tray measurement/render fixtures and view props to shared Usage, Quota, and refresh DTOs, including removal of Rust scanner refresh status. No native Windows/bootstrap files are modified by the frontend tray work.

No manual UI or browser verification was run.
