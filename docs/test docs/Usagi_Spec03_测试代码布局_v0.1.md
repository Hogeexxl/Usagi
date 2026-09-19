# Usagi Spec03 测试代码布局 v0.1

> 本文记录 Spec03 聚焦复核及阶段收口后的测试文件归属。当前测试范围与执行门唯一以 `Usagi_测试标准_Spec01-06_v0.17.md` 为准；本文保留的历史结果不替代当前执行记录。

## 1. 本轮处理原则

本轮只处理 Spec03 原先未完全收口的测试条目，不重复运行已 PASS 的旧测试，也不为了目录整洁搬动依赖 private seam 的测试。判断标准是：

- 只通过 crate public API 即可验证，并且跨 filesystem / SQLite / Ledger / coordinator / reopen 的真实链路测试，放顶层 `tests/`。
- 直接依赖 private parser、reader、planner、worker、fake store 或 transaction helper 的 unit/in-module integration test，继续留在 `src/` 对应模块。
- 前置 Spec 延期到 S03 执行的测试，保留在其原始 Spec 测试文件并由 S03 复用，不复制测试代码。

## 2. 新增顶层 integration test

文件：`tests/spec03_scanner_integration.rs`

| 测试函数 | 对应标准条目 | 最终执行结果 | 为什么放 `tests/` |
|---|---|---|---|
| `t_s03_009_real_ledger_crash_windows_resume_from_last_committed_offset` | T-S03-009 | PASS | 使用 public Ledger + public coordinator + 真实 SQLite/reopen，覆盖 observation 后、parse 后、commit 后三个持久化 crash window。 |
| `t_s03_016_real_scanner_preserves_child_fact_across_parent_replay_until_owning_live` | T-S03-016 | PASS | 从真实 child rollout、state edge 进入 public scanner，最终检查 SQLite safe fact 的 replay/owning 边界。 |
| `t_s03_017_missing_and_stale_safe_facts_force_real_worker_rebuild_from_zero` | T-S03-017 | PASS | 真实破坏持久化 safe fact，再通过 public coordinator 验证 missing / generation stale / offset stale 的 from-zero rebuild。 |
| `t_s03_019_state_unavailable_never_infers_main_without_explicit_evidence` | T-S03-019 | PASS | 真实缺失 state source，通过 public scanner + SQLite 证明不能制造 Main/root Session。 |

本轮最终新增测试代码为 **4 个 integration tests，4 PASS / 0 FAIL**。

## 3. 复用而不迁移的前置测试

| 现有测试 | 同时支撑的 Spec03 条目 | 处理 |
|---|---|---|
| `tests/spec02_metadata_integration.rs::t_s02_014_present_rollout_missing_fact_blocks_patch_only_commit` | T-S03-019 | 保留在 Spec02 文件；它的标准 ID/契约归属是 S02，只是在 S03 Gate 复用。 |
| `tests/spec02_metadata_integration.rs::t_s02_019_reopen_resumes_from_persisted_nonzero_safe_fact` | T-S03-016 | 保留；已实际 PASS，不重复执行。 |
| `tests/spec02_metadata_integration.rs::t_s02_019_late_foreign_meta_marks_metadata_rebuild` | T-S03-016 | 保留并直接复用；生产修复后已实际 PASS，不复制第二套 late-foreign 测试。 |
| `tests/spec01_storage_integration.rs::t_s01_025_schema_and_test_fixtures_enforce_privacy_boundary` | T-S03-023 | 保留；已实际 PASS。其源码扫描 guard 会在 scanner/storage/codex/usage 新增未审计日志 sink 时失败。 |

## 4. 继续留在 `src/` 的测试

| 位置 | 判断 | 原因 |
|---|---|---|
| `src/scanner/coordinator.rs` | 保持现状 | 大量测试依赖 private `ScanWorker`、`LifecycleStore`、EventLoop、fake store 和内部调度错误注入，不适合顶层 integration crate。 |
| `src/scanner/discovery.rs` | 保持现状 | 直接验证 private discovery/alias/region 枚举实现细节。 |
| `src/scanner/chunk_reader.rs` | 保持现状 | 直接访问 private fixed-view reader、guard、framing/oversized-line seam。 |
| `src/scanner/pipeline.rs` | 保持现状 | `pipeline` 模块不是 public API；计划器/resolve 测试属于内部算法测试。 |
| `src/scanner/mod.rs` | 保持现状 | 现有 worker 大测试直接调用 private `MetadataWorker`、`run_round_with_report` 与内部 `ScanReport` 指标。强行迁到顶层 `tests/` 需要扩大生产 API，仅为测试布局不值得。 |

## 5. Spec03 当前测试结论

Spec03 的测试代码缺口和生产闭环均已收口：原先部分覆盖的 T-S03-009、017、019、023 已关闭；T-S03-016 复用现有 Spec02 late-foreign regression test 完成生产修复验证，没有新增重复测试。

当前 S03 Gate：

- T-S03-001～024：**24 / 24 `✅ 覆盖完整 + PASS`**
- T-S03-025：P2，`⏸ 按计划延期 + 未进行`，按当前项目安排在 Spec06 完成后的最终完整测试执行

阶段收口修复位于 `src/scanner/mod.rs`：`needs_rebuild` 来源通过已有 metadata-only checkpoint rebuild 事务切换为 `offset=0 + rebuild_required`；usage checkpoint 保持独立。现有 `t_s02_019_late_foreign_meta_marks_metadata_rebuild` 未改预期并已真实 PASS。

因此，**Spec03 Gate 已关闭，具备进入后续 Spec 的条件**。
