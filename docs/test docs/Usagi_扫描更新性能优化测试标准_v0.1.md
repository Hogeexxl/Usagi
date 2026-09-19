# Usagi 扫描更新性能优化测试标准

> 版本：v0.1  
> 日期：2026-08-13  
> 对应实施方案：`Usagi_扫描更新性能优化实施方案_v0.2.md`  
> 格式基线：仓库现有独立功能测试标准文档

---

## 1. 测试边界与精简原则

本文只验证本轮“Usage 工作清单 + 按 Thread 精确详细计划 + 局部/全局重计划”性能改造，不重新复制 Spec04 全部 Token/Turn/rebuild/carry 功能测试。

测试按独立故障边界合并：

```text
worklist candidate 正确性
exact plan 范围正确性
正常增量 scanner 闭环
fixed discovery view
local/global replan 边界
错误隔离 / reload fatal
真实数据性能
```

本版本正式新增 **8 条自动化/静态测试条目 + 1 个真实数据性能 Gate**。

不要求：

```text
为每一种 PlanAction 单独建立新测试号
为每个 source 数量建立性能测试
为每个 carry phase 复制现有 Spec04 fixture
为 SQLite prepare 次数写依赖具体 rusqlite 内部实现的断言
为制造极低概率 CAS/reload 故障新增生产 failpoint/mock trait/public seam
用 CI 墙钟时间替代确定性工作范围计数
```

已有 Spec04 对以下语义继续作为回归基线：

```text
bounded batch
fixed-view reader
half-line
guard mismatch
LocalReplay
BuildFrom
BeginCarry / ResumeCarry / CompleteOnly
shadow rebuild / activation
Thread group atomic commit
```

---

## 2. Gate 与送测时机

| Gate | 实施阶段 | 正式测试 | 目的 |
|---|---|---|---|
| Gate A | Storage worklist + exact loader + usage facade + report 观察面完成 | T-PERF-001 ～ 003 | 先证明“找谁工作”和“只加载谁”本身正确 |
| Gate B | Scanner 正常增量主路径完成 | T-PERF-004 ～ 006 | 证明无变化、A/B/C 增量、持续写文件固定边界闭环 |
| Gate C | global/local transition、错误分流完成 | T-PERF-007 ～ 008 + 受影响 Spec04 回归 | 证明 rebuild/carry/multi-batch/错误语义未被性能优化破坏 |
| Gate D | 全部实现完成 | Gate A/B/C 全部 + 完整命令 + 真实数据性能 | 最终交付门 |

Gate 内允许先运行定向单测辅助开发；正式 Gate 只在对应施工波合并后执行。

---

## 3. T-PERF-001：stable/no-build 工作清单矩阵

| 字段 | 内容 |
|---|---|
| 优先级 | P0 |
| 执行点 | Gate A |
| 主要落点 | `src/storage/usage.rs` private tests / 独立 storage test 子文件 |

使用同一个 table-driven fixture 覆盖 stable active epoch、无 build 下的必要矩阵。

必须证明：

```text
稳定完整 tail、文件未变化 → 不进入 worklist
稳定 half-line、raw size 未变化 → 不进入 worklist
稳定 half-line 后文件继续增长 → 进入 worklist
新 source → 进入 worklist
同 identity 文件追加 → 进入 worklist
generation/identity replacement → 进入 worklist
checkpoint 缺失/pending/error/rebuild_required → 进入 worklist
state/parser/canonical/root/thread/open-turn proof 不匹配 → 不得被错误判定为 idle
本轮 metadata 已修复 ownership/root 且 source 仍有未处理 Usage → 进入 worklist
ownership/root 仍不可执行 → 不打开正文，不作为可执行 Thread
```

核心断言：

```text
worklist 只排除“可证明稳定 idle”或“当前明确不可执行”的来源
任何不能证明 idle 的可执行 source 都必须进入 candidate
结果按 thread_id/source_file_id 稳定排序且无重复
```

本测试允许异常 fixture 产生 conservative false positive，但禁止 false negative；正常稳定 fixture 必须精确得到空 worklist。

---

## 4. T-PERF-002：build / global control 工作清单矩阵

| 字段 | 内容 |
|---|---|
| 优先级 | P0 |
| 执行点 | Gate A |
| 主要落点 | storage worklist private tests |

单一矩阵覆盖：

```text
active_epoch=0、无 build：返回的 epoch 能让 scanner 先触发 begin rebuild，不要求先详细 plan 全部 source
parser mismatch：先触发 begin/replace build，不要求先详细 plan 全部 source
build pending/blocked member：进入候选
carry_phase != none：进入候选
仍有效 Rebuilt/Carried member：不进入 detailed worklist
source observation 使既有 completion proof 失效：重新进入候选
当前 present source 意外缺失 build membership：不得被静默跳过
无 ownership/root 的 incomplete member 不可被误判为完成；最终 activation 仍由 manifest 阻止
```

不得把“worklist 中没有可执行 Thread”等价成“build complete”。

---

## 5. T-PERF-003：exact detailed plan 只加载请求来源

| 字段 | 内容 |
|---|---|
| 优先级 | P0 |
| 执行点 | Gate A |
| 主要落点 | `src/storage/usage.rs` + `src/usage/ledger.rs` private tests |

Fixture 至少包含：

```text
多个不同 Thread
当前 build 中多个 member
请求 exact source IDs 只包含其中 1～2 个
```

必须断言：

```text
返回 plans 只含调用方明确请求的 source IDs
build 存在时也不会自动追加其他 build member
同一 Thread 多物理 source 可一次 exact load，并保持各自独立 checkpoint
返回 epoch 与 expected epoch 一致
expected epoch 不一致时拒绝使用旧计划
```

并复用/对照现有 `load_source_plan()` action fixture，证明新 exact loader 没有另写一套 PlanAction 规则。

Gate A 通过标准：worklist 和 exact loader 已可独立工作；此时尚不要求 scanner 已切换到新路径。

---

## 6. T-PERF-004：多 Thread 无变化 round 不进入 detailed planner

| 字段 | 内容 |
|---|---|
| 优先级 | P0 |
| 执行点 | Gate B |
| 主要落点 | `src/scanner/mod.rs` private scanner tests |

Fixture：

```text
临时 CODEX_HOME
至少 1 Main + 3 个直接/多层 Subagent
至少 4 个 owning Thread
可增加 archived 物理副本
首次运行直到 active epoch 稳定、build=NULL
```

第二轮不修改任何 rollout。

必须断言：

```text
round 成功
usage_worklist_loads == 1
usage_worklist_candidates == 0
usage_detail_plan_loads == 0
usage_detail_sources_loaded == 0
usage_global_replans == 0
body_open_attempts == 0
usage_bytes_read == 0
usage_events_inserted == 0
usage_db_write_duration_ms == 0
data_revision 不变化
```

这条测试替代 v0.1 的“全局 UsageScanState 只加载 1 次”目标。v0.2 正确目标是：**无变化时连完整 detailed plan 一次都不加载。**

---

## 7. T-PERF-005：A/B/C 三个变化 Thread 只处理工作清单来源

| 字段 | 内容 |
|---|---|
| 优先级 | P0 |
| 执行点 | Gate B |
| 主要落点 | `src/scanner/mod.rs` private scanner tests |

稳定 fixture 至少包含 6 个 owning Thread。

动作：

```text
只向 A、B、C 三个不同 Thread 的 rollout 追加合法、小于单批 budget 的 Usage 数据
D/E/F 以及其他 source 不修改
执行一轮 scanner
```

必须断言：

```text
worklist 只产生 A/B/C 可执行 Thread
usage_worklist_loads == 1
usage_global_replans == 0
usage_detail_source_ids 去重后只包含 A/B/C 对应 source
A/B/C checkpoint 到达各自新完整行边界
D/E/F 及其他 unchanged checkpoint 完全不变
只有 A/B/C 出现 Usage 正文增量读取
新增 Usage event 各计一次，不重复
Session self/inclusive/subagent 聚合仍正确
```

允许实现采用“每次 local mutation 后 exact reload 当前 Thread”的保守规则，因此不把 `usage_detail_plan_loads` 写死为 3；但其加载 source ID 集合不得包含 A/B/C 之外的 unchanged source。

Thread 执行必须串行；本测试不要求并行，也不得为了测试增加并行 hook。

---

## 8. T-PERF-006：持续写 B 仍遵守 fixed discovery boundary

| 字段 | 内容 |
|---|---|
| 优先级 | P0 |
| 执行点 | Gate B |
| 主要落点 | scanner private fixture + 现有 chunk_reader fixed-view test |

场景：

```text
A、B 均进入本轮 worklist
先取得一份已经冻结 B observed_size=S 的固定 Discovery View
在 Usage 使用该固定 view 处理 B 之前，向 B 文件追加完整合法数据，使真实 EOF > S
随后按该固定 view 执行 Usage；A/B 仍按生产逻辑串行处理
```

实现测试时可以直接在 scanner private fixture 中构造/保留固定的 DiscoverySnapshot（或等价 private 输入），再追加 B 后调用 Usage 路径；**不要求把测试动作精确插入生产 A 完成与 B 开始之间**，也不得为此增加生产 hook/failpoint。

必须断言：

```text
本轮 B 最多处理到 S
Discovery 后追加的字节本轮不进入 Usage event/checkpoint
若 S 落在半行中，只提交到 S 之前最后一个完整行边界
下一轮重新 Discovery 后，剩余数据能够继续处理且只计一次
```

同时保留并运行现有 chunk reader：

```text
fixed_view_does_not_expand_for_an_append_after_discovery
```

不得把“轮到 B 时重新 stat 当前 EOF 并继续追读”作为优化。

---

## 9. T-PERF-007：local replan / global replan 与 rebuild/carry/multi-batch

| 字段 | 内容 |
|---|---|
| 优先级 | P0 |
| 执行点 | Gate C |
| 主要落点 | scanner private tests + 既有 Spec04 storage/scanner tests |

使用矩阵验证两类 replan 边界。

### Local

```text
普通 commit_group 成功
BeginCarry
ResumeCarry（prefix proof 正常匹配）
CompleteOnly
一个 source 因 bounded budget 分多批
```

必须证明：

```text
后续需要继续规划时只 exact reload 当前 Thread
不会重新生成 440-source detailed state
local replan 不会额外加载当前 Thread 之外、且不属于当前 worklist 执行范围的无关 Thread
多批 source 只从新的 durable checkpoint 继续，已提交正文区间不被重新解析
```

### Global

```text
epoch 0 begin rebuild
parser mismatch begin/replace build
ResumeCarry prefix mismatch → replace build sources
RebuildRequired
NeedsRebuild / NeedsRebuildStop
commit requires_rebuild
```

必须证明：

```text
所有 global transition 的既有 discovery_complete 等 correctness guard 保持不变
transition 成功后旧 worklist 立即作废
重新执行的是轻量 worklist，不是全局 detailed plan
新的 build pending members 能进入后续执行
旧 A/B/C worklist 不会跨 global transition 继续提交
```

同时运行现有受影响回归，至少覆盖：

```text
首次 epoch 0 → shadow build → activation
parser mismatch replacement
BuildFrom 多批
BeginCarry / ResumeCarry / CompleteOnly
source missing carry
NeedsRebuild / NeedsRebuildStop
half-line / guard mismatch / identity replacement
```

---

## 10. T-PERF-008：普通组错误隔离与必要 reload fatal

| 字段 | 内容 |
|---|---|
| 优先级 | P0 |
| 执行点 | Gate C |
| 主要落点 | scanner private tests + 静态审查 |

### 场景 A：普通组错误

不得假设存在 Usage CAS 注入 seam。

使用仓库现有可达的 private fixture / 可确定普通 Usage 组错误，验证：

```text
失败组不提交错误数据
另一个有效 Thread 仍能继续并提交
first_group_error 保留
行为与优化前错误隔离一致
```

如果现有正式 Spec04 已有等价“一个 Usage Thread 失败、另一个继续”测试，可直接复用，不新增重复测试号。

### 场景 B：post-mutation 必要 reload 失败

不为此新增生产 failpoint/mock trait/public seam。

通过私有 loader 错误映射测试 + 外层控制流静态审查确认：

```text
local/global durable mutation 已成功
且后续仍需要继续规划
所需 exact/worklist reload 失败
→ 立即终止 Usage round
→ 不处理下一 Thread
→ 不使用旧 plan/worklist 提交后续数据
→ report 记录固定错误码
```

如果 cancellation 会立即结束本轮，则不要求为了一个即将丢弃的内存计划做额外 reload；但不得继续使用旧计划。

---

## 11. Gate D：真实数据性能验收

### 11.1 前置条件

```text
release 构建
真实 CODEX_HOME / 当前 MU 数据库
先做 1 次 warm-up，不计成绩
usage_build_epoch IS NULL
测试期间不主动在 Codex 产生新 rollout 数据
不运行 sample / Instruments / debugger / 高频日志
三次测试串行
```

### 11.2 三轮无变化更新

每轮触发 manual refresh，等待 target scan 进入 `completed`。

记录：

```text
scan_id
started_at_ms / finished_at_ms
data_revision 前后
usage_active_epoch / usage_build_epoch
```

### 11.3 真实数据硬门槛

当前约 440 present sources、无 build、无数据变化、同一台机器：

```text
三轮平均扫描时间 <= 1.0 秒
任意单轮 <= 1.5 秒
三轮均 completed
三轮 data_revision 均不变化
```

真实 manual refresh 只从现有外部观察面记录上述墙钟、完成状态、revision 和 epoch/build 状态。`ScanReport` 的内部性能计数不持久化、不加入 API/SSE，因此**不得要求从真实 manual refresh 响应中读取这些计数**。

### 11.4 自动化确定性门槛

Gate D 同时要求 Gate B/C 的 private 自动化测试已经证明：

```text
usage_worklist_loads == 1
usage_worklist_candidates == 0
usage_detail_plan_loads == 0
usage_detail_sources_loaded == 0
usage_global_replans == 0
body_open_attempts == 0
usage_bytes_read == 0
```

这些计数由 `ScanReport` private 测试观察面验证，不要求新增 SQLite/API/SSE/logging 暴露。最终 Gate 必须同时通过“真实数据墙钟门槛”和“private 自动化确定性门槛”；只通过其中一类不算完成。

### 11.5 未达门槛时

如果确定性门槛全部正确但真实三轮仍超时：

```text
停止交付
记录三轮原始时间、present source 数、build 状态
重新 profile 最新 release
提交新的最高耗时调用栈
```

不得在本方案内自行继续：

```text
statement cache
重新设计 schema/index
并行 Thread
融合 metadata/Usage
扩大 batch
```

---

## 12. 建议测试代码落点

| 测试 | 建议落点 |
|---|---|
| T-PERF-001～003 | `src/storage/usage.rs` private tests、`src/storage/usage/tests/usage_incremental_scan.rs`、`src/usage/ledger.rs` private tests |
| T-PERF-004～006 | `src/scanner/mod.rs` 现有 `#[cfg(test)]` private scanner tests；复用 `run_round_with_report()` |
| T-PERF-006 fixed reader | 复用 `src/scanner/chunk_reader.rs` 现有 fixed-view test |
| T-PERF-007 | 现有 Spec04 scanner/storage private/integration tests + 必要的新 scanner matrix |
| T-PERF-008 | scanner private test + 静态审查；禁止新增生产 failpoint |

文件名可以按仓库现有布局微调，但测试 ID、Gate 和语义必须保留。

为了施工并行避免文件冲突：

```text
Storage 实施 Track 负责 src/storage/usage.rs 的 module registration
测试 Track 如需新文件，只创建 src/storage/usage/tests/usage_incremental_scan.rs
Scanner 测试统一由单一 Track 修改 src/scanner/mod.rs
不得两个 Track 同时修改同一个 tests module 文件
```

---

## 13. Gate D 必跑命令

```bash
cargo fmt --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
git diff --check
```

并执行静态检查：

```bash
rg -n "load_scan_state\(&present_ids|load_scan_state\(present_ids" src/scanner/usage_consumer.rs
rg -n "load_usage_work_list|load_usage_scan_state_exact" src/scanner src/usage src/storage
```

第一条不得命中生产旧全量调用。

被明确标记为资源型 `ignored` 的既有 1 GiB 测试不要求为本轮强制解除 ignored；不得新增 skip/ignore 来隐藏本次失败。

---

## 14. 完成判定

1. T-PERF-001 ～ T-PERF-008 全部 PASS，Gate D 真实数据性能通过。
2. 正常稳定无变化轮只做轻量 worklist，0 detailed plan、0 Usage 正文读取。
3. A/B/C 变化时，详细计划和正文读取范围只涉及工作清单来源；未变化 source checkpoint 不受影响。
4. 工作清单不能只依赖 size/mtime；pending/rebuild/carry/ownership 修复等 durable 状态不得漏掉。
5. exact detailed loader 在 build 模式也只加载明确请求 IDs。
6. normal local mutation 后只 replan 当前 Thread；ResumeCarry prefix mismatch 等 global transition 才重新生成轻量 worklist，且所有既有 `discovery_complete` guard 保持不变。
7. fixed discovery size 是本轮硬读取上界，持续工作的 B 不会被本轮追到最新 EOF。
8. multi-batch 保持 bounded 语义；已 durable commit 区间不被无意义重复解析。
9. Thread 固定串行，不新增并行 writer/reader 复杂度。
10. rebuild/carry/activation/half-line/guard/LocalReplay 等受影响 Spec04 回归全部通过。
11. 普通 Thread 组错误仍隔离；必要 post-mutation reload 失败不会继续使用 stale state。
12. 不存在旧全量 Usage consumer fallback、schema/API/frontend/依赖或无关重构。
