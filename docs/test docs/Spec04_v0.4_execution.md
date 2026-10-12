# Spec04 v0.4 最终执行记录

唯一合同：`/Users/hogee/Downloads/Usagi_Spec04_Go_HTTP_API与Dashboard_PublicAPI_RevisionSSE实施方案_v0.4.md`。Phase 0–3 的实现和阶段 Gate 已由主线程验收。本记录完成 §6.8 阶段汇总与 §12 证据索引。最终相关 Go test/vet、frontend tests/build/禁项搜索均通过；全仓库 Go Gate 按用户授权豁免。Rust focused 命令自然失败：`compaction_upgrade_readiness` 在手动 scanner 请求前发现 active epoch 实际为 2、期望为 1；现有证据不能认定它是 baseline。用户明确授权记录并豁免本次最终 Rust Gate。该结果是失败记录加 Gate 豁免，不表示所有测试全绿；用户人工验收仍待完成。

本次收尾包含 Spec04 团队实现与阶段记录的本地提交；本轮没有修改代码/测试，也没有重跑测试。用户授权记录并豁免最终 Rust Gate 后提交本地版本供其验收。未推送 GitHub，也没有切换产品正式启动链。Windows tray/process bootstrap 与 Rust backend retirement 属于 Spec05 handoff。

## 基线与已授权的 Gate 豁免

起始状态：分支 `magpie-companion-spec03`，HEAD `bca8c3dd9e55b0360d78dce3434690dc7d87a2b6`，工作区干净。用户对全仓库 Go 基线失败的原话授权为：**“接受记录该失败，豁免基线和最终全仓库 Go Gate”**。这项豁免仅覆盖基线和最终全仓库 Go Gate；相关 Spec04 Go package gates 仍实际通过。最终 Rust Gate 的单独豁免及原话记录在 Phase 4 结果中；不把任一豁免描述为测试通过。

基线命令在 Phase 0 新增文件前执行：

| 命令 | 基线结果 | 准确原因或后续结果 |
| --- | --- | --- |
| `go test ./...` | FAIL，获准记录并豁免全仓库 Gate | 既有 `internal/codex/scan.go` 编译错误：未使用的 `os`、`sharedusage` imports；`beginScanBuild` 调用缺少 `[]scanSource` 参数；`planUsageFiles` 和 `runUsagePass` 未定义；第 646–647 行 `err` 未定义。没有修 scanner。 |
| `go vet ./...` | FAIL，获准记录并豁免全仓库 Go Gate | 同一基线 `beginScanBuild` 参数不匹配导致编译/vet 被阻断。 |
| `npm --prefix frontend test` | 首次 FAIL | 基线时没有 `frontend/node_modules`，`vitest: command not found`。按锁文件执行 `npm --prefix frontend ci` 后重跑：29 个 test files、235 个 tests PASS。 |
| `npm --prefix frontend run build` | 首次 FAIL | 首次运行找不到 `@testing-library/jest-dom`、`node`、`vitest` 类型；`npm ci` 后重跑，`tsc --noEmit` 与 Vite build PASS，转换 3350 个模块。 |
| `cargo test --all-targets` | Phase 0 未执行 | 当时没有 Rust 基线结果；Phase 4 focused failure 不能归因于 baseline，详见最终回归结果。 |

`npm ci` 使用既有 lockfile，没有修改 `package.json` 或 lockfile。npm 报告的 3 moderate、2 high audit 项没有通过 `audit fix` 改动。全仓库 Go 失败不是新引入问题；也没有把它用于豁免任何 Spec04 专项 Go Gate。

## Phase 0 Oracle provenance and frozen fixtures

- **Rust oracle source:** `src/api/public_v1.rs` and `src/range.rs` are the frozen Rust API and range behavior references. The recorded Usagi Phase 0 starting HEAD is `bca8c3dd9e55b0360d78dce3434690dc7d87a2b6`. Read-only verification at **2026-10-12 02:38:32 CST** found the Usagi checkout still at that HEAD.
- **PeekFlow contract source:** `/Users/hogee/Desktop/PeekFlow/Sources/PeekFlow/Services/Internal/Usagi/UsagiPublicAPIModels.swift` and `/Users/hogee/Desktop/PeekFlow/Tests/PeekFlowTests/UsagiPublicAPITests.swift` were read as the client DTO and test references. The PeekFlow repository HEAD is `44b82f20c4fb494de65bededdacf805d797028ea`, read-only verified by the main thread. The PeekFlow repository was not modified.
- **Six frozen Public fixtures:** `internal/api/testsupport/testdata/public_v1/` contains `info.json`, `revision.json`, `status.json`, `codex_quota.json`, `usage_summary.json`, and `events_revision.sse`.
- **Independent Go decode oracle and API test seams:** `internal/api/testsupport/public_v1_fixtures_test.go` declares standalone DTO shapes and `TestPublicV1FixturesDecodeLikePeekFlowDTOs`; it does not import production API DTOs. `internal/api/testsupport/fakes.go` supplies projection, query, runtime, clock, and revision fakes for HTTP and integration tests. The fixtures and fake seams are evidence/test inputs, not sources for production business types.
- **Recorded verification evidence:** No original Phase 0 command log is available in this execution record, so it does not claim a separately logged Phase 0 test invocation. The later Phase 2 scoped Go gate and Swift `JSONDecoder` run re-verified all six frozen fixtures with the existing PeekFlow model; the same Swift model also decoded 14 actual Go handler response bodies. Commands and results are recorded in [Phase 2 Gate record](Spec04_v0.4_phase2.md) (`GATE-P2-01`).

## Phase 0–3 Gate 状态与记录

| 阶段 | 验收 | 证据记录 |
| --- | --- | --- |
| Phase 0 | PASS；P0-01 全仓库基线 Gate 按上述授权记为豁免失败；P0-02/03 PASS | 本文件的[Phase 0 Oracle provenance 与 frozen fixtures](#phase-0-oracle-provenance-and-frozen-fixtures)；独立 Public DTO fixture decoder 及 Phase 2 fixture/真实 handler Swift decode 复验见 [Phase 2 Gate record](Spec04_v0.4_phase2.md) `GATE-P2-01`。 |
| Phase 1A | PASS | [Phase 1A Gate record](Spec04_v0.4_phase1a.md)：Watcher baselines/fingerprints、30/30/60 intervals、Runtime revisions、动态 Client、错误边界。 |
| Phase 1B | PASS | [Phase 1B Gate record](Spec04_v0.4_phase1b.md)：Internal API、Range、Static、Query/Projection seams 及对应回归。 |
| Phase 1C | PASS | [Phase 1C Gate record](Spec04_v0.4_phase1c.md)：Dashboard contract migration、dynamic filters、ThreadKey、epoch reset、Quota 和 refresh。 |
| Phase 2 | PASS | [Phase 2 Gate record](Spec04_v0.4_phase2.md)：P2-01–09；原 PeekFlow DTO 对 Phase 0 fixtures 和 14 个实际 Go handler response body 解码成功。 |
| Phase 3 backend | PASS | [Phase 3 backend Gate record](Spec04_v0.4_phase3_backend.md)：真实 Query/Projection/Runtime、SSE、Quota、loopback 静态 Dashboard smoke。 |
| Phase 3 frontend | PASS | [Phase 3 frontend Gate record](Spec04_v0.4_phase3_frontend.md)：真实 client/parser integration、epoch reset、SSE fallback、Quota 和 Detail lifecycle。 |
| Phase 4 related Go tests/vet | PASS | [Phase 4 backend Gate record](Spec04_v0.4_phase4_backend.md)：`internal/api`、`internal/api/testsupport`、`internal/magpie`、`internal/query`、`internal/projection` 的最终 `go test` 与 `go vet` 均退出 0。全仓库 Go Gate 仍按用户授权豁免。 |
| Phase 4 frontend tests/build/search | PASS | [Phase 4 frontend Gate record](Spec04_v0.4_phase4_frontend.md)：32 files/164 tests、纠正后目标测试 2 files/7 tests、TypeScript + Vite build、§9.5 禁项搜索通过。 |
| Phase 4 Rust `cargo test --all-targets` | 用户授权豁免（失败已记录） | [Phase 4 backend Gate record](Spec04_v0.4_phase4_backend.md)：全 Gate 中 `compaction_upgrade_readiness` 报失败，之后 Ctrl-C/exit 130；focused 命令自然 exit 101，定位为 parser-11 seeded active epoch 期望 1、实际 2，发生在 manual scanner 请求之前。无 baseline 归因。用户原话：“记录并豁免本次最终 Rust Gate，保持 Spec04 范围，完成本地提交并交我验收”。 |
| Phase 4 workspace / commit | PASS | 本地提交 `feat(api): implement Spec04 HTTP API and Dashboard contracts` 已创建；提交后 `git status --short` 核验为空。用户人工验收仍 pending。 |

Phase 1A 记录中的 `FakeRuntime` latest-only 事件通道问题已在 Phase 1B 解决，非当前开放项。`FakeRuntime` 现使用排队投递，`TestFakeRuntimeSubscriptionPreservesSnapshotAndEveryTransition` 验证初始快照与全部有序状态转换；Phase 1B 专项 package Gate 通过。详见更新后的 [Phase 1A record](Spec04_v0.4_phase1a.md) 和 [Phase 1B record](Spec04_v0.4_phase1b.md)。

## §12 最终验收逐项证据

下表的 YES 只表示对应实现/阶段 Gate 已由主线程验收；最终全仓回归、Windows 实机和用户产品人工验收不由 YES 推断。

| # | §12 要求 | 当前结论与证据 |
| --- | --- | --- |
| 1 | Go HTTP Server 只监听 localhost | YES — [P1B](Spec04_v0.4_phase1b.md)：默认 `127.0.0.1:3210`，地址验证和 server construction 测试。 |
| 2 | Internal handler 只消费 Query/Projection，不实现业务聚合 | YES — [P1B](Spec04_v0.4_phase1b.md)：HTTP handler mapper 与 service 调用测试；Magpie 边界留在 Query/Projection。 |
| 3 | Internal Usage 使用 Magpie/Spec03 新语义 | YES — [P1A](Spec04_v0.4_phase1a.md)、[P1B](Spec04_v0.4_phase1b.md)：`period=all` snapshot、Projection 结果和 HTTP DTO Gate。 |
| 4 | Agent 筛选完全动态 | YES — [P1C](Spec04_v0.4_phase1c.md) `GATE-P1C-02`。 |
| 5 | Model Provider 分组完全动态 | YES — [P1B](Spec04_v0.4_phase1b.md) `GATE-P1B-07`、[P1C](Spec04_v0.4_phase1c.md) `GATE-P1C-03`。 |
| 6 | Model identity 是 `(provider_id, model)`，同名跨 provider 可独立筛选 | YES — [P1B](Spec04_v0.4_phase1b.md) `GATE-P1B-08`、[P1C](Spec04_v0.4_phase1c.md) `GATE-P1C-03`。 |
| 7 | 主路径移除 `ACTIVE_MODEL_GROUPS` / `MODEL_RELEASE_DATES` | YES — [P1C](Spec04_v0.4_phase1c.md) `GATE-P1C-01/03` 与其源码搜索记录。 |
| 8 | Session identity 使用完整 ThreadKey | YES — [P1C](Spec04_v0.4_phase1c.md) `GATE-P1C-04`。 |
| 9 | Session 主表使用 `SessionListItemView` 并保留 inclusive totals/sub count | YES — [P1B](Spec04_v0.4_phase1b.md) `GATE-P1B-09`、[P1C](Spec04_v0.4_phase1c.md) `GATE-P1C-04`。 |
| 10 | Session hierarchy 只使用真实 Subagent/ParentAgent facts | YES — [P1B](Spec04_v0.4_phase1b.md) Projection root/inclusive tests。 |
| 11 | Session Detail 没有独立 timer | YES — [P1C](Spec04_v0.4_phase1c.md) Detail 按需加载；[P3 frontend](Spec04_v0.4_phase3_frontend.md) `GATE-P3-05` 关闭后停止请求。 |
| 12 | Filter Options 没有独立 timer | YES — [P1A](Spec04_v0.4_phase1a.md) 独立 watcher 仅 Usage/Sessions/Quota；[P1B](Spec04_v0.4_phase1b.md) FilterOptions 由 request 读取。 |
| 13 | Usage watcher 独立 30s | YES — [P1A](Spec04_v0.4_phase1a.md) `P1A-06`；[P1B](Spec04_v0.4_phase1b.md) 默认值与独立 interval。 |
| 14 | Sessions watcher 独立 30s | YES — [P1A](Spec04_v0.4_phase1a.md) `P1A-06/09`；[P1B](Spec04_v0.4_phase1b.md) 默认值与单次 refresh seam。 |
| 15 | Quota watcher 独立 60s | YES — [P1A](Spec04_v0.4_phase1a.md) `P1A-04/06`；[P1B](Spec04_v0.4_phase1b.md) 默认值。 |
| 16 | Background Quota 使用 `asked=false` | YES — [P1A](Spec04_v0.4_phase1a.md) `P1A-05`；[P3 backend](Spec04_v0.4_phase3_backend.md) cached refresh integration。 |
| 17 | Manual Quota refresh 使用 `asked=true` | YES — [P1A](Spec04_v0.4_phase1a.md) `P1A-05`；[P3 backend](Spec04_v0.4_phase3_backend.md) manual refresh integration。 |
| 18 | Quota 变化不增加 `data_revision` | YES — [P1A](Spec04_v0.4_phase1a.md) `P1A-04`；[P3 backend](Spec04_v0.4_phase3_backend.md) `P3-04`。 |
| 19 | SSE 首包立即包含当前 revision | YES — [P3 backend](Spec04_v0.4_phase3_backend.md) `P3-01/02`。 |
| 20 | Dashboard 保留 60s authoritative revision fallback | YES — [P3 frontend](Spec04_v0.4_phase3_frontend.md) `GATE-P3-03`。 |
| 21 | `epoch_reset` 使 Summary/Filter/Status/Session/Detail 接受新低 revision | YES — [P1C](Spec04_v0.4_phase1c.md) `GATE-P1C-06/09/10`；[P3 frontend](Spec04_v0.4_phase3_frontend.md) 真实 parser/client 集成。 |
| 22 | Public API v1 六个 path 不变 | YES — [Phase 2](Spec04_v0.4_phase2.md) `GATE-P2-02`。 |
| 23 | Public/Internal DTO 分离 | YES — [Phase 2](Spec04_v0.4_phase2.md) `GATE-P2-04`。 |
| 24 | PeekFlow 现有 DTO 不改即可 decode | YES — [Phase 2](Spec04_v0.4_phase2.md) `GATE-P2-01`：冻结 fixtures 和 14 个真实 Go handler bodies 均由现有 Swift DTO 解码。 |
| 25 | Public `/status` 只输出 legacy enum 域 | YES — [Phase 2](Spec04_v0.4_phase2.md) `GATE-P2-08`。 |
| 26 | 每次 non-ready→ready 恰好增加 status/data revision，starting 503 后自动恢复 | YES — [P1A](Spec04_v0.4_phase1a.md) `P1A-07/10`；[P3 backend](Spec04_v0.4_phase3_backend.md) 与 [P3 frontend](Spec04_v0.4_phase3_frontend.md) runtime/reload integration。 |
| 27 | Dashboard 无 `/api/refresh` 和 scanner tracking；刷新按钮只做 GET reload | YES — [P1C](Spec04_v0.4_phase1c.md) `GATE-P1C-07`。 |
| 28 | Quota cache 是 `projection.QuotaView`，后台和手动都经 `projection.Quotas` | YES — [P1A](Spec04_v0.4_phase1a.md) snapshot/watchers；[P3 backend](Spec04_v0.4_phase3_backend.md) cached/manual HTTP integration。 |
| 29 | ModelOption 来自 `By["modelAt"] + Providers`，同名跨 provider 不合并 | YES — [P1B](Spec04_v0.4_phase1b.md) `GATE-P1B-07/08`。 |
| 30 | Public empty summary/cost completeness/session health 兼容 §4.8.6 | YES — [Phase 2](Spec04_v0.4_phase2.md) `GATE-P2-06/09`。 |
| 31 | SummaryView 的 root facts 来自同一次 threadProjection，Public handler 不二次查 Sessions | YES — [P1B](Spec04_v0.4_phase1b.md) `GATE-P1B-10`；[Phase 2](Spec04_v0.4_phase2.md) `GATE-P2-09`。 |
| 32 | Static SPA fallback 工作 | YES — [P1B](Spec04_v0.4_phase1b.md) `GATE-P1B-05`；[P3 backend](Spec04_v0.4_phase3_backend.md) loopback smoke。 |
| 33 | `/api/*` 不会被 SPA fallback 吞掉 | YES — [P1B](Spec04_v0.4_phase1b.md) `GATE-P1B-04`；[P3 backend](Spec04_v0.4_phase3_backend.md) 404 smoke。 |
| 34 | Skills 未回到主产品 fetch chain | YES — [P1C](Spec04_v0.4_phase1c.md) migration scope；主 Dashboard fetch tests 与源码搜索记录。 |
| 35 | 未新增 SQLite/Schema | YES — Phase 0–3 所有阶段记录的文件范围没有 database/schema 迁移；本 Phase 4 只改文档。 |
| 36 | 未改 Windows tray/native process bootstrap | YES — Phase 0–3 未改 native bootstrap/启动入口；Phase 1C 的 TrayPanel UI API migration 不属于该启动链。实际 production 启动切换留给 Spec05。 |
| 37 | Go/Frontend/Rust 回归测试全部通过 | **未全部通过；豁免已记录。** 全仓库 Go Gate 按既有用户授权豁免，相关 Go 包与前端机器 Gate 通过；Rust `compaction_upgrade_readiness` 失败，用户明确授权豁免本次最终 Rust Gate。不得表述为所有测试全绿。证据：[Phase 4 backend](Spec04_v0.4_phase4_backend.md)、[Phase 4 frontend](Spec04_v0.4_phase4_frontend.md)。 |
| 38 | 工作区干净 | YES — 本地提交 `feat(api): implement Spec04 HTTP API and Dashboard contracts` 后，`git status --short` 核验为空；本次无 push。用户人工验收仍 pending。 |

## §6.8 最终回归当前结果

以下状态来自 A/C 的 Phase 4 实际命令记录；当前没有待填的 Go/frontend 报告。用户已授权记录并豁免最终 Rust Gate；用户人工验收仍 pending：

| 合同命令/检查 | 状态 |
| --- | --- |
| `go test ./...` | 全仓库 Go Gate 按授权豁免；不得修 `internal/codex/scan.go`。相关 Go packages Gate 通过，见 [Phase 4 backend](Spec04_v0.4_phase4_backend.md)。 |
| `go vet ./...` | 全仓库 Go Gate 按授权豁免；相关 Go packages vet 通过，见 [Phase 4 backend](Spec04_v0.4_phase4_backend.md)。 |
| `npm --prefix frontend test` | PASS：32 files/164 tests，见 [Phase 4 frontend](Spec04_v0.4_phase4_frontend.md)。 |
| 纠正前端测试类型后的目标回归 | PASS：2 files/7 tests；类型修正后没有重跑整套前端测试，见 [Phase 4 frontend](Spec04_v0.4_phase4_frontend.md)。 |
| `npm --prefix frontend run build` | PASS：`tsc --noEmit` 与 Vite 3347 modules，见 [Phase 4 frontend](Spec04_v0.4_phase4_frontend.md)。 |
| §9.5 禁项搜索 | PASS；历史结构 fixture 保留六处 `estimated_cost`，应用代码无禁项命中，见 [Phase 4 frontend](Spec04_v0.4_phase4_frontend.md)。 |
| `CARGO_BUILD_JOBS=2 cargo test --all-targets` | 失败后中断：library target 505 passed/2 ignored，`src/main.rs` 3 passed；integration target 的 `compaction_upgrade_readiness` 失败，随后 Ctrl-C/exit 130。用户已授权记录并豁免这次最终 Rust Gate；该失败不是 PASS。 |
| `CARGO_BUILD_JOBS=2 cargo test --test codex_compaction_integration compaction_upgrade_readiness -- --exact --nocapture` | 自然 exit 101：`tests/codex_compaction_integration.rs:1655` 的 `shadow build does not replace the seeded parser-11 active epoch` 断言，实际 2、期望 1；发生在 parser-11 历史写入后、manual scanner 请求前。该路径不调用 Go/frontend；无 baseline 归因。用户豁免原话及诊断证据见 [Phase 4 backend](Spec04_v0.4_phase4_backend.md)。 |
| `git status --short` | PASS：本地提交 `feat(api): implement Spec04 HTTP API and Dashboard contracts` 后核验无输出；无 push。 |

## 验收边界与待办

### 当前代码与契约验收（已完成）

Phase Gate、实际 Go handler response/PeekFlow DTO 解码和 §12 的代码证据已记录，Phase 0–3 已主线程验收。这些证据覆盖当前代码与 HTTP 合同；它们不表示 packaged Dashboard 或 PeekFlow 已连接到该 Go server，仓库还没有正式产品 Go bootstrap。

### 当前可立即执行的 Windows 检查

- Windows 主机在 `TZ` 环境变量未设置时，从仓库根目录运行既有测试 `go test -count=1 ./internal/api -run '^TestSystemTimezoneUsesOSDefaultWhenTZIsUnset$'`。测试本身会 unset `TZ` 并检查系统返回有效 IANA timezone。macOS cross-compile 不等同 WinRT 实机验证。

### Spec05 生产接线后再进行真实产品 UI 验收（当前 pending）

以下项目须等正式 app bootstrap/packaging 将 Go server 接入产品后再执行；目前没有该接线，不可用当前打包产品 UI 声称验收通过：

- 用真实 Usagi Dashboard 与 PeekFlow app 连接当前 Public/Internal API，确认现有 PeekFlow DTO 无需修改并验证实际服务响应。
- 在产品启动中观察 Magpie runtime 从 starting 到 ready 的自动恢复；重启 Go/Magpie 后观察 epoch reset，并检查打开的 Summary、Session Table 和 Session Detail 能重新拉取。
- 在真实 UI 检查 Quota 后台缓存读取、60 秒更新与手动 refresh 行为。
- 用真实 Session 数据检查主 session 与 subagent 的 inclusive totals、Detail ThreadKey 和 child 内容。
- 检查动态 Agent/Provider 选项；若同一 model 出现在多个 provider，分别选择并确认筛选结果独立。
以上生产 UI 验收仍 pending，且不表示已经切换正式生产启动链、完成分发 packaging 或完成最终 cutover；这些责任属于 Spec05。
