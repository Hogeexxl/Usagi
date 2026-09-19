# Usagi Spec05 测试代码布局 v0.1

> 本文记录 Spec05 验收测试的实际落点。测试条目与通过标准唯一取自 `Usagi_测试标准_Spec01-06_v0.17.md` 的 Spec05 部分；功能语义取自 Spec05 正文。本文不新增测试需求。

## 1. 布局原则

严格沿用项目测试标准第 10 章：

- 小型、只依赖模块私有 seam 的测试，留在对应正式模块的 `#[cfg(test)]` 中；
- private-seam 测试已较大或属于 P2/压力矩阵时，拆到 `src/<module>/tests/`，不继续膨胀生产 `.rs`；
- 需要真实 SQLite、临时 CODEX_HOME、真实 scanner、真实 router/HTTP 跨模块闭环的测试放顶层 `tests/`；
- 已由 Spec01/03 的 lifecycle/privacy 测试充分证明的契约直接复用，不复制第二套测试；
- 所有 fixtures 均在临时目录或 helper 中生成，不读取真实 `~/.codex`。

## 2. 模块内小型 private 测试

### `src/range.rs`

- T-S05-001：五个 named range、月/年边界、DST overlap/gap。
- T-S05-003（time 部分）：IANA/tzdb/转换失败映射前置语义。

这些测试需要直接访问本地时间转换器和 TZIF private helper，属于小型算法矩阵，因此留在正式模块的 `#[cfg(test)]`。

### `src/api/query.rs`

- T-S05-004：usage DTO/cache/cost 映射；
- T-S05-005：cursor HMAC/version/restart/range/revision contract；
- T-S05-006：model sort/unknown；
- T-S05-008：status DTO/projection matrix；
- T-S05-019：安全错误映射。

这些测试直接访问 cursor codec、DTO mapper、private error mapper，不应为了测试改成 public API。

### `src/api/live.rs`

- T-S05-009：refresh header、Started/Coalesced、durable ack；
- T-S05-013：coordinator error → 固定 HTTP 安全错误。

测试使用 private `ManualScanRequester` fake 证明“不等待扫描 I/O”这一时序契约，因此留在模块内。

### `src/storage/metadata.rs`

- T-S05-014 / 015：revision watch 仅 post-commit publish、latest-value coalescing、失败事务不发布、无订阅者不影响提交。

必须直接触达 storage transaction/private revision publisher，因此留在 storage 模块测试。

### `src/main.rs` 与 `src/api/tests.rs`

- T-S05-017：固定 `127.0.0.1:3210` listen contract。

这是很小的结构性契约，不需要顶层 integration fixture。

## 3. 大型 private-seam 测试目录

### `src/range/tests/spec05_p2.rs`

- T-S05-002：Pacific/Apia skipped date、Cuba 午夜转换、Lord Howe 半小时 DST、Chatham 45 分钟 offset。

P2 时区矩阵独立成文件，避免 `range.rs` 继续膨胀。

### `src/api/tests/spec05_concurrency.rs`

- T-S05-019：真实第二 SQLite connection 持写锁时 refresh 仅返回安全 busy code；
- T-S05-021：故意阻塞同步 SQLite seam，验证查询已从 Tokio executor 隔离。

测试需要 private router/context 和 test-only DB lock seam，但跨越多个异步时序，因此放 API tests 子模块。

### `src/api/tests/spec05_p2.rs`

- T-S05-016：SSE 慢 receiver/latest-value 合并、512 次 connect/drop 资源预算。

P2 生命周期压力测试显式标记 `#[ignore]`，普通 `cargo test` 不反复执行压力负载；Spec05 验收时使用 `--ignored` 单独实际运行。

## 4. 顶层真实跨模块 integration tests

### `tests/spec05_api_integration.rs`

使用真实临时 CODEX_HOME、真实 state/session index/rollout、真实 SQLite/Ledger、真实 ScanCoordinator 与 Axum router，覆盖：

- T-S05-003～008：HTTP query 参数、summary/session/model/status、snapshot revision/epoch；
- T-S05-009、010、012～015：refresh、target、reopen、SSE/revision；
- T-S05-013：source_changed 在 scanner request 前拒绝；
- T-S05-017、018：loopback/Host/Origin/Sec-Fetch/no-store/API 404/static fallback；
- T-S05-019、020：安全错误与 stable active usage；
- T-S05-021：真实扫描与大量并发 query 只观察完整 snapshot。

这里不使用直接向 usage API 注入返回值来替代生产链。

### `tests/spec05_api_stress.rs`

- T-S05-022：query + rollout append/scan + refresh + SSE 的真实并发 P2 压力测试。

该测试显式 `#[ignore]`，只在 Spec05 P2/最终测试时单独执行；包含 20s workload timeout、Tokio ticker、RSS/thread 增长预算，禁止无意义 timing assertion。

## 5. 直接复用的既有测试

以下契约已经由前置 Spec 的生产 seam 充分自动化，Spec05 不复制：

- `src/scanner/coordinator.rs`：durable follow-up coalescing、Busy retry、startup recovery、commit error linearization（T-S05-009～013）；
- `src/storage/lifecycle.rs`：target snapshot/history、queued→running 原子转换、start_failed 与 stale target（T-S05-010～012）；
- `tests/spec01_storage_integration.rs::t_s01_025_schema_and_test_fixtures_enforce_privacy_boundary`：隐私日志/fixture/真实 HOME guard（T-S05-019）。

## 6. 条目到主要测试文件映射

| 条目 | 主要落点 |
| --- | --- |
| T-S05-001 | `src/range.rs` |
| T-S05-002 | `src/range/tests/spec05_p2.rs` |
| T-S05-003 | `src/range.rs` + `tests/spec05_api_integration.rs` |
| T-S05-004～006 | `src/api/query.rs` + `tests/spec05_api_integration.rs` |
| T-S05-007 | `src/usage/ledger.rs` production snapshot + `tests/spec05_api_integration.rs` |
| T-S05-008 | `src/api/query.rs` + integration |
| T-S05-009～013 | `src/api/live.rs` + integration + reused lifecycle/coordinator tests |
| T-S05-014～015 | `src/storage/metadata.rs` + integration |
| T-S05-016 | `src/api/tests/spec05_p2.rs` |
| T-S05-017～018 | `src/main.rs` / `src/api/tests.rs` + integration |
| T-S05-019 | `src/api/query.rs` + `src/api/tests/spec05_concurrency.rs` + S01 privacy guard |
| T-S05-020 | `tests/spec05_api_integration.rs` |
| T-S05-021 | `src/api/tests/spec05_concurrency.rs` + integration |
| T-S05-022 | `tests/spec05_api_stress.rs` |

## 7. 禁止事项核对

本轮没有为了测试：

- 增加 HTTP handler 内 SQL；
- 给 private cursor/refresh/storage seam 增加 production public API；
- 伪造 usage response 或硬编码 expected API JSON；
- 读取真实 `~/.codex`；
- 放宽 Host/Origin/Sec-Fetch/CORS 安全规则；
- 新增 Spec05 未要求的 DB 表、远程访问、WebSocket、账户、价格或 query cache。
