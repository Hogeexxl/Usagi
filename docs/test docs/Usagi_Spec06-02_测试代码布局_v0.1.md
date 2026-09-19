# Usagi Spec06-02 Session 测试代码布局 v0.1

> 本文只记录 Spec06-02 的测试落点；条目、优先级、Gate 与通过标准唯一取自 `Usagi_测试标准_Spec01-06_v0.17.md` 的 T-S06-016～030、T-FINAL-014。它不是第二份测试标准，也不把未执行的浏览器或压力结果写成 PASS。

## 1. 生产与单元边界

- DTO/runtime 校验、Session formatter、revision transport 与 controller 状态使用前端 public seam 的 Vitest/jsdom 测试；真实 HTTP 分页和 cursor 只在 Rust/Axum integration fixture 中验证。
- Dashboard 与 Session 共享同一个 `RevisionFeed` 实例；Session 不创建第二条 EventSource 或第二个 fallback timer。
- Session usage 展示只消费 API 的 `inclusive_usage`；`self_usage`、`subagent_usage` 仅保留在 DTO，不在前端重算。
- 真实 browser 只使用临时 Axum/CODEX_HOME fixture；不读取真实 `~/.codex`，不通过 route mock 伪造生产 Query API。

## 2. T-S06-016～030 映射

| 条目 | 生产落点 | 自动化证据 | 当前结论 |
| --- | --- | --- | --- |
| T-S06-016 | `frontend/src/data/usagiClient.ts` Session parser 与 `data/types.ts` DTO | `frontend/src/data/usagiClient.test.ts` Session page、duplicate root、estimated_cost/runtime boundary | PASS（单元） |
| T-S06-017 | `frontend/src/dashboard/session/sessionFormat.ts` | `sessionFormat.test.ts` fallback、model list、同日/同年/跨年与 timezone | PASS（单元） |
| T-S06-018 | `frontend/src/data/revisionFeed.ts` | `revisionFeed.test.ts` one EventSource、one fallback timer、monotonic tuple、cleanup；`DashboardPage.test.tsx` StrictMode remount；`useSessionTableController.test.tsx` shared revision error/retry seam | PASS（单元） |
| T-S06-019 | `useSessionTableController.ts` per-range snapshot、Abort/generation、first page | `useSessionTableController.test.tsx` initial 50-row request、range snapshot | PASS（单元） |
| T-S06-020 | Session controller revision subscription | controller test emits newer revision and asserts first-page reload | PASS（单元） |
| T-S06-021 | Session controller cursor append | controller test loads second cursor page and preserves rows | PASS（单元） |
| T-S06-022 | Session controller `STALE_CURSOR`/`INVALID_CURSOR` one-shot recovery | controller test stale page triggers exactly one first-page request; real API stale/restart test in `tests/spec05_api_integration.rs` | PASS（单元/后端） |
| T-S06-023 | Session footer retry/error state | `SessionSection.test.tsx` footer/error callbacks; production client maps fixed error codes | PASS（单元） |
| T-S06-024 | `SessionTable.tsx` nine-column semantic contract | `SessionSection.test.tsx` exact nine headers and inclusive usage rendering | PASS（单元） |
| T-S06-025 | loading/empty/refresh/error/footer UI states | `SessionSection.test.tsx` six skeleton rows, empty state, error/retry, load-more | PASS（单元） |
| T-S06-026 | `index.css` responsive Session table wrapper | `frontend/tests/browser/dashboard.spec.ts` Session table/layout assertions; dev/dist browser gate | PASS（dev/dist 各 9/9） |
| T-S06-027 | table labels, scope, busy/live announcements | `SessionSection.test.tsx` semantic headers/ARIA; browser accessibility assertion | PASS（单元 + dev/dist 各 9/9） |
| T-S06-028 | Axum `/api/usage/sessions` fixed 50 + opaque cursor | `tests/spec05_api_integration.rs::t_s06_028_real_http_session_pagination_revision_and_restart_contract` (51 rows, stale/reopen) | PASS（真实 HTTP） |
| T-S06-029 | Dashboard/Session shared revision feed | `DashboardPage` passes one feed to both controllers; `revisionFeed.test.ts`; `DashboardPage.test.tsx` StrictMode lifecycle; browser page integration | PASS（单元 + dev/dist 各 9/9） |
| T-S06-030 | controller abort/generation/duplicate guard | `frontend/tests/browser/dashboard.spec.ts::T-S06-030` real Axum 50→100→150→200 pagination, scanner revision during load-more, blocked SSE transport, rapid range switch, duplicate-root and bounded page assertions; controller stress/abort tests | PASS（dev/dist 各 9/9） |

## 3. Final mapping boundary

| 条目 | 证据 | 状态 |
| --- | --- | --- |
| T-FINAL-014 | 同一真实 Axum fixture 的 Session 50+1 分页、scanner revision→STALE_CURSOR、server restart→INVALID_CURSOR 与 Dashboard table；对应 browser `T-FINAL-014` test（dev/dist 各 9/9），backend `t_s06_028` 证明 API stale/restart 错误 | PASS（真实 browser UI 在 stale/restart 后自动首屏恢复，旧稳定行保留；无 route mock） |

当前 `npm test -- --run`、`npm run check` 的数量以命令实际输出为准；本文件不保留过期的历史计数。
