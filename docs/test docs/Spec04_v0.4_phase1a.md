# Spec04 v0.4 Phase 1A Gate Record

Implementation status: Phase 1A implementation and required Go Gates pass. The Phase 1A handoff note about `FakeRuntime` below records an item that was open at that phase boundary; Phase 1B resolved it by replacing the latest-only fake channel with queued delivery. `TestFakeRuntimeSubscriptionPreservesSnapshotAndEveryTransition` verifies that subscribers receive the initial snapshot followed by every transition in order. Phase 1B's final `go test -count=1 ./internal/api ./internal/query ./internal/projection` gate passed.

Acceptance correction: `TestRuntimeHTTPTransitionRevisionsAvailabilityAndWatcherBaseline` now waits for the three asynchronous initial reads started by `Server.Start` (Usage, Sessions, and Quota) to return while Runtime is still starting before it transitions Runtime to ready. This removes a race between those reads and the test's exact session/quota call-count assertions. Verification: `go test ./internal/api -run '^TestRuntimeHTTPTransitionRevisionsAvailabilityAndWatcherBaseline$' -count=1` — Pass.

## A/B interface contract

Phase 1A exposes the following seams for Phase 1B:

- `RevisionService` owns the in-memory `RevisionTuple` and provides `Current`, `BumpData`, `BumpStatus`, and `Subscribe`.
- `RuntimeStatusSource` provides `Status` and `Subscribe`. `magpie.Runtime.Subscribe` first yields an atomic current-state snapshot, then every later state transition in order. Its event queue does not coalesce transitions. The API bridge ignores the first snapshot, bumps status once for each later transition, and adds one data bump on each non-ready to ready transition.
- `QuotaSnapshot.Current` returns the cached `projection.QuotaView`, whether any successful read exists, and the most recent refresh error. `Replace` installs a successful value and clears the error; `RecordError` preserves the last successful value while recording failure.
- `query.Service.UsageSnapshot` reads `/api/usage?period=all` through its upstream seam. `RefreshSessions` returns sorted facts derived from the same `/api/sessions` response that atomically replaces the query snapshot.
- `magpie.WebError` marks only Magpie HTTP transport, non-2xx, and JSON decode failures, while preserving the underlying error chain (including `HTTPError`). Phase 1B can classify it with `errors.As` as HTTP 502; `query.ErrUpstreamUnavailable` remains HTTP 503, and unclassified errors map to HTTP 500.
- `newWatchers` takes Query, Projection, Revision, QuotaSnapshot, then Usage, Session, and Quota intervals as separate duration arguments. Phase 1B's `Config` owns the three named config fields and passes them through independently.
- `startRuntimeStatusSubscription(ctx, runtime, revisions)` is the API-to-revision bridge. It consumes the subscription snapshot and later transition events; it does not poll `Status()`.

Historical Phase 1A handoff item, resolved in Phase 1B: `FakeRuntime.Subscribe` needed to emit its current status before later changes and preserve each later transition. At the Phase 1A boundary, its capacity-one latest-only channel could drop rapid transitions. Phase 1B changed it to queued delivery; `TestFakeRuntimeSubscriptionPreservesSnapshotAndEveryTransition` now covers the initial snapshot and ordered transitions. See the [Phase 1B record](Spec04_v0.4_phase1b.md).

## Gate evidence

| Gate | Test / command | Result | Open item |
|---|---|---|---|
| P1A-01, P1A-02 | `go test ./internal/api ./internal/query ./internal/projection ./internal/magpie` — `TestUsageWatcherBaselinesAndBumpsOnceForChangedSnapshot`, `TestUsageWatcherFindsHistoricalOnlyChanges` | Pass | No open item. |
| P1A-03 | Same selected package command — `TestSessionWatcherUsesOneRefreshSeamAndFingerprintsVisibleFacts` | Pass | No open item. |
| P1A-04, P1A-05 | Same selected package command — `TestQuotaWatcherStoresProjectionViewWithoutBumpingRevision`, `TestInternalAPIQuotaCacheAndManualRefresh` | Pass | Background watcher uses `asked=false`; manual HTTP refresh uses `asked=true`. |
| P1A-06 | Same selected package command — `TestWatchersKeepIntervalsIndependent`, `TestDefaultConfigAndServerAcceptsStartingRuntime` | Pass | Config carries independent 30s/30s/60s fields. |
| P1A-07, P1A-10 | `go test ./internal/api ./internal/query ./internal/projection ./internal/magpie` — `TestRuntimeHTTPTransitionRevisionsAvailabilityAndWatcherBaseline` | Pass | Same Query/Projection/Server returns 503 while starting, emits status/data revisions over SSE on ready, successfully re-fetches Summary, and does not add a revision when failed startup reads later establish the first successful Usage/Sessions baselines. |
| P1A-08 | Same selected package command — `TestRuntimeHTTPTransitionRevisionsAvailabilityAndWatcherBaseline`, `TestRuntimeSubscriptionDeliversInitialAndEveryTransitionInOrder`, existing `TestRuntimeUnexpectedExitRevokesClientBeforeDone`, `TestRuntimeUpstreamLooksUpCurrentClientAtEachOperation` | Pass | The integration transition to unavailable returns 503 and confirms the prior HTTP client receives no further request; the real Runtime unit test verifies client revocation on sidecar exit. |
| P1A-09 | Same selected package command — `TestRefreshSessionsReturnsSortedFactsFromSinglePublishedSnapshot`, `TestSessionWatcherUsesOneRefreshSeamAndFingerprintsVisibleFacts` | Pass | No open item. |
| §8.1 failure/baseline/fingerprint | Selected package command — `TestWatcherFailuresRetainLastSuccessfulBaseline`, `TestUsageFingerprintExcludesGeneratedAndTimingFields`, `TestUsageSnapshotAlwaysRequestsPeriodAll` | Pass | No open item. |
| Error boundary | Same selected package command — `TestClientNon2xx`, `TestClientDecodeAndTransportErrorsAreWebErrors`, `TestRuntimeHTTPTransitionRevisionsAvailabilityAndWatcherBaseline`, `TestInternalProjectionErrorClassification` | Pass | Magpie's wrapped non-2xx error reaches HTTP as 502; Runtime non-ready is 503 and unknown internal errors are 500. |
| Focused API race | `go test -race internal/api/revision.go internal/api/watchers.go internal/api/status.go internal/api/revision_test.go internal/api/watchers_test.go` | Pass | Ran before the synchronous status baseline change; final code has the standard selected-package Gate below. |
| API/Query race run | `go test -race ./internal/api ./internal/query` | Pass | This run preceded the final synchronous initial-status baseline and P1A07/10 integration-test addition; it was not repeated per the parent instruction to limit verification to the required gates. |
| Broad race probe | `go test -race ./internal/api ./internal/query ./internal/magpie` | Fail | Existing `TestRuntimeStartupFailures/readiness_fails_after_sidecar_start` failed under race instrumentation because its helper was killed during healthcheck; the focused Runtime subscription race test and Query race suite pass. |
| Selected Go package gate | `go test ./internal/api ./internal/query ./internal/projection ./internal/magpie` | Pass | No open item. |
| Full Go suite | P0 baseline evidence, not rerun during Phase 1A | Known failure accepted under the user's explicit exemption for the baseline and final full-repository Go gate. | The P0 baseline reports the pre-existing `internal/codex/scan.go` compiler error, outside Spec04 Phase 1A; it remains unfixed. |
