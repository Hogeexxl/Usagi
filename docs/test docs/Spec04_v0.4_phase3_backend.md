# Spec04 v0.4 Phase 3 Backend Gate Record

Phase 3 backend implementation already provides distinct Internal/Public SSE handlers over the shared `RevisionService`, cached Internal/Public quota reads, background and manual quota refresh paths, runtime status mapping, and static SPA fallback. This phase adds focused real-graph backend evidence; no production implementation file required a correction.

## Gate evidence

| Requirement | Test / command | Result |
|---|---|---|
| P3-01 / P3-02, §8.6 | `TestPhase3SSEContractsFanoutKeepaliveAndCancellation` in `go test ./internal/api -run '^TestPhase3' -count=1` | Pass. Internal and Public handlers independently send an immediate exact `revision` frame with `id: <status>-<data>` and only the two revision fields; identical tuples are suppressed, both streams receive data/status bumps, 15 second keepalive comments leave revisions unchanged, and disconnect removes both subscriptions. |
| §6.7 tasks 3–4, §8.1 | `TestPhase3WatcherRevisionSSEHTTPChainAndRuntimeClients` | Pass. Real Query and Projection services use local Magpie HTTP fixtures. A title/Cwd/Last-only session metadata change causes one data revision and one `/api/sessions` request for that watcher tick; SSE carries the new tuple, Dashboard Sessions GET returns the updated title/project, Summary usage stays equal, and the Sessions handler does not call `Projection.SessionDetail`. |
| §6.7 task 7, P1A-07/08/10 integration | `TestPhase3WatcherRevisionSSEHTTPChainAndRuntimeClients` | Pass. Starting Summary returns 503; ready publishes status/data revisions and a subsequent Summary GET succeeds. Unavailable returns 503 and Public status uses legacy `failed/unbound/failed` mapping. A later ready transition uses the replacement Magpie client; the old client receives no further calls. |
| §6.7 tasks 5–6, P3-04, §4.3.7 | `TestPhase3QuotaRefreshAndCachedInternalPublicReads` | Pass. Background refresh sends no `asked` parameter; Internal and Public cached GETs send no vendor request. Manual refresh sends `asked=1`, replaces the same snapshot read by both endpoints, leaves revisions at zero, and makes no Usage request. |
| §6.7 task 9, §8.7 | `TestPhase3ViteAssetsAndAPIFallbackOverLoopbackHTTP` | Pass. A Go `httptest.Server` serves the current `frontend/dist/index.html`, its real hashed JavaScript asset, and a deep link over loopback; `/api/not-found` returns JSON 404 rather than SPA HTML. |
| `go test ./internal/api -run '^TestPhase3' -count=1` | Pass (`ok github.com/Hogeexxl/Usagi/internal/api 15.514s`). This is the only Go test command run for this phase. |

The focused tests were formatted with `gofmt -w internal/api/phase3_backend_test.go` before the passing command. The full Go suite and unrelated test suites were not run; the existing `internal/codex/scan.go` whole-repository compiler failure remains covered by the user's explicit baseline/final Go Gate exemption and was not repaired or re-run here.

## Frontend-owned Phase 3 checks

`GATE-P3-03` SSE failure recovery through the Dashboard's 60 second authoritative revision fallback, `GATE-P3-05` stopping Detail refresh after the drawer closes, and frontend consumers' restart epoch reset/full bootstrap belong to the parallel frontend integration work. This backend record does not claim those frontend checks passed.
