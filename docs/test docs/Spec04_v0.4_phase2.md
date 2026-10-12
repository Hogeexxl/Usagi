# Spec04 v0.4 Phase 2 Gate Record

Scope: §6.6 Public API v1 compatibility. This phase adds the six frozen read-only Public routes, standalone Public DTOs and mappers, and the Public revision event stream. No Phase 3 integration gate or Phase 4/full-repository gate was run.

## Gate evidence

The final scoped Go command passed:

```text
go test -count=1 ./internal/api ./internal/api/testsupport ./internal/query ./internal/projection
ok   github.com/Hogeexxl/Usagi/internal/api            0.543s
ok   github.com/Hogeexxl/Usagi/internal/api/testsupport 0.508s
ok   github.com/Hogeexxl/Usagi/internal/query           0.472s
ok   github.com/Hogeexxl/Usagi/internal/projection     0.537s
```

| Gate | Result | Evidence |
| --- | --- | --- |
| `GATE-P2-01` PeekFlow models decode every Public fixture without model changes | PASS | Ran the original checked-in `UsagiPublicAPIModels.swift` with Swift's `JSONDecoder` against the six frozen Phase 0 fixtures and all 14 actual Go handler response bodies listed below. `TestPublicV1FixturesDecodeLikePeekFlowDTOs` also runs the standalone Go decode oracle over the frozen fixture directory. PeekFlow files were read only. |
| `GATE-P2-02` Six frozen path literals | PASS | `TestPublicV1FrozenPathsAndInfoRevision` checks all six route constants against the exact §4.8.1 strings, and calls each path through the Public router. |
| `GATE-P2-03` Capabilities list | PASS | `TestPublicV1FrozenPathsAndInfoRevision` decodes `/api/v1/info` and checks the exact ordered capabilities: `revision`, `revision-events`, `status`, `codex-quota`, `usage-summary`. |
| `GATE-P2-04` Public DTO independence | PASS | `TestPublicV1DTOsAreIndependentFromInternalDTOs` parses `dto_public_v1.go` imports and rejects Internal package imports. Public DTOs are declared independently in that file. |
| `GATE-P2-05` Codex-only summary | PASS | `TestPublicV1SummaryLegacyMappingsAndSingleCodexProjection` checks the single `Summary` call carries exactly `Agents: []string{"codex"}`. |
| `GATE-P2-06` Legacy token formulas | PASS | The nonzero golden checks `Input=11`, `CacheRead=13`, `CacheWrite=17`, `CacheWrite1h=7`, `Output=29`, `Reasoning=7` map to `input_tokens=41`, `cached_tokens=13`, `cache_write_tokens=17`, `uncached_input_tokens=11`, `other_output_tokens=22`, and `total_tokens=70`. The nonzero 1h subset remains within `CacheWrite` and is not counted again. |
| `GATE-P2-07` Quota compatibility | PASS | `TestPublicV1QuotaUsesCachedSnapshotAndLegacyFourStates` covers the first Codex item after another provider, multiple Codex items, exact `5 hours`/`7 days` identities, 18000/604800-second legacy windows, ReadAt-over-AsOf and AsOf fallback, one missing window, selected-card error, and `loading`/`unavailable`/`auth_required`/`ready`. Every subcase checks the Public GET did not call `Projection.Quotas` (vendor path). |
| `GATE-P2-08` Legacy status compatibility | PASS | `TestPublicV1StatusCompatibilityUsesOnlyLegacyEnums` checks `starting -> running/unbound/null`, `ready -> idle/ready/completed`, and `unavailable -> failed/unbound/failed` with `MAGPIE_UNAVAILABLE`; the three `last_scan_*_at_ms` timestamps remain null, and `last_scan_error_code` is null for starting/ready. |
| `GATE-P2-09` Empty and multi-root summary facts | PASS | The empty golden checks `estimated_cost=0.0`, `complete`, zero roots, and zeroed health. The two-root golden checks one unpriced root, `session_count=2`, `cost_incomplete_session_count=1`, and health `2/1/1/0`. Public summary asserts exactly one Codex `Summary` call and no Sessions or Detail calls; `TestSummary` in `internal/projection/service_test.go` checks the root facts are computed from one thread projection and its single UsageRows load. |

### PeekFlow DTO decode command

This command concatenated the checked-in model source with a temporary decoder program on standard input. It wrote no files and did not modify the PeekFlow repository.

```sh
cat '/Users/hogee/Desktop/PeekFlow/Sources/PeekFlow/Services/Internal/Usagi/UsagiPublicAPIModels.swift' - <<'SWIFT' | swift - '/Users/hogee/Desktop/Usagi/internal/api/testsupport/testdata/public_v1'
let decoder = JSONDecoder()
decoder.keyDecodingStrategy = .convertFromSnakeCase
let root = CommandLine.arguments[1]
func fixture<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
    let path = URL(fileURLWithPath: root).appendingPathComponent(name)
    return try decoder.decode(type, from: Data(contentsOf: path))
}
_ = try fixture(UsagiInfoResponse.self, "info.json")
_ = try fixture(UsagiRevisionResponse.self, "revision.json")
_ = try fixture(UsagiStatusResponse.self, "status.json")
_ = try fixture(UsagiQuotaResponse.self, "codex_quota.json")
_ = try fixture(UsagiUsageSummaryResponse.self, "usage_summary.json")
let eventData = try Data(contentsOf: URL(fileURLWithPath: root).appendingPathComponent("events_revision.sse"))
let eventText = String(decoding: eventData, as: UTF8.self)
guard let eventLine = eventText.split(separator: "\n").first(where: { $0.hasPrefix("data: ") }) else { fatalError("missing SSE data") }
_ = try decoder.decode(UsagiRevisionResponse.self, from: Data(eventLine.dropFirst(6).utf8))
print("decoded 6 frozen Public v1 fixtures with PeekFlow models")
SWIFT
```

Result:

```text
decoded 6 frozen Public v1 fixtures with PeekFlow models
exit_code=0
```

### Decode actual Go handler responses with PeekFlow DTOs

`TestExportPublicV1HTTPResponsesForSwiftDecode` is a test-only exporter. It writes only when `USAGI_PUBLIC_V1_EXPORT_DIR` is set. The command below produced 14 response bodies in `/tmp`: `info`, `revision`, three runtime statuses, four quota states, four summary cost states, and the Public SSE data JSON. The exporter exercised `Server.ServeHTTP`; it did not write committed fixture files.

Generation command and result:

```sh
USAGI_PUBLIC_V1_EXPORT_DIR=/tmp/usagi-public-v1-phase2-http.v2VpaQ go test -count=1 ./internal/api -run '^TestExportPublicV1HTTPResponsesForSwiftDecode$' -v
```

```text
=== RUN   TestExportPublicV1HTTPResponsesForSwiftDecode
    public_v1_test.go:545: exported 14 actual Public handler response bodies to /tmp/usagi-public-v1-phase2-http.v2VpaQ
--- PASS: TestExportPublicV1HTTPResponsesForSwiftDecode (0.00s)
PASS
ok   github.com/Hogeexxl/Usagi/internal/api 0.454s
exit_code=0
```

The following command decoded each actual response with the unchanged PeekFlow DTO source and `JSONDecoder`:

```sh
cat '/Users/hogee/Desktop/PeekFlow/Sources/PeekFlow/Services/Internal/Usagi/UsagiPublicAPIModels.swift' - <<'SWIFT' | swift - '/tmp/usagi-public-v1-phase2-http.v2VpaQ'
let decoder = JSONDecoder()
decoder.keyDecodingStrategy = .convertFromSnakeCase
let root = CommandLine.arguments[1]
func fixture<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
    let path = URL(fileURLWithPath: root).appendingPathComponent(name)
    return try decoder.decode(type, from: Data(contentsOf: path))
}
_ = try fixture(UsagiInfoResponse.self, "info.json")
_ = try fixture(UsagiRevisionResponse.self, "revision.json")
for name in ["starting", "ready", "unavailable"] {
    _ = try fixture(UsagiStatusResponse.self, "status_\(name).json")
}
for name in ["loading", "unavailable", "auth_required", "ready"] {
    _ = try fixture(UsagiQuotaResponse.self, "quota_\(name).json")
}
for name in ["complete", "partial", "unknown", "empty"] {
    _ = try fixture(UsagiUsageSummaryResponse.self, "summary_\(name).json")
}
_ = try fixture(UsagiRevisionResponse.self, "sse_revision.json")
print("decoded 14 actual Go Public handler responses with PeekFlow models")
SWIFT
```

```text
decoded 14 actual Go Public handler responses with PeekFlow models
exit_code=0
```

Additional §6.6 behavior covered by the same Go gate: `TestPublicV1SummaryRejectsFiltersAndRequiresReadyRuntime` checks missing/duplicate ranges, rejects extra filter keys before Projection, and returns unavailable while Runtime is not ready. `TestPublicV1EventsSendsInitialAndChangedPublicRevisionFrames` checks the Public handler sends the frozen initial SSE frame, serializes later data/status tuples with Public revision JSON, and exits on request cancellation.

The full-repository Go command was not run under the explicit P0 baseline exemption; the scanner compile failure remains outside this phase and was not changed. No other Phase 2 verification limitation remains.
