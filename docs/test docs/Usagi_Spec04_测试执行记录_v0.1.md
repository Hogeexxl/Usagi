# Usagi Spec04 测试执行记录 v0.1

> 执行依据：`Usagi_测试标准_Spec01-05_v0.12.md` 的 Spec04 部分。Spec04 正文中的测试章节不作为本记录的测试验收来源。本记录同时包含 S04 完成门 P0/P1 与本轮最终交付要求提前执行的 T-S04-048～052 P2。

## 1. 执行环境

- 日期：2026-08-08
- Rust：1.97.1 x86_64-unknown-linux-gnu（用户提供离线 toolchain）
- Cargo：1.97.1
- crates：项目 `.cargo/config.toml` 指向用户提供的 `vendor/`，全程 `--offline`
- 运行环境：Linux 容器，当前进程为 root
- 工作副本：`/mnt/data/usagi_spec04_work`

## 2. 验收集合与结果

测试标准 §8 对 S04 完成门的定义保持不变：

- T-S04-001～047（P0/P1）：47 项
- T-S01-016～019（延期到 S04）：4 项
- T-S02-020（延期到 S04）：1 项
- S04 Gate：**52 / 52 PASS**

本轮按最终交付要求另外实际执行：

- T-S04-048～052（P2 / 最终完整测试）：**5 / 5 PASS**

因此：

- Spec04 自身 T-S04-001～052：**52 / 52 PASS**
- 加上延期到 S04 的 S01/S02 五项：**57 / 57 PASS**

## 3. P2 T-S04-048～052 实际执行

### 3.1 T-S04-048 planner 冲突优先级

```bash
cargo test --offline --lib t_s04_048_planner_conflict_priority_matrix
```

结果：`1 passed; 0 failed`。

证据：`src/storage/usage/tests/spec04_p2.rs`。实际覆盖 parser bump+eligible carry、relationship unresolved、offset>raw、offset=raw+unverified、offset<raw+ready 的正式优先级。

### 3.2 T-S04-049 carry 四 phase × present resume

```bash
cargo test --offline --lib \
  t_s04_049_carry_four_phase_present_resume_is_exact_and_complete_only_finishes
```

结果：`1 passed; 0 failed`。

对 occurrences / turns / anomalies / finalize 四个 durable phase 分别制造来源重新 present；observation 必须保留 carry cursor，ResumeCarry 完成 active prefix 后进入 `CompleteOnly`，build occurrence 数与 distinct source offset 完全一致，无重复/漏项。

### 3.3 T-S04-050 carry 四 phase × 失配 replacement

```bash
cargo test --offline --lib \
  t_s04_050_carry_four_phase_mismatch_replaces_only_affected_member
```

结果：`1 passed; 0 failed`。

四 phase 分别覆盖 generation / inode / binding / active-prefix guard 失配；均验证 `Replaced`、受影响 build rows/cursor/orphan 清理、usage checkpoint `rebuild_required/0`，并验证另一个 manifest 成员的 proof/progress 未被重置。

### 3.4 T-S04-051 large LocalReplay / Carried 多批 crash-resume

```bash
cargo test --offline --lib \
  t_s04_051_durable_carry_pages_resume_after_reopen_and_finalize_only_at_end
cargo test --offline --test spec04_usage_integration \
  t_s04_030_041_buildfrom_multibatch_and_localreplay_over_budget_promotes_to_shadow_build
```

第一条结果：`1 passed; 0 failed`；第二条也包含在最终 `8 / 8` Spec04 integration 中并 PASS。

- durable carry 每个 copy phase 使用 4097 rows，强制形成 3 pages（2048 + 2048 + 1）。
- 在每个中间 batch 后关闭/reopen `Ledger`，只允许从持久化 cursor 继续。
- finalize 前 checkpoint 始终 `rebuild_required/0`、manifest 始终 `pending`。
- LocalReplay 使用真实 2050 Token candidates，超过 2048 candidate 单批上限；尝试必须零写入 active epoch 并转 shadow build，随后多批 BuildFrom 完成。

### 3.5 T-S04-052 1 GiB / replay 资源验收

代码位置：`src/scanner/chunk_reader/tests/spec04_p2.rs`。该测试标记 `#[ignore]`，只为避免日常 unit 回归每次创建 1 GiB 文件；最终验收显式执行：

```bash
cargo test --offline --lib \
  t_s04_052_one_gib_bounded_reader_keeps_batches_and_process_memory_bounded \
  -- --ignored
```

最终结果：

```text
1 passed; 0 failed
finished in 17.51s
```

该测试不是缩小 fixture：

- 实际生成并读取 `1,073,741,824` bytes 的合法 JSONL；
- 固定为 `256` 个 batch；
- 每个 batch 恰好 `4,194,304` bytes / 1 complete line；
- 每批直接断言 `peak_buffered_body_bytes <= MAX_BUFFERED_BODY_BYTES`；
- 读取每一批时在完整 line 仍处于 callback 路径内采样当前测试进程 RSS，断言 sampled RSS 增长 `<= 128 MiB`、sampled process RSS `<= 384 MiB`；
- fixture 创建 + 1 GiB 完整读取断言 `<= 300 s`；
- 测试只保存当前 batch 和标量 counter，不保存全部 batch。

T-S04-052 的另外两个维度不使用 reader fixture 伪造：

- `tests/spec04_usage_integration.rs::t_s04_024_long_subagent_replay_prefix_is_ephemeral_until_owning_live`：真实 public scanner 上 5000 条 Subagent parent replay，OwningLive 前 durable usage progress 仍为 0，下一轮从 0 重读后只计 child usage；
- `tests/spec04_usage_integration.rs::t_s04_030_041_buildfrom_multibatch_and_localreplay_over_budget_promotes_to_shadow_build`：真实 2050 candidates，证明 2048 candidate 上限导致多批处理而不缓存整个来源；
- `src/usage/pipeline.rs::exclusive_large_and_oversized_batches_preserve_contract_without_fake_candidates`：特殊大行/oversized 的正式 exclusive-batch candidate 语义。

## 4. 最终回归命令与结果

### 4.1 格式与编译

```bash
cargo fmt --check
cargo check --offline
```

结果：**PASS**。`cargo check` 无 error；有 6 个 non-fatal dead-code warning（保留的内部 enum/helper seam），不影响构建和验收。

### 4.2 library/private seam

```bash
cargo test --offline --lib -- \
  --skip scanner::discovery::tests::missing_roots_are_complete_and_an_unreadable_root_is_unavailable
```

结果：

```text
151 passed; 0 failed; 1 ignored; 1 filtered out
```

`ignored` 即 T-S04-052 1 GiB 测试，已在 §3.5 用 `--ignored` 单独实际执行并 PASS。`filtered out` 是旧 discovery chmod 权限测试，原因见 §6。

### 4.3 Spec04 public integration

```bash
cargo test --offline --test spec04_usage_integration
```

结果：

```text
8 passed; 0 failed
```

全部走真实临时 filesystem / CODEX_HOME / SQLite / public scanner / aggregate，不使用 production fake data 或 hardcoded bypass。

### 4.4 S01 回归与隐私 guard

```bash
cargo test --offline --test spec01_storage_integration
```

结果：

```text
9 passed; 0 failed
```

其中 `t_s01_025_schema_and_test_fixtures_enforce_privacy_boundary` 明确扫描新增测试源码，确保没有 unchecked logging sink；T-S04-052 因此没有在 Rust 测试中打印 rollout 或测试指标。

### 4.5 S03 scanner 回归

```bash
cargo test --offline --test spec03_scanner_integration
```

结果：

```text
4 passed; 0 failed
```

### 4.6 binary health

```bash
cargo test --offline --bin usagi
```

结果：

```text
1 passed; 0 failed
```

### 4.7 patch whitespace/integrity

```bash
git diff --check
```

结果：**PASS**。

## 5. Spec04 实现证据摘要

- Schema/domain/raw adapter：usage migration、Token constraints、cache-write 三态、parser/canonical versioning。
- Processor：normal/recovered/duplicate、chain interruption/reset、Turn compensation、model/block、synthetic Turn key。
- Scanner：metadata 与 usage 共用同一 discovery/fixed view；usage 独立 checkpoint/planner；真实 Subagent replay、late foreign、root unresolved、half-line、archive copy、bounded multibatch。
- Storage：usage event/occurrence/turn/anomaly/state/checkpoint 原子提交；strict resume；LocalReplay；source observation/build 同事务；root reconcile/build replacement。
- Rebuild/carry：shadow epoch、durable manifest、selective replacement、四 phase durable carry cursor、partial seed、present resume、activation CAS、inactive epoch bounded cleanup。
- Aggregate：active epoch、UTC `[start,end)`、Summary/Session/model、cache-write 三态、多层 Subagent、多视图 invariant、overflow/keyset pagination。
- Privacy/resource：usage 正文/rate-limit payload 不落 DB/log/diagnostic；unchanged `usage_bytes_read=0`；append 只读新增范围；1 GiB P2 有显式 batch/memory/time budgets。

详细 ID→代码位置见 `Usagi_Spec04_测试代码布局_v0.1.md` 与更新后的测试标准第 6 章。

## 6. 环境边界与非本轮范围

### 6.1 root 权限下旧 discovery 测试

`scanner::discovery::tests::missing_roots_are_complete_and_an_unreadable_root_is_unavailable` 依赖普通用户 chmod 后目录产生 `PermissionDenied`。当前容器以 root 运行，chmod 无法真实构造该语义，因此最终 library 回归仅在命令行过滤此**既有且非 Spec04**测试。未修改它的断言或生产实现。

### 6.2 既有 T-S02-019

`tests/spec02_metadata_integration.rs::t_s02_019_late_foreign_meta_marks_metadata_rebuild` 在本轮修改前的基线 commit 上也可复现失败；它属于既有 Spec03/metadata 语义，不属于 Spec04 验收。本轮按“不扩大范围”没有借 Spec04 修改它。

## 7. 结论

依据 `Usagi_测试标准_Spec01-05_v0.12.md`：

- **Spec04 S04 Gate：52 / 52 PASS**；
- **Spec04 P2 T-S04-048～052：5 / 5 PASS**；
- **Spec04 自身 T-S04-001～052：52 / 52 PASS**；
- **计入延期到 S04 的 S01/S02 五项后：57 / 57 PASS**。

本轮没有采用 Spec04 正文测试章节替代测试标准，没有为了通过测试增加生产假数据/硬编码分支，也没有为 private 测试扩大 production public API。
