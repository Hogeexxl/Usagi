# Usagi Spec 04：Token 账本与聚合

> 版本：v0.2  
> 状态：当前契约修订版（Spec07 已完成，Spec08 清理目标）  
> 更新日期：2026-08-09  
> 依赖：`Spec_01_数据模型和数据库骨架_v0.2.md`、`Spec_02_Codex原始数据与元数据适配_v0.2.md`、`Spec_03_增量扫描器_v0.2.md`、`normalizedTokenUsage数据口径.md`、`codex rollout数据口径.md`  
> 当前唯一测试标准：`Usagi_测试标准_Spec01-06_v0.17.md`  
> 当前版本范围：完整 Token 用量；仅美元费用 `estimated_cost` 保持 `null`

---

## 0. 当前实现口径

当前实现已经完成 Spec07 的 Token canonical 改造：

```text
usage parser = v3
canonical algorithm = v3
Adapter 后唯一 Token snapshot = NormalizedTokenUsage
```

当前运行时不得恢复 `TokenVector`、`CacheWriteStatus`、`cache_tokens`、reported/derived total 双字段或旧 API 字段。

Spec04 历史上新增 `0002_usage_ledger.sql`；Spec07 新增 `0003_normalized_token_usage.sql`；Spec08 将新增 `0004_metadata_parent_v2_cleanup.sql`。因此正文中“user_version=2”只描述 Spec04 当时的 migration，不代表 current latest schema。

**Carry 生产语义只有一套实现**：当前目标以 `src/storage/usage.rs` 的持久化 carry/rebuild 路径为准。不得为旧 Spec/旧测试继续保留独立 `src/usage/carry.rs` reference state machine；其有效测试覆盖必须迁移到真实 production path。

## 1. 目标与边界

本 Spec 实现 rollout `token_count` 到标准化 `usage_events` 的完整链路，包括跨扫描恢复、去重、缺失恢复、Turn 补偿、Subagent replay 排除，以及 Dashboard、根 Session、模型三个维度的聚合。

本 Spec 不实现 HTTP、SSE 或界面；Spec 05 只消费本 Spec 的查询 interface。第一版不建立日/月 rollup，不计算美元费用。

---

## 2. 固定口径

- 正常事件只来自去重后的 `last_token_usage`；
- `total_token_usage` 只用于重复判断、缺失恢复、Turn 校验和重启恢复，绝不直接相加；
- `total_tokens = input_tokens + output_tokens`；cached、cache write、reasoning 都是子项；
- Token 原始归属保留在 `thread_id`，Session 聚合使用 confirmed `root_session_id`；
- 只统计 ownership=`Owning` 的记录，排除 Subagent 文件中的父历史；
- metadata 与 usage 使用独立 checkpoint 和 parser version；
- 事件、Turn 状态、异常、usage state 与 usage checkpoint 同事务提交；
- 当前只有 `estimated_cost` 不实现，其他 Token 字段和聚合均为必需能力。

---

## 3. 模块与 interface

`UsageLedger` 是深模块。scanner 不理解累计链、补偿、事件 ID 或 SQL；Spec 05 不理解 rollout 或 checkpoint。

```text
UsageLedger::load_scan_state(source_file_ids, parser_version)
  -> UsageScanState

UsageLedger::process_chunk(context, complete_lines)
  -> UsageChunkResult

UsageLedger::commit(batch)
  -> UsageCommitOutcome

UsageLedger::begin_rebuild(parser_version, reason)
  -> UsageRebuildState

UsageLedger::activate_rebuild(epoch, completion_proof)
  -> UsageActivationOutcome

UsageLedger::summary(range)
  -> UsageSummary

UsageLedger::sessions(range, page)
  -> SessionUsagePage

UsageLedger::models(range)
  -> ModelUsageRows
```

scanner 仍负责 Spec 03 的 discovery、identity、fixed `observed_size`、guard、完整行和单轮互斥。`UsageLedger` 只接收这些检查已经通过的固定视图。

### 3.1 `UsageChunkContext`

```text
scan_id
ledger_epoch
source_file_id
file_generation
usage_parser_version
start_offset
observed_size
confirmed_owning_thread_id
confirmed_root_session_id
expected_previous_thread_id
resume_state
existing_usage_state?
open_turn?
```

`confirmed_root_session_id` 必须来自 Spec 02 已提交的完整父链。root 或 owning ID 未确认时，不得把 Subagent 当成独立 Session，也不得提交该来源的 usage checkpoint。

### 3.2 `UsageChunkResult`

```text
events[]
occurrences[]
turn_mutations[]
anomalies[]
updated_usage_state
last_complete_offset
source_bytes_consumed
complete_line_count
candidate_count
replayed_prefix_bytes
replayed_prefix_lines
final_continuation = OwningLive { owning_thread_id } | Unstable
needs_rebuild
diagnostics
fixed_observed_raw_size
fixed_view_exhausted
tail_status = unverified | none | half_line
tail_start_offset?
```

所有集合只含结构化数字、ID、时间、offset 和错误码，不含原始 JSON、Prompt、回复或工具正文。`fixed_view_exhausted=false` 时 `tail_status` 必须为 unverified、`tail_start_offset=NULL`；只有 reader 确认已经读到本轮 fixed raw view 末尾时才返回 true，此时 tail 必须为 none 或 half_line，half_line 必须携带其起点。

### 3.3 有界分批

`process_chunk` 的普通单批硬上限固定为：输入完整行累计最多 4 MiB、最多 4096 行、最多 2048 个 Token candidate，任一先到即在上一条完整行边界结束；这些常量编译进代码并由测试锁定。下一条完整行若会越界则留给下一批，绝不拆行。两个保证进展的例外固定为：

- 单条合法完整行可独占一批，大小可到 Spec 03 的 8 MiB `max_line_bytes`；独占批只含这一行，仍受 2048 candidates 上限；
- 超过 8 MiB 的完整行由 Spec 03 reader 流式丢弃原文，只把一个 bounded `OVERSIZED_COMPLETE_LINE` 诊断交给 usage；该独占批允许 `source_bytes_consumed>8 MiB`，但 `complete_line_count=1`、`candidate_count=0`，且内存中不持有原文。

因此不存在 4–8 MiB 合法行永久留给下一批，也不存在 oversized 行扩张内存。

`UsageChunkResult`、`UsageCommitBatch` 及一次 SQLite 事务的 adapter payload 除上述两种独占行外都不得超过同一组上限；scanner 每得到一批就立即 commit，然后从已提交 checkpoint/state 继续，不能为整个 source 或整个 Thread 累积数组。每批原子写 event、occurrence、Turn、anomaly、usage state 和 checkpoint；open Turn、accounted、active model 与累计 baseline 通过持久化 state 跨批延续。中间批次即使成功也不完成 build manifest；只有 reader 已验证 raw tail、checkpoint 到达同 generation 的 `required_through_offset` 且全部 proof 成立，最终批次才可原子标 `rebuilt`。进程在任意批次间退出时，从最后 committed offset 恢复。

offset 0 可能先遇到任意长的 Subagent replay 前缀。scanner 用同样的 4 MiB/4096 行窗口流式运行 ownership classifier：窗口末仍未到 `OwningLive` 时丢弃该窗口的原始行与空输出，只在内存保留固定大小 classifier state、累计 `replayed_prefix_bytes/lines` 和最新完整行 offset，不提交 checkpoint；继续读下一窗口。首次到达 owning `session_meta` 后，构造一个 ownership-establish batch，`replayed_prefix_*` 精确记录从 start offset 到 owning 边界的已忽略前缀，candidate/occurrence/Turn mutation 均为 0，并把 OwningLive state/checkpoint 原子提交到该边界；随后正常分批。崩溃前未建立 OwningLive 时没有持久化进度，重启从 0 重读。该 discovery 可读取任意长前缀，但任一时刻只保留一个窗口和固定 classifier state。

计数等式固定为：`source_bytes_consumed = last_complete_offset - batch_start_offset`；`adapter_input_bytes = source_bytes_consumed - replayed_prefix_bytes`；`adapter_line_count = complete_line_count - replayed_prefix_lines`；`candidate_count = occurrences.len = candidate proposals 数`；两项差值必须非负。普通批 `replayed_prefix_*=0`。ownership-establish batch 可以有任意累计 replay prefix，但 `adapter_input_bytes/adapter_line_count/candidate_count` 仍受普通预算且所有事件数组为空；前缀由多个有界非提交窗口读取，不进入 SQLite 事务数组。`UsageCommitBatch` 原样携带五个原始计数和 batch start/last offset，storage 在写事务前重算两个 adapter 计数并验证等式、普通/独占行预算与 candidate/occurrence 等式；不满足即拒绝且不推进 checkpoint。

---

## 4. Migration 与表结构

Spec 04 历史上新增 migration `0002_usage_ledger.sql`，该单层 migration 完成后 `user_version=2`。migration 整体事务执行，失败时 schema 和 `user_version` 都保持旧值。**current fresh-open latest 由后续 0003/0004 继续升级到 v4；不得把本段的 v2 当作当前运行时 latest。**

新增六张表：

```text
usage_events
usage_event_occurrences
turns
ingest_anomalies
usage_source_states
usage_build_sources
```

`app_meta` 是固定 `id=1` 的单行表。`0002` 必须通过 SQLite table-rebuild（创建完整新表、复制原单行、删除旧表、rename）增加以下列，不能把它实现成键值表：

```sql
usage_active_epoch INTEGER NOT NULL DEFAULT 0 CHECK (usage_active_epoch >= 0),
usage_build_epoch INTEGER CHECK (usage_build_epoch >= 1),
usage_parser_version INTEGER NOT NULL DEFAULT 0 CHECK (usage_parser_version >= 0),
usage_build_parser_version INTEGER CHECK (usage_build_parser_version >= 0),
CHECK ((usage_build_epoch IS NULL) = (usage_build_parser_version IS NULL)),
CHECK (usage_build_epoch IS NULL
       OR usage_build_epoch = usage_active_epoch + 1)
```

复制 v1 单行时四列确定初始化为 `0, NULL, 0, NULL`；原有全部列和值原样复制，仍保留 `CHECK(id=1)` 与“恰好一行”的初始化约束。新库执行 `0001` 后再执行同一 `0002`，得到相同结果。复制前若 `app_meta` 不是唯一 `id=1` 行则 migration 失败并整体回滚。`user_version=2` 只在六张表、索引、新 `app_meta` 与初始化全部成功后设置。

全篇统一定义：

```text
working_epoch = usage_build_epoch ?? usage_active_epoch
working_parser_version = usage_build_parser_version ?? usage_parser_version
```

每个来源唯一的 usage checkpoint 只对应 `working_epoch`。加载、非零续读和提交只能匹配该 epoch、该 parser version 的 `usage_source_states`；旧 active/inactive epoch 的 state 仅供旧账本查询或清理，不参与 checkpoint 等式。

build 中没有新增 checkpoint 枚举：本文所有“尚未建立 working state、必须从 0 重建”的标记精确映射为 Spec 01 现有 `processing_status='rebuild_required'`。一旦某批原子写入匹配的 working state/checkpoint，checkpoint 就是 `ready`；manifest 仍可为 pending/blocked。manifest 未完成本身不禁止续读，能否非零续读只由 `ready` checkpoint 与 working state/guard 的严格等式决定。

第一版不创建 `daily_usage_rollups`、`session_daily_rollups` 或 `model_daily_rollups`。

### 4.1 Token 向量

下列表中的 canonical Token 字段都使用 SQLite `INTEGER`，Rust 使用经过检查的 `i64`：

```text
input_tokens
cached_tokens
cache_write_tokens?         # 无法确定时为 null
uncached_input_tokens?      # cache-write 未知时为 null（派生值）
output_tokens
reasoning_tokens
other_output_tokens         # 派生值
total_tokens
```

有效向量必须满足：

```text
所有已知字段 >= 0
cached_tokens <= input_tokens
cached_tokens + cache_write_tokens <= input_tokens  # cache write 已知时
reasoning_tokens <= output_tokens
total_tokens = input_tokens + output_tokens
```

账本中的 `total_tokens` 不信任原始 total，始终保存 `input_tokens + output_tokens`。

### 4.2 `usage_events`

| 字段 | 约束 | 含义 |
|---|---|---|
| `ledger_epoch` | PK 部分，正数 | 所属账本代次 |
| `event_id` | PK 部分，TEXT | 可重复计算的逻辑事件 ID |
| `event_kind` | 枚举 | `normal` / `recovered` / `turn_compensation` |
| `occurred_at_ms` | NOT NULL | 有效事件时间，UTC epoch ms |
| `thread_id` | FK，NOT NULL | 原始 owning Thread |
| `root_session_id` | FK，NOT NULL | confirmed 根 Session |
| `turn_key` | 可空 | 原始或合成 Turn key |
| `model` | NOT NULL | 事件发生时模型；无法确认用 `unknown` |
| `input_tokens` | 非负 | 输入 Token |
| `cached_tokens` | 非负 | 缓存读取输入 |
| `cache_write_tokens` | 可空、非负 | 缓存写入；无法确定为 null |
| `output_tokens` | 非负 | 输出 Token |
| `reasoning_tokens` | 非负 | 推理输出子项 |
| `total_tokens` | 非负 | 固定为 input + output |
| `quality_status` | 枚举 | `complete` / `partial` |
| `source_file_id` | FK，NOT NULL | 首次写入该逻辑事件的来源，仅作审计 provenance |
| `file_generation` | 正数 | 来源内容代次 |
| `source_start_offset` | 非负 | 原始完整行起点；补偿为结束记录起点 |
| `source_end_offset` | 正数 | 原始完整行终点 |
| `created_at_ms` | NOT NULL | MU 写入时间 |

约束：

- 主键 `(ledger_epoch, event_id)` 阻止普通/归档副本和重读重复累计；
- 来源位置去重、来源贡献枚举与 carry 只使用 `usage_event_occurrences`；不得用 `usage_events` 的首次 provenance 代替 occurrence；
- `root_session_id` 不能为 null；未确认关系必须等待，不得伪造 root；
- `cache_write_tokens` 为 `null` 时事件质量为 `partial`，明确 `0` 仍为已知 0；
- `quality_status=partial` 只允许表示 cache-write 未知，不能掩盖 required Token 字段错误；
- 不保存价格或费用列，查询结果中的 `estimated_cost` 固定为 null。

索引：

```sql
(ledger_epoch, occurred_at_ms)
(ledger_epoch, thread_id, occurred_at_ms)
(ledger_epoch, root_session_id, occurred_at_ms)
(ledger_epoch, model, occurred_at_ms)
```

### 4.3 `usage_event_occurrences`

该表记录“某个来源位置产生了哪个 canonical event”。每个有效 normal、recovered 或 turn-compensation candidate 都必须写一行；即使 `usage_events.event_id` 已由普通/归档副本写入，也不能省略本来源 occurrence。

| 字段 | 约束 | 含义 |
|---|---|---|
| `ledger_epoch` | PK 部分，正数 | 所属账本代次 |
| `source_file_id` | PK 部分，FK | candidate 所在来源 |
| `file_generation` | PK 部分，正数 | 来源内容代次 |
| `source_start_offset` | PK 部分，非负 | candidate 的完整行起点 |
| `source_end_offset` | 正数 | candidate 的完整行终点 |
| `event_id` | NOT NULL | 指向同 epoch canonical event |
| `created_at_ms` | NOT NULL | MU 首次写入 occurrence 的时间 |

主键固定为 `(ledger_epoch, source_file_id, file_generation, source_start_offset)`；外键 `(ledger_epoch,event_id)` 引用 `usage_events` 并延迟到事务提交检查。一个来源位置只能映射一个 canonical event；重试命中主键时必须回读比较 `event_id` 和 `source_end_offset`，任一不同都是 hard conflict。不同来源位置可以映射同一个 `event_id`，这是跨副本去重的正常结果。

normal/recovered 的位置取 token-count 完整行，turn-compensation 的位置取关闭 Turn 的 lifecycle 完整行；步骤 7/9 的互斥决策保证一个来源位置至多产生一个 Token candidate。若未来协议允许一行产生多个 candidate，必须 bump usage parser/canonical algorithm 并升级 occurrence 主键，不能在当前 schema 下静默覆盖。

索引：

```sql
(ledger_epoch, event_id)
(ledger_epoch, source_file_id, file_generation, source_start_offset)
```

`usage_events` 是唯一聚合事实；`usage_event_occurrences` 不参与 Token SUM。仅新增 occurrence 而 canonical event 已存在时不增加 `data_revision`。

### 4.4 `turns`

Turn 是来源级解析状态，不是前端 Session。普通和归档副本可以各有自己的相同 Turn 状态。

主键：

```text
(ledger_epoch, source_file_id, file_generation, turn_key)
```

字段：

```text
thread_id
raw_turn_id?
started_at_ms?
ended_at_ms?
start_offset
end_offset?
status = open | completed | aborted | failed

start_total_*?             # Turn 开始前上一条可信累计快照
last_total_*?              # Turn 内最后一条可信累计快照
accounted_*                # 本来源 Turn 已识别的 normal + recovered + compensation
accounted_cache_write_tokens # 任一 candidate 未知则为 null
accounted_candidate_count

model_state = none | single | mixed
single_model?
unresolved_model_seen      # 见过无法确认的事件模型
compensation_allowed       # 跨重启保留的最终门禁
block_start_missing
block_time_missing
block_reset
block_ownership_gap
block_parser_gap
block_required_invalid
block_model_unresolved
quality_status = complete | partial | conflict
state_through_offset
updated_at_ms
```

每组 `*_total_*` 与 `accounted_*` 都展开为 canonical Token 字段、可空 cache-write 和 snapshot fingerprint；禁止用 JSON 保存快照。`accounted_candidate_count` 为非负整数，用于区分“尚无 candidate 的 `Some(0)`”和“已有 known-zero candidate”。七个 block 字段均为 NOT NULL 0/1，并以 CHECK 保证 `compensation_allowed=1` 当且仅当七项全为 0。它们只允许从 0 变 1，直到 Turn 关闭；重启不能把 `start_snapshot_missing`、时间缺失、reset、ownership/parser gap、required 异常或 unresolved model 恢复成可补偿状态。

`accounted_*` 统计本来源解析得到的逻辑事件候选，即使 canonical `usage_events` 因另一副本已存在而被判定为重复，也必须计入 Turn 校验，防止归档副本错误生成二次补偿。

### 4.5 `usage_source_states`

这是 usage consumer 的重启恢复状态，不能由最终聚合反推。

主键：

```text
(ledger_epoch, source_file_id)
```

字段：

```text
file_generation
device_id
inode
usage_parser_version
canonical_algorithm_version
resolved_through_offset
observed_raw_size
raw_tail_status = unverified | none | half_line
raw_tail_start_offset?
owning_thread_id
root_session_id
continuation_state = owning_live
previous_total_*?
previous_total_offset?
chain_state = continuous | interrupted
chain_block_reason?        # malformed/oversized/total_invalid/ownership_gap/parser_gap
active_turn_key?
active_model?
active_model_offset?
updated_at_ms
```

成功持久化时 `chain_state=continuous` 要求 `chain_block_reason=NULL`；`interrupted` 要求非空原因。reset/time_missing 若 current total 可信，只把原因写入 anomaly/active Turn block，source chain 最终为 continuous/null；不得把 reset 持久化为 interrupted，也不得留下一个会牺牲下一条请求的伪 gap。

不变量：

```text
usage_source_states[working_epoch].resolved_through_offset
= source_checkpoints[consumer_kind=usage].committed_offset

usage_source_states.file_generation
= source_files.file_generation

usage_source_states.owning_thread_id
= source_files.thread_id
= OwningLive.owning_thread_id
```

raw-tail proof 也属于 source state 的持久化不变量：`raw_tail_status=none` 时 `resolved_through_offset=observed_raw_size` 且 tail start 为空；`half_line` 时 `resolved_through_offset=raw_tail_start_offset<observed_raw_size`；`unverified` 时只允许 `resolved_through_offset<=observed_raw_size` 且 tail start 为空。proof 只对 state 的 `file_generation` 和 `observed_raw_size` 有效。它随 build epoch 激活后继续留在新的 active `usage_source_states`，不能依赖随后被删除的 manifest。

`device_id/inode`、confirmed root 与 canonical algorithm version 是判定当前 epoch 能否局部从 0 确定性重放的持久化证据。强制不变量为 `canonical_algorithm_version = canonical_algorithm_for(usage_parser_version)`，该映射是代码中的一对一版本常量；event ID 编码、payload canonicalization、fingerprint 或去重比较字段任何变化都必须 bump usage parser version。禁止在 parser version 不变时改变 canonical algorithm。由此 app meta/build manifest 绑定 parser 即同时唯一绑定 canonical algorithm；加载、提交、恢复、Carried 和 activation 都重新计算并验证 state 值。只有 `continuation_state=owning_live` 才允许保存非零 usage checkpoint。只有处理前 `chain_state=continuous` 才允许用相邻累计快照生成 recovered；`interrupted` 表示上一边界没有可信 current total，必须由后续可信 total 只建基线。reset 只是一条 anomaly 和 Turn 的单调 `block_reset`，不是可持久化 source chain 状态；当前记录的可信 total 必须成为新 baseline，source commit 最终为 continuous。working epoch、generation、parser version、offset、物理身份、binding/root 或 canonical algorithm 任一不匹配时 state 为 stale，按步骤 2 的严格边界处理。

### 4.6 `usage_build_sources`

这是跨扫描轮次和进程重启持久化的 epoch build manifest；它不是临时内存 proof。

主键：

```text
(build_epoch, source_file_id)
```

字段：

```text
build_epoch
source_file_id
target_parser_version
expected_file_generation
expected_device_id
expected_inode
expected_owning_thread_id?
expected_root_session_id?
active_committed_offset
active_guard_hash?
active_state_fingerprint?
required_generation
required_through_offset
observed_raw_size
raw_tail_status = unverified | none | half_line
raw_tail_start_offset?
membership_reason = active_contributor | present_at_build_start | both | discovered_during_build
completion_status = pending | rebuilt | carried | blocked
completion_error_code?
completed_generation?
completed_through_offset?
carry_from_epoch?
carry_phase = none | occurrences | turns | anomalies | finalize
carry_after_start_offset?
carry_after_turn_key?
carry_after_anomaly_id?
created_at_ms
updated_at_ms
```

约束：manifest 的 `build_epoch`、`target_parser_version` 必须等于 `app_meta` 当前两项 build 值；generation、device/inode、binding/root 是当前 replacement generation 的预期身份。`required_generation=expected_file_generation`。`required_through_offset` 是该 generation 已由完整行 reader 证明、最终账本必须覆盖的最大 `last_complete_offset`，只在 required generation 不变时单调增加，绝不直接取 raw `observed_size`。

present observation 原子更新 `observed_raw_size`，但不能预先推进 required boundary。generation 与 raw size 都和现有 manifest proof 完全相同时保留已验证的 none/half_line；新成员、generation 变化或 raw size 变化必须设 `raw_tail_status=unverified/raw_tail_start_offset=NULL`。usage reader 每批提交都可用同 generation 的 `last_complete_offset` 更新 `required_through_offset=MAX(old,last_complete_offset)`，但 `fixed_view_exhausted=false` 的中间批必须继续保持 manifest/state raw tail unverified。只有 `fixed_view_exhausted=true` 时才同时写 manifest 和 working source state：若 tail_status=none，要求 `last_complete_offset=observed_raw_size`；若 half_line，要求 `tail_start_offset=last_complete_offset<observed_raw_size`。half-line 不是有效记录，不要求 checkpoint 越过；unverified 禁止 completion/carry/activation。generation/identity 变化通过 replacement 保留成员身份，但新 row 设置 `required_generation=new generation`、`required_through_offset=0`、新 raw tail 状态；旧 generation 的字节 offset 不得继承或参与新 generation 单调性。

`rebuilt` 必须有与 required generation 相同的完成 generation，且 working checkpoint/state 与 `completed_through_offset >= required_through_offset`；half-line 已明确忽略时允许 completed offset 停在 tail start。`carried` 必须有经过验证的 active state fingerprint，并严格满足 `required_generation=active generation` 与 `required_through_offset=active_committed_offset=completed_through_offset`，且 raw tail 不为 unverified。`pending/blocked` 永远不能进入 activation proof。build 开始后新发现的 present rollout 必须在同一 discovery observation 事务加入 manifest，不能只依赖激活时重新枚举。

carry cursor 仅允许 completion=pending/blocked。`carry_phase=none` 时所有 cursor/carry_from 为空；开始 carry 时固定 `carry_from_epoch=usage_active_epoch`，按 `(source_start_offset)`、`turn_key`、`anomaly_id` 升序分别推进三个 after-cursor。每次 cursor 只能前进，重启从已提交 cursor 继续；只有 finalize 事务完成后才清空 cursor。finalize 时来源仍 missing 才改 carried；同一身份已 present 时按转换协议保持 pending。

`BeginCarry` 有且仅有两个入口，均在一个 `BEGIN IMMEDIATE` 中 CAS source missing、manifest、active proof 与当前 working 状态：

- `fresh`：usage checkpoint 已是 target parser 的 `rebuild_required/offset 0/guard NULL`，working epoch 没有该来源 state；事务直接设置 carry epoch、occurrences phase 与空 cursors。
- `partial_seed`：部分 `BuildFrom` 已留下 `ready` checkpoint 和严格匹配的 working state，且 `working resolved offset <= active_committed_offset`、`required_through_offset=active_committed_offset`，target/active parser、generation、identity、binding/root 与 canonical algorithm 全相同。事务删除仅一行 working source state，把 checkpoint 重置为 `rebuild_required/offset 0/guard NULL`，保留已提交的 partial occurrence/Turn/anomaly/event 作为不可见 seed，再设置 carry epoch、occurrences phase与空 cursors。ResumeCarry 必须从 active epoch 的第一个 key 重新枚举全部事实，逐行使用正式双键比较命中 seed；任何不一致 hard fail，finalize 的完整集合计数/fingerprint 必须证明没有额外 seed/orphan。这样不需要无界清理，同时 carry-in-progress 提交后一定没有 working source state。

除这两种前态外一律 blocked。BeginCarry 事务失败保留原 ready state/checkpoint 或原 fresh 前态，不能只翻转 checkpoint；崩溃重启要么看到 `carry_phase=none` 的完整前态，要么看到 phase=occurrences、无 working state 的完整后态。

状态机固定为：`pending -> rebuilt | carried | blocked`；暂时条件恢复后允许 `blocked -> pending`，也可在同一验证事务直接 `blocked -> rebuilt | carried`。`rebuilt/carried` 是“截至当前来源观察”的完成态，不是忽略后续 observation 的永久终态：同一物理身份追加时 `rebuilt -> pending`；carried 来源以相同冻结身份重新 present 时 `carried -> pending`；两者都在 `record_source_observations` 更新 source row 的同一事务发生。冻结 generation/device/inode/binding/root/parser 任一不兼容变化不允许状态回退或改写 expected 字段，必须执行保留旧成员全集的 replacement。`blocked` 每轮可重验，不能成为无重试入口的永久锁。

未完成 manifest 与 checkpoint 的组合只有三类合法状态：

- 初始 `pending/blocked`：尚无匹配 working state，checkpoint 必须为 `rebuild_required`、offset 0，只能从 0 开始；
- 可续读 `pending/blocked`：已由先前 chunk 或 `rebuilt/carried` 同身份失效完成证明而保留匹配 working state，checkpoint 必须为 `ready`，可以严格校验 state/guard 后从非零 offset 续读，边界已追平时可 `CompleteOnly`。
- carry-in-progress `pending/blocked`：`carry_phase!=none`，checkpoint 保持 `rebuild_required` 且没有 working source state；只能按持久化 carry cursor 执行 `ResumeCarry`，不能进入文件 BuildFrom/ReadFrom。来源仍 missing 时 finalize 一次变为 carried + ready；来源以同一冻结身份重新 present 时按下述转换协议继续完成 active prefix，finalize 后变为 pending + ready，再从 active offset 读取文件新增部分。

禁止仅因为 manifest 为 pending/blocked 就把 ready checkpoint 改回 rebuild_required；也禁止在没有匹配 working state 时把初始未完成成员标 ready。

文件状态协议：

- `rebuilt + present + size 增长`：原子改 pending，保留 working checkpoint/state，从旧完成 offset 增量追平；
- `carried + present`：身份/guard/offset 仍匹配时原子改 pending，从 carry 恢复的 working checkpoint 增量读取；不匹配则执行保留旧成员全集的 replacement；
- `rebuilt + missing`：只要 generation/device/inode/binding/root 未变，保留 rebuilt；build epoch 已含截至最后一次 present `observed_size` 的完整数据，不需要再 carry；
- `carried + missing`：保留 carried；
- missing 后以同身份重新出现：按上述状态转 pending；以不同身份出现：执行 replacement，旧 manifest 其他成员不得消失。

`carry_phase!=none` 时来源重新 present 是独立的最高优先级转换，不得先套用普通 `carried + present` 规则，也不得把 checkpoint 直接改 ready：

1. `record_source_observations` 在同一事务比较 generation、device/inode、binding/root、target parser、active offset guard 与 manifest 冻结证据。任一不匹配，立即执行 `replace_build_preserving_all_members`，由 replacement 清理该来源已复制的 build rows/orphan events、清空 carry cursor，并令新 generation 从 0 重建；旧 manifest 其他成员完整保留。
2. 全部匹配时，不丢弃已提交 carry 批，也不清空 cursor；记录当前 raw view，保持 checkpoint=`rebuild_required`，计划仍为 `ResumeCarry`。若 generation/raw size 与从 active state 冻结的 verified proof 完全相同则保留 none/half_line，否则标 unverified。后续每个 carry 批除原 CAS 外，还允许 `present + 同一冻结身份 + 文件前缀 guard 在 active_committed_offset 仍匹配`；不读取 active offset 之后的正文。
3. finalize 事务验证 occurrences/Turns/anomalies 已完整复制到 active offset、集合 fingerprint/计数相等、source state 匹配，并重新校验 present 文件的 identity 与 active-prefix guard。然后复制唯一 source state、恢复 checkpoint=`ready`/`active_committed_offset`、清空全部 carry 字段，但把 manifest 留在 `pending`，不能标 `carried` 或 `rebuilt`。
4. 若保留的 raw-tail proof 已验证且 checkpoint 已等于 required boundary，下一计划可直接 `CompleteOnly`；否则下一计划必须是 `BuildFrom(active_committed_offset)`，reader 验证当前 generation 的完整行边界并处理新增区间。raw size 大于 active offset 但只有 half-line 时仍通过一个 exhausted reader result 落 proof；任何已复制行冲突、active prefix 不等或 finalize 证明失败都整体回滚并转 replacement，不允许留下“部分 carry + 文件重建”混合状态。

这个转换只复用已经验证的 active prefix，不删除大型 partial carry，因此每个复制事务仍最多 2048 行；它以完整集合验证替代无界清理事务。来源在转换过程中再次 missing 时仍可把 active-prefix 复制到 finalize：若恢复 present 时 generation/raw size 未变并保留了 durable verified tail，可按普通 proof 标 carried；若 raw size 曾变化而 tail 仍 unverified，只能清 cursor、恢复 ready 并标 `blocked/SOURCE_MISSING_WITH_UNVERIFIED_TAIL`，不能标 carried，以后同身份再次 present 时从 active offset 验证尾部；身份变化则 replacement。

present observation 在 generation 或 raw size 变化时把 raw tail 标 unverified；两者均未变化时保留既有 verified proof。reader 中间批只按同 generation 的 last complete offset 单调推进 `required_through_offset`，仍保持 tail unverified；exhausted 最终批才写 none/half_line。来源在 active offset 后已有完整追加、build 只处理部分 complete tail 后变 missing 时，`required_through_offset>active_committed_offset`；此时禁止 Carried，必须保留 pending/blocked 等待来源恢复。只有 Rebuilt 已覆盖 required complete boundary，或 active carry 事实本身已覆盖全部 required complete boundary，才能形成完成证明。已验证 half-line 本身不计 Token，但 proof 必须同时持久化在 working source state，供激活后的下一次 build 使用。

`pending` 的完成判定优先于普通 checkpoint `Skip`。若 pending 成员当前 present，且 frozen identity、guard、binding/root、parser/canonical algorithm、working checkpoint/state 与当前 generation 的 `required_through_offset` 全部匹配，且 raw tail 已由 exhausted reader 或同 generation/raw size 的 durable proof 明确为 `none|half_line`，即使没有新增完整行，也必须执行 completion-only 事务：不读取正文、不生成 candidate，CAS manifest 仍为 pending，原子写 `completion_status=rebuilt`、`completed_generation` 和 `completed_through_offset`。`blocked` 条件解除且边界已追平时使用同一事务，可直接 `blocked -> rebuilt`。这样同大小恢复与零字节追平不会永久 pending；`raw_tail_status=unverified` 时先 VerifyRawTail，禁止走此捷径或 Skip。

activation 重新读取每个来源最新 file status/manifest proof：present 的 rebuilt 成员必须 raw view 已验证、`completed_through_offset=required_through_offset=working checkpoint/state offset`；若有 half-line，checkpoint 等于 tail start 而非 raw size。missing 的 rebuilt 成员必须 raw tail 已验证、`completed_through_offset=required_through_offset=最后一次已知完整边界` 且 generation/身份仍匹配；carried 只允许当前 missing，并要求 active generation/offset 精确覆盖 required boundary、raw tail 非 unverified、carry cursor 已 finalize。任何 pending/blocked、未完成 carry、unverified raw tail 或 boundary 不匹配均不得激活。

migration 创建 `INDEX usage_build_sources_status_idx ON usage_build_sources(build_epoch, completion_status)`；所有 epoch、generation、offset、identity 数值使用非负/正数 CHECK，枚举使用 CHECK，`source_file_id` 外键指向 `source_files`。应用层在同一写事务补充校验这些字段与单行 `app_meta` 相等，因为 SQLite CHECK 不能跨表引用。

### 4.7 `ingest_anomalies`

| 字段 | 约束 |
|---|---|
| `ledger_epoch` | PK 部分 |
| `anomaly_id` | PK 部分，确定性 ID |
| `detected_at_ms` | NOT NULL |
| `occurred_at_ms` | 可空 |
| `thread_id` | 可空 FK |
| `source_file_id` | 可空 FK |
| `file_generation` | 可空 |
| `source_start_offset` | 可空 |
| `anomaly_type` | NOT NULL，枚举 |
| `severity` | `warning` / `error` |
| `details_json` | NOT NULL，安全白名单对象 |
| `resolved` | NOT NULL，默认 false |

`details_json` 只允许数字字段名、期望/实际值、parser version、ID 和错误码。不得放原始行、任意 payload、标题正文或消息内容。

`anomaly_id` 使用与事件相同的带版本 canonical binary encoding 和 BLAKE3。输入字段固定为：

```text
anomaly_id_version
anomaly_type
source_file_id?
file_generation?
source_start_offset?
thread_id?
turn_key?
facts_fingerprint          # 仅编码错误码以及白名单期望值/实际值
```

`detected_at_ms`、`resolved` 和 `details_json` 的序列化文本不进入 ID；同一异常重扫时只命中原记录，不因检测时间或 JSON key 顺序产生新行。不同物理副本允许各自记录来源异常，但不能影响 canonical `usage_events` 的跨副本去重。

---

## 5. 实施步骤

### 步骤 1：把 usage consumer 接入同一扫描协调器

1. Spec 03 每轮只做一次目录发现和 `record_source_observations`。
   build 存在时，Spec 01 的该事务必须同时把新发现 present 来源加入 `usage_build_sources`、记录 raw proof（同 generation/raw size 不变时保留 verified tail，否则 unverified），并按追加/missing/重新 present 规则失效完成证明；required boundary 只能由随后完整行 reader 的提交推进。manifest 成员身份/binding 冻结证据失配时，同一事务执行保留旧成员全集的 build replacement。`SourceOutcome.build_disposition=unchanged|member_added|completion_invalidated|carry_resumed_present|replaced` 是唯一结果，禁止 scanner 在提交后补写。
2. 对同一 discovery plan，先完成 metadata Thread 分组，再执行 usage 分组；usage 必须看到本轮已经确认的 owning/root 关系。
3. metadata 和 usage 各自生成计划、读取自己的 checkpoint，但共享相同 `source_file_id`、generation、fixed `observed_size`、handle 身份检查、guard 和完整行 reader。
4. 不建立第二个 timer、watcher 或目录枚举器。
5. usage 文件增长与 metadata 相同：只等下一次 `config.interval`、手动请求或已存在的外部 follow-up，不自触发连续扫描。
6. 单个 Thread usage 组失败只阻止该组；其他 Thread 组和 metadata 结果继续提交。

同一文件可以出现：

```text
metadata checkpoint = observed_size
usage checkpoint < observed_size
```

此时 metadata Skip，usage 仍必须打开新增区间。

### 步骤 2：批量加载 usage 计划状态

`load_scan_state` 在一个 SQLite 只读快照中批量返回：

```text
source_files row
usage checkpoint
active/build/working ledger epoch
active/build target parser version
matching working-epoch usage_source_state?
matching open turn?
matching usage_build_sources row?
confirmed thread/root relationship
```

`resume_state` 只有两个合法构造：`start_offset=0` 时固定为 `AwaitOwningMeta`，解析器必须从文件证据重新确认 owning；`start_offset>0` 时只能从完全匹配 working state 的 `OwningLive { owning_thread_id }` 构造。调用方不得从 `source_files.thread_id` 单独伪造非零 continuation。

从 0 处理分成两个不同命令，禁止共用名字或计划分支：

- `BuildFrom(0)`：只用于存在 `usage_build_epoch`、当前 source present 的 manifest 成员。它向不可见 shadow build epoch 写入，允许按第 3.3 节多批提交；首批从 0 成功后 checkpoint/state 变 ready，后续 `BuildFrom(offset)` 继续，最终追平同 generation 的 required complete boundary 且 raw tail 已验证才标 rebuilt。它只验证 manifest 冻结 identity/binding/root/parser 和 replacement 清理结果，不受 LocalReplaySafe/单批限制；missing 成员必须先走 BeginCarry 或 blocked。
- `LocalReplay(0)`：只用于无 build、需要在当前 active epoch 原子替换某来源既有事实的保守优化。它必须满足下述 `LocalReplaySafe` 且单批完成；否则先 `begin_rebuild` 创建 shadow epoch，再对 manifest 成员使用 `BuildFrom(0)`。

定义 `LocalReplaySafe`：active epoch/parser 未变化；已有 state 中的 generation、device/inode、owning binding、confirmed root、canonical algorithm version 都与当前值相等；若 checkpoint 为非零，state 必须存在且能提供这些证据。state 不存在只允许 checkpoint=0 且 active epoch 无该来源任何 occurrence、Turn、anomaly/state，并且这是 active 从未贡献过的全新来源。

当前版本的 LocalReplay 只允许单批完成，不引入 active epoch shadow staging：从 0 流式尝试重放，若在 reader 验证 raw tail 并到达该 generation 的完整行边界前触及 bytes/lines/candidates 上限，立即丢弃内存结果、不修改数据库，并转完整 epoch rebuild。只有整个来源的完整行范围满足单批预算时，才在一个事务中暂存旧 occurrence 集、删除该来源 working epoch 的 occurrence/Turn/anomaly/state、写回完整重放结果，并仅删除“旧 occurrence 已消失且全 epoch 已无 occurrence 引用”的 orphan canonical event。canonical event 不能按 `usage_events.source_file_id` 盲删，重放写入必须执行正式 event-ID/occurrence 双键协议；事务中任一冲突使旧集合完整回滚。这样 LocalReplay 不可能绕过有界事务限制。

任何 owning/binding、root、generation、device/inode、parser、canonical algorithm 或已存在 canonical payload 不一致，以及 nonzero checkpoint 的 state 缺失，均为 `LocalReplaySafe=false`，必须启动完整 epoch rebuild。禁止在 active epoch 留下无法证明仍有效的旧事件。

计划表按从上到下的顺序取第一个匹配分支，禁止先做 offset 大小比较再补看 manifest/tail：

| 条件 | usage 计划 |
|---|---|
| `usage_active_epoch=0` 且无 build | 最高优先级：`begin_rebuild(current usage parser)`，得到 working epoch 1；本轮重新加载计划，禁止向 epoch 0 提交 event/Turn/anomaly/state |
| 当前代码 usage parser 与 working parser 不同 | 有 build 时先执行保留旧成员全集的 replacement 到 current parser；无 build 时 begin_rebuild；提交后重新加载计划，禁止复用旧 parser 的 BeginCarry/CompleteOnly/Skip/读取结果 |
| owning/root 未确认 | `BlockedRelationship`，不推进来源、不打开正文；关系确认后下一轮重新计划 |
| source present 且 checkpoint offset > observed_size | 启动全账本 rebuild/replacement；该非法 offset 判断优先于任何 CompleteOnly/Skip/BuildFrom/ReadFrom |
| source present 且 generation、身份或 guard 不可信 | 启动全账本 rebuild/replacement；不得进入任何读取或复用分支 |
| checkpoint=`error` 且 verified-error 条件全部成立 | 从旧 nonzero offset 续读；成功提交时原子恢复 ready；该错误分支优先于所有正常 carry/完成/读取分支 |
| checkpoint=`error`、有 build，且任一 verified-error 条件失败 | 调用 `replace_build_preserving_all_members(cause=usage_error_proof_failed)`；受影响 present 来源按 occurrence 清理并重置 pending+rebuild_required/offset0，missing 来源保留 manifest 成员并标 blocked；提交后重新加载计划，禁止非零续读 |
| checkpoint=`error`、无 build，且任一 verified-error 条件失败 | 仅 `LocalReplaySafe` 时 `LocalReplay(0)`；否则 begin_rebuild 后重新加载计划，present 才走 `BuildFrom(0)`，missing 走 BeginCarry/blocked |
| manifest pending/blocked 且 `carry_phase!=none` | 最高于普通 build 计划：`ResumeCarry(cursor)`；missing 时按普通 carry，present 且冻结身份/active-prefix guard 匹配时按转换协议完成 prefix；finalize 前不读新增正文、不恢复 checkpoint |
| manifest pending/blocked、`carry_phase=none`、source missing，且 target parser=active parser、active identity/binding/root/checkpoint/state/canonical algorithm/durable raw-tail proof 全匹配，`required_generation=active generation`、`required_through_offset=active_committed_offset`；working 状态满足 fresh 或 partial-seed 契约 | `BeginCarry(fresh|partial_seed)`：按下述原子转换初始化 carry；提交后重新加载并进入 `ResumeCarry` |
| manifest pending/blocked、`carry_phase=none`、source missing，但任一 BeginCarry 条件不满足 | 原子标 `blocked` 并保存确定错误码；每轮可重验，不得尝试打开文件或进入 `BuildFrom(0)` |
| build manifest 为 pending/可解除 blocked，checkpoint=`ready`、state 已严格匹配同 generation 的 required complete boundary，且 generation/raw size/identity/guard 对应的 durable tail proof 已验证 | `CompleteOnly`：none 与 half-line 均不读正文，原子标 rebuilt；该条件优先于所有 offset/Skip 分支 |
| 无 build，checkpoint=`ready`、state/identity/guard 与 durable tail proof 全匹配，tail=none 且 offset=observed_raw_size | `Skip` |
| 无 build，checkpoint=`ready`、state/identity/guard 与 durable tail proof 全匹配，tail=half_line 且 offset=raw_tail_start_offset<observed_raw_size | `Skip`；已验证的同一半行正文读取为 0 |
| nonzero offset 但 usage state 缺失，或 state 的 epoch/parser/generation/identity/binding/root/canonical 证据 stale | 完整 epoch rebuild/replacement；不能从 events/Turn 聚合猜状态 |
| build manifest pending/blocked、source present、checkpoint=`rebuild_required`、无 matching working state、`carry_phase=none` | `BuildFrom(0)`；允许在 shadow epoch 多批推进，不检查 LocalReplaySafe；missing 来源禁止进入本分支 |
| 无 usage checkpoint 且 source present | `ReadFrom(0)` |
| manifest pending/blocked、source present、checkpoint=`ready`、working state/guard 严格匹配、offset < observed_size，且不是上述 verified-tail completion 分支 | 从非零 offset 增量读取；raw size 变化后的 half-line 从旧 tail start 重读 |
| source present、checkpoint=`ready`、offset = observed_size，但 working state 或 manifest tail proof 为 unverified | `VerifyRawTail(offset)`：零 candidate reader 到 fixed view EOF，提交 exhausted tail proof；build pending 时条件满足可在同事务完成，优先于 `Skip` |
| source present、checkpoint=`ready`、offset < observed_size、state/guard 匹配且没有匹配 verified half-line proof | 从 offset 增量读取 |

普通追加只重读新增区间；replacement、truncate、guard mismatch 和 parser 升级使用第 11 步的 epoch rebuild，旧 active 账本在新账本完整前保持可查询。

`processing_status=error` 的非零恢复必须同时满足：`committed_offset>0`、working epoch、working parser version、generation、文件身份、confirmed binding/root、guard、`usage_source_state` offset/ownership/chain state 以及 open Turn 状态全部匹配。任一项不满足都不得非零续读；成功事务必须把 checkpoint 与 state 一起恢复为 `ready`。

### 步骤 3：实现 rollout usage 记录适配

只解析 Spec 03 交付的完整行。兼容基线：

```text
line.type = event_msg
line.payload.type = token_count
payload.info.total_token_usage
payload.info.last_token_usage
```

Token 字段白名单：

```text
input_tokens
cached_input_tokens
cache_write_input_tokens
output_tokens
reasoning_output_tokens
total_tokens
```

Turn 生命周期白名单：

```text
task_started | turn_started
task_complete | turn_complete
turn_aborted
```

同时读取 `turn_context.turn_id/model` 和安全的外层 timestamp。当前 Codex protocol 对 `task_*`/`turn_*` 采用别名兼容；实现必须用枚举适配，不把具体字符串散落在 scanner。

兼容规则：

- `info=null`、unknown record、rate limits 等不产生 Token；
- 明确出现 cache-write 数值（包括 0）时 Adapter 输出 `Some(value)`；字段缺失时输出 `None`，不得按模型能力推断 0；
- `last_token_usage` 与 `total_token_usage` 都经过同一个 Codex Adapter；两种 snapshot 的 canonical 结果都严格区分 `Some(0)` 与 `None`；
- required Token 字段缺失、非整数或越界时产生 anomaly，不生成有效事件；
- 新增未知字段忽略；记录类型未知只产生有限计数，不输出 payload；
- timestamp 优先使用 token record 自身时间，其次外层时间；两者都缺失则不生成事件，记录 `USAGE_TIME_MISSING`；禁止使用文件 mtime。

时间缺失但 `current_total` 可信时仍更新本来源累计基线和 Turn 的最后快照，并在本事务结束时保持/恢复 `chain_state=continuous`；同时把该 Turn 标为 `partial`，该记录不进入 `accounted`，且该 Turn 禁止补偿。下一条可正常走 normal/recovered，不能因本条已有可信边界再牺牲一次请求。

scanner 报告 malformed、oversized 或无法归一化但已越过的完整行时，usage adapter 必须把 `chain_state=interrupted`，并对 active Turn 设置持久化 compensation block。required total 无效、ownership/parser gap 同样中断 source chain。累计 reset 不走此分支：只要 current total 可信，就在当前事务记录 `TOTAL_CHAIN_RESET`、设置 active Turn 的单调 `block_reset`、以 current total 建立新 baseline，并最终提交 `chain_state=continuous`。不能只记内存 anomaly 后推进 checkpoint。

### 步骤 4：复用 ownership 分类器

把 Spec 02 的逐记录 ownership 状态机提取为共享 `RolloutOwnershipClassifier`，metadata adapter 与 usage adapter 使用同一 implementation 和 fixture。

每条 normalized record 先得到：

```text
Owning
ReplayedAncestor
UnknownOwnership
```

usage 规则：

- `Owning`：允许更新模型、Turn、累计链并生成事件；
- `ReplayedAncestor`：完全忽略 Token、模型和生命周期，不更新 owning 来源累计基线；
- `UnknownOwnership`：不计 Token；若可能影响后续链，整个来源结果不提交并要求从 0 重建；
- 非零 resume 首次出现 foreign `session_meta`、owning ID 冲突或迟到 replay，返回 `needs_rebuild`，本 chunk 事件、Turn、state、checkpoint 全部不提交。

只有 chunk 结束为稳定 `OwningLive` 才可推进非零 usage checkpoint。

### 步骤 5：维护模型与 Turn 状态

1. Owning `turn_context` 更新 active model；空字符串或无效类型不覆盖已有模型。
2. `task_started/turn_started` 打开 Turn：优先使用原始 `turn_id`；缺失时按版本化 canonical binary encoding 计算 `BLAKE3("synthetic-turn-v1", thread_id, start_offset, tagged_start_time)`。`tagged_start_time` 必须显式编码 `Some(timestamp_ms)` 或 `None`，所以 Turn ID 与时间同时缺失仍可得到稳定 key。source ID、路径、generation 不得进入 key，普通/归档副本必须相同；时间缺失时仍持久化并可关闭该 Turn，但立即设置单调 `block_time_missing=1`，禁止补偿。
3. Turn start snapshot 只有在开始记录之前 `usage_source_state.chain_state=continuous`，且 previous total 是当前断点后可信链的 baseline 时才能使用。若 state 为 interrupted、baseline 缺失或其 offset 早于未恢复 gap，则 `start_total` 置空，设置 `block_start_missing=1`，并按 `chain_block_reason` 单调设置 parser/ownership/required 对应 block；该 Turn 即使后来链恢复也不得恢复补偿资格。reset 已在发生记录上阻断当时 active Turn，不作为后续 source chain 状态传播。
4. 同一来源又出现新 Turn start 而旧 Turn 仍 open：旧 Turn 标为 `aborted/partial`，记录 anomaly，再打开新 Turn。
5. token event 的模型取该记录之前最后一个 Owning `turn_context.model`；没有时为 `unknown`，不能用 `threads.metadata_model` 追填。
6. Turn 内只观察一个非 unknown 模型时 `model_state=single`；观察到多个模型时为 `mixed`。
7. `task_complete/turn_complete` 关闭 completed；`turn_aborted` 关闭 aborted；带 error 的 complete 关闭 failed。三种结束都执行相同的可用快照校验。
8. 结束记录 turn ID 与 active Turn 不同：不关闭其他 Turn，记录 `TURN_ID_MISMATCH`。

Turn accounted 的固定归并算法：打开 Turn 时所有 accounted 数值为 0、cache-write 为 `Some(0)`、`accounted_candidate_count=0`。每个 normal/recovered/compensation candidate 在 canonical 去重结果之前加入一次本来源 accumulator；跨副本 event-ID 已存在的 candidate 也完全同样处理。required 字段 checked-add，count 加一；cache-write 按 Option 传播：任一 candidate 为 `None`，accounted 即为 `None`，否则 checked-sum。

```text
first candidate: 直接采用 candidate 的 Some(value) 或 None
已有或新 candidate 为 None: sticky None
所有 candidate 为 Some(value): checked SUM
```

`accounted_candidate_count=0` 时保持 `Some(0)`；一旦 count>0，不得把 initial zero 当作一个 candidate。溢出持久化 block 并禁止补偿。该 count 与值和 Turn 同事务保存，重启后继续同一归并。

没有 lifecycle 记录不阻止正常 `last_token_usage` 事件，但无法执行 Turn 补偿；记录有限 anomaly 后仍可推进完整行。

### 步骤 6：规范化和验证 Token 快照

每个 `token_count` 先独立解析：

```text
current_total = normalize_cumulative(total_token_usage)
last_usage = normalize_single(last_token_usage?)
```

验证顺序：

1. required 字段存在且是 `i64` 范围内整数；
2. 所有字段非负；
3. total 等于 input + output；
4. cached 不超过 input；
5. cache write known-valued 时，cached + cache write 不超过 input；
6. reasoning 不超过 output。

`cache_write_tokens` 的 fingerprint 必须编码 Some/None tag 与 Some(value)；`Some(0)` 与 `None` 不相等。canonical 层不保存第三套 status 或模型 capability 推断。

`current_total` required 校验失败、malformed/oversized 或其他没有可信当前累计快照的 gap：累计链不更新，当前记录不产生事件，持久化 `chain_state=interrupted` 并阻断 active Turn；下一条可信 total 只能建新基线。`last_usage` 失败时不能降级为 recovered，因为“字段存在但无效”不等于“字段缺失”；但 current total 可信时，本记录直接保存它为新 baseline，并在同一事务结束时置 `chain_state=continuous`。anomaly 与 Turn 的 required block 仍保留，下一条记录正常处理，不能额外丢弃。

所有加减使用 checked arithmetic。溢出产生 `TOKEN_ARITHMETIC_OVERFLOW`，不截断、不饱和、不写负数。

### 步骤 7：按固定顺序处理 `token_count`

对 ownership=`Owning` 且 current total 可信的记录严格执行：

1. 读取处理前持久化的 `chain_state`。若为 `interrupted`（上一条越过边界却没有可信 current total），本条无条件走“断点后建基线”：即使 fingerprint 与断点前旧快照相同，也不读取 last、不生成事件、不进入 Turn accounted；保存 current total、offset 与 fingerprint，在同一 source commit 切回 `continuous` 后结束本记录。
2. 只有处理前为 `continuous` 才执行统一累计下降前置检查，它优先于重复、normal 和 recovered：先比较 previous/current 的 4 个 required 字段；再在两端 cache-write 都 `Some` 时比较 cache-write。任一 required `current < previous` 记录 `TOTAL_CHAIN_RESET`；Some cache-write `current < previous` 记录 `CACHE_WRITE_CHAIN_DECREASE`；两者可同时记录，但只执行一次结果：若 active Turn 存在则单调设置 `block_reset=1`；若 `last_token_usage` 存在且有效、时间有效，则只生成一条 normal candidate，若 last 缺失或无效则不生成事件，绝不生成 recovered；最后无条件以 current total 保存新 baseline，并以 `chain_state=continuous, chain_block_reason=NULL` 结束本记录。下降不得持久化为 interrupted，也不得把判断推迟到 7.2/7.3。
3. 只有未发生 reset 才按 7.1–7.3 选择唯一分支。

#### 7.1 重复累计快照

比较全部 required 字段、cache-write Option tag 和 Some(value) 的 snapshot fingerprint。

```text
chain_state = continuous AND current fingerprint == previous fingerprint
→ 不使用 last_token_usage
→ 不生成事件
→ 只更新安全的记录位置和诊断计数
```

重复快照不能更新 Turn accounted usage。

#### 7.2 正常事件

当前快照不重复且 `last_token_usage` 存在并有效：

```text
event_kind = normal
usage = last_token_usage（total 改为 input + output）
```

当前 total 与 previous total 的差值只做连续性校验，不再生成第二条事件；累计下降已由进入本节前的统一 reset 分支处理，本节不再包含第二套 reset 行为。

#### 7.3 缺失恢复

只有 `last_token_usage` 字段确实缺失，且 previous/current total：

- 都可信；
- 同一 source generation 和 owning Thread；
- 持久化 `chain_state=continuous`；
- 中间没有 ownership 不稳定或 parser gap，且本记录未命中统一 reset 分支；
- required 字段差值全部非负；

才生成：

```text
event_kind = recovered
usage = current_total - previous_total（逐字段）
```

cache-write 只有两端都 `Some` 时才计算；任一端为 `None` 时 recovered event 的 cache-write 为 `None`、quality=`partial`。Some 负差已由进入 7.1 前的统一下降分支处理，本节不能定义第二套行为。任何 required 维度为负时也已由统一分支处理，不能只补正数维度。

#### 7.4 更新基线

无论 normal 是否因 canonical event ID 已存在而去重，只要 current total 可信且 ownership 稳定，都把 current total 保存为本来源 previous total。只有“上一边界没有可信 current total”的 interrupted 首条在进入 7.1 前只建基线并结束；invalid last、时间缺失和 reset 已在其自身可信 current total 记录内建好新基线并恢复 continuous。这样不会多丢下一条有效 last。

### 步骤 8：生成确定性事件 ID

使用 BLAKE3 对带版本的 canonical binary encoding 求 hash；禁止拼接未转义字符串或 hash 原始 JSON。

normal/recovered canonical fields：

```text
event_id_version
thread_id
turn_key?
event_kind
occurred_at_ms
previous_total_fingerprint?
current_total_fingerprint
effective canonical usage vector + cache-write Option
model
```

turn compensation canonical fields：

```text
event_id_version
thread_id
turn_key
event_kind=turn_compensation
turn start snapshot fingerprint
turn end snapshot fingerprint
compensation canonical vector + cache-write Option
occurred_at_ms
model
```

结果编码为 lowercase hex。相同逻辑记录从 active、archived、移动路径、重读区间或崩溃重试到达时必须得到相同 ID。

source ID、path 和 generation 不进入逻辑 event ID，否则复制到 archived 会重复累计；首次来源可写 canonical event 的 provenance，但每个 candidate 的来源位置必须独立写入 `usage_event_occurrences`。

### 步骤 9：执行 Turn 结束补偿

Turn 结束时必须同时存在可信 start total 和 end total，且持久化 `compensation_allowed=true`；start snapshot 缺失、时间缺失、ownership/parser gap、累计 reset、required 字段异常或其他 block reason 任一存在都不得补偿。

```text
turn_delta = end_total - start_total
accounted = 本来源该 Turn 的 normal + recovered + 已有 compensation 候选
missing = turn_delta - accounted
```

判断使用 required 字段的向量偏序：

- 全部 required `missing = 0`：通过，不生成事件；
- 全部 required `missing >= 0` 且至少一个 `> 0`：生成一条 compensation；
- 任一 required `missing < 0`：记录 `TURN_ACCOUNTED_EXCEEDS_TOTAL`，不自动扣减、不生成补偿；
- start/end 任一 required 差值为负：记录 chain reset，不补偿；
- cache-write 只在 start、end、accounted 都 `Some` 时补偿，否则该字段为 `None`；若 Some `end-start < 0`，记录 `TURN_CACHE_WRITE_DELTA_NEGATIVE`，按累计下降处理并整条不补偿；若 `end-start-accounted < 0`，记录 `TURN_ACCOUNTED_EXCEEDS_TOTAL`，整条不补偿。

补偿事件时间取 Turn 结束记录时间；结束时间缺失时不生成补偿。模型选择：Turn 内只有一个已确认模型时使用该模型；多个均已确认模型时使用 `unknown`，不能把缺失 Token 猜给最后一个模型；`model_state=none` 或 `unresolved_model_seen=true` 时门禁必须关闭，不生成补偿。

normal/recovered event 即使因全局 event ID 已存在而未插入，也必须加入本来源 Turn 的完整 `accounted` 向量和 cache-write Option，防止副本生成重复 compensation。`unresolved_model_seen` 与所有补偿 block reason 只允许从 false 变 true，直到该 Turn 关闭，重启不能清除。

### 步骤 10：构造并原子提交 usage batch

每个 owning Thread 组构造：

```text
UsageCommitBatch {
  ledger_epoch
  thread_id
  root_session_id
  source_commits[] {
    source_file_id
    expected_file_generation
    expected_previous_thread_id
    expected_usage_checkpoint
    batch_start_offset
    fixed_observed_raw_size
    last_complete_offset
    source_bytes_consumed
    complete_line_count
    candidate_count
    replayed_prefix_bytes
    replayed_prefix_lines
    fixed_view_exhausted
    tail_status
    tail_start_offset?
    events[]
    occurrences[]             # 每个 candidate 一行，包含 canonical duplicate
    turn_mutations[]
    anomalies[]
    updated_usage_state
    usage_checkpoint_advance
    build_manifest_transition? # 仅追平 required complete boundary 的最终 build 批次或 CompleteOnly 携带
  }
}
```

事务内按顺序：

1. 验证 active/build/working epoch、active/build parser version；batch 的 `ledger_epoch` 必须等于 working epoch；
2. CAS 校验 generation、binding、旧 usage checkpoint，并要求 `fixed_observed_raw_size` 等于该计划/manifest 当前 raw view；
3. 验证 group thread/root 与当前 `threads` 关系一致；
4. 对每个 canonical event 与对应 occurrence 显式处理，禁止使用无法区分冲突来源的裸 `INSERT OR IGNORE`：
   - `(ledger_epoch,event_id)` 已存在：只比较 event kind、时间、thread/root、Turn、model、完整 Token 向量、cache-write/quality 等 canonical 逻辑字段；完全相同视为跨副本重复。`source_file_id`、generation、offset、created time 等 provenance 不参与比较；
   - canonical event 校验/插入成功后，无论它是新事件还是跨副本重复，都插入 `(ledger_epoch,source_file_id,file_generation,source_start_offset)->event_id` occurrence；occurrence 主键已存在时回读，只有 event ID 与 source end offset 都相同才视为重试，否则 hard conflict；
   - canonical event 与 occurrence 都不存在时先写 event 后写 occurrence；延迟外键在 commit 前验证。任何竞争导致的 unique error 都重新按上述双键规则读取验证，不能静默忽略；
   任一 hard conflict 整组回滚并触发 rebuild，不能推进 checkpoint；
5. upsert 来源级 turns；
6. insert deterministic anomalies；
7. replace matching usage source state；
8. 推进 usage checkpoint 到 `last_complete_offset`，写 guard/status/ready；
9. reader commit 同事务验证 `expected_file_generation=required_generation`，以 `last_complete_offset` 单调推进 required boundary，并把本批 raw proof 写入 working `usage_source_states`；build 中间批次不携带 manifest transition，manifest 保持 pending/blocked，matching state/checkpoint 写为 ready。`fixed_view_exhausted=false` 时 storage 强制 tail=unverified，并保持 manifest/state unverified；不得因 `last_complete_offset<observed_raw_size` 猜成 half-line。只有 exhausted=true 且 tail 结构满足 none/half-line 等式时，才把 manifest/state raw tail 改为已验证。checkpoint 到达 required complete boundary、tail 已验证的最终批次才 CAS manifest 当前为命令的 expected `pending|blocked`，验证 frozen identity/parser/membership 与完整 proof 后原子改为 `rebuilt` 并保存完成 generation/offset；`CompleteOnly` 使用相同最终事务但要求 events/occurrences/Turn/anomaly 均为空、raw tail 已验证、checkpoint/state 已等于 required boundary；active commit 时此字段必须为空；
10. active epoch 新增或改变 canonical event 时 `data_revision + 1`；仅为已存在 canonical event 新增 occurrence 时不增加；
11. commit。

必须满足：

```text
usage events / event occurrences / turns / anomalies / usage state / usage checkpoint
要么共同提交，要么共同回滚
```

每个 batch 必须先验证第 3.3 节的 bytes/lines/candidates 上限，超限 batch 整体拒绝。纯重复事件仍必须提交本来源 occurrence；occurrence/checkpoint 前进但 canonical 查询事实未变时 `data_revision` 不变。pending build epoch 的中间提交不改变 `data_revision`；只有最终 epoch 激活时递增一次。

metadata checkpoint 在该事务中只读不写。

### 步骤 11：实现完整账本 epoch rebuild

以下情况不能在 active 账本上局部猜测删除范围，统一启动 rebuild：

- usage parser version 变化；
- rollout replacement、truncate 或 guard mismatch；
- usage checkpoint 大于 observed size；
- canonical event payload conflict；
- 持久化 usage state 与 checkpoint 无法证明连续。

所有需要“废弃并重建”的调用方共用 `replace_build_preserving_all_members(target_parser, cause)`；当前版本禁止先删除旧 manifest、回到无 build 状态后再另起事务。replacement 在一个 `BEGIN IMMEDIATE` 中完成：

1. 以 SQL 集合操作冻结 replacement membership：旧 `usage_build_sources` 全部成员、active epoch 的 occurrence/Turn/anomaly/state contributors、当前 discovery present 来源三者并集。旧 manifest 成员无论是否 active/present 都必须进入新 proof。同一 generation 的 `required_through_offset` 原值保留且只能增加；replacement generation 改变时建立新的 `required_generation` 并从 0 计算，严禁把旧 generation 的字节 offset 当成新 generation 下界。
2. 对旧成员逐行分类，但不把其所有事件加载进内存：
   - target parser 相同、identity/binding/root 未受 cause 影响且 build evidence 自洽的 build-only 成员，保留其 occurrences/events/Turn/anomaly/state、ready checkpoint、required boundary 和 completion/progress；已 rebuilt 且完成 offset 覆盖 required boundary 时可保留 rebuilt；
   - 尚未完成但证据仍自洽的 missing build-only 成员保留已有部分数据和 required boundary，状态改/保持 blocked，错误码 `OLD_BUILD_MEMBER_MISSING`；它不能激活，也不能被 active carry 覆盖未知 tail；
   - parser、identity、binding/root 或 canonical 证据失效的成员，删除该来源 build occurrences/Turn/anomaly/state，再删除全 epoch 已无 occurrence 引用的 orphan events；清空 carry cursor与完成证明。来源 present 时重置为 pending + rebuild_required/offset0，来源 missing 时保留 manifest 行并标 blocked，required boundary 不降低；
   - 身份可信 active contributor 的 active offset/guard/state fingerprint 继续冻结在 replacement manifest，供后续合法 Carried；不得因 replacement 丢失。
3. 新 present 来源加入 manifest时，设置 `required_generation=expected_file_generation`、`required_through_offset=0`、`observed_raw_size=本轮 fixed raw size`、`raw_tail_status=unverified`；只有后续 reader commit 能按完整行推进 required boundary。旧 manifest 没有 active 事实且当前 missing 的成员也绝不能删除，只能按上一步保留完成证据或 blocked。
4. `usage_build_epoch` 继续使用 `active+1`，target parser 更新为参数；完成所有来源级清理/保留、manifest 重写和 checkpoint 状态后，验证“replacement membership 包含旧 manifest 全集”与 required boundary 单调不减，再 commit。任何一步失败整体回滚到旧 build。

该协议允许安全成员留在同一 build epoch，避免复制大型 build-only 数据；不安全成员按来源 occurrence 精确清理。它从语义上替代旧的“删除全部 build manifest/rows”协议，任何 source observation、metadata reconcile、parser 冲突或 identity 变化都不得自行实现另一套 discard。

流程：

1. `BEGIN IMMEDIATE` 设置 `usage_build_epoch = usage_active_epoch + 1`，同时保存 `usage_build_parser_version=target_parser_version`。已有 build 只有目标 parser version 完全相同且 manifest 自洽时才可直接恢复；否则调用上述 replacement 协议，不能清空旧 manifest 后遗漏 build-only missing 成员，也不能混用两个 parser 结果。
2. 首次 build 在同一事务创建持久化 `usage_build_sources`：成员是 active epoch 中有 occurrence、Turn、anomaly 或 source state 的全部贡献来源，与 build 开始 fixed discovery 中全部 present rollout 的并集；replacement build 还必须并入旧 manifest 全集。事件贡献必须通过 `usage_event_occurrences.source_file_id` 枚举，禁止使用 `usage_events` 的首次 provenance。为每个成员冻结 generation、device/inode、binding/root、active checkpoint offset/guard、active state fingerprint、required boundary、目标 parser和 membership reason。active source state 的 `file_generation/observed_raw_size/raw_tail_status/raw_tail_start_offset` 与 source row 最后观察完全匹配时，把该 durable proof 冻结进 manifest，并令 `required_generation=state.file_generation`、`required_through_offset=state.resolved_through_offset`；present 当前 raw size 若更大则 observation 再标 unverified，后续 reader 推进。缺 state、generation/raw size 不等或状态非法时 manifest tail 固定为 unverified；missing 成员只能 blocked，绝不能猜为可 carry。build 期间每轮发现的新 present 来源也必须在来源 observation 事务中以 `discovered_during_build` 加入，不能等激活时临时计算成员。
3. 初始化清零只作用于两类成员：首次 build 的全部成员，以及 replacement 分类为失效/新增、明确需要 `BuildFrom(0)` 的成员。对这些成员把 usage checkpoint 设为 target parser、offset 0、guard null、`rebuild_required`，并删除其 build occurrence/Turn/anomaly/state 与 orphan event；原 active 证据保留在 manifest。`BuildFrom(0)` 首批成功后写入 matching state/checkpoint=ready，manifest 仍 pending，后续可 `BuildFrom(offset)`。replacement 判定可保留的成员绝对不得再次清零 checkpoint、删除 state/rows、清空 completion/carry cursor 或覆盖 required proof；其原进度原样继续。metadata checkpoint 不变。
4. 查询继续只过滤 `usage_active_epoch`，旧稳定账本可用。
5. 后续每轮 fixed view 按第 3.3 节分批把 event/occurrence、Turn、anomaly、source state 写入 build epoch；中间批次只把 checkpoint/state 原子推进为 ready 并保持 manifest pending/blocked。只有最终批次把 checkpoint/state 写到本轮已证明的 last complete boundary、验证 identity/binding/root/parser 及其唯一 canonical algorithm 后才更新 manifest 为 Rebuilt。`blocked` 成员每轮按正式状态机重验，条件恢复后重新进入 BuildFrom/Carried 流程。
6. manifest 中每个来源在 completion proof 必须有且仅有一种结果：
   - `Rebuilt`：present、身份稳定、raw tail 已验证、working checkpoint/state offset=`required_through_offset`、completed generation=`required_generation`、working state/Turn 匹配、无 blocked relationship/hard error；half-line 存在时该 offset 是 tail start，不是 raw size；
   - `Carried`：仅当前 missing、manifest 冻结的 active generation/device/inode/binding/root/checkpoint/state fingerprint 仍完整可验证，目标 parser 与 active state parser 相同，且 `required_through_offset=active_committed_offset`，不存在 active checkpoint 之后的已知 build-only tail。`carry_phase=none` 时先执行计划表的 `BeginCarry` 事务，CAS 全部条件后设置来源 epoch、occurrences phase 和空 cursors；条件不满足就 blocked，绝不尝试打开 missing 文件。随后 carry 按 manifest cursor 分批，每事务最多 2048 行：`occurrences` phase 按 source_start_offset 读取 active occurrence 及 canonical event，复用步骤 10 双键协议写入 build并提交 cursor；再分别按 turn_key/anomaly_id 分批复制 turns/anomalies。每批 CAS 允许 source 仍 missing，或已按文件状态协议验证为同一冻结身份且 active-prefix guard 匹配的 present；中间批次 checkpoint 保持 rebuild_required、manifest 保持 pending/blocked，崩溃后从 cursor 继续。`finalize` 单事务重新验证 required=active offset、三个 phase 已到 EOF 和 working occurrence/Turn/anomaly 集合计数/fingerprint，再复制唯一 source state、恢复 checkpoint offset/guard 为 target parser/ready并清空 cursor。source 始终 missing 且 raw tail proof 仍已验证时才标 carried；同身份 present 则保持 pending，下一计划固定 `BuildFrom(active_committed_offset)`；曾 present 又在 reader 验证前 missing、raw tail 仍 unverified 时标 blocked/ready，等待同身份恢复后从 active offset 验证。任何冲突 hard fail；禁止通过 `usage_events.source_file_id` 猜贡献、禁止逐列 `INSERT OR IGNORE`，也禁止在最终批前恢复 checkpoint 或完成 proof；
   - `Blocked`：不满足以上两者，阻止激活。parser 升级时 missing 的旧 parser 来源不能 carry，必须等来源恢复并重建。
7. build 开始写入后，manifest 任一来源发生 generation/device/inode/binding/root 不兼容变化，立即调用 `replace_build_preserving_all_members`；旧 manifest 全集进入 replacement，安全 build-only 证据保留，不安全来源按 occurrence 精确清理并继续 pending/blocked。不得删除整个 manifest 后仅从 active+present 重新枚举。
8. 只有某一完整 discovery round 确认：manifest 不遗漏任何 active contributor、build-start/newly-discovered present 来源；每行恰为 `rebuilt/carried`；所有当前 present rollout 的 raw tail 都已验证，并已追平同 generation 的 required complete boundary；working checkpoint/state/Turn 与完成证据一致，才形成 `completion_proof`。开始后尚未处理就变 missing 的来源仍保留 pending，因此只能走 Carried 或阻断，不能从 proof 消失。
9. 一个事务中重新枚举并验证 proof，切换 `usage_active_epoch=usage_build_epoch`、`usage_parser_version=usage_build_parser_version`、清空两个 build 列、删除本次 manifest、`data_revision + 1`。任一条件变化整体回滚。
10. 旧 inactive epoch 在切换后分批删除，顺序为 occurrences、events、turns、anomalies、states；删除不改变查询事实和 revision。清理中退出只留下不可见旧行。

build 期间文件继续增长时，present observation 会令 raw tail 重新变为 unverified；reader 未重新证明同 generation 的 complete boundary 并追平前不能激活。持续写入不会由当前轮自触发无限 follow-up。

首次导入使用相同流程：active epoch 0 只代表空稳定查询账本。步骤 2 的最高优先级规则必须先创建 build epoch 1，再处理任何来源；无论当前 parser 是否也为 0，都禁止向 epoch 0 提交任何 usage 行。build epoch 1 完整后一次激活。

### 步骤 12：处理关系变更

metadata 阶段若 confirmed binding/root 发生变化，由 Spec 01 的 `commit_metadata(MetadataCommitBatch)` 在同一个 `BEGIN IMMEDIATE` 内调用 storage 私有函数：

```text
reconcile_usage_for_metadata_change(transaction, binding_and_root_before_after)
```

它不是 scanner 可在 metadata commit 后补调用的公开接口。完整 batch 的 binding、safe facts、Thread/root patch、metadata checkpoints 与下述 usage 副作用共同回滚。

唯一协议如下：

- 先验证新 root confirmed 且不存在环/多父冲突；任一关系不完整则不改旧 confirmed root，等待完整 metadata resolution；
- 无 build：一个事务内更新 active epoch 中受影响 Thread 的 `usage_events.root_session_id` 和对应 `usage_source_states.root_session_id`，重新计算 state fingerprint；同时提交 metadata 关系变化，查询事实变化只递增一次 `data_revision`；
- 有 build：不尝试就地改写受影响来源的冻结 proof。联合 metadata 事务先写 confirmed 关系和上述 active events/states root，再调用 `replace_build_preserving_all_members`；受影响来源按新 binding/root 重建，未受影响且证据自洽的 build-only 成员保留，所有旧 manifest 成员都进入 replacement，missing 未完成成员继续 blocked。事务只增加一次 `data_revision`；
- 禁止只改 build events，或让旧 manifest 成员在 replacement 中消失。关系提交、active 事实更新、replacement manifest/rows/checkpoints 要么共同提交，要么共同回滚。

root 未确认的新 Thread 在首次 usage ingest 时保持 blocked，不能先写错误 root 后再依赖修复。

### 步骤 13：实现聚合查询

所有查询只读取 `usage_active_epoch`，接受显式 UTC ms 区间 `[start_ms, end_ms)`。Spec 05 负责把 today/yesterday/week/month/year 按本机当前时区转换为该区间。

#### 13.1 Summary

SQL 过滤：

```text
ledger_epoch = active
AND occurred_at_ms >= start
AND occurred_at_ms < end
```

计算：

```text
input = SUM(input_tokens)
output = SUM(output_tokens)
total = input + output
reasoning = SUM(reasoning_tokens)
cached = SUM(cached_tokens)
cache_write = null if any matching event cache_write_tokens is null, else SUM(cache_write_tokens)
uncached = null if cache_write is null, else input - cached - cache_write
other_output = output - reasoning
cache_hit_rate = null if input = 0, else cached / input
session_count = COUNT(DISTINCT root_session_id)
estimated_cost = null
```

空范围的整数和为 0，比例为 null，cache-write 与 uncached input 均为 `Some(0)`；范围内存在 `cache_write_tokens=null` 的事件时，两者均返回 null。Session 与模型分组逐组执行同一 Option 传播规则。

#### 13.2 根 Session

按 `root_session_id` 分组；Subagent 不单独成行。每行：

- inclusive usage：根 Thread + 全部后代事件之和；
- self usage：`thread_id = root_session_id`；
- subagent usage：`thread_id != root_session_id`；
- `subagent_count`：范围内有有效事件的不同后代 thread 数；
- `last_activity_at`：范围内 `MAX(occurred_at_ms)`；
- title/project 只 join root Thread；
- `models_used`：按 `(MIN(occurred_at_ms), MIN(event_id), model)` 排序去重，包含 Subagent，unknown 保留；
- cache 和比例使用与 Summary 相同的分子/分母规则。

分页固定排序：

```text
last_activity_at DESC, root_session_id ASC
```

#### 13.3 模型

按事件 `model` 分组，不能使用 `threads.metadata_model`。返回每模型 Token 向量、cache hit、不同 root Session 数、第一/最后活动时间。`unknown` 是正式分组，不把 Token 猜给其他模型。

模型默认排序留给 Spec 05；本层提供稳定 tie-break `model ASC` 和全部数值。

#### 13.4 跨视图不变量

同一范围必须满足：

```text
summary required Token sums = Σ session inclusive required Token sums
summary required Token sums = Σ model required Token sums
summary.session_count = session rows 总数
```

cache-write 只要范围内任一事件为 `null`，该范围相关 `cache_write_tokens` 与 `uncached_input_tokens` 就为 null；明确的 `Some(0)` 作为已知 0 求和。cache hit 只使用 cached/input，不受 cache-write 是否未知影响。

SQL `SUM` 溢出、无效 range 或数据库约束错误返回结构化错误，不能饱和或回绕。

### 步骤 14：错误隔离、诊断和隐私

| 情况 | 处理 |
|---|---|
| 单条 Token required 字段无效 | anomaly；不生成该事件；持久化 chain interruption 和 Turn block |
| total 缺失/无效 | anomaly；不生成事件、不更新基线；持久化 chain interruption |
| last 字段存在但无效 | anomaly；禁止 recovered fallback；可信 current total 在本记录建新基线并恢复 continuous，下一条正常处理；Turn block 保留 |
| malformed/oversized/parser gap | 越过完整行时持久化 chain interruption 和 Turn block |
| 累计 reset | 当前 normal last 可计；同事务以 current total 建新基线/continuous；anomaly 与 Turn reset block 持久化，下一条正常处理 |
| Turn 校验不足 | 不补偿；normal/recovered 保留 |
| owning/root 未确认 | 该 Thread 组不推进；其他组继续 |
| 单来源 I/O/身份竞态 | 该 Thread 组回滚；其他组继续 |
| event ID 相同但 payload 不同 | hard conflict；启动 rebuild，不覆盖 |
| active rebuild 未完成 | 继续展示旧 active epoch |
| 查询溢出 | 查询失败并返回稳定错误码 |

日志与 `ScanReport` 只增加计数：

```text
usage_lines_seen
token_records_seen
usage_events_inserted
usage_events_deduplicated
normal_events
recovered_events
compensation_events
anomalies_created
usage_bytes_read
usage_parse_duration_ms
usage_db_write_duration_ms
```

禁止日志或 anomaly 保存原始 JSON、Prompt、回复、reasoning、工具输入输出、rate limit payload 或用户项目源码。

---

## 6. 实施顺序

1. 创建 migration、约束、索引和 epoch app meta 键。
2. 实现 `NormalizedTokenUsage` checked arithmetic、验证、fingerprint 和单元测试。
3. 实现 usage scan state 批量读取、state/checkpoint 强一致性校验。
4. 把 ownership classifier 提取为 metadata/usage 共用 implementation，复跑 Spec 02 fixtures。
5. 实现 raw record allowlist 与 normalized usage records。
6. 实现模型、Turn 和来源累计状态机；先只输出内存 batch。
7. 实现 normal、duplicate、recovered、compensation 的固定决策表。
8. 实现 canonical event/anomaly ID、每来源 occurrence 与跨副本去重。
9. 实现 `commit` 的 CAS、原子性和 data revision 规则。
10. 接入 Spec 03 同轮 fixed view 和错误隔离。
11. 实现 epoch rebuild、首次激活、重启恢复和旧 epoch 清理。
12. 实现 summary/session/model SQL 与跨视图一致性测试。
13. 最后接 Spec 05；本 Spec 不提前增加 HTTP 类型。

---

## 7. 测试方案

所有测试使用临时 `$CODEX_HOME`、临时 MU SQLite 和脱敏合成 JSONL，不读取或修改真实 `~/.codex`。

### 7.1 Migration 与约束

- v1 数据库升级到 v2，旧 metadata 数据不变；
- migration 中途失败完全回滚；
- 六张 Token 表、外键、索引和 `app_meta` 新列正确；v1 单行升级结果严格为 `usage_active_epoch=0, usage_build_epoch=NULL, usage_parser_version=0, usage_build_parser_version=NULL`；
- 两个 build 列一空一非空、负 epoch/parser、build epoch 非 `active+1` 均被约束拒绝；checkpoint 的 build/rebuild 标记只使用 `rebuild_required`；
- active/build parser version、working epoch 计算与 usage checkpoint 唯一对应；
- `canonical_algorithm_for(parser)` 一对一：canonical/event-ID/fingerprint 规则变化但 parser 未 bump 的构建测试失败；旧 build/Carried state 的算法值与目标 parser 映射不等时拒绝；
- Token 负数、子项越界、total 关系错误被约束或 domain validation 拒绝；
- inactive epoch 不参与查询。

### 7.2 Raw 兼容

- `event_msg/token_count` 完整结构；
- info null；last 缺失；cache-write 显式数值（包括 0）与字段缺失分别覆盖 `Some(0)`、`None`；
- raw snapshot 缺失 cache-write 时 Adapter 输出 `None`，不读取模型能力表，也不把缺失值猜为 0；
- 同一 Thread 切换模型不改变缺失语义；last、cumulative total、Turn snapshot、recovered、accounted 和补偿都沿用 Adapter 的 `Some(0)`/`None`；
- `task_started/turn_started`、`task_complete/turn_complete` 两组别名；
- aborted、failed、缺 Turn ID、缺 timestamp，以及 Turn ID 与 timestamp 同时缺失；后者跨普通/归档副本生成相同 synthetic key、可关闭但持久化 `block_time_missing` 且不补偿；
- unknown fields/records 不阻塞后续完整行；
- malformed、半行、oversized 延续 Spec 03 行边界规则。
- 4–8 MiB 合法完整行独占一批并推进；超过 8 MiB 完整行以 bounded oversized-only 批推进；两者都不会永久重试或把原文留在 batch；

### 7.3 Token 验证

- total=input+output；
- cached、cache write、reasoning 上下界；
- required 字段缺失、浮点、字符串、负数、i64 越界；
- checked add/sub 溢出；
- invalid last 不触发 recovered；
- invalid total 不更新 baseline，并跨 checkpoint/restart 保持 chain interruption；
- malformed/oversized/ownership/parser gap 后的下一条 total 只建新基线，不跨 gap 恢复。
- interrupted 后首条可信 total 即使 fingerprint 与断点前相同，也原子保存为新基线并切回 continuous，不产生事件/accounted；再下一条才可按新链恢复；
- invalid last、时间缺失和累计 reset 的 current total 可信时，本记录即保存新 baseline/continuous；reset 当前有效 last 仍计入，下一条有效 last 正常计入，不因过度 interruption 再少一次；
- required 累计任一维下降在重复/normal/recovered 前统一判 reset：有有效 last 只计 normal，last 缺失或无效时不生成事件；两者都记录 `TOTAL_CHAIN_RESET`、单调 `block_reset` 并以 current baseline/continuous 提交；

### 7.4 去重与正常事件

- 首次有效 last 生成 normal；
- 完全重复 total 不生成事件，即使 last 存在；
- total 增长 + last 只计 last，不再计 total delta；
- 扫描重试、崩溃重读、rename、archive copy 得到相同 event ID；
- sessions/archived 两份相同记录只计一次；
- 两个真实请求数值相同但时间/累计锚点不同，各计一次；
- event ID 相同但 payload 不同触发 hard conflict。
- event ID 相同、canonical 字段相同但 provenance 不同正常去重；
- 每个跨副本 duplicate candidate 都新增本来源 occurrence，canonical `usage_events` 仍只有一行，occurrence-only 写入不增加 data revision；
- occurrence 来源位置键相同但 event ID/source end offset 不同，或所指 canonical event 字段不同，触发 hard conflict，checkpoint 不推进；
- A 先写事件、B duplicate 后，A replacement、B missing/carry：B 的 occurrence 可独立枚举并把 canonical event 带入新 epoch，事件不丢失；
- unique race 必须回读比较，不能静默 ignore。

### 7.5 缺失恢复

- last 真正缺失、连续 total 非负差生成 recovered；
- previous 缺失、total reset、ownership gap、任一 required 负差均不恢复；
- cache-write 两端均为 `Some` 时正常求差；任一端为 `None` 时 recovered 的 cache-write 保持 `None`，不做能力推断；
- cache write 两端 known-valued 但 current<previous 时，在 duplicate/normal/recovered 前写 `CACHE_WRITE_CHAIN_DECREASE`、单调阻断 Turn并以 current baseline/continuous 继续；有效 last 仍只计 normal，last 缺失/无效不恢复；
- recovered 后 current total 成为新 baseline；
- 同一段不能同时 normal 和 recovered。

### 7.6 Turn 补偿

- delta=accounted 不补；
- component-wise delta>accounted 只补 missing；
- 任一 required accounted>delta 不扣减并记录 anomaly；
- start/end 缺失、reset、timestamp 缺失不补；
- start/end cache-write 为 `Some` 且出现负差，或 cache delta 小于已知 accounted 时，记录对应异常且整条不补偿；`None` 不参与差额计算并使该字段无法补偿；
- aborted/failed Turn 也按已有可信快照校验；
- single model 补偿归该模型；多个已确认模型归 unknown；none/unresolved model 禁止补偿；
- archive 副本的 deduplicated normal candidates 仍进入本来源 accounted，不生成二次补偿；
- open Turn 跨进程重启后继续并正确关闭。
- accounted 完整 canonical 向量/cache-write `Option`、unresolved model 与全部 compensation block reason 跨重启不丢失；
- accounted 初始为每个必需维度 0、cache-write 为 `Some(0)`；candidate 的 cache-write 为 `Some` 时按值求和，任一 candidate 为 `None` 后该维度 sticky `None`；deduplicated candidate 使用同一规则；
- start missing、time missing、reset、ownership/parser gap、required invalid 在重启后仍禁止补偿。
- malformed/invalid total/parser gap 发生在 Turn start 之前：start 不得使用 gap 前 previous total，必须持久化空 start snapshot 与对应 block；进程重启后关闭 Turn 仍不补偿；断点后基线建立再开始的新 Turn 才可使用新 snapshot；

### 7.7 Ownership 与关系

- top-level owning Token 正常计入；
- Subagent 文件复制的父 token/lifecycle/model 全部排除；
- owning live 边界后的子 Token 归子 thread、同一 root；
- 多层 Subagent 归最上层 root；
- nonzero resume 遇到 late foreign meta 整组不提交并 rebuild；
- root 未确认时 checkpoint 不推进；父到达后重读只计一次；
- confirmed root 变更原子 reconcile 已有 events。
- 无 build 的 root reconcile 同事务更新 active events 与 active source states；有 build 时同事务更新 active 事实并执行 replacement，旧 manifest 全集保留，受影响来源重建，未受影响 build-only proof/progress 不丢失；
- 首次 `None -> confirmed` binding 或 root patch 与 safe facts/metadata checkpoints、active usage reconcile、build replacement disposition 在一个 `commit_metadata` 事务；任一步失败共同回滚，scanner 不存在事后 reconcile 调用；

### 7.8 Checkpoint、事务与恢复

- metadata/usage checkpoint 独立；
- event/occurrence/turn/anomaly/state/checkpoint 任一失败全部回滚；
- 事务前、事务中、事务后退出分别不会漏记或重记；
- 纯 duplicate 写入 occurrence 并推进 checkpoint，data revision 不变；
- active 新事件提交只增加一次 data revision；
- checkpoint 只匹配 working epoch/working parser 的 state，旧 active/inactive state 不能用于续读；
- generation、parser、offset、binding、root、chain、open Turn state 不匹配不能非零续读；
- verified error 仅在全部正式条件匹配时从 nonzero offset 恢复，并在成功事务内原子回到 ready；
- build 内 error 且 chain/open-Turn/state 任一验证失败时执行保留成员全集的 replacement：分别令 offset=raw 与 offset<raw，断言都不会命中 VerifyRawTail/增量读取；present 来源清理为 pending+rebuild_required/0，missing 来源保留成员并 blocked；
- offset 0 使用 `AwaitOwningMeta`；nonzero 只能由匹配 working state 构造 `OwningLive`，调用方不能只凭 source binding 续读；
- 超过多个 4 MiB/4096 行窗口的 Subagent replay 前缀只保留固定 classifier state，首次 OwningLive 后用 replay prefix 计数建立 checkpoint；到达 owning 前崩溃从 0 重读且不产生历史事件；
- LocalReplaySafe 的 epoch/parser/generation/device/inode/binding/root/canonical algorithm 全匹配才允许当前 epoch 从 0 重放；重放只替换本来源 occurrence/Turn/anomaly/state，且只删除全 epoch 无 occurrence 引用的 orphan event；任一变化、nonzero state 缺失或 canonical payload mismatch 都启动全 epoch rebuild；
- 文件读取期间增长不扩大 fixed view、不自触发 follow-up。
- build 期间新 present 来源的 source observation、manifest add、required boundary、usage checkpoint reset 同事务；manifest 冻结身份失配的 source observation 与完整 build replacement 同事务，注入崩溃后无跨表窗口；
- replacement membership 包含旧 manifest 全集：可信 build-only proof/progress 保留；未完成 missing 成员继续 blocked；受影响来源按 occurrence 清理；任何旧成员不得因非 active/非 present 消失；
- 初始 pending/blocked 无 working state 时 checkpoint 为 rebuild_required 且从 0 开始；中间批次或 completed 同身份失效后的 pending/blocked 保持 ready + matching state，可非零续读/CompleteOnly；manifest 未完成本身不禁止续读；
- 同一大文件从 offset 0 开始时，shadow build 的 `BuildFrom(0)` 可提交多个有界批并从 committed offset 续跑；相同输入若走无 build 的 `LocalReplay(0)`，触及首批预算必须零写入并创建完整 build，证明两个命令不会误用同一单批门禁；
- replacement 前构造一个 retained rebuilt、一个 retained pending-ready、一个失效成员；replacement 后前两者的 checkpoint/state/rows/completion/required proof 逐字段不变，只有失效成员被清理为 offset 0；随后运行通用 build 初始化也不得二次清零 retained 成员；
- 计划表逐对覆盖冲突条件：parser bump+eligible carry、关系未确认+新 present、offset>raw+ready、offset=raw+unverified、offset<raw+ready；断言依次命中 replacement、BlockedRelationship、rebuild、VerifyRawTail、增量读取，后置分支不会抢先；
- reader 因 4 MiB/4096 行预算结束的中间批返回 `fixed_view_exhausted=false/tail=unverified`：只推进 last-complete boundary，manifest/state tail 仍 unverified且不能 CompleteOnly；真正到 fixed EOF 的无尾行与半行分别返回 exhausted+none、exhausted+half_line(start)，storage 对矛盾组合全部拒绝；
- generation/raw size 未变化的 present observation 保留 verified tail，因此 unchanged EOF 可 Skip/CompleteOnly；raw size 变化后标 unverified并要求 reader/VerifyRawTail。half-line 时 checkpoint/required 停在 tail start，补全同 generation 后继续单调增长；replacement generation 改变后 boundary 从 0 重建，旧 generation 的更大 offset 不参与 MAX；
- build 激活并删除 manifest 后，active `usage_source_states` 仍保存 generation/raw size/tail proof；下一次 build 对 matching proof 正确冻结并允许 missing carry，对 proof 缺失、generation/raw size mismatch 或非法等式只生成 unverified/blocked；
- `SourceOutcome` 的五个 build disposition 逐一测试；carry-in-progress 同身份恢复 present 必须返回 `carry_resumed_present` 并继续 ResumeCarry，不能落入普通 completion_invalidated；Spec 01/04 枚举序列化值完全一致；

### 7.9 Epoch rebuild

- active=0 且无 build 时，无论 parser 是否为 0，都先创建 working epoch 1；任何向 epoch 0 写 event/Turn/anomaly/state 的提交被拒绝；首次 build 完整前查询为空 stable epoch，激活后一次出现完整数据；
- parser bump 和 truncate 启动 build，旧 active 数据继续可查；
- 持久化 manifest 在进程重启后成员和 pending 状态不丢失；build 开始时 present、处理前变 missing 的来源不能从 proof 消失；
- manifest 中任一 active contributor/build-start present/newly-discovered present source 没有 Rebuilt 或可信 Carried 结果时不能激活；
- missing 来源在同 parser 且 active generation/binding/checkpoint/state 全匹配时可 carry；parser bump 下必须重建或阻断；
- carry_phase=none 的 eligible missing active contributor 先以单事务 BeginCarry 初始化 source epoch/occurrences phase/空 cursors，再由 ResumeCarry 分批复制；分别在事务前后崩溃可重试且只初始化一次；identity、binding/root、parser、state、durable tail 或 required=active offset 任一不满足时标 blocked，断言没有文件 open/BuildFrom(0)；
- partial BuildFrom 提交 ready state/部分 occurrences、Turns、anomalies 后来源变 missing：BeginCarry(partial_seed) 同事务 CAS 并删除 working source state、重置 checkpoint、保留 partial facts、初始化 cursor；重启后 ResumeCarry 从 active 首 key 全量回读，matching seed 幂等命中并最终集合相等。注入额外 seed/canonical mismatch 时 hard fail 且不能激活；在 state 删除与 cursor 初始化任一注入点失败均保持完整前态；
- Carried 前 checkpoint 为不可续读的 `rebuild_required`；只有 required boundary=active offset 时才允许 carry；occurrence/Turn/anomaly 按持久化 cursor 和每批 2048 行复制，批间崩溃可恢复，最终批才复制 state、恢复 checkpoint、验证 working 不变量并标 carried；
- existing build 的目标 parser 不同必须 replacement，旧 parser 数据不能复用，但旧 manifest 成员必须继续存在；
- manifest 覆盖 pending→rebuilt/carried/blocked、条件恢复后的 blocked→pending→完成和验证事务直接 blocked→完成；rebuilt/carried 只可因同身份新 observation 转 pending，冻结证据变化执行 replacement；
- rebuilt 来源同身份追加时原子转 pending 并从完成 offset 追平；已完成的 carried 来源同身份恢复 present 时转 pending 并追平；carry-in-progress 恢复 present 使用独立 ResumeCarry 转换；rebuilt 后变 missing 保留截至最后完整行 boundary 的完成证明；activation 用最新 status/raw-tail/required/completed offset 复验；
- carried/rebuilt 同身份、同大小恢复 present，或 blocked 条件解除但没有新增字节时，manifest 优先于普通 Skip 执行 completion-only 事务并转 rebuilt，不得永久 pending；
- 无 build 的 verified none 与 verified half-line 都直接 Skip 且正文读取为 0；half-line checkpoint 精确等于 tail start，即使它小于 observed raw size也不进入 offset<raw 增量分支。raw size 变化或 proof unverified 后才从 tail start 重读/VerifyRawTail；build pending 对相同 proof 优先 CompleteOnly；
- build 写入后任一来源 generation/identity/binding 变化必须执行 replacement；受影响来源从 0 重来，未受影响 build-only proof/progress 与全部旧成员保留；
- build 期间追加先令 raw tail unverified；reader 重新验证并追平新 required complete boundary 后才可完成；
- 来源在 active offset 后曾 present/追加又 missing 时，required boundary 阻止错误 Carried；build-only tail 未完整 rebuilt 前 activation 必须 blocked；
- 大型 LocalReplay 一旦不能单批完成就转全 epoch rebuild；大型 Carried 始终按 cursor 分批，二者均不突破事务上限；
- 大型首次导入和 epoch rebuild 按 4 MiB/4096 adapter 行/2048 candidates 任一上限分批，合法单行和 oversized-only 使用正式例外；每批只在完整行边界提交，峰值内存不随 JSONL 总大小增长，中间批次保持 manifest pending，崩溃后从已提交 offset 继续；
- UsageChunkResult/UsageCommitBatch 的 start/end、source bytes、完整行、candidate、replay prefix 五项计数满足正式等式，exhausted/tail 字段满足状态等式；storage 对篡改计数、超预算普通批、occurrence 数不等于 candidate 或伪造 half-line 的批次拒绝且不推进；
- 大型 LocalReplay 触及首个批次上限时数据库保持原样并转全 epoch rebuild；大型 Carried 在 occurrences/turns/anomalies 各 phase 的任意批间崩溃后从已提交 cursor 恢复，最终批前 checkpoint 仍 rebuild_required、manifest 未 carried；
- carry 分别在 occurrences、turns、anomalies、finalize 前四个断点让同一身份文件重新 present：observation 保留 cursor 与 rebuild_required，ResumeCarry 从精确 cursor 继续，finalize 校验 active prefix 后恢复 ready 但保持 pending，再从 active offset 处理新增完整行；无新增完整行也要先验证 raw tail再 CompleteOnly；全程无 duplicate/漏 occurrence；
- 上述四个断点分别注入 generation、inode、binding 和 active-prefix guard 失配：同一 observation 事务执行 replacement，部分 carry rows/orphan event 被按 occurrence 清理，carry cursor 清空，受影响来源 BuildFrom(0)，旧 manifest 其他成员/proof/progress 不变；
- 激活事务失败保持旧 active；
- 激活成功只增加一次 revision；
- 清理旧 epoch 中途退出不影响 active 查询；
- rebuild 前后聚合一致且无重复。

### 7.10 聚合

- `[start,end)` 精确包含边界；跨日事件按事件时间拆分；
- summary total 只等于 input+output；
- cache-write 已知时 `uncached=input-cached-cache_write`；cache hit=cached/input；input=0 时 null；
- 明确 `Some(0)` 作为已知 0 求和；任一 cache-write `None` 使相关范围 cache-write/uncached 为 null；
- Summary/Session/model 使用同一 cache-write Option 归并，禁止模型 capability 推断或额外 status；
- Session 只按 root 成行，含所有层级 Subagent；
- self/subagent/inclusive、subagent_count 正确；
- models_used 按首次有效事件稳定去重；事件模型来自上下文，unknown 不猜测；
- model 分组 required sums 等于 summary；
- summary sums 等于所有 Session inclusive sums；
- session_count 等于根 Session 行数；
- estimated_cost 始终 null；
- SQL SUM 溢出返回错误。

### 7.11 隐私与资源

- fixtures 放入正文哨兵，数据库、日志、diagnostics 均找不到；
- 不读取用户项目目录；
- 不保存 rate-limit payload 或完整 JSON；
- unchanged 且 usage checkpoint/state 匹配的文件正文读取为 0；
- 普通追加读取量接近新增区间 + guard；
- 查询不把全部历史事件加载进内存；Session 使用分页。
- 1 GiB 级合成 rollout 的首次导入与 rebuild 记录峰值内存，证明 batch 数量增长但单批 adapter bytes/lines/candidates 和进程峰值保持在测试预算内；另用超长 replay 前缀证明累计扫描跨度增长但窗口内存不增长；不得缓存全部 batch 结果。

---

## 8. 独立验收标准

### 8.1 账本正确性

- [ ] normal、recovered、turn_compensation 三类事件按固定优先级生成；
- [ ] `total_token_usage` 不直接累加；同一增量只进入一种路径；
- [ ] total、缓存和 reasoning 包含关系完整校验；
- [ ] 相同事件跨重读、移动和普通/归档副本只计一次；
- [ ] 每个 candidate（含跨副本 duplicate）都保存独立 occurrence；来源位置映射冲突 hard fail，active contributor、LocalReplay、Carried 与 epoch 清理均以 occurrence 为准；
- [ ] Token 原始 thread 与 confirmed root 同时保存；Subagent replay 不计入；
- [ ] 单次与累计 snapshot 的 cache-write 缺失都保持 `None`；明确 0 保持 `Some(0)`，模型切换不污染快照、恢复或补偿；
- [ ] estimated_cost 固定 null。

### 8.2 增量与恢复

- [ ] usage consumer 复用 Spec 03 scanner，不建立第二套轮询；
- [ ] metadata 与 usage checkpoint/parser version 完全独立；
- [ ] checkpoint 只对应 working epoch/working parser；旧 active/inactive state 不参与匹配；
- [ ] offset 0 只从 `AwaitOwningMeta` 开始，非零只从匹配 working state 的 `OwningLive` 开始；
- [ ] 非零续读要求 checkpoint、usage state、generation、guard、binding/root、ownership、chain 与 open Turn 全匹配；
- [ ] 初始未完成成员使用 rebuild_required/offset 0；已有 matching working state 的未完成成员使用 ready，可跨有界批次非零续读，不能仅因 manifest pending/blocked 禁止续读；
- [ ] open Turn、完整 accounted/cache-write Option、补偿门禁、累计 baseline/chain 和模型状态可跨进程恢复；
- [ ] UsageChunkResult/UsageCommitBatch 显式携带 `fixed_view_exhausted` 与 tail 结果；中间批只能保持 unverified，只有真正 fixed EOF 可写 none/half-line，storage 校验全部字段等式；
- [ ] Turn accounted 以 candidate count 区分空集合，按 `None` sticky、全 `Some` checked-sum 的固定规则归并；跨副本去重 candidate 也计入；
- [ ] 事件、Turn、异常、state、checkpoint 原子提交；
- [ ] canonical event 与 occurrence 原子提交；duplicate-only 仍写 occurrence，但不增加 `data_revision`；
- [ ] duplicate-only 批次可推进 checkpoint 而不增加 data revision；
- [ ] CompleteOnly、Skip、VerifyRawTail 和普通增量读取都显式要求 checkpoint=ready；error 分支优先，build 内 verified-error 失败统一 replacement，offset 等于或小于 raw 都不能误入正常分支；
- [ ] parser/identity 不可信时使用完整 epoch rebuild，旧 active 数据保持可查；
- [ ] active epoch 的 `LocalReplay(0)` 只在 epoch/parser/generation/device/inode/binding/root/canonical algorithm 均可证明未变且单批可完成时允许；shadow build 成员使用可多批的 `BuildFrom(0/offset)`，两者计划不混用；
- [ ] 持久化 build manifest 覆盖每个 active contributor、build-start present、build 中新发现的 present rollout；成员跨轮/重启不丢失，missing 来源只能可信 carry，否则阻断；
- [ ] source observation 与 manifest add/完整 replacement 原子提交；不存在来源身份已变化而 manifest 未更新的崩溃窗口；
- [ ] build replacement 保留旧 manifest 全集、required boundary 与可信 build-only proof/progress；missing 成员不会因既非 active 又非 present 而消失；
- [ ] replacement 的通用初始化只清零首次 build 或明确失效/新增成员；retained rebuilt/pending-ready 成员的 checkpoint、state、rows、completion、required proof 不被覆盖；
- [ ] Carried 每个复制批复用 canonical event-ID/occurrence 比较；working state、checkpoint 与 manifest carried 只在最终 finalize 事务共同完成；
- [ ] Carried 只在 active offset 覆盖 required boundary 时启动，按持久化 cursor 有界复制，最终批才恢复 state/checkpoint 与完成证明；
- [ ] carry_phase=none 的 eligible missing 成员通过原子 `BeginCarry` 初始化 cursors 后进入 ResumeCarry；不满足条件时 blocked，missing 来源永不进入 BuildFrom(0)；
- [ ] partial BuildFrom 后变 missing 通过 `BeginCarry(partial_seed)` 原子退役 working state、重置 checkpoint并保留 seed 供 ResumeCarry 全量验证；提交后 carry-in-progress 不存在 working state，冲突/额外 seed 不能通过 finalize；
- [ ] carry-in-progress 来源同身份重新 present 时保持 cursor/rebuild_required 并继续有界 `ResumeCarry`；finalize 验证 active prefix 后恢复 ready 但保持 pending，再 `BuildFrom(active offset)`。身份或 prefix guard 失配时 observation 与 replacement 原子提交，部分 carry 数据被清理且其他 manifest 成员不丢失；
- [ ] generation/raw size 未变化的 observation 保留 verified tail；变化时清为 unverified。unverified 且 offset=raw size 时优先 `VerifyRawTail`，不会被普通 Skip 永久阻塞；该规则覆盖 carry-present 零新增字节；
- [ ] verified none/half-line 都可零正文读取 Skip；half-line checkpoint=tail start 即使小于 raw size也不会误入增量读取，build pending 时 CompleteOnly 优先；
- [ ] active `usage_source_states` 持久化 generation-scoped raw-tail proof，build 初始化只冻结完全匹配 proof；manifest 删除后 missing carry 仍可证明，proof 缺失/stale 时只能 blocked；
- [ ] Spec 01/04 的 `SourceOutcome.build_disposition` 都固定为五值枚举，carry-in-progress 同身份恢复 present 唯一返回 `carry_resumed_present`；
- [ ] manifest 状态转换与 blocked 重试入口固定；rebuilt/carried 遇追加、恢复 present 或 missing 时按来源观察协议更新证明，冻结身份变化执行保留旧成员全集的 replacement；activation 校验最新 status/size/required/completed offset；
- [ ] pending/可解除 blocked 且边界已追平时优先执行无正文读取的 completion-only 事务，同大小恢复不会永久 pending；
- [ ] required boundary 只由同 generation 完整行 reader 的 `last_complete_offset` 推进；raw size/generation 变化才标 unverified，half-line 停在 tail start，generation replacement 不继承旧 offset；unverified raw tail 不能 completion、carry 或 activation；
- [ ] usage ingest 固定按 4 MiB、4096 adapter 完整行、2048 candidates 任一上限分批提交；4–8 MiB 合法单行、oversized-only 与非提交 ownership discovery 有明确有界例外；显式计数可由 storage 校验；
- [ ] LocalReplay 只能单批完成，否则无写入地转全 epoch rebuild；Carried 通过 manifest cursor 有界复制并可跨崩溃恢复；
- [ ] build parser 目标冲突或进行中来源代次/身份变化时执行 replacement；旧 manifest 全集不丢失；
- [ ] canonical algorithm 由 usage parser version 唯一决定；任何 canonical 规则变化必须 bump parser，加载、carry 和激活验证同一映射；
- [ ] active epoch 0 只用于空查询；首次来源处理前强制创建 build epoch 1，epoch 0 永不接收 usage 行；
- [ ] 严格计划顺序先检查 current parser、关系、present 非法 offset/身份，再执行 carry/完成/读取；ready 增量分支明确要求 offset<raw，不会吞掉 VerifyRawTail 或 offset>raw；
- [ ] build 只有在完整 discovery proof 下才激活。

### 8.3 Turn 与异常

- [ ] Turn 差额只补缺失部分，相等不补，已统计更多时不扣减；
- [ ] 缺原始 Turn ID 时使用不含来源信息的版本化 synthetic key；时间也缺失时仍能持久化/关闭但永久阻断补偿；
- [ ] persisted interrupted 后第一条可信累计快照只原子建立新基线，即使与旧 fingerprint 相同也不生成事件或 accounted；
- [ ] 只有没有可信 current total 的 gap 才让下一条只建基线；invalid last、时间缺失、reset 已在当前可信 total 记录恢复 continuous，不额外丢下一条；
- [ ] 累计下降无论 last 是否存在都先记录 reset 并阻断当前 Turn；有效 last 只计 normal，last 缺失/无效不恢复，source commit 最终为 continuous；
- [ ] root reconcile 无 build 时同时更新 active events/states；有 build 时原子更新 active 事实并执行保留旧成员全集的 replacement，不混用两套协议；
- [ ] metadata binding/root、safe facts/checkpoints、active usage reconcile 与 build disposition 由一个 `commit_metadata` 联合事务提交，无事后补调用；
- [ ] Turn start 只接受 continuous 且属于断点后可信链的 baseline；gap 前 snapshot 永不用于补偿，相关 block 跨重启保留；
- [ ] 多个已确认模型的补偿归 unknown；none/unresolved model 持久化阻断补偿；
- [ ] invalid last 不冒充“缺失 last”触发恢复；
- [ ] 负差、reset、字段越界、关系缺失和 ID conflict 产生安全 anomaly；
- [ ] known Cache Write 累计下降在所有事件分支前判定：有效 last 只计 normal、缺失/无效 last 不恢复；Turn delta/accounted 负差记录确定异常且不生成补偿；
- [ ] anomaly 和日志不含正文或原始 payload。

### 8.4 聚合

- [ ] Summary、Session、模型都只查询 active epoch 的 `usage_events`；
- [ ] 时间范围统一为 `[start,end)`，使用有效事件时间；
- [ ] 根 Session 行包含主 Thread 与全部后代 Subagent；Subagent 不单独成行；
- [ ] self/subagent/inclusive usage 与 subagent_count 正确；
- [ ] 模型按事件发生时上下文聚合，支持多模型与 unknown；
- [ ] cache hit 使用总分子/总分母，不平均百分比；
- [ ] cache-write 的 `Some(0)` 与 `None` 在 Summary、Session、模型三类视图按同一 Option 传播规则归并；
- [ ] required Token 满足 summary = Σsessions = Σmodels；
- [ ] session_count 等于当前范围有有效用量的 root 数。

### 8.5 工程与范围

- [ ] migration 可升级、回滚、重开；`app_meta` 固定单行的四个 usage 列、build 成对/相邻约束、六张 Token 表和索引齐全；
- [ ] fixed view、完整行、guard、竞态和错误隔离沿用 Spec 03；
- [ ] 所有测试只使用临时脱敏数据；
- [ ] 未实现 HTTP、SSE、Dashboard UI、价格表或费用算法；
- [ ] 实施员不需要自行决定任何 Token 来源优先级、去重范围或补偿规则。

---

## 9. 交付物

```text
src/usage/ 下的 raw adapter、ownership 接入、Token/Turn 状态机、事件 ID/occurrence 与聚合实现
storage migration 0002_usage_ledger.sql
UsageLedger scan/commit/rebuild/query interface
脱敏 fixtures、domain tests、SQLite integration tests、scanner integration tests
```

完成本 Spec 后，Usagi 具备完整、可恢复、可去重的 Token 事实层和聚合能力。Spec 05 只需把这些稳定查询结果映射为 HTTP 与更新通知。

---

## 附录：兼容基线

编写本 Spec 时核对了 OpenAI Codex 官方 protocol：raw `TokenUsage` 当前包含 input、cached input、cache write input、output、reasoning output、total；`TokenCountEvent.info` 可空；Turn start/complete 在 v1 wire 中使用 `task_*` 并接受 `turn_*` 别名。实现以本地 rollout 的 raw 字段为 Adapter 输入，canonical 层只输出本 Spec 定义的字段，不提供旧 canonical 字段的兼容回退。

- [Codex protocol.rs](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/protocol.rs)
- [Codex protocol v1](https://github.com/openai/codex/blob/main/codex-rs/docs/protocol_v1.md)
