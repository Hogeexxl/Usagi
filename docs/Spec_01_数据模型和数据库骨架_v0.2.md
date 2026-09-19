# Usagi Spec 01：数据库骨架与来源检查点

> 版本：v0.2  
> 状态：当前契约修订版（Spec08 实施目标）  
> 更新日期：2026-08-09  
> 上游文档：`Usagi_Codex本地数据口径_v0.2.md`、`Usagi_程序运行机制与数据持久化方案_v0.3.md`、`Spec_02_Codex原始数据与元数据适配_v0.2.md`  
> 当前唯一测试标准：`Usagi_测试标准_Spec01-06_v0.17.md`  
> 当前版本范围：完整实现 Token 用量；仅 Token 美元费用 `estimated_cost` 占位

---

## 0. 当前契约收口

本 Spec 同时保留 **历史 `0001_initial.sql` 的 v1 迁移定义** 与 **当前 latest schema 的约束**。两者不得混淆：

```text
0001 = 历史不可改写的 v1 migration
0002 = Spec04 usage ledger
0003 = Spec07 NormalizedTokenUsage
0004 = Spec08 metadata parent v2 / current-schema cleanup
current latest user_version = 4
```

因此，后文“首版数据库 schema”中如果出现已经在 v4 删除的历史列，只描述 `0001` 当时的真实结构，**不代表 current runtime 仍允许这些列存在**。

当前 runtime 必须满足：

- `app_meta.metadata_parser_version` 已从 latest schema 删除；
- `app_meta.last_full_import_completed_at_ms` 已从 latest schema 删除；
- 当前 metadata parser version 由代码常量 `METADATA_PARSER_VERSION = 2` 唯一定义；
- durable metadata parser version 只存在于 `source_checkpoints.parser_version` 与 `rollout_metadata_facts.metadata_parser_version`；
- `rollout_metadata_facts.parent_hint_provenance` current schema 必须允许 `session_meta_parent` / `subagent_source` / `forked_from_id`；
- parent 解析优先级为 `state explicit edge > session_meta_parent > subagent_source > forked_from_id`；
- Spec07/Spec08 不得通过改写 `0001/0002/0003` 完成升级，只能新增 migration。

## 1. 文档目标

本 Spec 把 Usagi（下文简称 MU）的数据库基础落实为可独立实施、测试和验收的工程方案。

本 Spec 完成后，项目应具备：

1. 可自动创建和迁移的 MU SQLite 数据库；
2. rollout 物理文件身份和状态存储；
3. 按消费器隔离的增量读取检查点；
4. Thread、主 Session 与 Subagent 关系及规范化元数据存储；
5. 全局数据 revision、扫描状态 revision 和 `CODEX_HOME` 身份保护；
6. Thread 元数据与元数据检查点的原子提交能力；
7. 可供当前版本 Token Spec 扩展的 usage 检查点 seam；
8. 不依赖真实 Codex 文件的数据库级测试。

本 Spec 不解析 Codex 文件，也不实现 Token 算法。Token 账本与聚合属于当前完整版本的 Spec 04，不是未来版本能力。

---

## 2. 当前完整版本范围

### 2.1 必须实现的 Token 能力

当前完整版本必须实现：

- `token_count` 解析；
- `last_token_usage` 去重；
- 可信累计差值恢复；
- Turn 结束补偿与一致性校验；
- Subagent 历史重放排除；
- 标准化 `usage_events` 账本；
- Dashboard Token 聚合；
- 根 Session 包含用量聚合；
- 模型 Token 聚合；
- 缓存命中率和各 Token 子项；
- 汇总与 Session 的一致性约束。

这些能力由当前版本的 Spec 04 实施。Spec 01 只建立其依赖的数据库和检查点基础。

### 2.2 唯一不实施的 Token 能力

当前版本不计算：

```text
estimated_cost
Token 美元费用
每个对话花费多少美元
```

`estimated_cost` 保持 `null`，界面显示 `—` 或隐藏。费用占位不能影响 Token 用量统计的实施。

---

## 3. Spec 拆分和依赖顺序

当前完整版本拆为六个独立 Spec：

| 顺序 | Spec | 核心职责 | 依赖 |
|---:|---|---|---|
| 01 | 数据库骨架与来源检查点 | SQLite、基础 schema、migration、来源身份、独立 checkpoint、Thread 元数据、revision | 无 |
| 02 | Codex 原始数据与元数据适配 | `state_5.sqlite`、`session_index.jsonl`、rollout 结构、Thread 关系、规范化元数据合并 | Spec 01 |
| 03 | 文件发现与增量扫描 | 五分钟轮询、手动刷新、完整行、增量读取、重建、首次导入、扫描生命周期 | Spec 01、02 |
| 04 | Token 账本与聚合 | 去重、恢复、Turn 补偿、Subagent 历史排除、`usage_events`、Dashboard/Session/模型聚合 | Spec 01～03 |
| 05 | 查询 API 与更新通知 | 查询、刷新、`data_revision`、`status_revision`、SSE、错误状态 | Spec 01～04 |
| 06 | Dashboard 与 Session 页面 | 正式 Token 数据、图表、Session 表、导入与刷新状态 | Spec 05 |

完成 Spec 01～03 只代表内部采集基础完成。必须完成 Spec 04～06，当前版本才构成可交付产品。

---

## 4. 本 Spec 范围

### 4.1 必须完成

- 引入 SQLite 持久化依赖；
- 确定数据库文件位置和打开方式；
- 建立 `app_meta`、`source_files`、`source_checkpoints`、`rollout_metadata_facts`、`threads`；
- 建立首版索引和完整约束；
- 建立原子 migration 机制；
- 建立来源文件观察结果写入；
- 建立规范化 Thread patch 写入；
- 建立 metadata 与 usage 独立检查点；
- 建立扫描生命周期写入 interface；
- 建立 `data_revision` 和 `status_revision`；
- 建立 `CODEX_HOME` 切换保护；
- 建立初始化、重开、回滚、约束和 migration 测试；
- 更新 README 中数据库位置和检查命令。

### 4.2 明确不完成

- 不读取 `$CODEX_HOME/state_5.sqlite`；
- 不读取 `session_index.jsonl` 或 rollout JSONL；
- 不枚举会话目录；
- 不实现定时或手动扫描；
- 不实现 Token 去重、恢复、Turn 补偿或聚合；
- 不创建 `usage_events`、`turns`、`ingest_anomalies`；
- 不实现 HTTP/SSE；
- 不实现前端；
- 不保存 Prompt、Assistant 回复、工具输入输出或完整 JSONL；
- 不建立日、Session 或模型 rollup 表；
- 不为假设中的第二种数据库实现建立 trait。

---

## 5. 核心设计原则

### 5.1 SQLite 是可重建派生数据库

MU SQLite 不是 Codex 数据库副本，也不是对话数据库。

原始事实仍是 Codex 本地文件；MU SQLite 保存已经确认的结构化派生事实、文件处理状态和恢复检查点。

### 5.2 持久化模块是深模块

调用方不直接管理：

- SQLite connection；
- PRAGMA；
- migration；
- SQL；
- 事务开始、提交或回滚；
- 写入顺序；
- revision 递增。

这些实现集中在持久化模块内部。调用方只提交结构化命令或读取结构化结果。

### 5.3 物理来源与消费进度分离

必须区分：

- `source_files`：rollout 物理文件是谁、在哪里、当前大小和状态；
- `source_checkpoints`：某个消费器对该文件处理到了哪里。

一个 rollout 文件可以同时有：

```text
metadata checkpoint
usage checkpoint
```

元数据扫描推进到文件末尾，不得推进 usage checkpoint。这样 Spec 04 启用时仍可从 usage offset 0 回放历史 `token_count`。

### 5.4 Thread 与来源保持一对多

一个 Thread 可能同时存在普通和归档 rollout 副本：

```text
多个 source_files → 一个 thread_id
```

因此只允许 `source_files.thread_id` 指向 Thread；`threads` 不保存 `source_file_id`。

### 5.5 多来源元数据先合并再持久化

`state_5.sqlite`、`session_index.jsonl` 和 rollout 可以同时提供元数据。存储模块不按扫描顺序决定优先级。

Spec 02 必须先完成全来源合并；有稳定 Thread 字段变化时产生规范化 `ResolvedThreadPatch`，无变化时产生 `None`。`commit_metadata` 只接受包含该可选结果的 `MetadataThreadCommit`。

### 5.6 不建立第二套持久化事实

不建立：

- JSON/plist 解析缓存；
- 独立 offset 文件；
- Dashboard 快照文件；
- Session 聚合缓存文件。

SQLite 是唯一 MU 持久化事实来源，内存只是运行时加速层。

---

## 6. 技术选择

### 6.1 SQLite 驱动

使用 `rusqlite` 和 bundled SQLite：

```toml
rusqlite = { version = "实施时锁定版本", features = ["bundled"] }
```

理由：

- 单机、单主要写入者；
- 不需要网络数据库协议；
- bundled SQLite 固定运行版本；
- 同步扫描和写入可放入 `spawn_blocking`，不阻塞 Tokio async executor。

### 6.2 Migration

使用按版本排序、编译进二进制的 SQL migration，不引入额外 migration 框架。

唯一 schema 版本来源：

```sql
PRAGMA user_version;
```

metadata parser 和 usage parser 的版本与 schema version 相互独立。

### 6.3 数据库访问模型

- 一个主要写入流程；
- 短事务；
- 批量提交，不逐行提交；
- 扫描和写入运行在阻塞任务；
- 查询使用独立只读或短生命周期连接；
- 浏览器不能直接打开数据库。

---

## 7. 模块布局与 interface

推荐最小布局：

```text
src/
├─ main.rs
├─ lib.rs
├─ domain.rs
└─ storage/
   ├─ mod.rs
   ├─ migrations.rs
   └─ schema/
      └─ 0001_initial.sql
```

规则：

- `domain.rs` 只保存当前 Spec 实际使用的类型；
- `storage` 隐藏连接、SQL、migration 和事务；
- 不为每张表建立浅层 repository；
- Spec 04 再增加 Token 领域类型和 migration；
- 只有出现第二个真实存储 adapter 时才抽象 storage trait。

### 7.1 对外 interface

```rust
Ledger::open(options) -> Result<Ledger>

Ledger::app_state() -> Result<AppState>

Ledger::scan_status_snapshot(target_scan_id?) -> Result<ScanStatusSnapshot>

Ledger::record_source_observations(batch) -> Result<SourceOutcome>

Ledger::load_metadata_scan_state(source_file_ids) -> Result<MetadataScanState>

Ledger::commit_metadata(batch) -> Result<CommitOutcome>

Ledger::require_checkpoint_rebuild(command) -> Result<CheckpointOutcome>

Ledger::mark_scan_started(event) -> Result<ScanState>

Ledger::reserve_scan_followup(event) -> Result<ScanState>

Ledger::mark_followup_started(event) -> Result<ScanState>

Ledger::mark_followup_start_failed(event) -> Result<ScanState>

Ledger::mark_scan_completed(event) -> Result<ScanState>

Ledger::mark_scan_failed(event) -> Result<ScanState>
```

语义：

- `open`：创建目录、打开数据库、设置 PRAGMA、执行 migration、验证 `CODEX_HOME`；
- `app_state`：读取两个 revision、扫描状态和数据源绑定状态；
- `scan_status_snapshot`：在同一 SQLite 只读事务中返回 app_meta 当前投影与可选 `scan_runs` target 行；不得先查 app state 再另开事务查 target；
- `record_source_observations`：保存 rollout 物理身份和当前文件状态；Spec 01/v1 本身不推进消费进度，Spec 04 build 存在时仅按 15.1 原子创建/reset usage checkpoint 与维护/replace manifest；
- `load_metadata_scan_state`：在一个只读事务中批量返回指定来源的 `source_files`、metadata checkpoint 和匹配状态明确的 safe fact，供 Spec 03 计划与分组；
- `commit_metadata`：原子保存规范化 Thread patch、可选 rollout binding/safe fact/checkpoint；Spec 04 上线后同一事务还执行由 binding/root 前后态推导的 active usage reconcile 与 build disposition；
- `require_checkpoint_rebuild`：将指定消费器 checkpoint 标记为待重建，不影响其他消费器；
- 扫描生命周期与 follow-up 方法：保存 active/排队/启动失败状态并按 15.4 递增 `status_revision`；
- interface 不暴露 `rusqlite::Connection`、SQL 或数据库行类型。

`AppState/ScanState` 的扫描部分固定包含 `status_revision`、`scan_state`、`active_scan_id`、`last_finished_scan_id/result`、全部可空时间/错误、`source_binding_status`，以及 follow-up 的 ID/state/trigger/requested/enqueued-revision/error。`ScanStatusSnapshot` 在此基础上增加可选 `target_scan: ScanRun`。所有 lifecycle 方法返回其自身事务的 post-commit `ScanState`。

`SourceOutcome` 必须返回每个 observation 对应的 `source_file_id`、当前 generation、是否新建/移动/换代、换代导致的 rebuild consumers，以及 Spec 04 上线后的 `build_disposition=unchanged|member_added|completion_invalidated|carry_resumed_present|replaced`；`carry_resumed_present` 只表示 carry-in-progress 来源以匹配冻结身份恢复 present、计划仍须 ResumeCarry，不能当作普通 completion invalidation。调用方不得靠路径再次猜测来源 ID，也不得在事务外补写 manifest。

`MetadataScanState` 的每项固定为：

```text
source: SourceFileState
metadata_checkpoint: None | MetadataCheckpointState
safe_fact: None | Matching(RolloutMetadataFact) | Stale(SafeFactMismatchReason)
```

读取方法必须在同一 SQLite 快照中完成批量查询。`Matching` 只有在 generation、parser version、checkpoint offset、confirmed binding 和 owning ID 全部满足 11.4 不变量时才能返回；否则返回 `Stale`，不得把不一致的 fact 交给 resolver。

`commit_metadata` 的公开入参固定为：

```text
MetadataCommitBatch {
  groups: [MetadataThreadCommit]
}

MetadataThreadCommit {
  thread_id
  resolved_patch: None | Some(ResolvedThreadPatch)
  sources: [MetadataSourceCommit]   # patch-only 时为空
}

MetadataSourceCommit {
  source_file_id
  expected_file_generation
  expected_previous_thread_id?
  confirmed_owning_thread_id
  safe_fact                         # 完整、已合并的来源事实
  metadata_checkpoint_advance
}
```

同一批次可包含多个 Thread 组；每组独立事务提交，组内任一来源前置条件失败则整组回滚。`safe_fact` 不是隐藏副作用或可省略字段。`resolved_patch=None` 专门表示 resolver 确认 Thread 稳定查询字段无变化，但来源 binding、完整 safe fact 与 metadata checkpoint 仍需原子提交；`resolved_patch=None` 且 `sources` 为空是无效空组。

### 7.2 `ResolvedThreadPatch`

`commit_metadata` 接收的是 Spec 02 已按全部可用来源合并后的规范化 patch，不是某个来源的原始字段。

可空字段使用三态，而不是裸 `Option<T>`：

```text
Keep       # 不改变已有值
Set(value) # 写入新的可信值
Clear      # 已完成全来源重算，确认应清空
```

规则：

- 低优先级来源的 `null` 必须产生 `Keep`，不能产生 `Clear`；
- 较旧的来源记录不得覆盖较新的规范化结果；
- 只有完成当前 Thread 的全来源重算后才能使用 `Clear`；
- patch 携带 `resolved_at_ms`；比现有 `metadata_resolved_at_ms` 更旧的 patch 被拒绝；
- 同优先级可信来源冲突时写入选定值，同时设置 `metadata_quality_status=conflict`；
- 存储模块不重新解释来源优先级。

### 7.3 多来源字段优先级

Spec 02 必须遵守以下最低规则：

| 规范化字段 | 优先级，从高到低 | 补充规则 |
|---|---|---|
| `title` | `state_5.threads.name/title` → `session_index.name` → 空 | rollout 正文不得生成标题 |
| `project_path` | 主 Thread 初始 `session_meta.cwd` → 主 Thread 初始 Turn cwd → `state_5.threads.cwd` | Subagent cwd 不覆盖根 Session 项目 |
| `parent_thread_id` | state explicit edge → owning `payload.parent_thread_id` → nested `source.subagent.thread_spawn.parent_thread_id` → 受限 `forked_from_id` → 空 | state 无 child edge 不是“无父”证据；无法确认时不能猜测 |
| `root_session_id` | 根据已确认父链计算 | 父链不完整时为空 |
| `agent_role` | 已确认父链/角色元数据 → `unknown` | `unknown` 不是产品角色 |
| `archived` | `state_5.threads.archived` → 目录区域 | 状态索引更新更旧时不能覆盖较新观察 |
| `current_rollout_path` | `state_5.threads.rollout_path` → 当前存在的普通目录副本 → 归档副本 | 路径只是投影，不是主键 |
| `metadata_model` | `state_5.threads.model` → 最新明确 `turn_context.model` → 空 | 不代表 `models_used` 或模型用量 |
| `created_at_ms` | `state_5.threads.created_at_ms/created_at` → `session_meta` 时间 | 取可确认的原始创建时间 |
| `updated_at_ms` | `state_5` 更新时间 → 最新元数据事件时间 | 文件移动 mtime 不能冒充活动时间 |

如果同一优先级来源冲突，Spec 02 必须使用稳定、可测试的决策规则，禁止“最后扫描者覆盖”。

---

## 8. 数据库位置与打开规则

默认位置：

```text
~/Library/Application Support/Usagi/mu.sqlite3
```

WAL 文件：

```text
mu.sqlite3-wal
mu.sqlite3-shm
```

测试必须显式使用临时目录。

打开规则：

1. 父目录不存在时创建；
2. 不创建或修改 `CODEX_HOME`；
3. 新数据库执行全部 migration；
4. 已有数据库按 `user_version` 顺序升级；
5. schema 版本高于程序支持版本时拒绝写入；
6. migration 失败时保留旧数据库；
7. 数据库损坏时不静默删除或重建；
8. 校验调用方传入的规范化 `CODEX_HOME` fingerprint。

---

## 9. `CODEX_HOME` 绑定

### 9.1 Fingerprint

fingerprint 基于规范化绝对路径生成不可逆摘要，不包含认证信息、目录内容或账号信息。

### 9.2 首次绑定

新数据库：

```text
codex_home_fingerprint = null
→ 写入当前 fingerprint
→ source_binding_status = ready
→ status_revision 保持 0
```

首次绑定属于数据库初始化，不视为运行期间的状态变化，因此不递增 `status_revision`。只有已完成初始绑定后发生来源不一致、恢复或其他可观察状态迁移时才递增。

### 9.3 相同来源

fingerprint 一致时正常打开和写入。

### 9.4 来源不一致

fingerprint 不一致时：

```text
source_binding_status = source_changed
status_revision + 1
拒绝所有常规采集写入
保留旧数据库可读
```

若同一事务前存在 `followup_state=queued`，同时将其改为 `start_failed`并写入 `SOURCE_CHANGED`；整个 binding + follow-up 迁移只计一次 `status_revision + 1`。这使排队手动请求不会在来源变更后无限保持 queued。

不得把两个 Codex Home 数据静默合并。

恢复必须是显式操作：重建当前数据库，或切换到另一个数据库文件。具体用户交互由 Spec 03、05、06 定义；Spec 01 只负责检测和阻止混写。

---

## 10. SQLite 运行参数

每个连接必须设置并验证：

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
```

规则：

- 写事务使用 `BEGIN IMMEDIATE`；
- 不逐条提交；
- 不在打开数据库时无条件 `VACUUM`；
- WAL checkpoint 由后续维护任务低频触发；
- 测试必须断言 `foreign_keys` 已实际开启。

---

## 11. 历史首版数据库 schema（v1，`0001` 不可改写）

Spec 01 创建六张表：

```text
app_meta
scan_runs
source_files
source_checkpoints
rollout_metadata_facts
threads
```

### 11.1 `app_meta`

只允许一行。

| 字段 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `id` | INTEGER | PK，固定 1 | 单行键 |
| `metadata_parser_version` | INTEGER | NOT NULL，非负 | **历史 v1 列**；current v4 已删除，不再是 parser authority |
| `data_revision` | INTEGER | NOT NULL，非负 | 所有稳定查询事实版本 |
| `status_revision` | INTEGER | NOT NULL，非负 | 扫描和数据源状态版本 |
| `scan_state` | TEXT | NOT NULL，枚举 | `idle` / `running` / `failed` |
| `active_scan_id` | TEXT | 可空 | 当前扫描 ID |
| `last_finished_scan_id` | TEXT | 可空 | 最近完成或执行失败的已启动扫描 ID |
| `last_finished_scan_result` | TEXT | 可空枚举 | `completed` / `failed`，与 ID 同时空/非空 |
| `last_scan_started_at_ms` | INTEGER | 可空 | 最近扫描开始时间 |
| `last_scan_completed_at_ms` | INTEGER | 可空 | 最近成功完成时间 |
| `last_scan_failed_at_ms` | INTEGER | 可空 | 最近失败时间 |
| `last_scan_error_code` | TEXT | 可空 | 不含正文的错误码 |
| `followup_scan_id` | TEXT | 可空 | 单槽 follow-up 的预留 scan ID |
| `followup_state` | TEXT | 可空枚举 | `queued` / `start_failed` |
| `followup_trigger` | TEXT | 可空 | 首个合并请求的 trigger |
| `followup_requested_at_ms` | INTEGER | 可空 | 首次排队时间 |
| `followup_enqueued_status_revision` | INTEGER | 可空非负 | 首次排队事务的 revision |
| `followup_error_code` | TEXT | 可空 | 仅 `start_failed` 非空 |
| `last_full_import_completed_at_ms` | INTEGER | 可空 | **历史 v1 列**；current v4 已删除，无生产写入/消费契约 |
| `codex_home_fingerprint` | TEXT | 可空 | 绑定的数据源摘要 |
| `source_binding_status` | TEXT | NOT NULL，枚举 | `unbound` / `ready` / `source_changed` |

`data_revision` 定义为：

> 任何会改变 API 稳定查询结果的事实提交后增加一次。

Spec 01 阶段由 Thread 元数据变化触发；Spec 04 上线后，有效用量事件变化也触发同一个 revision。

`status_revision` 由扫描开始、完成、失败、follow-up 首次排队/启动失败和初始化完成后的数据源绑定变化触发，即使 `data_revision` 不变也要增加。新库首次绑定是初始化的一部分，不递增。

follow-up 列是持久化单槽的当前投影：全空表示无 follow-up。`queued` 时 ID/trigger/requested/revision 全部非空且 error 为空；`start_failed` 时额外要求 error 非空。`followup_scan_id` 不得等于 `active_scan_id`。新请求只能复用 queued ID，不能建第二个排队项。`last_finished_*` 也只是界面展示投影，禁止当作指定 target 的唯一终态证明。

### 11.2 `scan_runs`

每个 scan ID 一行，v1 不删除历史记录；它是 refresh target 生命周期的唯一持久化事实。

| 字段 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `scan_id` | TEXT | PK | 永不复用的 UUID |
| `trigger` | TEXT | NOT NULL | Startup/Scheduled/Manual/SourceChanged/Rebuild |
| `request_kind` | TEXT | NOT NULL 枚举 | `direct` / `followup` |
| `state` | TEXT | NOT NULL 枚举 | `queued` / `running` / `completed` / `failed` / `start_failed` |
| `requested_at_ms` | INTEGER | NOT NULL，非负 | 请求或预留时间 |
| `enqueued_status_revision` | INTEGER | 可空非负 | follow-up 首次排队 revision |
| `started_at_ms` | INTEGER | 可空非负 | started commit 时间 |
| `started_status_revision` | INTEGER | 可空非负 | started commit revision |
| `finished_at_ms` | INTEGER | 可空非负 | 终态事务时间 |
| `terminal_status_revision` | INTEGER | 可空非负 | 终态事务 revision |
| `error_code` | TEXT | 可空 | 不含正文的固定错误码 |

CHECK 固定为：

- `queued`：`request_kind=followup`，enqueued 字段非空，started/terminal/error 字段为空；
- `running`：started time/revision 非空，terminal/error 为空；direct 的 enqueued 为空，followup 的 enqueued 保留；
- `completed`：started 与 terminal time/revision 非空，`error_code=null`；
- `failed`：started 与 terminal time/revision 非空，安全 error 非空；`SCAN_CANCELLED` 与 `SCAN_INTERRUPTED` 都以 `failed` 状态保存，由 `error_code` 区分；
- `start_failed`：enqueued 与 terminal time/revision 非空，started 为空，安全 error 非空；
- 终态行不可重新进入 queued/running；scan ID 不可复用。

建立 `idx_scan_runs_state` 供启动恢复验证与测试；正常 target 查询始终使用主键。

### 11.3 `source_files`

本表只管理：

```text
$CODEX_HOME/sessions/**/rollout-*.jsonl
$CODEX_HOME/archived_sessions/**/rollout-*.jsonl
```

`state_5.sqlite` 每轮通过只读查询读取；`session_index.jsonl` 的更新和兼容策略由 Spec 02 明确定义，二者不使用本表的 rollout 字节 checkpoint。

| 字段 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `source_file_id` | INTEGER | PK | MU 内部来源键 |
| `thread_id` | TEXT | 可空 | 识别后的 Thread ID |
| `current_path` | TEXT | NOT NULL，UNIQUE | 当前规范化绝对路径 |
| `source_area` | TEXT | NOT NULL，枚举 | `sessions` / `archived_sessions` |
| `device_id` | INTEGER | NOT NULL | device ID |
| `inode` | INTEGER | NOT NULL | inode |
| `file_generation` | INTEGER | NOT NULL，正数 | 物理代次 |
| `observed_size` | INTEGER | NOT NULL，非负 | 文件大小 |
| `observed_mtime_ns` | INTEGER | NOT NULL | 纳秒 mtime |
| `file_status` | TEXT | NOT NULL，枚举 | `present` / `missing` / `replaced` |
| `last_seen_at_ms` | INTEGER | NOT NULL | 最近枚举时间 |

关键约束：

```text
UNIQUE(current_path)
UNIQUE(device_id, inode, file_generation)
file_generation > 0
observed_size >= 0
```

不使用 `AUTOINCREMENT`；SQLite `INTEGER PRIMARY KEY` 已满足当前内部键需求。

路径不是业务身份。文件移动时优先更新同一物理来源的 `current_path` 和 `source_area`。

纯移动保持 `source_file_id`、generation 和已确认 `thread_id`。文件替换、截断或原地改写导致 generation 变化时，必须在来源观察事务内清空旧 `thread_id` 并把所有已存在 consumer 标记为 `rebuild_required`；新 generation 必须重新确认 owning Thread。

### 11.4 `source_checkpoints`

保存不同消费器的独立进度。

| 字段 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `source_file_id` | INTEGER | NOT NULL，FK CASCADE | 物理来源 |
| `consumer_kind` | TEXT | NOT NULL，枚举 | `metadata` / `usage` |
| `parser_version` | INTEGER | NOT NULL，非负 | 当前消费器解析版本 |
| `committed_offset` | INTEGER | NOT NULL，非负 | 已提交完整行之后位置 |
| `guard_hash` | BLOB | 可空 | offset 附近字节摘要 |
| `processing_status` | TEXT | NOT NULL，枚举 | `pending` / `ready` / `rebuild_required` / `error` |
| `last_successful_scan_at_ms` | INTEGER | 可空 | 最近成功处理时间 |
| `last_error_code` | TEXT | 可空 | 不含正文的错误码 |

主键：

```sql
PRIMARY KEY (source_file_id, consumer_kind)
```

关键约束：

```text
committed_offset >= 0
parser_version >= 0
```

`committed_offset <= source_files.observed_size` 是跨表事务不变量，SQLite 单表 `CHECK` 无法直接引用父表。持久化模块必须在推进 checkpoint 的同一事务中读取并校验当前 `observed_size`；不满足时拒绝提交并将该消费器标记为 `rebuild_required`。

创建规则：

- Spec 02/03 为 rollout 创建 `metadata` checkpoint；
- Spec 04 为同一 rollout 创建 `usage` checkpoint；
- 如果 usage checkpoint 尚不存在，默认从 offset 0 开始；
- metadata 提交只能推进 metadata checkpoint；
- usage 提交只能推进 usage checkpoint；
- 任一消费器重建不改变另一消费器 offset；
- checkpoint 只推进到换行结束的完整 JSONL 记录之后。

### 11.5 `rollout_metadata_facts`

保存 Spec 02 从单个 rollout 得到的最小、安全、可重算来源事实。它解决增量扫描 Skip 未变化文件后，resolver 仍需获得全部 present rollout 候选值的问题；不是 JSONL 缓存，不保存正文或原始 payload。

| 字段 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `source_file_id` | INTEGER | PK，FK CASCADE | 对应物理来源 |
| `file_generation` | INTEGER | NOT NULL，正数 | fact 所属内容代次 |
| `metadata_parser_version` | INTEGER | NOT NULL，非负 | 生成 fact 的 parser 版本 |
| `resolved_through_offset` | INTEGER | NOT NULL，非负 | fact 已覆盖的完整行 offset |
| `owning_thread_id` | TEXT | NOT NULL | 已确认 owning Thread |
| `continuation_state` | TEXT | NOT NULL，枚举 | `owning_live` / `unstable` |
| `cwd` | TEXT | 可空 | rollout 允许字段解析后的来源候选 |
| `cwd_provenance` | TEXT | 可空，枚举 | `session_meta` / `turn_context`；`cwd` 非空时必填 |
| `cwd_record_offset` | INTEGER | 可空，非负 | 产生 cwd 候选的记录起始 byte offset |
| `created_at_ms` | INTEGER | 可空 | 来源创建时间候选 |
| `latest_context_model` | TEXT | 可空 | 最新 owning context 模型候选 |
| `latest_context_at_ms` | INTEGER | 可空 | 对应 context 时间 |
| `parent_thread_id_hint` | TEXT | 可空 | 允许的父关系 hint |
| `parent_hint_provenance` | TEXT | 可空，枚举 | v1 历史值为 `subagent_source` / `forked_from_id`；current v4 另允许 `session_meta_parent`；hint 非空时必填 |
| `parent_hint_record_offset` | INTEGER | 可空，非负 | 产生 parent hint 的记录起始 byte offset |
| `agent_role_hint` | TEXT | 可空 | 允许的角色 hint |
| `agent_role_provenance` | TEXT | 可空，枚举 | `session_meta_role` / `subagent_source`；hint 非空时必填 |
| `agent_role_record_offset` | INTEGER | 可空，非负 | 产生 role hint 的记录起始 byte offset |
| `replay_start_offset` | INTEGER | 可空，非负 | 已确认 replay 区间起点 |
| `owning_records_start_offset` | INTEGER | 可空，非负 | 已确认 owning live 起点 |
| `ownership_confidence` | TEXT | NOT NULL，枚举 | `confirmed` / `unresolved` |
| `fact_quality_status` | TEXT | NOT NULL，枚举 | `complete` / `partial` / `conflict` |
| `updated_at_ms` | INTEGER | NOT NULL | 最近成功生成时间 |

不变量：

- generation、parser version、resolved offset 必须与对应 metadata checkpoint 和 source 状态匹配后才能供 resolver 使用；
- `resolved_through_offset` 必须等于同事务提交后的 metadata `committed_offset`；
- `continuation_state=owning_live` 才能支持非零 offset 续读；
- resolver 可用的 fact 必须满足 `rollout_metadata_facts.owning_thread_id = source_files.thread_id`；加载为 `OwningLive` 时，其 typed state 中的 `owning_thread_id` 也必须等于这两个值；任一不等即为 stale/conflict，不能分组或续读；
- `cwd`、`parent_thread_id_hint`、`agent_role_hint` 的值、provenance 与 record offset 必须三者同时为空或同时非空；解析器按 provenance 比较优先级，不能把持久化单值当作无来源值；
- cwd 优先级为 `session_meta > turn_context`；current parent hint 优先级为 `session_meta_parent > subagent_source > forked_from_id`；role hint 为 `subagent_source > session_meta_role`。同 provenance 固定保留 byte offset 最小的第一条可信记录；任意两个非空可信 parent 候选值不一致都必须记录 conflict，高优先级候选仅决定 winner，不得吞掉冲突诊断；
- generation 变化时旧 fact 在来源观察事务内删除；
- parser version 变化时旧 fact 保留作诊断但视为 stale，不能参与 resolver；成功重建后原子替换；
- 不允许增加 JSON/BLOB 原文列、标题正文或任意消息内容。

### 11.6 `threads`

| 字段 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `thread_id` | TEXT | PK | Codex Thread ID |
| `parent_thread_id` | TEXT | 可空 | 直接父 Thread ID |
| `root_session_id` | TEXT | 可空 | 最上层主 Thread ID |
| `agent_role` | TEXT | NOT NULL，枚举 | `main` / `subagent` / `unknown` |
| `title` | TEXT | 可空 | Codex 已有标题 |
| `project_name` | TEXT | 可空 | 项目显示名 |
| `project_path` | TEXT | 可空 | 规范化工作目录 |
| `metadata_model` | TEXT | 可空 | 单个可确认模型元数据 |
| `created_at_ms` | INTEGER | 可空 | 创建时间 |
| `updated_at_ms` | INTEGER | 可空 | 原始元数据更新时间 |
| `archived` | INTEGER | NOT NULL，0/1 | 归档状态 |
| `current_rollout_path` | TEXT | 可空 | 当前选定路径投影 |
| `metadata_quality_status` | TEXT | NOT NULL，枚举 | `complete` / `partial` / `conflict` |
| `metadata_resolved_at_ms` | INTEGER | NOT NULL | 最近规范化合并时间 |

本表没有 `source_file_id`。多个 `source_files` 可通过 `thread_id` 对应同一 Thread。

关系规则：

- `main`：`parent_thread_id = null` 且 `root_session_id = thread_id`；
- `subagent`：`parent_thread_id != null`；父链未完整时 `root_session_id` 可暂为空；
- `unknown`：仅为待解析内部状态，`root_session_id` 必须为空；
- `unknown` 不生成正式根 Session、不计入 Session 数量；
- 关系修复后必须变为 `main` 或 `subagent`；
- 父 Thread 暂缺时允许保存记录，因此父字段不设强外键；
- 不能因关系缺失把 Subagent 猜成主 Session。

`metadata_model` 不等于 v0.2 的 `models_used`，禁止用于模型用量统计。Spec 04 必须根据有效用量事件发生时的模型计算 `models_used`。

---


## 11.8 Current latest schema（v4）强制叠加规则

Spec08 完成后，`Ledger::open()` 的 latest schema 必须为 `user_version=4`。当前运行时以 v4 为准。

### `app_meta`

current v4 **不得存在**：

```text
metadata_parser_version
last_full_import_completed_at_ms
```

首次导入、同步状态和稳定用量是否可读，只能由现有真实状态组合判断：

```text
scan_state / scan_runs / followup
source_binding_status
usage_active_epoch / usage_build_epoch
source/checkpoint/build 状态
```

不得重新增加一个“完整导入时间”投影来代替真实状态。

### `rollout_metadata_facts`

current v4 的 parent provenance 枚举必须包含：

```text
session_meta_parent
subagent_source
forked_from_id
```

其中 `session_meta_parent` 专指 owning `session_meta.payload.parent_thread_id`，不得伪装成 `subagent_source`。

### Metadata parser version authority

```text
代码当前版本：METADATA_PARSER_VERSION = 2
durable source version：
  source_checkpoints.parser_version
  rollout_metadata_facts.metadata_parser_version
```

不存在第二个 global app-meta parser-version authority。旧 v1 fact/checkpoint 只能因 mismatch 被真实重放后升级，禁止 SQL 直接改版本号。


## 12. 后续当前版本 Token migration

Spec 04 通过新 migration 创建：

```text
usage_events
usage_event_occurrences
turns
ingest_anomalies
usage_source_states
usage_build_sources
```

以上共六张 Token 表；`usage_event_occurrences` 是 canonical event 与每个来源 candidate 的规范化关系。

并启用：

```text
source_checkpoints.consumer_kind = usage
```

`0002` 通过 SQLite table-rebuild 为固定单行 `app_meta` 增加：`usage_active_epoch INTEGER NOT NULL DEFAULT 0 CHECK >=0`、可空正数 `usage_build_epoch`、`usage_parser_version INTEGER NOT NULL DEFAULT 0 CHECK >=0`、可空非负 `usage_build_parser_version`。两个 build 列必须同时为空或非空；非空时 `usage_build_epoch = usage_active_epoch + 1`。v1 升级单行确定初始化为 `0,NULL,0,NULL`，原列和值不变；失败整体回滚。

固定 `working_epoch = usage_build_epoch ?? usage_active_epoch`；每个来源唯一的 usage checkpoint 只对应 working epoch 及其目标 parser version，不新增 `build` 枚举。尚无 matching working state 的初始 pending/blocked 使用现有 `processing_status=rebuild_required`、offset 0；首批成功写入 state 后即切为 `ready`，即使 manifest 仍未完成也允许严格匹配 state/guard 后非零续读。由 rebuilt/carried 同身份追加或恢复而转成 pending 的 checkpoint 保持 ready；manifest 未完成本身不禁止续读。carry-in-progress 是唯一例外：checkpoint 仍 rebuild_required、无 working state，但非空 carry cursor 强制计划 `ResumeCarry`，不能误走文件 BuildFrom；finalize 才一次恢复 ready。BeginCarry 只接受 fresh rebuild_required+无 state，或以同一事务把严格匹配的 partial-BuildFrom ready state 退役：删除 working source state、重置 checkpoint 后保留 partial facts 为待全量回读验证的 seed，再初始化 cursor；不得只翻转 checkpoint 留下 state。若 carry 中来源以同一冻结身份重新 present，继续有界复制并验证 active prefix，finalize 后保持 pending，再从 active offset 读取；身份或 prefix guard 失配则原子 replacement。`usage_event_occurrences` 按 `(ledger_epoch,source_file_id,file_generation,source_start_offset)->event_id` 保存每个 candidate 的来源关系，包括 canonical event 已存在的跨副本 duplicate；active contributor、LocalReplay、Carried 与 epoch 清理以 occurrence 为准，不能用 `usage_events` 的首次来源代替。仅新增 occurrence 不改变稳定查询事实。`usage_source_states` 按 ledger epoch/source 持久化 usage checkpoint 对应的累计快照、连续/中断状态、活动 Turn、模型、物理身份、confirmed root、ownership continuation，以及 `observed_raw_size/raw_tail_status/raw_tail_start_offset`。raw-tail proof 随 epoch 激活留在 active state，供下一次 build 冻结；缺失或与 generation/raw size 不匹配只能 unverified/blocked。reset 只进入 anomaly/Turn block，可信 current total 的 source commit 最终为 continuous。旧 active/inactive state 不参与 working checkpoint 等式。`usage_build_sources` 还持久化 generation-scoped 的 `required_generation/required_through_offset`、`observed_raw_size`、`raw_tail_status/raw_tail_start_offset` 与分批 Carried 的 phase/after-cursors；present observation 在 generation/raw size 未变化时保留 verified tail，变化时标 unverified。每个 reader batch 可用同 generation 的 `last_complete_offset` 推进 required boundary，但只有明确 `fixed_view_exhausted` 的最终结果能把 tail 写为 none/half-line；中间批保持 unverified。carry 只有 active offset 覆盖 required boundary时可开始，最终批前不得恢复 checkpoint。build replacement 的 membership 必须包含旧 manifest 全集；同 generation 的 required boundary 不降低，replacement generation 从 0 建立新 boundary，旧 generation 字节 offset 不继承；build-only missing 成员只能保留可信 proof 或 blocked，不能消失。这些表是安全账本/恢复事实，不是聚合缓存。

Spec 04 的 parser 升级、truncate、replacement 或 usage state 不可信时使用 build epoch 从 offset 0 重建；新 epoch 完整激活前，查询继续读取旧 active epoch，不能在 active 事件表上猜测局部删除范围。

Spec 01 不预建这些表，是为了让 Token schema 与最终算法一同评审；这不代表它们不属于当前版本。

第一版不预建：

```text
daily_usage_rollups
session_daily_rollups
model_daily_rollups
```

Dashboard、Session 和模型先直接查询带索引的 `usage_events`。只有性能测量证明必要时才增加可重建 rollup。

---

## 13. 索引

```sql
CREATE INDEX source_files_thread_idx
    ON source_files(thread_id);

CREATE INDEX source_files_status_idx
    ON source_files(file_status);

CREATE INDEX source_checkpoints_status_idx
    ON source_checkpoints(consumer_kind, processing_status);

CREATE INDEX rollout_metadata_facts_thread_idx
    ON rollout_metadata_facts(owning_thread_id);

CREATE INDEX threads_parent_idx
    ON threads(parent_thread_id);

CREATE INDEX threads_root_idx
    ON threads(root_session_id);

CREATE INDEX threads_updated_idx
    ON threads(updated_at_ms);
```

Token 查询索引由 Spec 04 migration 创建。

---

## 14. Migration 原子性

规则：

- migration 只追加，不修改已发布版本；
- 文件名递增，例如 `0001_initial.sql`；
- SQL 编译进二进制；
- 不支持自动向下 migration；
- schema version 高于程序版本时拒绝写入。

每个 migration 必须执行：

```text
BEGIN IMMEDIATE
执行 migration SQL
设置 PRAGMA user_version = 目标版本
COMMIT
```

Migration SQL 与 `user_version` 必须在同一事务。中途退出时两者共同回滚，不能出现 schema 已变化但版本号未前进。

---

## 15. 事务和 revision

### 15.1 来源观察

`record_source_observations`：

```text
BEGIN IMMEDIATE
insert/update source_files
必要时创建 metadata checkpoint(offset=0)
generation 变化时清空旧 thread_id、删除旧 rollout_metadata_fact，并标记所有已有 consumer rebuild
若 usage build 存在：原子维护 usage_build_sources 或执行保留旧成员全集的 build replacement
COMMIT
```

Spec 04 上线后，`record_source_observations` 增加不可拆分的 build side effect：

- 当前有 build 时，任何本轮新建或此前未入 manifest 的 present rollout，必须在插入/更新 `source_files` 的同一事务中加入 `usage_build_sources`，冻结 generation、device/inode、当前 binding/root、目标 parser，设置 `required_generation=current generation`、`required_through_offset=0`、`observed_raw_size=current observed_size`、`raw_tail_status=unverified`，原因记为 `discovered_during_build`，并创建/重置其 usage checkpoint 为 target parser、offset 0、guard null、`rebuild_required`；已有成员 present observation 的 generation 与 raw size 都未变化时保留现有 verified tail，任一变化才更新 raw size并标 unverified，不得预先推进 required boundary；后续 usage reader 中间批只能用同 generation 的 `last_complete_offset` 单调推进 boundary并保持 unverified，只有 `fixed_view_exhausted=true` 的结果才能明确 tail 为 none 或 half-line；
- 已有 manifest 成员的 generation、device/inode 变化，或 observation 因 generation 变化清空 binding 导致 frozen binding/root 不再成立时，不更新冻结证据；同一事务调用 Spec 04 的 `replace_build_preserving_all_members`。replacement membership 必须包含旧 manifest 全集；未受影响且自洽的 build-only proof/progress 保留，受影响来源精确清理后 present→pending、missing→blocked，任何旧成员不得消失；
- 同一身份的 completed build 成员出现普通追加时，source raw size/tail-unverified 与 `rebuilt -> pending` 原子提交，保留 `ready` working checkpoint/state 以增量追平；已完成的 `carried` 来源同身份重新 present 时原子转 pending 并保持 ready，不同身份则执行上述 replacement；`rebuilt` 来源变 missing 且身份未变时保留完成证明；
- `carry_phase!=none` 的来源重新 present 时不得套用 completed-carried 分支：同一冻结身份及 active-prefix guard 匹配则保留 cursor、checkpoint=rebuild_required，继续 `ResumeCarry`；finalize 恢复 active offset 的 ready state 后仍为 pending，再读取新增区间。身份/prefix 失配则在本 observation 事务执行 replacement，清理部分复制行并保留旧 manifest 全集；
- completed 成员同身份、同大小恢复 present 时，即使 observation 先将其转 pending，generation/raw size 均匹配的既有 verified tail 也必须保留；checkpoint/state/guard/binding/root/parser 已严格匹配同 generation 的 required complete boundary 时可优先于普通 Skip 执行 completion-only。proof 缺失/unverified 则先走零增量 `VerifyRawTail`，不得 Skip；blocked 条件解除但无新增完整行时同理；
- 来源行变化与 manifest add/completion invalidation/carry resume/replacement 要么共同提交，要么共同回滚。`SourceOutcome` 明确返回 `build_disposition=unchanged | member_added | completion_invalidated | carry_resumed_present | replaced`；carry-in-progress 同身份恢复 present 必须返回 `carry_resumed_present`，scanner 据此保持 `ResumeCarry` 优先级，不在事务外补写 manifest。

由此不存在“`source_files` 已提交新身份，但 build manifest 仍保留旧身份”的崩溃窗口。binding/root 的 metadata 关系变更另按 Spec 04 root reconcile 的同事务废弃协议处理。

仅文件大小、mtime、路径或 `last_seen_at_ms` 变化不增加 `data_revision`。

### 15.2 元数据提交

```text
BEGIN IMMEDIATE
验证 CODEX_HOME 绑定
若存在则应用已合并的 ResolvedThreadPatch；None 表示稳定 Thread 事实无变化
验证每个 MetadataSourceCommit 的 generation、旧 binding、owning ID、fact provenance 与 checkpoint 前置条件
绑定/确认 source_files.thread_id
插入或替换对应完整 rollout_metadata_facts
更新 metadata checkpoint
若稳定查询事实变化：data_revision + 1
COMMIT
```

只更新来源 binding/safe fact/checkpoint、`resolved_patch=None` 的事务不改变 `data_revision`。这允许新增区间只有 TokenCount、Ignored 或没有改变 Thread 字段的已知记录时安全推进 metadata consumer；`status_revision` 也只由扫描生命周期接口更新，不在此事务变化。

来源级 safe fact、Thread 元数据与 metadata offset 要么共同提交，要么共同回滚。禁止先推进 offset，也禁止推进后仍保留旧 fact。

`commit_metadata` 的 checkpoint 列表为可选字段，单个批次可以携带同一 Thread 组的多个来源绑定和 metadata checkpoint：

- rollout 解析批次必须携带来源绑定、更新后的 safe fact 和 metadata checkpoint；
- `state_5.sqlite`、`session_index.jsonl` 产生的规范化 patch 使用 patch-only 事务，不伪造 rollout source 或 offset；
- patch-only 事务仍在稳定查询事实变化时递增一次 `data_revision`；
- confirmed owning Thread 绑定、rollout patch 与 metadata checkpoint 必须处于同一事务；ID 冲突时不得覆盖已有 `source_files.thread_id`，也不得推进 checkpoint。
- 写入前对每个来源执行 CAS 前置校验：数据库当前 `source_files.thread_id` 必须等于命令中的 `expected_previous_thread_id`；该期望值可以为空，非空时必须等于 `confirmed_owning_thread_id`。空值表示允许本事务建立首次 binding，不能因尚未绑定而拒绝。
- 完成 binding 写入后，再验证 `MetadataThreadCommit.thread_id = 每个 confirmed_owning_thread_id = safe_fact.owning_thread_id = source_files.thread_id`；若 `resolved_patch=Some`，还必须等于 `resolved_patch.thread_id`。`owning_live` continuation 由该 ID 构造。前置 CAS 或写入后等式任一失败都使该 Thread 组整组回滚。

Spec 04 上线后，`commit_metadata` 是唯一 metadata/usage 联合提交入口，不允许提交成功后再调用独立 reconcile：

1. `MetadataCommitBatch` 保留每个 binding 与 Thread/root patch 的 expected previous 值；事务内计算 binding/root 的前后态；
2. 若无 usage build，root 变化时在写 metadata 的同一事务更新 active `usage_events.root_session_id` 与对应 active `usage_source_states.root_session_id`；首次 binding 尚无 active usage 时无需伪造 usage state；
3. 若有 build，只要 manifest 成员的 binding 或 root 前后态变化（包括 `None -> confirmed`），先提交新 metadata 前态验证，再在同一事务更新 active usage root（如适用）并调用 `replace_build_preserving_all_members`；旧 manifest 全集必须进入 replacement；
4. safe facts、metadata checkpoints、Thread/binding patch、active usage reconcile、build manifest/rows/app_meta disposition、恢复后的 usage checkpoints 和 revisions 共同 commit/rollback；整个联合事务的稳定查询变化只让 `data_revision + 1`；
5. storage 内部可调用接受当前 transaction handle 的私有 `reconcile_usage_for_metadata_change`，但不得暴露成 scanner 事后调用的第二事务。

### 15.3 未来 usage 提交

Spec 04 必须保持同一原则：

```text
usage_events / usage_event_occurrences / turns / anomalies / usage source state
+ usage checkpoint
+ data_revision
= 同一事务
```

### 15.4 扫描状态

`mark_scan_started`：

- 只允许在 `source_binding_status=ready`、无 active scan 且无 `queued` follow-up 时直接开始；旧 `start_failed` follow-up 属于终态，新的直接开始事务必须在同一事务清空 app_meta 的全部 follow-up 投影列，但不得删除或改写对应的终态 `scan_runs` 行；
- 插入唯一 `scan_runs(state=running,request_kind=direct)` 行，写入 requested/started time；
- `scan_state=running`；
- `active_scan_id=scan_id`；
- 更新开始时间；
- `status_revision + 1`；
- 把 post-increment revision 写入 `scan_runs.started_status_revision`；
- 事务成功后返回该事务的 post-commit `ScanState`，其 `active_scan_id` 为新 ID，`status_revision` 为本次 started revision。返回值与状态更新来自同一事务，不允许调用方事后另查 `app_state()` 拼接因果锚点。

`reserve_scan_followup`：

- 只允许 `source_binding_status=ready` 且当前 active scan 仍为 running 时调用；
- 单槽为空时插入新的 `scan_runs(state=queued,request_kind=followup)`，并写入 app_meta 预留 scan ID、trigger 和请求时间，设 `followup_state=queued`、`status_revision + 1`，把 post-increment revision 同时写入 scan row 和 `followup_enqueued_status_revision`；
- 已有 `queued` 时验证对应 scan row 仍为 queued，不创建新 ID、不改写首次 trigger/time、不增加 revision，直接返回同一排队项；
- active scan ID 已变或不再 running 时 CAS 失败，协调器必须重新按当前状态线性化请求，不能返回假 Coalesced。

`mark_followup_started`：

- 只允许在 `source_binding_status=ready`、无 active scan、`followup_state=queued`、ID 匹配且对应 scan row 仍为 queued 时调用；来源已变化时必须改调 `mark_followup_start_failed(SOURCE_CHANGED)`；
- 同一事务将 scan row 更新为 running 并写 started time，原子清空 follow-up 槽、将预留 ID 设为 active、写入 `scan_state=running`并 `status_revision + 1`，把 post-increment revision 写入 row.started revision；不存在可观察的“队列消失但 active 尚未建立”状态；
- 返回的 post-commit state 中 active ID 是 follow-up ID，它的 `status_revision` 是 follow-up started revision。

`mark_followup_start_failed`：

- 只允许 `followup_state=queued` 且 ID 匹配时调用；
- 仅用于非重试终态 `SCAN_START_FAILED` / `SCANNER_UNAVAILABLE` / `SOURCE_CHANGED`；SQLite Busy 不得写成永久 start_failed；
- 同一事务把 `scan_runs` row 写为 start_failed，写 terminal time/revision/error；app_meta 必须保留同一 follow-up ID、trigger、requested/enqueued revision，将 `followup_state` 改为 `start_failed` 并写安全 `followup_error_code`，随后 `status_revision + 1`；
- 若该失败标记事务也 busy，scan row 和单槽继续保持 queued，由协调器后续重试与进程启动恢复接管，不得留下无人处理的 queued。

`mark_scan_completed`：

- 只允许当前 `active_scan_id` 完成；
- 将对应 `scan_runs` running row 更新为 completed，写 finished time 和 terminal revision；
- `scan_state=idle`；
- 更新完成时间；
- 清空失败错误；
- 写入 `last_finished_scan_id=current active ID`、`last_finished_scan_result=completed`；
- 在同一事务设 `active_scan_id=null`；
- 保留 follow-up 槽（queued 或 start_failed）不变；因此当前扫描终态和 follow-up started 之间仍可观察队列，shutdown/source-changed 已记录的启动失败也不会被覆盖；
- `status_revision + 1`。

`mark_scan_failed`：

- 只允许当前 `active_scan_id` 失败；
- 将对应 `scan_runs` running row 更新为 failed，写 finished time、terminal revision 和安全 error；正常扫描 hard error、启动恢复的 `SCAN_INTERRUPTED` 以及 shutdown 的 `SCAN_CANCELLED` 都使用该同一终态，禁止产生另一种未定义的 cancelled 提交路径；
- `scan_state=failed`；
- 更新失败时间和结构化错误码；
- 写入 `last_finished_scan_id=current active ID`、`last_finished_scan_result=failed`；
- 在同一事务设 `active_scan_id=null`；
- 保留 follow-up 槽（queued 或 start_failed）不变；
- `status_revision + 1`；
- 不改变上次稳定业务数据。

即使扫描没有产生业务变化，前端也能通过 `status_revision` 得知开始、完成或失败。

固定 CHECK/事务不变量：`scan_state=running <=> active_scan_id IS NOT NULL`；`scan_state IN (idle,failed) <=> active_scan_id IS NULL`。app_meta active/follow-up 投影必须与对应 `scan_runs` running/queued 行一致。

---

## 16. 错误处理和恢复

| 错误 | 处理 |
|---|---|
| 数据库目录不可创建 | 启动失败，返回系统错误类别 |
| schema 版本过新 | 拒绝写入，不降级 |
| migration 失败 | 整体回滚 |
| busy 超时 | 当前事务失败，不推进 checkpoint |
| CHECK/FK 失败 | 当前事务回滚，视为实现或适配错误 |
| `CODEX_HOME` 不一致 | 标记 `source_changed`，拒绝采集写入 |
| 数据库损坏 | 停止写入，不自动删除 |
| 磁盘空间不足 | 事务回滚，保留旧数据 |
| 过期 scan ID 提交完成 | 拒绝，不能覆盖当前扫描状态 |
| 过期 metadata patch | 拒绝或记为未应用，不能覆盖新数据 |

错误和日志不得包含：

- JSONL 原始行；
- Prompt 或回复正文；
- 工具参数和输出；
- 认证信息。

允许包含路径、Thread ID、offset、SQLite 错误码和结构化错误码。

---

## 17. 实施步骤

### 步骤 1：领域类型

- 新增 `src/lib.rs`；
- 定义 AppState、SourceObservation、SourceCheckpoint、ResolvedThreadPatch、CommitOutcome、ScanEvent；
- 定义 patch 三态；
- 校验时间、大小、offset 和 generation；
- 不提前定义 Token 事件类型。

### 步骤 2：数据库打开

- 默认应用数据路径；
- 测试注入临时路径；
- 创建父目录；
- 设置和验证 PRAGMA；
- 验证 schema 与 `CODEX_HOME`。

### 步骤 3：首版 migration

- 创建六张表（含 `scan_runs`）；
- 创建 CHECK、UNIQUE、FK 和索引；
- 初始化 `app_meta`；
- 原子设置 `user_version=1`。

### 步骤 4：来源与 checkpoint

- 保存物理来源观察；
- 创建独立 metadata checkpoint；
- 支持 usage consumer 类型但不创建 usage 进度；
- 实现单消费器重建状态。

### 步骤 5：规范化元数据提交

- 只接受 `MetadataThreadCommit`，其中 `resolved_patch` 为 `None | Some(ResolvedThreadPatch)`；
- 实现三态字段；
- 拒绝过期 patch；
- Thread 和 metadata checkpoint 原子提交；
- 正确递增 `data_revision`。

### 步骤 6：扫描生命周期

- 实现 started/completed/failed；
- 校验 scan ID；
- 递增 `status_revision`；
- 扫描失败保留稳定数据。

### 步骤 7：测试和文档

- 使用临时数据库；
- 覆盖迁移、事务、双 checkpoint、来源切换和 revision；
- 更新 README；
- 不生成或提交真实用户数据库。

---

## 18. 测试方案

### 18.1 初始化

- 新数据库创建六张表，`scan_runs` 初始为空；
- `user_version=1`；
- `data_revision=0`、`status_revision=0`；active/last-finished/follow-up 全部列为 null；
- PRAGMA 生效；
- 重开不破坏数据。

### 18.2 来源身份

- 相同路径不能对应两个来源；
- device/inode/generation 唯一；
- 负大小和零 generation 被拒绝；
- 文件移动更新路径但保持来源 ID；
- generation 变化清空旧 Thread binding，并让所有已有 consumer 重建；
- 普通和归档副本可以同时指向同一 Thread。
- Spec 04 integration：build 中新发现 present 来源时，source row、manifest member 与 usage `rebuild_required` checkpoint 原子提交；任一写入失败全部回滚；
- Spec 04 integration：manifest 成员 generation/device/inode 或因 generation 导致的 binding 失配时，source observation 与 build replacement 原子提交；replacement 包含旧 manifest 全集，崩溃重开不存在新 source 身份配旧 proof；
- Spec 04 integration：rebuilt 来源同身份追加、carried 来源同身份重新 present 时，observation 与 manifest 转 pending 原子提交；rebuilt 来源变 missing 时保留最后 present boundary 证明；
- Spec 04 integration：初始 pending/blocked 无 working state 时为 rebuild_required/offset 0；有 matching working state 的中间批次或同身份恢复 pending 保持 ready，可非零续读或 CompleteOnly；
- Spec 04 integration：部分 BuildFrom ready state 后来源变 missing，BeginCarry 在一个事务退役 working state、重置 checkpoint 并初始化 cursor，partial facts 只作为 ResumeCarry 从 active 首 key 全量回读验证的 seed；任一步失败保持完整前态；
- Spec 04 integration：SourceOutcome 五个 disposition 都可达；carry-in-progress 同身份恢复 present 唯一返回 `carry_resumed_present`，scanner 继续 ResumeCarry，不能当作普通 completed-carried invalidation；
- Spec 04 integration：同 generation/raw size 的 observation 保留 verified tail；变化时标 unverified。reader 中间批即使推进 complete offset 也不能写 half-line，只有 fixed-view exhausted 结果可写 none/half-line；offset 已等于 raw size但 proof unverified 时走 VerifyRawTail 而非 Skip；
- Spec 04 integration：build 激活删除 manifest 后，active usage source state 仍保存 raw-tail proof；下一 build 的 missing contributor 只有 generation/raw size/proof 全匹配才可冻结并 carry，否则 blocked；

### 18.3 来源级 metadata fact

- fact 不含 JSONL 原文或正文；
- generation/parser/offset 匹配时才可读取；
- cwd、parent 和 role 候选都保留 provenance，后到低优先级候选不能覆盖高优先级候选；
- 三类候选同时保存 record offset；同 provenance 保留第一条可信记录，冲突只标记、不按后到覆盖；
- fact owning ID、source binding 与 `OwningLive` ID 任一不一致时批量读取返回 stale/conflict，提交整组回滚；
- group ID、patch ID 与组内任一来源 owning ID 不一致时提交整组回滚；
- 首次 binding 在写入前允许数据库 ID 与 expected previous 同为空；写入后必须进入完整 owning ID 等式；
- 批量读取在同一快照返回 source、metadata checkpoint 和 safe fact 的匹配状态；
- fact、Thread patch 和 metadata checkpoint 原子提交；
- 新增区间只有 TokenCount/Ignored 时以 `resolved_patch=None` 原子推进 fact/checkpoint，`data_revision` 不变；
- generation 变化删除旧 fact；
- 未变化来源可从 fact 恢复 resolver 输入而不打开正文；

### 18.4 独立 checkpoint

- 同一来源可同时具有 metadata 和 usage checkpoint；
- metadata offset 前进不改变 usage offset；
- usage offset 前进不改变 metadata offset；
- 任一 offset 不能超过 observed size；
- 任一消费器进入 rebuild 不改变另一消费器；
- 删除来源级联删除全部 checkpoint。

### 18.5 元数据合并提交

- `Keep` 不覆盖已有值；
- `Set` 写入可信值；
- `Clear` 只在全来源重算标记下允许；
- 旧 `resolved_at_ms` patch 不覆盖新值；
- 冲突状态可保存；
- `unknown` 不产生 root Session；
- 同一 Thread 可由多个来源发现且只保留一行 Thread。

### 18.6 原子性

- Thread 与 metadata checkpoint 同时提交；
- 制造约束失败后二者和 revision 均不变；
- 重复批次不产生重复 Thread；
- 一个事务最多增加一次 `data_revision`；
- 未提交退出后重开无部分写入。
- build manifest add/replacement 与来源 observation 的事务中途退出后，source、manifest、app_meta build 列和全部 usage checkpoints 保持同一前态或同一后态；
- build replacement membership 包含旧 manifest 全集；可信 build-only proof/progress 保留，不可信 present 来源重建，missing 来源继续 blocked，任何旧成员不得消失；
- `commit_metadata` 中 binding/root、safe facts/checkpoints、active usage reconcile 与 build disposition 任一失败全部回滚；覆盖 build 中首次 None→confirmed binding，不存在 commit 后补调用；

### 18.7 扫描状态

- started 插入 scan row 与 active 投影原子；completed/failed 更新 row 终态、last-finished 投影并清空 active 原子；每个迁移的 row revision 与 post-commit status revision 一致；
- `running <=> active ID 非空`、`idle/failed <=> active ID 为空`，queued 单槽必须对应 `scan_runs.state=queued`；所有 CHECK 和 CAS 失败整事务回滚；
- 当前扫描 running 时首个 follow-up 创建持久化单槽，后续请求复用同 ID/revision；当前终态后槽仍 queued，follow-up started 与清槽同事务；
- follow-up started Busy 保持 scan row/槽 queued；非重试 internal、shutdown 和 source changed 才持久化 `start_failed`；shutdown 中的 active scan 以 `failed + SCAN_CANCELLED` 终止；
- target scan T 终态后连续完成 F/G，按 T 主键仍可读其终态；start_failed 后又开始新 scan 不覆盖旧 target row；
- 不产生业务变化的扫描不增加 `data_revision`；
- 失败状态能在数据 revision 不变时被观察；
- 过期 scan ID 不能完成当前扫描；
- 失败不清空稳定数据。

### 18.8 `CODEX_HOME`

- 首次打开完成绑定；
- 相同 fingerprint 正常写入；
- 不同 fingerprint 标记 `source_changed`；
- 不同 fingerprint 下采集写入被拒绝；
- 旧数据库仍可读取；
- 不发生两个 Home 数据混合。

### 18.9 Migration

- 新建迁移成功；
- migration SQL 与 user_version 同事务；
- 制造失败时 schema 和版本号共同回滚；
- 高版本数据库拒绝写入；
- migration 重复打开无副作用。

### 18.10 隐私和范围

- schema 没有正文列；
- Spec 01 不创建 Token 表；
- Spec 总计划明确包含当前版本 Token Spec；
- 错误日志没有 fixture 正文哨兵；
- 测试不读取真实 `~/.codex`。

---

## 19. 独立验收标准

### 19.1 功能

- [ ] 创建并重开数据库成功；
- [ ] 创建六张基础业务表；`scan_runs` 保存不可覆盖的扫描生命周期，`rollout_metadata_facts` 只含安全结构化来源事实；
- [ ] 来源身份与消费进度分离；
- [ ] metadata 和 usage checkpoint 可独立存在；
- [ ] 规范化 Thread patch 可提交；
- [ ] 未变化 rollout 可从匹配 generation/parser/offset 的 safe fact 恢复 resolver 输入；
- [ ] safe fact 保留 cwd/parent/role provenance，且 owning ID 与来源绑定、`OwningLive` ID 强一致；
- [ ] safe fact 为 cwd/parent/role 保存 record offset，并固定 provenance 优先级与同 provenance 选择规则；
- [ ] Thread group ID、patch ID 与所有来源 owning ID 必须完全一致；
- [ ] binding 使用 expected previous CAS：首次绑定允许 None→confirmed，陈旧期望或非空冲突整组回滚；
- [ ] Ledger 提供批量 metadata scan state 读取，并在提交入参中显式接收完整 safe fact；
- [ ] 无 Thread 字段变化时支持 `resolved_patch=None` 的 source/fact/checkpoint 提交，且不增加 `data_revision`；
- [ ] Thread 与 metadata checkpoint 原子提交；
- [ ] 扫描生命周期可独立更新；
- [ ] `CODEX_HOME` 不匹配时阻止混写。

### 19.2 一致性

- [ ] 路径不作为 Thread 或 Session 主键；
- [ ] `threads` 不包含 `source_file_id`；
- [ ] 一个 Thread 可对应多个物理 rollout；
- [ ] metadata offset 不会越过 usage 历史；
- [ ] 多来源元数据不使用最后写入者覆盖；
- [ ] `null` 不覆盖可信值；
- [ ] 旧 patch 不覆盖新 patch；
- [ ] 事务失败不单独推进 checkpoint。
- [ ] Spec 04 build 存在时，新 present 来源的 source observation、manifest membership/required boundary 和 usage checkpoint 原子提交；冻结身份失配与保留旧成员全集的 replacement 也在同一 observation 事务完成；
- [ ] Spec 04 的 completed manifest 成员在追加、missing、重新 present 时由 source observation 原子更新证明；replacement 保留全部 build-only missing 成员与可信 proof/progress；
- [ ] Spec 04 的 occurrence 与 canonical event 原子提交；跨副本 duplicate 也保留来源映射，active contributor/LocalReplay/Carried 不依赖 canonical event 的首次 provenance；
- [ ] Spec 04 的 pending/可解除 blocked 成员在边界已追平时走 completion-only，不会因普通 Skip 永久阻塞 activation；
- [ ] Spec 04 未完成 manifest 的 checkpoint 契约不冲突：初始无 state 才是 rebuild_required，已有 matching working state 时保持 ready 并允许非零续读；
- [ ] Spec 04 的 BeginCarry partial-seed 转换原子删除 working source state、重置 checkpoint并初始化 cursor；carry-in-progress 后态没有 working state，partial facts 必须经 ResumeCarry 全量验证；
- [ ] Spec 04 raw-tail proof 持久化在 active usage source state；fixed-view 中间批、最终 none/half-line、unchanged observation 保留、unverified VerifyRawTail 和下一 build missing carry 的契约闭环；
- [ ] `SourceOutcome.build_disposition` 与 Spec 04 同为五值枚举，carry-in-progress 同身份恢复 present 返回 `carry_resumed_present`；
- [ ] Spec 04 上线后，metadata binding/root 与 active usage reconcile/build disposition 由同一个 `commit_metadata` 事务提交，不依赖事后调用；

### 19.3 Revision 与恢复

- [ ] `data_revision` 表示全局稳定查询事实版本；
- [ ] `status_revision` 表示扫描、follow-up 与来源状态版本；`mark_scan_started` 原子返回含本次 started revision 的 post-commit state；
- [ ] app_meta 只投影当前 active/follow-up/last-finished，`scan_runs` 才是 target 历史事实；当前扫描终态不清队列，follow-up 启动时清队列/设 active 同事务，后续 scan 不覆盖旧 target 终态；
- [ ] completed/failed 同事务清空 active ID，且 running/active、idle-or-failed/no-active 不变量由 CHECK 和测试固定；shutdown active scan 固定写为 `failed + SCAN_CANCELLED`，不产生额外 cancelled 状态；
- [ ] 扫描失败但数据不变时 status 仍可通知；
- [ ] migration 与 user_version 原子提交；
- [ ] busy、约束失败和中途退出保留旧数据；
- [ ] 数据库损坏不静默删除。

### 19.4 当前完整版本范围

- [ ] 文档明确 Token 账本和聚合属于当前版本 Spec 04；
- [ ] 当前版本包含去重、恢复、Turn 补偿、Subagent 历史排除和三类聚合；
- [ ] Spec 04 在 API 和 Dashboard 之前；
- [ ] 仅 `estimated_cost` / 美元费用占位；
- [ ] 不把 Token 用量误标为未来版本。

### 19.5 隐私与工程质量

- [ ] 不保存 Prompt、回复、工具正文和完整 JSONL；
- [ ] `cargo fmt --check` 通过；
- [ ] `cargo test` 通过；
- [ ] 失败路径有测试；
- [ ] 测试只使用临时数据库；
- [ ] 没有第二套持久化缓存；
- [ ] 没有实现超出 Spec 01 的解析、扫描、Token、API 或前端功能。

---

## 20. 交付物

```text
Cargo.toml / Cargo.lock 的 SQLite 依赖
src/lib.rs
src/domain.rs
src/storage/mod.rs
src/storage/migrations.rs
src/storage/schema/0001_initial.sql
数据库、事务、checkpoint、revision 和来源绑定测试
更新后的 README
```

实际文件可少量合并，但不得把 SQL、migration 和事务堆入 `main.rs`。

---

## 21. 与后续 Spec 的契约

### Spec 02

获得：

- rollout 来源身份；
- metadata checkpoint；
- ResolvedThreadPatch interface；
- 明确的字段优先级和三态写入规则。

### Spec 03

获得：

- 文件身份、generation 和 observed size；
- 单消费器 checkpoint；
- checkpoint rebuild 状态；
- 扫描生命周期和 `status_revision`。

### Spec 04

获得：

- 独立 `usage` checkpoint；
- Thread、父子关系和 root Session；
- 全局 `data_revision` 语义；
- 可追加新 migration 的基础。

Spec 04 必须创建 Token 表并原子提交事件与 usage checkpoint。

### Spec 05

获得：

- `data_revision`；
- `status_revision`；
- 扫描状态和错误码；
- 来源绑定状态。

### Spec 06

不直接读取 SQLite，只通过 Spec 05 HTTP interface 获取正式 Token 数据和状态。

---

## 22. 最终边界

完成 Spec 01 只证明：

> MU 具备可迁移、可恢复、来源安全、支持多消费器独立进度的数据库基础，后续元数据和 Token 采集不会互相跳过历史，也不会因来源副本或扫描顺序破坏 Thread 事实。

当前版本仍必须继续完成 Spec 02～06，其中 Spec 04 的 Token 账本与聚合是核心必需能力，只有美元费用保持占位。
