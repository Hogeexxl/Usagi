# Usagi Spec05 测试执行记录 v0.1

> 验收依据：`Usagi_测试标准_Spec01-05_v0.14.md` 第 7 章（内容基线来自完成 Spec04 后的 v0.12）。Spec05 正文中的测试章节不作为本轮验收清单。

## 1. 执行环境

- Rust：`rustc/cargo 1.97.1`（用户提供工具链）
- 依赖：项目 `vendor/`，全部使用 `cargo --offline`
- OS：Linux x86_64 容器
- 最终全量回归权限：普通非特权用户 `oai`；root 仅用于工具链安装/最初环境诊断，不作为权限语义测试的最终执行身份
- Node/npm 不属于本轮 Spec05 Rust API 验收依赖
- 测试 fixture：临时 CODEX_HOME / 临时 SQLite / 测试生成 rollout；不读取真实 `~/.codex`

## 2. 静态与编译检查

实际执行：

```bash
cargo fmt --check
cargo check --offline
```

结果：均 PASS。`cargo check` 只有项目既有的 6 个 dead-code warning，无编译错误。

额外范围检查：

- Spec04 与 Spec05 的 `src/storage/schema/` 目录 `diff -ru` 无差异：Spec05 未新增 migration/table；
- `src/api.rs`、`src/api/`、`src/range.rs`、`src/random.rs` 中未出现 handler SQL `SELECT/INSERT/UPDATE/DELETE`；同步 SQLite 访问仍由 Ledger/query snapshot 封装；
- 新增 Spec05 源码/测试中无 `TODO`、`FIXME`、`not implemented` 残留；
- 完成 Spec04 后的 `Usagi_测试标准_Spec01-05_v0.12.md` SHA-256 在工作副本中保持不变，本轮另生成 v0.14，不覆盖测试基线。

## 3. Spec05 验收测试实际执行

### 3.1 Range / timezone（T-S05-001～003）

```bash
cargo test --offline --lib 'range::' -- --nocapture
```

结果：**2 passed / 0 failed**。

包括：

- `named_range_matrix_covers_calendar_boundaries_and_transition_rules`
- `t_s05_002_skipped_date_and_rare_tzdb_boundaries_are_deterministic`

T-S05-002 虽是 P2，本轮按完整交付要求实际执行，不延期。

### 3.2 API private seam P0/P1

```bash
cargo test --offline --lib 'api::' -- --nocapture
```

结果：**10 passed / 0 failed / 1 ignored**。ignored 的唯一条目是 T-S05-016 P2，随后单独用 `--ignored` 实际执行并 PASS。

覆盖 query DTO/cursor/status/error、refresh durable ack/error mapping、loopback contract、真实 SQLite busy、Tokio executor 隔离。

### 3.3 真实 HTTP + SQLite + scanner integration

```bash
cargo test --offline --test spec05_api_integration -- --nocapture
```

结果：**5 passed / 0 failed**。

五个跨模块测试覆盖 T-S05-003～015、017～021 的真实 HTTP 路径，包括：

- range/limit/cursor 安全错误；
- summary/sessions/models snapshot、revision、active epoch；
- keyset pagination 与 stale cursor；
- status/refresh/target/reopen；
- SSE initial/revision polling；
- source_changed refresh 在 scanner request 前拒绝；
- Host/Origin/Sec-Fetch/no-store/API 404/static fallback；
- running/shadow rebuild/failed 下 stable usage；
- active epoch 0 空结果；
- 96 个并发 usage query + 真实扫描只观察完整前态/后态。

### 3.4 Main listen contract

```bash
cargo test --offline --bin usagi -- --nocapture
```

结果：**1 passed / 0 failed**。监听地址固定为 `127.0.0.1:3210`。

### 3.5 T-S05-016 P2：SSE 背压/生命周期

```bash
cargo test --offline --lib \
  t_s05_016_sse_slow_receiver_coalesces_and_disconnects_stay_bounded \
  -- --ignored --nocapture
```

结果：**1 passed / 0 failed**；本次阶段收口以普通非特权用户再次执行，约 **0.06s**。

测试负载/预算：

- receiver 不消费期间提交 64 次 start + 64 次 complete，共 **128 次 status revision**；下一次读取只能看到 latest tuple，不排队 128 个事件；
- **512 次 SSE connect/drop**；
- Linux RSS 增长预算：`<= 16 MiB`；
- OS thread 增长预算：`<= 2`；
- 本次实际通过全部预算断言。

### 3.6 T-S05-022 P2：query/scan/refresh/SSE 压力

```bash
cargo test --offline --test spec05_api_stress \
  t_s05_022_query_scan_refresh_sse_stress_is_bounded_and_nonstarving \
  -- --ignored --nocapture
```

结果：**1 passed / 0 failed**；本次阶段收口以普通非特权用户再次执行，约 **1.37s**。

测试负载/预算：

- 4 个 query worker × 120 次请求 = **480 次** revision/status/summary/sessions/models HTTP 查询；
- rollout 从 token total 2 追加到 40，并经真实 `/api/refresh` 驱动 scanner；
- **16 次 SSE 连接/断开**；
- workload timeout：**20s**；总 elapsed budget：**30s**；
- Tokio 10ms ticker 120 次，要求至少 **100 ticks**，防 executor starvation；
- Linux RSS 增长预算：`<= 64 MiB`；
- OS thread 增长预算：`<= 8`；
- 最终 HTTP summary `total_tokens == 40`；
- 本次实际通过全部预算断言。

### 3.7 隐私前置测试复用（T-S05-019）

```bash
cargo test --offline --test spec01_storage_integration \
  t_s01_025_schema_and_test_fixtures_enforce_privacy_boundary -- --nocapture
```

结果：**1 passed / 0 failed**。

Spec05 不复制第二套隐私 guard；复用 S01 对 schema/log/test fixture/真实 HOME fallback 的可失败检查，并由 Spec05 的真实 busy/API 错误测试继续检查 response 不泄漏 SQL、路径、Prompt/正文 sentinel。

## 4. Spec05 条目结论

`Usagi_测试标准_Spec01-05_v0.14.md` 的 Spec05 共 22 条：

- P0/P1（S05 Gate）：**19 / 19 PASS**；
- P2（T-S05-002、016、022）：**3 / 3 PASS**；
- T-S05-001～022 总计：**22 / 22 PASS**；
- FAIL：**0**；
- 未进行：**0**。

## 5. Spec01～05 阶段收口与完整项目回归

### 5.1 T-S02-019 / T-S03-016 生产闭环修复

Spec05 完整交付后，本地审查确认仍有一个真实功能性失败：

```text
tests/spec02_metadata_integration.rs::
t_s02_019_late_foreign_meta_marks_metadata_rebuild
```

根因是 parser 在 nonzero resume 遇到 late foreign `session_meta` 后已经正确返回 `needs_rebuild`，worker 也记录 `METADATA_CONTINUATION_UNSTABLE`，但 scanner 没有把 metadata checkpoint 持久化为 `offset=0 + rebuild_required`。

本次只修改 `src/scanner/mod.rs` 的相关生产路径：

- `MetadataWorker::parse_sources` 保持原 parser/diagnostic 行为；
- 新增 `MetadataWorker::persist_metadata_rebuilds`，从本轮 `ParsedSource` 中收集 `needs_rebuild` 的 source IDs；
- 通过已有 `Ledger::require_checkpoint_rebuild(CheckpointRebuildCommand { consumer_kind: Metadata, ... })` 一次 Immediate transaction 持久化；
- 只重置 metadata consumer，usage checkpoint 不参与该事务；
- 不稳定 fact/patch 仍不会进入 metadata commit；
- planner 对 `rebuild_required` 的既有规则保证下一轮从 offset 0 Rebuild。

没有修改现有 regression test 的断言，也没有另写重复测试。修复后：

```bash
cargo test --offline --test spec02_metadata_integration \
  t_s02_019_late_foreign_meta_marks_metadata_rebuild -- --exact
```

结果：**1 passed / 0 failed**。

### 5.2 无过滤、无 `--skip` 的完整 `cargo test --offline`

当前容器 root 身份无法用 `chmod` 模拟普通用户的 unreadable-directory 语义，因此 root 下运行会让既有 discovery 权限测试产生环境型失败。最终验收没有过滤该测试、没有修改断言，而是对**同一份代码、同一 Rust 1.97.1、同一 vendor**使用普通非特权用户 `oai` 执行：

```bash
cargo fmt --check
cargo check --offline
cargo test --offline
```

结果：

- `cargo fmt --check`：**PASS**；
- `cargo check --offline`：**PASS**，仍只有 6 个非阻塞 dead-code warning；
- `cargo test --offline`：**196 passed / 0 failed / 3 ignored / 0 filtered**。

这里没有 `--skip`、没有测试名过滤。3 个 `ignored` 是源码中显式标记的 P2 压力测试，不是失败项；本轮要求确认的 Spec05 T-S05-016 与 T-S05-022 已分别用 `--ignored` 单独执行并 PASS。

分 target：

- lib：**165 PASS / 0 FAIL / 2 ignored**；
- `src/main.rs`：**1 PASS / 0 FAIL**；
- Spec01 integration：**9 PASS / 0 FAIL**；
- Spec02 integration：**4 PASS / 0 FAIL**；
- Spec03 integration：**4 PASS / 0 FAIL**；
- Spec04 integration：**8 PASS / 0 FAIL**；
- Spec05 integration：**5 PASS / 0 FAIL**；
- Spec05 stress：默认 **1 ignored**，随后单独执行 PASS；
- doctest：0。

### 5.3 审查报告要求的专项复核

全部使用普通非特权用户、无测试断言修改：

```bash
cargo test --offline --test spec02_metadata_integration
cargo test --offline --test spec03_scanner_integration
cargo test --offline --test spec04_usage_integration
cargo test --offline --test spec05_api_integration
cargo test --offline --lib \
  t_s05_016_sse_slow_receiver_coalesces_and_disconnects_stay_bounded \
  -- --ignored
cargo test --offline --test spec05_api_stress -- --ignored
```

实际结果：

- Spec02 integration：**4 / 4 PASS**；
- Spec03 integration：**4 / 4 PASS**；
- Spec04 integration：**8 / 8 PASS**；
- Spec05 integration：**5 / 5 PASS**；
- T-S05-016 SSE P2：**1 / 1 PASS**；
- T-S05-022 stress P2：**1 / 1 PASS**。

T-S02-019 与 T-S03-016 因此均更新为真实 PASS，Spec03 Gate 正式关闭。旧记录中曾出现的非零 checkpoint 数字只是修复前不同 fixture 长度下的失败实例，已经不再作为当前状态或验收结果记录。

## 6. 生产范围核对

Spec05 实施保持正文边界：

- HTTP 只监听 loopback；
- 无 wildcard CORS；
- 未加入 WebSocket、远程访问、账户、pricing、query cache；
- `estimated_cost` 恒 null；
- 未新增 DB table/migration；
- usage 查询继续读 stable active epoch；
- handler 不直接写 SQL；同步 SQLite 查询经 `spawn_blocking`；
- client disconnect 时 abort 尚未开始的 blocking query，已开始的短 read transaction 可自然结束；
- cursor 为 process-random HMAC-SHA256 验证，restart 后旧 cursor 失效；
- SSE 使用 Ledger-owned Tokio watch/latest-value coalescing，publish 发生在成功 commit 后。

## 7. 最终结论

按 Spec05 唯一验收清单，**T-S05-001～022 已全部实际覆盖并通过，Spec05 Gate 关闭**。本次后续阶段收口又完成了 T-S02-019/T-S03-016 共用生产修复，并在普通非特权用户下完成无过滤、无 `--skip` 的 `cargo test --offline`：**196 PASS / 0 FAIL / 3 ignored / 0 filtered**。

因此当前 **Spec01～05 各阶段 Gate 均已正式关闭，具备进入 Spec06 的条件**。按当前项目安排，测试标准第 9 章 `T-FINAL-001～012` 统一在 **Spec06 完成后**执行；本轮不实现、不执行，也不将其标记为已完成。
