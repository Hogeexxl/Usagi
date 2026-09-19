# Usagi Spec02 测试代码布局 v0.1

## 1. 本轮原则

- 只处理 Spec02 此前标记为“部分覆盖”的条目：T-S02-014、T-S02-018、T-S02-019。
- 已经实际 `PASS` 的旧测试不重复执行；能复用 Spec01 已有 integration test 的地方直接复用，不为 Spec02 再复制一份等价测试。
- 通过项目公开 API 驱动 `Ledger`、SQLite 文件、`ScanCoordinator`、真实文件系统、进程式 reopen/restart 的测试放到顶层 `tests/`。
- adapter/parser/resolver 的纯算法测试以及依赖 private parser/planner/worker seam 的白盒测试继续留在 `src/**.rs`。
- 不为移动测试而扩大生产 API；不为了让测试变绿而弱化标准断言。

## 2. 本轮新增 / 复用的测试

| 标准条目 | 测试 | 位置 | 类型 | 本轮结果 | 布局决定 |
|---|---|---|---|---|---|
| T-S02-014 | `t_s01_005_safe_fact_reuse_mismatch_matrix` | `tests/spec01_storage_integration.rs` | Integration / 跨 Spec 复用 | 既有 PASS，不重复执行 | 保持在既有 `tests/` 文件，不复制 |
| T-S02-014 | `t_s02_014_present_rollout_missing_fact_blocks_patch_only_commit` | `tests/spec02_metadata_integration.rs` | Integration | PASS | 新增到 `tests/` |
| T-S02-018 | `t_s01_025_schema_and_test_fixtures_enforce_privacy_boundary` | `tests/spec01_storage_integration.rs` | Guard / 跨 Spec 复用 | 既有 PASS，不重复执行 | 保持在既有 `tests/` 文件，不复制 |
| T-S02-018 | `t_s02_018_scanner_does_not_require_access_to_project_files` | `tests/spec02_metadata_integration.rs` | Integration / privacy | PASS | 新增到 `tests/` |
| T-S02-019 | `t_s02_019_reopen_resumes_from_persisted_nonzero_safe_fact` | `tests/spec02_metadata_integration.rs` | Integration / restart | PASS | 新增到 `tests/` |
| T-S02-019 | `t_s02_019_late_foreign_meta_marks_metadata_rebuild` | `tests/spec02_metadata_integration.rs` | Integration / recovery | PASS | 保留在 `tests/`；本次阶段收口直接复用该 regression test 验证生产修复 |

本轮新增的 4 个测试在本次阶段收口中再次以非特权用户执行：**4 PASS / 0 FAIL**。此前由 late foreign meta 用例暴露的生产缺口已经关闭；测试预期保持不变。

## 3. Spec02 现有测试代码的布局判断

| 当前文件 | 主要测试性质 | 是否迁到顶层 `tests/` | 判断 |
|---|---|---|---|
| `src/codex/state_index.rs` | SQLite schema 适配、白名单 SQL、时间规范化 | 否 | adapter white-box/unit tests，紧贴私有列选择与解析 helper 更合适。 |
| `src/codex/session_index.rs` | JSONL streaming、latest-wins、half-line、bounded line | 否 | parser/unit tests，依赖内部 reader 细节；继续内联。 |
| `src/codex/rollout.rs` | ownership 状态机、replay/OwningLive、continuation、malformed 分类 | 否 | 核心状态机白盒测试；大量依赖 private parser seam，迁出会迫使扩大生产 API。 |
| `src/codex/metadata.rs` | 多来源优先级、relationship graph、conflict/provenance | 否 | 纯 resolver 算法 unit tests；继续内联。 |
| `src/storage/source.rs` | `classify_safe_fact` 私有规则、source/checkpoint 内部边界 | 否 | white-box storage tests；公共契约已由 `tests/spec01_storage_integration.rs`/Spec02 integration 补充。 |
| `src/storage/metadata.rs` | metadata commit CAS / rollback / transaction seam | 否 | 依赖私有 transaction helper；继续内联。 |
| `src/scanner/pipeline.rs` | plan/resolve completeness、safe-fact decision | 否 | planner white-box tests，依赖 private `FilePlan`/`ParsedSource` 等类型。 |
| `src/scanner/mod.rs` | MetadataWorker + fixture 的较大集成测试 | 暂不迁 | 形式上接近 integration test，但大量直接调用 private `MetadataWorker::run_round`、`MetadataPipeline`、`ScanReport`。现在迁出会扩大生产 API；公共端到端契约由新 `tests/spec02_metadata_integration.rs` 承担。 |
| `tests/spec01_storage_integration.rs` | Ledger/SQLite 黑盒契约与 guard | 保持 | T-S02-014/018 可直接复用其中已有测试，体现“一项测试可覆盖多个 Spec 标准条目”。 |
| `tests/spec02_metadata_integration.rs` | public `ScanCoordinator` + Ledger + filesystem + reopen/restart | 是 | Spec02 跨模块/真实运行链测试的固定归宿。 |

## 4. late-foreign 回归测试的最终定位

`t_s02_019_late_foreign_meta_marks_metadata_rebuild` 继续保留在 `tests/spec02_metadata_integration.rs`，不复制第二套测试。当前生产链为：

1. parser 在非零续读遇到 foreign `session_meta` 后返回 unstable / `needs_rebuild`；
2. worker 将本轮 scan 标为 `METADATA_CONTINUATION_UNSTABLE`，不提交不稳定 fact/patch；
3. `MetadataWorker::persist_metadata_rebuilds` 收集 `needs_rebuild` 来源，并通过已有 `Ledger::require_checkpoint_rebuild(ConsumerKind::Metadata, ...)` 一次事务把 metadata checkpoint 切为 `offset=0 + rebuild_required`；
4. `src/storage/source.rs::checkpoint_rebuild_isolated_to_requested_consumer` 继续保证 usage checkpoint 不受影响；planner 对 `rebuild_required` 从 offset 0 重建。

该现有 regression test 已真实执行 PASS；没有修改测试预期。

## 5. 后续约束

- Spec02 adapter/parser/resolver 的局部回归继续放源码旁 unit tests。
- 涉及 public coordinator、Ledger、真实 SQLite、真实文件、restart/reopen 的测试统一进入 `tests/spec02_metadata_integration.rs` 或后续更高层 Spec 的 integration test 文件。
- 跨 Spec 已有测试可直接映射复用，不复制等价测试。
- permission/privacy 类测试必须用非特权用户执行；root 会绕过 Unix 文件权限，不能作为有效结果。
