# Spec04 v0.4 Phase 1B execution record

Scope: §6.4 Internal HTTP API + Static, including the explicitly assigned Query and Projection changes needed by the Phase 1B gates. No Phase 2 or Phase 3 work was started.

## Interface coordination

- `Server` consumes the Phase 1A `RevisionService`, `RuntimeStatusSource`, `QuotaSnapshot`, `newWatchers`, and `startRuntimeStatusSubscription` seams. `Start(ctx)` starts the runtime subscription and the three independently configured watchers; construction accepts the `starting` runtime state. The test runtime publishes its initial snapshot followed by every transition in order, using the production subscriber's queued delivery pattern.
- `Config` defaults to `127.0.0.1:3210` and 30s usage, 30s session, and 60s quota intervals. Static content enters through one `fs.FS` field; tests provide a filesystem and the production caller can provide its embedded filesystem.
- Internal handlers depend on Projection and the quota snapshot. Projection/query expose provider plus model selections and root session rollups. HTTP errors recognize `magpie.WebError` (and the wrapped `HTTPError` fake seam) as 502, runtime unavailability as 503, and unclassified internal errors as 500.
- No Public endpoints were registered. The planned registration location is the same API route setup used by the Internal handlers.

## Gates

The command below is the final Phase 1B package gate and passed after formatting the Phase 1B files:

```text
go test -count=1 ./internal/api ./internal/query ./internal/projection
ok github.com/Hogeexxl/Usagi/internal/api       0.321s
ok github.com/Hogeexxl/Usagi/internal/query     0.438s
ok github.com/Hogeexxl/Usagi/internal/projection 0.511s
```

| Gate | Result | Evidence |
| --- | --- | --- |
| `GATE-P1B-01` Endpoint success, invalid input, and upstream errors | PASS | `TestInternalUsageEndpointsValidateMapAndClassifyErrors` exercises success, invalid input, and unavailable errors for summary, model distribution, project distribution, filter options, sessions, and detail. `TestInternalAPIQuotaCacheAndManualRefresh` covers cached quota and manual refresh success, invalid query, and upstream unavailable. `TestDefaultConfigAndServerAcceptsStartingRuntime` and `TestInternalAPIBaseRoutesErrorsAndGuards` cover health, status, revision, and events success/invalid query. Health, revision, status, events, and cached quota have no upstream call path by contract. |
| `GATE-P1B-02` Usage JSON omits `estimated_cost_status` | PASS | `TestInternalUsageEndpointsValidateMapAndClassifyErrors` checks every usage response for the retired field. |
| `GATE-P1B-03` Session JSON omits retired identity/status fields | PASS | The same endpoint test checks sessions and detail for `source`, `native_session_id`, `root_session_id`, `data_status`, and `error_code`; DTO shape is also checked by `TestInternalDTOMapperPreservesNullAndEmptyShapes`. |
| `GATE-P1B-04` API 404 never serves `index.html` | PASS | `TestInternalAPIBaseRoutesErrorsAndGuards` checks both `/api/` and an unknown `/api/*` path. |
| `GATE-P1B-05` SPA deep links serve `index.html` | PASS | `TestStaticAssetsAndDashboardFallback` checks `/dashboard/sessions`, the root, and a static asset. |
| `GATE-P1B-06` Range semantics and strict wire parser | PASS | `TestResolveRangeNamedAndCustomRanges`, `TestResolveRangeUsesLocalCivilDaysAcrossDST`, `TestResolveRangeMatchesRustMidnightOverlapAndHalfHourDST`, `TestResolveRangeHandlesMidnightGapAndSkippedDate`, `TestSystemTimezoneUsesIANANameFromEnvironment`, `TestSystemTimezoneUsesOSDefaultWhenTZIsUnset`, and `TestResolveRangeRejectsWireOutsideContract` cover named/custom/year-boundary, UTC, DST overlap/gap, skipped civil date, `TZ` precedence, OS default timezone selection, and invalid/missing/duplicate wire values. Windows obtains the default IANA name from the same WinRT `Windows.Globalization.Calendar.GetTimeZone()` API used by the Rust Oracle; Windows builds embed Go's standard-library tzdata so that `time.LoadLocation` can apply the IANA rules. |
| `GATE-P1B-07` Provider-aware FilterOptions | PASS | `TestFilterOptions` uses duplicate `gpt-x` entries from two providers, provider display names, fallback names, malformed values, and first-page rows that must not affect options. |
| `GATE-P1B-08` Provider/model pair filtering | PASS | `TestProviderModelPairFiltersUsageGroupsAndSessions` verifies provider A and B both have `gpt-x`, each selection returns only its matching Usage group and Session, and each request is pushed down with the matching provider/model pair. `TestModelDistributionSeparatesSameModelAcrossProviders` drives the real Query and Projection services through a local Magpie HTTP fixture and verifies the same model has provider-specific totals in each distribution result. |
| `GATE-P1B-09` Root-only session list rollups | PASS | Projection session tests cover flat roots and Claude main/subagent inclusive totals/counts. `TestInternalUsageEndpointsValidateMapAndClassifyErrors` checks the HTTP list DTO and verifies the handler makes no Detail call. |
| `GATE-P1B-10` Summary root counts from one projection | PASS | `TestSummary` verifies `RootSessionCount` and `RootUnpricedSessionCount` and asserts Summary makes exactly one Usage, Sessions, and UsageRows call before any later list call. |

The user-authorized P0 baseline exception still applies: the full-repository Go gate is blocked by the pre-existing `internal/codex/scan.go` compile error. It was not rerun for this phase, and the scanner was not changed. The Phase 1B and directly affected Query/Projection package gate above passed.

### Windows system-timezone validation

`GOOS=windows GOARCH=amd64 go test -c -o /tmp/usagi-api.test.exe ./internal/api` passed as a compile check. This host is macOS, so the Windows WinRT call was not executed here. On a Windows machine, unset the `TZ` environment variable and run `go test -count=1 ./internal/api -run TestSystemTimezoneUsesOSDefaultWhenTZIsUnset`; the test must return a valid IANA name and a matching loaded location from the Windows system setting. The deterministic range tests separately cover UTC boundaries and DST behavior.
