# Usagi Spec04 测试代码布局 v0.1

> 本文记录 Spec04 完成后的测试代码归属。测试范围、优先级和验收唯一以 `Usagi_测试标准_Spec01-06_v0.17.md` 的 Spec04 部分为准；Spec04 正文中的测试章节不作为验收清单。

## 1. 布局原则

严格按测试标准 §10 处理：

- 只通过 public API 即可验证，且跨 filesystem / SQLite / CODEX_HOME / scanner / reopen / aggregate 的真实链路，放顶层 `tests/`。
- 必须访问 private parser、reader、processor、planner、transaction、rebuild/carry seam 的矩阵放对应 `src/` 模块测试中，不为测试扩大生产 public API。
- 小型 private seam 测试可留在现有 `#[cfg(test)] mod tests`；本轮新增的 P2 大型矩阵单独放 `src/storage/usage/tests/spec04_p2.rs` 与 `src/scanner/chunk_reader/tests/spec04_p2.rs`，避免继续让正式 `.rs` 文件膨胀数百行。
- 合成 rollout/state/session-index 数据只由测试 helper 在临时目录生成，不读取真实 `~/.codex`，不把正文 fixture 或“为了过测试的固定结果”写进生产代码。
- S01/S02 已有且能覆盖 S04 后置联动的测试直接复用，不复制第二套。
- P2 压力测试必须使用真实规模和预先定义的资源预算；T-S04-052 实际生成并读取 1 GiB JSONL，不使用缩小版或 `elapsed >= 0` 之类无效断言。

## 2. 顶层 Spec04 integration tests

文件：`tests/spec04_usage_integration.rs`

| 测试函数 | 主要覆盖标准条目 | 结果 | 为什么放 `tests/` |
|---|---|---|---|
| `t_s04_010_026_035_046_047_real_scanner_builds_active_usage_and_dedupes_archive_copy` | T-S04-010/026/035/046/047 | PASS | public scanner + 临时 CODEX_HOME + SQLite + public aggregate；首次 epoch、fixed view、归档副本 canonical 去重、隐私与 unchanged 资源行为。 |
| `t_s04_018_t_s02_020_real_scanner_excludes_parent_replay_and_counts_child_after_owning_live` | T-S04-018、T-S02-020 | PASS | 真实 Subagent rollout/state edge 经 scanner→usage consumer，验证父 replay 排除与 OwningLive 后子 usage。 |
| `t_s04_033_036_037_040_042_missing_source_carries_active_facts_and_reactivates_atomically` | T-S04-033/036/037/040/042 | PASS | 真实 source missing、shadow build、durable carry、activation、reopen/aggregate。 |
| `t_s04_007_008_009_013_014_031_032_043_045_047_incremental_recovery_half_line_and_queries` | T-S04-007/008/009/013/014/031/032/043/045/047 | PASS | 真实 append、recovered、half-line 补全、tail proof 与三类 aggregate。 |
| `t_s04_019_root_unconfirmed_blocks_usage_then_parent_resolution_replays_once` | T-S04-019 | PASS | root 未确认零推进；父关系到达后从证据重读且只计一次。 |
| `t_s04_030_041_buildfrom_multibatch_and_localreplay_over_budget_promotes_to_shadow_build` | T-S04-030/041/051/052 的 candidate-budget 证据 | PASS | 真实 2050 candidates；BuildFrom 多批，LocalReplay 超 2048 candidate 单批预算时零写并转 shadow build。 |
| `t_s04_019_t_s02_020_late_foreign_meta_discards_preceding_usage_and_starts_rebuild` | T-S04-019、T-S02-020 | PASS | nonzero resume 的 late foreign meta 使本 chunk 零部分提交并进入 usage rebuild。 |
| `t_s04_024_long_subagent_replay_prefix_is_ephemeral_until_owning_live` | T-S04-024/052 的 replay 语义证据 | PASS | 5000 条父 replay 前缀只保留固定 classifier state，不形成 durable usage progress；后续从 0 重放。 |

该文件共 **8 tests，8 PASS / 0 FAIL**。

## 3. Private seam 测试归属

这些测试若迁到顶层 `tests/` 会要求公开仅供测试使用的内部 API，因此按 §10 放模块测试代码。

| 位置 | 对应条目 | 主要证据 |
|---|---|---|
| `src/storage/migrations.rs` | T-S04-001 | v1→v2 migration、usage schema/app_meta/checkpoint/Token 约束、失败回滚。 |
| `src/domain.rs` | T-S04-002/003 | parser→canonical algorithm 版本映射、NormalizedTokenUsage/cache-write validation。 |
| `src/codex/usage.rs` | T-S04-003/004/005 | raw token/lifecycle compatibility 与 required field validation；raw 字段经 Adapter 进入 canonical。 |
| `src/scanner/chunk_reader.rs` | T-S04-006/026/031/041 | fixed-view framing、half-line、guard、合法大行与 bounded oversized。 |
| `src/scanner/chunk_reader/tests/spec04_p2.rs` | T-S04-052 | **实际 1 GiB** JSONL、256×4 MiB batch、窗口内存、sampled process RSS、总耗时预算；测试默认 `ignore` 以免日常回归每次写 1 GiB，最终验收显式 `--ignored` 执行。 |
| `src/usage/pipeline.rs` | T-S04-006/017/018/019/023/024/030/031/052 | ownership handoff、resume、exclusive batch、candidate contract 与 storage-ready DTO。 |
| `src/usage/processor.rs` | T-S04-005/007～009/013～017 | normal/recovered/duplicate、chain break、Turn compensation/model/block、synthetic Turn key 与 Option cache-write 传播。 |
| `src/storage/usage.rs` | T-S04-011/012/020/022/023/025/029/031/037/038/040 | atomic commit、occurrence conflict、planner、LocalReplay、root reconcile、persistent carry/partial seed。 |
| `src/storage/usage/tests/spec04_p2.rs` | T-S04-048～051 | planner 冲突优先级；四 carry phase present resume；四 phase × generation/inode/binding/guard replacement；4097-row/phase 三页 carry 每个中间 batch reopen。 |
| `src/storage/source.rs` | T-S01-016～018、T-S04-027/028/032～034/039/040 | source observation + build disposition 同事务、replacement rollback、carry-present active-prefix guard。 |
| `src/storage/metadata.rs` | T-S01-019、T-S04-020/021 | first binding/root + safe facts/checkpoint + active reconcile + build replacement 单 metadata transaction。 |
| `src/usage/rebuild.rs` | T-S04-002/027～030/035/036/039/042 | frozen manifest、bounded progress、parser/identity replacement、保留未受影响 proof、activation CAS。 |
| `src/usage/aggregate.rs` | T-S04-001/043～045/047 | active-epoch-only、UTC range、nullable cache-write、root/subagent、多视图 invariant、overflow、keyset pagination。 |
| `src/scanner/mod.rs` | T-S04-047 | private worker/report 资源基线；不为测试公开 worker。 |

## 4. 复用的前置 Spec 测试

| 标准条目 | 复用证据 | 结果 |
|---|---|---|
| T-S01-016 | `src/storage/source.rs::spec04_source_observation_and_build_replacement_roll_back_together`、`spec04_build_observation_dispositions_are_atomic_and_preserve_required_boundary` | PASS |
| T-S01-017 | source disposition/active-prefix guard + usage planner/carry | PASS |
| T-S01-018 | `src/usage/rebuild.rs::replacement_preserves_unaffected_build_progress_and_old_manifest_members` | PASS |
| T-S01-019 | `src/storage/metadata.rs::spec04_first_binding_reconciles_build_in_same_metadata_transaction` + root reconcile build test | PASS |
| T-S02-020 | 两个顶层 `t_s04_*_t_s02_020_*` scanner→usage tests | PASS |
| T-S01-025（辅助 T-S04-046） | `tests/spec01_storage_integration.rs::t_s01_025_schema_and_test_fixtures_enforce_privacy_boundary` | PASS |

## 5. P2 最终完整测试布局

| ID | 位置与执行方式 | 结果 |
|---|---|---|
| T-S04-048 | `src/storage/usage/tests/spec04_p2.rs::t_s04_048_planner_conflict_priority_matrix` | PASS |
| T-S04-049 | `src/storage/usage/tests/spec04_p2.rs::t_s04_049_carry_four_phase_present_resume_is_exact_and_complete_only_finishes` | PASS |
| T-S04-050 | `src/storage/usage/tests/spec04_p2.rs::t_s04_050_carry_four_phase_mismatch_replaces_only_affected_member` | PASS |
| T-S04-051 | `src/storage/usage/tests/spec04_p2.rs::t_s04_051_durable_carry_pages_resume_after_reopen_and_finalize_only_at_end` + public LocalReplay multibatch test | PASS |
| T-S04-052 | `src/scanner/chunk_reader/tests/spec04_p2.rs::t_s04_052_one_gib_bounded_reader_keeps_batches_and_process_memory_bounded`（显式 `--ignored`）+ T-S04-024/T-S04-030 public integration | PASS |

T-S04-052 的 1 GiB 测试在测试进程中只保存当前 batch 与标量计数。固定断言包括：总输入 `1,073,741,824` bytes、`256` batches、每批 `4,194,304` bytes/1 complete line、reader `peak_buffered_body_bytes <= MAX_BUFFERED_BODY_BYTES`、sampled RSS 增长 `<=128 MiB`、sampled process RSS `<=384 MiB`、fixture+read `<=300 s`。candidate 上限不是用 reader 伪造，而由真实 2050-candidate scanner→usage test 验证；replay 不缓存语义由真实 5000-line Subagent scanner test 验证。

## 6. 环境边界

- 旧 `scanner::discovery::tests::missing_roots_are_complete_and_an_unreadable_root_is_unavailable` 依赖普通用户 chmod 权限语义；当前容器为 root，无法真实制造 unreadable directory。最终 lib 回归只对该既有、非 S04 条目做命令行过滤，不改测试或生产代码。
- 既有 `tests/spec02_metadata_integration.rs::t_s02_019_late_foreign_meta_marks_metadata_rebuild` 在未修改基线亦失败，属于 S03/metadata 既有问题，不借 Spec04 改其语义。

## 7. 布局结论

Spec04 自身 T-S04-001～052 与延期到 S04 的 T-S01-016～019、T-S02-020 均已有与层级匹配的自动化证据。跨模块 public 链路在 `tests/`；small private seam 留模块 inline tests；本轮新增的大型 P2 private tests 拆到 `src/<module>/tests/spec04_p2.rs`。没有为了测试扩大生产 API，也没有用缩小压力、硬编码生产结果或虚假数据库返回代替真实逻辑。
