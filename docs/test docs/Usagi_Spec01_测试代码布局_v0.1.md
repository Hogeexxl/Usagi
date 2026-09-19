# Usagi Spec01 测试代码布局 v0.1

## 1. 本轮原则

- 只处理 Spec01 中此前标记为“部分覆盖”的当前 Gate 条目。
- 已经 PASS 的旧测试不重复执行、不为整理目录而重写。
- 需要跨 `Ledger` 公共 API、SQLite 文件、重开、并发连接或 schema 观察的测试，放到顶层 `tests/`，作为 integration test。
- 依赖模块私有 helper、私有 transaction seam、私有 planner/classifier 的测试继续留在对应 `src/**.rs` 的 `#[cfg(test)] mod tests` 中。
- 不为了“搬测试”扩大生产 API 或增加 test-only public seam。

## 2. 本轮补齐的 Spec01 条目

| 标准条目 | 新增测试 | 位置 | 类型 | 布局决定 |
|---|---|---|---|---|
| T-S01-001 | `t_s01_001_v1_schema_initial_state_pragmas_and_reopen_matrix` | `tests/spec01_storage_integration.rs` | Integration | 独立到 `tests/` |
| T-S01-002 | `t_s01_002_source_identity_database_constraints_matrix` | `tests/spec01_storage_integration.rs` | Integration | 独立到 `tests/` |
| T-S01-005 | `t_s01_005_safe_fact_reuse_mismatch_matrix` | `tests/spec01_storage_integration.rs` | Integration | 独立到 `tests/` |
| T-S01-006 | `t_s01_006_safe_fact_provenance_record_offsets_persist` | `tests/spec01_storage_integration.rs` | Integration | 独立到 `tests/` |
| T-S01-008 | `t_s01_008_batch_metadata_state_is_one_sqlite_snapshot` | `tests/spec01_storage_integration.rs` | Integration | 独立到 `tests/` |
| T-S01-009 | `t_s01_009_metadata_transaction_rollback_survives_reopen` | `tests/spec01_storage_integration.rs` | Integration | 独立到 `tests/` |
| T-S01-011 | `t_s01_011_generation_change_deletes_persisted_safe_fact` | `tests/spec01_storage_integration.rs` | Integration | 独立到 `tests/` |
| T-S01-013 | `t_s01_013_deleting_source_cascades_all_consumer_checkpoints` | `tests/spec01_storage_integration.rs` | Integration | 独立到 `tests/` |
| T-S01-025 | `t_s01_025_schema_and_test_fixtures_enforce_privacy_boundary` | `tests/spec01_storage_integration.rs` | Integration / guard | 独立到 `tests/` |

## 3. 现有 Spec01 相关测试代码的布局判断

| 当前文件 | 主要测试性质 | 是否迁到顶层 `tests/` | 判断 |
|---|---|---|---|
| `src/domain.rs` | 纯 domain validation / invariant | 否 | 典型 unit test，需要紧贴数据模型；继续内联。 |
| `src/storage/migrations.rs` | 私有 migration runner、内部 rollback | 否 | 依赖私有 `migrate` 与 migration 细节；继续内联。 |
| `src/storage/source.rs` | source plan、safe-fact classifier、checkpoint 私有边界 | 否 | 大量依赖模块私有 helper；迁出会迫使扩大生产 API，不值得。 |
| `src/storage/metadata.rs` | transaction pre/postcondition、内部 CAS/rollback | 否 | 属于 storage white-box tests；继续内联。 |
| `src/storage/lifecycle.rs` | scan lifecycle 状态机与内部事务 | 否 | 主要验证模块内部不变量；继续内联。 |
| `src/codex/metadata.rs` | resolver precedence/conflict 算法 | 否 | 纯算法 unit test；继续内联。 |
| `src/scanner/pipeline.rs` | planner / safe-fact decision | 否 | 依赖 pipeline 内部状态与枚举；继续内联。 |
| `src/scanner/mod.rs` | worker + fixture 的较大集成测试 | 暂不迁 | 从规模上像 integration test，但依赖 `pub(crate)` / private worker/report seam。现在迁到顶层 `tests/` 会要求扩大生产可见性；等 scanner 对外公共接口稳定后再评估。 |
| `src/storage/mod.rs` | opener / binding / schema bootstrap | 暂不迁 | 其中少数测试技术上可迁，但体积小且已稳定 PASS；本轮新 `tests/spec01_storage_integration.rs` 已承担外部契约验证，没有必要为目录整洁重复搬动。 |
| `tests/spec01_storage_integration.rs` | 公共 API + SQLite + reopen + concurrency + schema guard | 是 | 本轮新增，作为 Spec01 黑盒/跨模块测试的固定归宿。 |

## 4. 后续约束

Spec01 后续若出现真实缺陷：

- 纯函数/私有状态机回归：加在对应 `src/**.rs` 的 unit tests。
- 跨 `Ledger`、SQLite 文件、重启/重开、多个连接、完整事务链：加到 `tests/spec01_storage_integration.rs`。
- 不再因为原 Spec 测试章节存在某个 bullet 而新增重复测试；只按 `Usagi_测试标准_Spec01-06_v0.17` 执行。
