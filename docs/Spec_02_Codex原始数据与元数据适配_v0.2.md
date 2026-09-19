# Usagi Spec 02：Codex 原始数据与元数据适配

> 版本：v0.2  
> 状态：修订待实施  
> 更新日期：2026-08-09  
> 上游文档：`Spec_01_数据模型和数据库骨架_v0.2.md`、`Usagi_Codex本地数据口径_v0.2.md`、`Usagi_程序运行机制与数据持久化方案_v0.3.md`  
> 当前唯一测试标准：`Usagi_测试标准_Spec01-06_v0.17.md`  
> 真实格式核验基线：2026-08-09 提供的 3 份真实 Codex rollout、`.codex/state_5.sqlite`、Usagi `mu.sqlite3`  
> 当前版本范围：完整实现 Token 用量；仅 Token 美元费用 `estimated_cost` 占位

---

## 1. 文档目标

本 Spec 定义 Usagi（下文简称 MU）如何把 Codex 本地来源中的原始结构适配为稳定、规范化、可持久化的 Thread 元数据。

本 Spec 完成后，项目应具备：

1. `state_5.sqlite` 的只读结构探测和快照读取；
2. `session_index.jsonl` 的流式兼容读取；
3. rollout JSONL 外层结构和非 Token 元数据解析；
4. rollout 所属 Thread 与历史重放记录的区分；
5. 主 Thread、Subagent、直接父 Thread 和根 Session 解析；
6. 多来源字段优先级、时间新旧、空值和冲突处理；
7. 符合 Spec 01 interface 的 `ResolvedThreadPatch`；
8. 不读取、保存或输出对话正文的隐私保证；
9. 供 Spec 03 增量扫描器和 Spec 04 Token 账本使用的明确契约。

本 Spec 不负责文件发现和调度，不实现 Token 账本算法，也不写 HTTP 或前端代码。

---

## 2. 当前完整版本中的位置

```text
Spec 01：数据库骨架与来源检查点
    ↓
Spec 02：Codex 原始数据与元数据适配（本 Spec）
    ↓
Spec 03：文件发现与增量扫描
    ↓
Spec 04：Token 账本与聚合
    ↓
Spec 05：查询 API 与更新通知
    ↓
Spec 06：Dashboard 与 Session 页面
```

本 Spec 只推进 `consumer_kind=metadata` 的 checkpoint。它不得创建、推进或借用 `consumer_kind=usage` 的 checkpoint。

---

## 3. 范围

### 3.1 必须完成

- 探测 `state_5.sqlite` 表和列；
- 在只读事务中读取 Thread 快照和父子边；
- 流式读取 `session_index.jsonl`；
- 解析 rollout 完整 JSONL 行的外层 envelope；
- 解析 `session_meta`；
- 解析 `turn_context` 中允许的元数据；
- 识别 `token_count` 记录类型但不解析用量；
- 识别 rollout 自有 `session_meta` 和 fork 重放的其他 Thread 历史；
- 规范化 ID、时间、路径、标题、项目、归档和模型元数据；
- 构建 Thread 父子图并解析 `root_session_id`；
- 按固定优先级合并多来源元数据；
- 生成三态 `ResolvedThreadPatch`；
- 返回不含正文的诊断结果；
- 提供纯 fixture 测试和临时 SQLite 集成测试。

### 3.2 明确不完成

- 不递归枚举会话目录；
- 不判断文件是否变化；
- 不决定 `observed_size`、generation 或 guard；
- 不处理文件末尾半行；
- 不定时扫描或手动刷新；
- 不计算 Token；
- 不做 Token 去重、缺失恢复或 Turn 补偿；
- 不生成 `usage_events`、`turns` 或 `ingest_anomalies`；
- 不计算 `models_used` 或最后活动时间；
- 不根据对话正文生成标题或摘要；
- 不计算美元费用；
- 不直接向前端暴露原始 Codex 结构。

文件发现、完整行边界和调度属于 Spec 03。Token 账本属于 Spec 04。

---

## 4. 输入来源和职责

| 来源 | 本 Spec 用途 | 是否 Token 来源 | 持久化 checkpoint |
|---|---|---:|---:|
| `$CODEX_HOME/state_5.sqlite` | Thread 主清单、标题、路径、时间、归档、模型元数据、spawn 边 | 否 | 否；每轮只读快照 |
| `$CODEX_HOME/session_index.jsonl` | 标题兼容和旧版本补充 | 否 | 否；每轮流式全读，不跨轮缓存 |
| `sessions/**/rollout-*.jsonl` | owning `session_meta`、Turn 模型上下文、cwd、父关系补充 | Spec 04 才是 | metadata checkpoint |
| `archived_sessions/**/rollout-*.jsonl` | 与普通 rollout 相同 | Spec 04 才是 | metadata checkpoint |

### 4.1 `state_5.sqlite`

MU 只通过 SQLite 只读连接查询，不读取数据库文件页、WAL 或 SHM 内容。

### 4.2 `session_index.jsonl`

该文件只作标题兼容来源。它不是 Thread 主清单，也不是 Token 来源。

由于 Spec 01 的 `source_files` 只管理 rollout，本文件不使用 rollout byte checkpoint。Spec 03 每一轮扫描都流式全读该文件并保留本轮完整 `SessionNameSnapshot`；不得用最终 `threads.title` 反推或代替 session-index 候选，因为最终标题可能来自更高优先级 state。第一版不为它增加持久化表或跨轮缓存。

### 4.3 rollout

Spec 03 负责传入从 metadata checkpoint 到本轮固定 `observed_size` 之间的完整行。本 Spec 不自行打开未知目录，也不处理半行。

---

## 5. 真实格式基线

当前已验证的结构包括：

### 5.1 `session_meta`

主 Thread 常见字段：

```text
timestamp
payload.id
payload.timestamp
payload.cwd
payload.source
payload.originator
payload.cli_version
payload.model_provider
```

Subagent owning `session_meta` 当前已实测至少存在两种父关系编码形态：

**形态 A：thread-spawn / fork 形态**

```text
payload.parent_thread_id
payload.forked_from_id
payload.source.subagent.thread_spawn.parent_thread_id
payload.source.subagent.thread_spawn.depth
payload.agent_nickname
payload.thread_source = "subagent"
```

同一条 owning `session_meta` 中，`payload.parent_thread_id`、嵌套 `thread_spawn.parent_thread_id`、`forked_from_id` 可能同时存在且值相同。

**形态 B：Guardian / other Subagent 形态**

```text
payload.parent_thread_id
payload.source.subagent.other
payload.thread_source = "subagent"
```

该形态可以**没有**：

```text
payload.forked_from_id
payload.source.subagent.thread_spawn.parent_thread_id
```

并且 `state_5.thread_spawn_edges` 中也可能没有该 child 的边。

因此 `payload.parent_thread_id` 是本版本必须支持的直接父 Thread 字段，不能只依赖嵌套 `source.subagent.thread_spawn.parent_thread_id` 或 `forked_from_id`。

主 Thread 实测形态仍可为：

```text
payload.parent_thread_id = 缺失
payload.forked_from_id = 缺失
payload.source = "vscode" | "cli" | 其他主来源
payload.thread_source = "user" | 其他非 subagent 来源
```

`payload.thread_source` 属于已观察的格式信息；本版本不需要把它单独持久化为 Thread 字段。Subagent 判定仍以可信父关系和 `source.subagent` 为主，不允许仅凭未知字符串猜测父关系。

### 5.2 `turn_context`

允许读取：

```text
timestamp
payload.turn_id
payload.cwd
payload.model
payload.current_date
payload.timezone
```

其他字段默认忽略。

### 5.3 `session_index.jsonl`

当前字段：

```text
id
thread_name
updated_at
```

不得假定标题字段名为 `name`。

### 5.4 `state_5.sqlite`

当前相关表：

```text
threads
thread_spawn_edges
```

`threads` 的可用列可能随 Codex 版本变化，因此必须先探测列，再构造兼容查询。

---

## 6. 模块和 seam

推荐布局：

```text
src/
├─ codex/
│  ├─ mod.rs
│  ├─ state_index.rs
│  ├─ session_index.rs
│  ├─ rollout.rs
│  └─ metadata.rs
├─ domain.rs
└─ storage/
```

### 6.1 模块职责

| 模块 | 职责 |
|---|---|
| `state_index` | schema 探测、只读快照、类型转换 |
| `session_index` | JSONL 解码、每个 ID 选择最新名称 |
| `rollout` | envelope 分类和允许字段解析 |
| `metadata` | 多来源合并、父子图、patch 生成 |
| `storage` | Spec 01 定义的 SQLite 持久化，不解释 Codex 格式 |

### 6.2 对外 interface

```rust
StateIndexReader::read_snapshot(path) -> Result<StateSnapshot>

SessionIndexReader::read_snapshot(reader) -> Result<SessionNameSnapshot>

RolloutMetadataParser::parse_chunk(context, reader) -> RolloutParseResult

ThreadMetadataResolver::resolve(input) -> ResolutionResult
```

`parse_chunk` 的 context 必须携带：

```text
chunk_start_offset
owning_thread_id candidate/confirmed
resume_state = AwaitOwningMeta | OwningLive { owning_thread_id }
```

非零 offset 的 `OwningLive` 只能由 Spec 03 在来源绑定、checkpoint 和 guard 均验证通过后提供；adapter 不能仅根据当前 chunk 猜测此前归属状态。

这些是两个真实来源 adapter 和一个 rollout adapter，不建立通用“任意 Provider”抽象。

### 6.3 Interface 不泄漏的内容

调用方不需要知道：

- `state_5.sqlite` 当前列组合；
- `session_index` 当前标题字段名；
- Subagent source JSON 的嵌套路径；
- rollout 哪些记录含正文；
- 多来源优先级和空值规则；
- 父子图遍历实现。

---

## 6.4 Metadata parser version 契约

`rollout_metadata_facts` 是可复用的 durable safe fact，因此**任何会改变 safe fact 含义、字段提取、父关系候选或 provenance 优先级的 parser 变更都必须提升 metadata parser version**。

本次新增：

```text
payload.parent_thread_id
parent_hint_provenance = session_meta_parent
```

会改变既有 rollout 的 `parent_thread_id_hint` 结果，因此属于 parser 语义变更。当前 v1 口径升级为 **metadata parser v2**。

要求：

1. 生产入口不得继续用散落的数字字面量表示 metadata parser version；必须有单一版本常量或等价唯一来源；
2. checkpoint/fact 的 parser version 与当前版本不一致时，旧 safe fact 不得复用；
3. 对 mismatch 来源必须从 offset 0 重放 metadata，生成新的 safe fact；
4. v1 中已经 `ready`、但因漏读 `payload.parent_thread_id` 而缺 parent 的 fact，升级 v2 后必须重新解析；
5. metadata v2 解析成功并修复 parent/root 后，Spec 04 的 usage build 必须能重新获得可信 `root_session_id`，不能继续被旧 v1 fact 卡住；
6. parser version 升级只使 metadata consumer 重建；不得直接改写 usage checkpoint。usage 是否需要 reconcile/rebuild 由 Spec 04 根据 root/binding 变化决定。

当前 metadata parser 版本必须由代码中的单一常量（建议命名 `METADATA_PARSER_VERSION`）唯一声明；本 Spec 当前值为 **2**。

持久化的 parser-version authority 只存在于每个来源自身的 durable 状态：

```text
source_checkpoints.parser_version
rollout_metadata_facts.metadata_parser_version
```

`app_meta.metadata_parser_version` **不再作为当前 metadata parser 的全局 authority，也不要求在 current schema 中保留该字段**。scanner/storage 判断 safe fact 是否可复用时，只比较“代码当前 parser version”与对应 source checkpoint / safe fact 的 parser version。

版本升级必须通过真实重放产生新的 v2 checkpoint/fact；禁止仅修改任何数据库版本字段来“宣称”旧 v1 fact 已升级。

---

## 7. 标准化中间类型

### 7.1 `StateThreadFact`

```text
thread_id
rollout_path?
created_at_ms?
updated_at_ms?
archived?
title?
name?
cwd?
metadata_model?
agent_role_hint?
```

不包含 `first_user_message`、`preview` 或正文列。

### 7.2 `SpawnEdgeFact`

```text
parent_thread_id
child_thread_id
status?
source = state_spawn_edge | session_meta_parent | subagent_source | forked_from_id
observed_at_ms
```

### 7.3 `SessionNameFact`

```text
thread_id
thread_name
updated_at_ms
```

### 7.4 `RolloutThreadFact`

```text
source_file_id
owning_thread_id
cwd?
cwd_provenance? = session_meta | turn_context
cwd_record_offset?
created_at_ms?
latest_context_model?
latest_context_at_ms?
parent_thread_id_hint?
parent_hint_provenance? = session_meta_parent | subagent_source | forked_from_id
parent_hint_record_offset?
agent_role_hint?
agent_role_provenance? = session_meta_role | subagent_source
agent_role_record_offset?
source_area
current_path
metadata_warnings[]
ownership_boundary
```

`ownership_boundary` 包含：

```text
replay_start_offset?
owning_records_start_offset?
confidence = confirmed | unresolved
```

`RolloutParseResult` 还必须返回：

```text
final_continuation = OwningLive { owning_thread_id } | Unstable
needs_rebuild
last_processed_offset
```

只有 `final_continuation=OwningLive` 才允许 Spec 03 保存可供非零 offset 续读的 metadata checkpoint。

Spec 03 将 `RolloutThreadFact` 以安全结构化列持久化到 `rollout_metadata_facts`。`cwd`、parent hint、role hint 非空时必须同时携带 provenance 和产生该候选的 record offset；值为空时三者都为空。固定优先级为：cwd `session_meta > turn_context`，parent hint `session_meta_parent > subagent_source > forked_from_id`，role hint `subagent_source > session_meta_role`。`session_meta_parent` 专指 owning `session_meta.payload.parent_thread_id`，不得为了少做 schema 变更而伪装成 `subagent_source`。同 provenance 保留 byte offset 最小的第一条可信记录；后续同 provenance 不同值设置 conflict 但不覆盖。不同 provenance 的非空可信 parent 候选只要值不一致也必须设置 conflict；高优先级只决定 winner，不能消除冲突诊断。持久化 schema 中 `parent_hint_provenance` 的枚举约束也必须真实支持 `session_meta_parent`；如果现有 SQLite CHECK 只允许旧值，必须通过新的正式 migration 扩展，禁止改写历史 migration，也禁止把 direct parent 伪装成旧 provenance 来绕过 migration。增量 chunk 未提供的字段保持已有候选，后到低优先级值不能覆盖；offset 0 rebuild 完整替换旧 fact。最终 `threads` 行不能反向充当来源 fact。

持久化或加载的 fact 还必须满足 `fact.owning_thread_id = source_files.thread_id`；恢复 `OwningLive { owning_thread_id }` 时三者 ID 必须相同。不一致时 adapter 不接收该 fact，scanner 将来源标为 stale/conflict 并从 0 重建或中止该 Thread 组。

### 7.5 `ResolutionInput`

```text
state_snapshot
session_name_snapshot
rollout_facts
source_file_observations
existing_threads
resolved_at_ms
```

### 7.6 `ResolutionResult`

```text
patches[]
diagnostics[]
affected_thread_ids[]
```

不得返回原始 JSON 行或任意正文片段。

---

## 8. `state_5.sqlite` 适配方案

### 8.1 打开方式

要求：

- 使用 SQLite read-only 模式；
- 设置 `query_only=ON`；
- 设置有限 busy timeout；
- 在一个只读事务中读取 `threads` 和 `thread_spawn_edges`；
- 不运行 migration；
- 不创建表或临时持久化对象；
- 不解析 WAL 文件。

### 8.2 Schema 探测

读取：

```sql
PRAGMA table_info(threads);
PRAGMA table_info(thread_spawn_edges);
```

先确认表和列存在，再构造固定白名单查询。禁止 `SELECT *`。

### 8.3 允许查询的 `threads` 列

按存在情况读取：

```text
id
rollout_path
created_at
created_at_ms
updated_at
updated_at_ms
archived
cwd
title
name
model
agent_role
```

不得查询：

```text
first_user_message
preview
sandbox_policy
approval_mode
认证或远程控制信息
```

### 8.4 必需字段和降级

- `threads.id` 是读取 Thread 主清单的唯一必需列；
- `threads` 表缺失或 `id` 缺失：本轮 state source 不可用；
- 其他列缺失：对应字段为 unavailable，不猜测；
- `thread_spawn_edges` 缺失：使用 rollout 父关系补充；
- 单个字段类型不合法：丢弃该字段，保留同一行其他可信字段；
- 数据库被锁或暂时不可读：保留已有规范化值，本轮不执行 `Clear`。

### 8.5 时间转换

优先级：

```text
created_at_ms / updated_at_ms
→ created_at / updated_at（秒 × 1000）
```

必须检测溢出和明显不合理时间。无效时间返回诊断，不使用文件 mtime 替代。

### 8.6 快照一致性

`threads` 和 `thread_spawn_edges` 必须来自同一个只读事务视图，避免边指向本轮尚未读取到的不同数据库状态。

---

## 9. `session_index.jsonl` 适配方案

### 9.1 读取方式

- 按行流式读取；
- 只处理换行结束的完整行；
- 单行大小设上限；
- 不把整个文件读成一个字符串；
- 同一 ID 多条记录只保留最新可信记录。

完整行边界由 Spec 03 保证；本 Spec 的独立 reader 测试仍需拒绝半行。

### 9.2 字段规则

```text
id          → thread_id
thread_name → title candidate
updated_at  → ISO 8601 UTC 毫秒
```

标题处理：

- 去除首尾空白；
- 空字符串视为缺失；
- 不修改内部字符；
- 设置合理长度上限；
- 超长标题作为无效字段，不截断后冒充原值；
- 不把其他字段当作标题。

### 9.3 同 ID 冲突

同一 ID 多条记录：

1. `updated_at` 较新者优先；
2. 相同时间、相同值视为重复；
3. 相同时间、不同值使用文件中后出现者作为确定性选择，并产生 conflict 诊断；
4. 无有效时间的记录不能覆盖有有效时间的记录。

### 9.4 错误行

单条完整行 JSON 无效时：

- 不阻止其他行；
- 返回带行号的结构化诊断；
- 不返回行内容；
- 由调用方决定本轮 source 状态；
- 下次 parser version 重建时可以重新读取。

---

## 10. rollout envelope 分类

每条完整行先只读取：

```text
timestamp
type
payload.type（当需要分类）
```

分类结果：

```text
SessionMeta
TurnContext
TokenCount
Lifecycle
Ignored
Unknown
Malformed
```

每条 rollout 记录还必须得到独立归属：

```text
Owning
ReplayedAncestor
UnknownOwnership
```

规则：

- `SessionMeta` 先参与 owning/replay 状态机；只有 Owning 的允许字段进入元数据解析；
- `TurnContext` 只有归属为 Owning 时才能更新 cwd 或 `metadata_model`；
- `TokenCount` 只标记类型，不读取 Token 数字，不生成用量，不推进 usage checkpoint；
- Lifecycle 只在 Spec 02 明确需要元数据时间时解析白名单字段；
- 对话、工具、推理和消息正文一律 `Ignored`；
- `Unknown` 产生格式诊断但不复制 payload；
- `Malformed` 只记录 source ID 和 byte offset。

### 10.1 Metadata checkpoint

一条完整行即使是 `TokenCount` 或 `Ignored`，metadata consumer 也可以把自己的 checkpoint 推过该行，因为 usage consumer 有独立 checkpoint。

禁止：

```text
metadata offset → usage offset
metadata parser version → usage parser version
```

---

## 11. rollout 所属 Thread 判定

Subagent rollout 可能在开头复制父 Thread 历史，因此一个文件内可能出现多个不同 `session_meta.id`。

### 11.1 Owning Thread ID 来源

固定优先级：

1. `state_5.threads.rollout_path` 对应的 Thread ID；
2. Spec 03 从已验证文件名提取的 Thread ID；
3. 文件中第一条可信 `session_meta.id`。

前两者不一致时标记 conflict，不自动选择低优先级值覆盖高优先级值。

### 11.2 Owning `session_meta`

只有满足以下条件的 `session_meta` 才能更新当前文件所属 Thread：

```text
session_meta.payload.id == owning_thread_id
```

文件中其他 ID 的 `session_meta` 视为 fork/重放历史：

- 不更新 owning Thread 的标题、cwd、角色或创建时间；
- 不创建来源到错误 Thread 的关联；
- 可以作为“检测到历史重放”的结构化诊断；
- Spec 04 另行决定 Token 历史排除规则。

### 11.3 ID 冲突

如果文件中完全找不到匹配 owning ID 的 `session_meta`：

- 仍可使用 state index 元数据；
- rollout metadata quality 标记 partial；
- 不把其他 ID 的 `session_meta` 当作 fallback owning metadata；
- 若 owning ID 已由 state path 映射或 state 与文件名一致性确认，允许绑定来源并推进 metadata checkpoint，但不生成 rollout 自有字段 patch；
- 若 owning ID 同时未确认或存在冲突，不绑定来源、不推进 metadata checkpoint，并标记 `rebuild_required`。

前一条允许推进的附加条件是解析结束时没有未解决的 foreign replay，且 `final_continuation=OwningLive`。若 owning ID 已由外部确认、当前已读记录均与该 ID 一致且未观察到 foreign `session_meta`，即使 rollout 自有字段缺失，也可以形成稳定 continuation；以后增量 chunk 若首次出现 foreign meta，必须要求从 0 重建。

### 11.4 逐记录归属状态机

仅排除 foreign `session_meta` 不够，因为复制的父历史还包括 `turn_context`、生命周期和 `token_count`。每条记录必须经过以下状态机：

```text
AwaitOwningMeta
→ OwningBootstrap
→ ReplayedAncestor（检测到 foreign session_meta）
→ OwningLive（确认跨过重放边界）
```

规则：

1. owning `session_meta` 自身标记为 `Owning`；
2. owning meta 之后若出现 foreign `session_meta`，从该行开始进入 `ReplayedAncestor`；
3. 在重放状态中的所有 `turn_context`、TokenCount 和生命周期记录均标记为 `ReplayedAncestor`，不能更新 owning Thread；
4. 只有检测到可信 owning live Turn 边界后才进入 `OwningLive`；
5. `OwningLive` 后的记录标记为 `Owning`；
6. 无法确认边界时保持 `UnknownOwnership` 或 `ReplayedAncestor`，宁可不采用元数据，也不能把父历史归给子 Thread。
7. 从非零 offset 以 `OwningLive` 恢复后，如果新 chunk 出现 foreign `session_meta`、owning ID 冲突或其他表明先前边界不成立的信号，立即返回 `needs_rebuild`，本 chunk 不生成 patch、不推进 checkpoint；
8. offset 0 且 owning ID 已由外部 confirmed、已读记录没有 foreign replay 时，解析结束可以返回稳定 `OwningLive` continuation；若之后才追加 replay，按上一条从 0 重建。

### 11.5 Owning live Turn 边界

Subagent 历史中已观察到父 Turn 与子 Turn 都可能被重写为接近文件创建的外层 timestamp，因此不能只按外层时间判断。

确认边界至少需要：

- 当前记录是结构有效的 `turn_context`；
- `turn_id` 是可解析的 UUIDv7；
- owning Thread ID 是可解析的 UUIDv7；
- `turn_id` 的 UUIDv7 时间不早于 owning Thread 的 UUIDv7 创建时间；
- 该记录位于 foreign `session_meta` 及其重放记录之后；
- 没有更强的格式冲突信号。

外层 timestamp 只作辅助一致性校验，不作为唯一条件。

任何关键条件不满足时，边界为 `unresolved`，本 Spec 不使用重放区之后的 Turn 元数据。Top-level rollout 若从未出现 foreign `session_meta`，owning meta 之后的结构有效 Turn 可直接视为 Owning。

### 11.6 向 Spec 04 交付边界

`RolloutParseResult` 必须返回确定性的 byte offset 归属区间和置信状态。Spec 04 必须重新验证并使用相同归属规则排除父历史；不能只拿到 owning Thread ID 后把整个文件都算给子 Thread。

---

## 12. `session_meta` 解析规则

### 12.1 允许字段

只读取：

```text
id
timestamp
cwd
agent_role
agent_nickname（仅诊断或未来详情，不写当前 threads）
parent_thread_id
forked_from_id
source.subagent.thread_spawn.parent_thread_id
source.subagent.thread_spawn.depth
originator
cli_version
model_provider
```

其中：

- `parent_thread_id` 指 `payload.parent_thread_id`；
- `payload.parent_thread_id` 为 UUID 时生成 `session_meta_parent` provenance 的 parent candidate；
- 非 UUID、空字符串或 self-parent 不得作为可信 parent；
- `payload.thread_source` 可以用于安全格式诊断，但本版本不要求将其持久化；不得用它合成不存在的 parent ID。

### 12.2 禁止字段

不得持久化或输出：

```text
base_instructions
dynamic_tools 描述和 schema
完整 source 对象
任意提示词或消息内容
```

### 12.3 创建时间

优先使用 owning `session_meta.payload.timestamp`，其次使用外层事件 timestamp。两者都缺失时不使用文件 mtime。

### 12.4 cwd

- 保存规范化绝对路径；
- 相对路径或无效路径视为缺失；
- 不读取该路径中的任何文件；
- `project_name` 取路径最后一个正常组成部分；
- Subagent cwd 只属于该 Subagent Thread，不覆盖根 Thread 行。

---

## 13. `turn_context` 解析规则

`turn_context` 只用于 metadata fallback：

- 记录归属必须是 `Owning`；`ReplayedAncestor` 和 `UnknownOwnership` 一律不参与；
- 最新可信 `payload.model` → `metadata_model` candidate；
- owning Thread 的第一条可信 `payload.cwd` → cwd fallback；
- `turn_id` 只用于结构验证，不在 Spec 02 建立 Turn 表；
- current date 和 timezone 只作诊断，不替代事件 timestamp。

同一文件有多个 Turn：

- `metadata_model` 取时间最新的可信值；
- 相同时间冲突时按 byte offset 后者确定，并标记 conflict；
- 这不等于 `models_used`；
- Spec 04 必须按有效用量事件发生时上下文计算模型聚合。

---

## 14. Thread 父子关系和根 Session

### 14.1 父关系来源优先级

```text
state_5.thread_spawn_edges
→ owning session_meta.payload.parent_thread_id
→ owning session_meta.source.subagent.thread_spawn.parent_thread_id
→ 明确属于 Subagent 时的 owning session_meta.forked_from_id
→ 无法确认
```

规则：

1. `state_5.thread_spawn_edges` **存在明确 child→parent 边时**仍是最高优先级；
2. `thread_spawn_edges` 中**没有该 child 的边不能当作“该 Thread 没有父关系”的否定证据**。真实 Guardian rollout 已证明：state 表可以无 edge，而 owning `session_meta.payload.parent_thread_id` 明确存在；
3. `payload.parent_thread_id` 是 owning session 自身的直接父字段，优先于嵌套 legacy `thread_spawn.parent_thread_id`；
4. `source.subagent.thread_spawn.parent_thread_id` 继续作为兼容的次级直接父候选；
5. `forked_from_id` 仍只作受限 fallback：只有 owning meta 已明确是 Subagent、且两个更直接的 parent 字段都不存在时才允许使用；
6. `agent_role` 字符串提示不能单独覆盖可信 parent；
7. 一个可信 parent 一旦确认，当前 MU Thread 关系中该 child 按 Subagent 处理，并沿父链解析 root。

### 14.1.1 同一 owning meta 的父字段一致性

同一 owning `session_meta` 可能同时出现：

```text
payload.parent_thread_id
payload.source.subagent.thread_spawn.parent_thread_id
payload.forked_from_id
```

处理：

- 多字段都合法且值相同：视为一致重复证据，不产生 conflict；
- 高优先级字段合法、低优先级字段不同：采用高优先级字段，但 `metadata_quality_status=conflict` 并产生安全诊断；
- `payload.parent_thread_id` 非法：忽略该字段并允许继续检查次级来源；
- `payload.parent_thread_id == owning_thread_id`：self-parent，关系 conflict，root 为空；
- state edge 与 rollout 直接 parent 不同：继续按 state edge 选择，但必须 conflict；不得静默覆盖。



### 14.2 Main 判定

只有同时满足以下条件才判定为 `main`：

- 不存在任何可信父边或 Subagent 父关系 hint；
- state 关系快照状态为 `complete` 并确认没有该 Thread 的父边，或者 owning `session_meta` 有明确 `agent_role=main` 证据；
- owning `session_meta` 没有 `payload.parent_thread_id`、Subagent source parent 或其他可信父候选；
- owning `session_meta` 没有 Subagent source；
- 没有明确 subagent role；
- Thread 自身身份可信。

`thread_spawn_edges` 缺表、state source 不可用或父边查询失败时，“没有观察到父边”不是 main 证据。若 owning meta 也没有明确 main 角色，只能判定为 `unknown`，待权威关系来源恢复后重算。

### 14.3 Subagent 判定

存在可信父 Thread ID 时：

```text
agent_role = subagent
parent_thread_id = direct parent
```

### 14.4 Unknown 判定

来源冲突、出现多个可信父节点、父 ID 无效或图存在环时：

```text
agent_role = unknown
root_session_id = null
metadata_quality_status = conflict 或 partial
```

Unknown 不生成独立 Session，不纳入 Session 计数。

### 14.5 Root 解析

对每个已确认 Thread 沿直接父链向上：

- 到达 main：`root_session_id = main.thread_id`；
- 多层 Subagent 全部得到同一个 root；
- 父记录暂缺：root 为空，保留直接父 ID；
- 检测到环：关系冲突；
- 设置最大深度保护，但超过深度不能猜测 root；
- 后续父记录到达时重新解析受影响后代。

### 14.6 多父冲突

如果同一 child 在同一高优先级来源中有多个不同 parent：

- 不采用“最后一条边”；
- child 标记 conflict；
- root 为空；
- 返回 parent conflict 诊断。

---

## 15. 多来源元数据合并

### 15.1 字段优先级

| 字段 | 优先级，从高到低 |
|---|---|
| `title` | state `name` → state `title` → session index `thread_name` → 缺失 |
| `project_path` | owning `session_meta.cwd` → owning 第一条 Turn cwd → state `cwd` |
| `parent_thread_id` | state spawn edge → owning `payload.parent_thread_id` → owning subagent source parent → 明确 Subagent 的 `forked_from_id` |
| `root_session_id` | 已确认父链计算，不直接信任任意路径或标题 |
| `agent_role` | 已确认父关系 → owning role/source → main 完整判定 → unknown |
| `archived` | 较新的 state archived → 当前物理目录区域 |
| `current_rollout_path` | 较新的 state rollout path → 存在的 sessions 副本 → archived 副本 |
| `metadata_model` | state model → 最新 owning Turn model |
| `created_at_ms` | state ms → state seconds → owning session meta timestamp |
| `updated_at_ms` | state ms → state seconds → 最新允许元数据事件时间 |

### 15.2 空值

- 低优先级 `null` → `Keep`；
- 高优先级来源暂时不可用 → `Keep`；
- 空字符串按缺失处理；
- 只有完成该 Thread 全来源重算且确认字段已不存在时才能 `Clear`；
- 不把未知值写成空字符串、0 或 `unknown` 模型名；
- `agent_role=unknown` 是明确内部状态，不是缺失字符串 fallback。

### 15.3 新旧时间

- 时间只用于比较同一来源、同一优先级的重复事实；
- 跨来源合并始终先遵守字段优先级，不能拿 Thread 的 `updated_at` 当作标题、cwd 或角色字段的版本号；
- 较旧的 session index 行不能覆盖同一来源较新的名称；
- 较旧的 Turn context 不能覆盖同一 rollout 中较新的模型 context；
- state 秒级时间与 JSONL 毫秒时间比较前统一到 UTC 毫秒；
- 文件 mtime 只用于 Spec 03 变化检测，不参与业务字段新旧比较。

### 15.4 冲突

冲突包括：

- owning Thread ID 不一致；
- 同优先级标题同时间不同值；
- 同一 child 多父；
- owning `payload.parent_thread_id` 与同条 owning meta 的 nested parent/forked parent 冲突；
- main 同时具有父边；
- state path 指向另一个 Thread 文件；
- 角色提示与明确 spawn edge 矛盾；
- 父链成环。

冲突处理：

- 使用本 Spec 明确的确定性选择；
- 无安全选择时保留旧值或置为 unknown；
- `metadata_quality_status=conflict`；
- 返回不含正文的诊断；
- 禁止按扫描顺序覆盖。

---

## 16. `ResolvedThreadPatch` 生成

每个 patch 包含：

```text
thread_id
parent_thread_id: Keep | Set | Clear
root_session_id: Keep | Set | Clear
agent_role: Keep | Set
title: Keep | Set | Clear
project_path: Keep | Set | Clear
project_name: Keep | Set | Clear
metadata_model: Keep | Set | Clear
created_at_ms: Keep | Set | Clear
updated_at_ms: Keep | Set | Clear
archived: Keep | Set
current_rollout_path: Keep | Set | Clear
metadata_quality_status: Set
resolved_at_ms
```

规则：

- 没有实际变化时不生成 patch；
- patch 只包含规范化值，不含原始 JSON；
- `Clear` 必须附带 `full_resolution=true`；
- state source 不可用时不得生成依赖 state 缺失的 `Clear`；
- `resolved_at_ms` 使用本轮解析视图时间，不使用单个文件 mtime；
- 同一 Thread 一轮最多输出一个最终 patch；
- patches 按 Thread ID 稳定排序，便于测试和日志计数。

### 16.1 有 checkpoint 与无 checkpoint 的提交

`commit_metadata` 的批次必须允许：

```text
Thread group {
  resolved patch: None | Some
  zero or more source commits {
    source/generation preconditions
    confirmed owning binding
    complete safe fact
    metadata checkpoint advance
  }
}
```

- state snapshot 和 session index 产生的 patch 使用 `checkpoint=None`，但仍必须经过包含全部可用 rollout safe facts 的 resolver；
- rollout 产生的 patch 必须同时提交 metadata checkpoint；
- 无 checkpoint 的 patch-only 事务仍按事实变化递增一次 `data_revision`；
- 不能为了复用 rollout interface 给 state/session index 伪造 source ID 或 offset。
- 若某 Thread 有 present rollout、但其 safe fact 缺失/stale/读取失败，state/session-index 不得独立覆盖该 Thread；该 Thread 组保持旧值并等待完整重算。
- scanner 通过 Spec 01 `load_metadata_scan_state` 一次批量读取 source、metadata checkpoint 和 safe fact；adapter 只接收被 Ledger 判为 `Matching` 的 fact。
- resolver 没有 Thread 字段变化时返回 `resolved patch=None`；scanner 仍提交完整 source commit。只含 TokenCount、Ignored 或其他不改变元数据的新增区间不得因此停留在旧 metadata offset。

### 16.2 Owning Thread 来源绑定

rollout owning ID 达到 confirmed 后，同一事务必须完成：

```text
source_files.thread_id 绑定或确认
None 或一个 ResolvedThreadPatch
完整 rollout safe fact
metadata checkpoint advance
data_revision（如事实变化）
```

规则：

- `source_files.thread_id` 为空时可绑定 confirmed owning ID；
- 已等于 owning ID 时只确认，不产生额外变化；
- source commit 必须携带 scanner 读取快照中的 `expected_previous_thread_id`；存储先校验数据库当前值等于该期望，再执行 None→confirmed 或相同 ID 确认，防止并发陈旧写入；
- 已有不同可信 ID 时不得覆盖，返回 conflict 并将 metadata checkpoint 标记 `rebuild_required`，不推进 offset；
- owning ID 未确认时不得建立或覆盖关联，不提交 rollout patch，不推进 offset；
- state/session-index patch-only 提交不更新 `source_files.thread_id`。

---

## 17. 路径规范化

### 17.1 规则

- 必须是绝对路径；
- 词法消除 `.` 和可安全解析的 `..`；
- 不要求目标仍然存在；
- 不读取项目目录；
- 不解析 Git 仓库根；
- 不做大小写折叠；
- 不用路径作为 Thread ID；
- 保留原本有意义的 Unicode。

### 17.2 rollout 路径

state path、物理观察路径和文件名 Thread ID 需要交叉校验。路径冲突只影响质量状态，不能重写 Thread ID。

---

## 18. 时间规范化

统一输出 UTC Unix 毫秒 `i64`。

输入：

- ISO 8601 / RFC 3339 字符串；
- state `_ms` 整数；
- state 秒整数。

规则：

- 显式时区优先；
- 无时区字符串视为格式不完整，不按本机时区猜测；
- 秒转毫秒检测溢出；
- 不使用当前时间填充原始事件时间；
- `resolved_at_ms` 是 MU 解析视图时间，与原始业务时间分开。

---

## 19. 诊断与数据质量

### 19.1 诊断结构

```text
diagnostic_type
severity
thread_id?
source_file_id?
source_start_offset?
field?
source_kind
```

禁止包含：

- 原始行；
- payload；
- 对话文本；
- 工具内容；
- base instructions。

### 19.2 质量状态

`complete`：本轮所需权威来源可用且关系无冲突。

`partial`：字段缺失、state 暂不可用、owning meta 缺失或父记录暂未发现，但没有互相矛盾的可信事实。

`conflict`：存在互相矛盾的可信身份、父关系或同优先级字段。

### 19.3 单文件错误隔离

单个 rollout 的格式错误不得阻止其他来源产生 patch。解析结果必须同时返回：

```text
成功 facts
诊断
最后可提交完整行 offset
```

最后 offset 的提交由 Spec 03 与 Spec 01 storage 共同完成。

对于已经以换行结束、但 JSON 无效或类型未知的完整行，metadata consumer 记录安全诊断后可以把自己的 checkpoint 推过该行，避免永久卡住；不得推进 usage checkpoint。解析器版本升级或显式重建时可从旧 checkpoint 之前重新检查。半行永远不能被推过。

---

## 20. 隐私白名单

### 20.1 允许持久化

- Thread ID；
- 父和根 Thread ID；
- 角色；
- Codex 已有标题；
- cwd 派生项目路径和名称；
- 模型元数据；
- 时间戳；
- 归档状态；
- rollout 路径；
- 来源 ID 和 offset；
- 不含正文的错误码和质量状态。

### 20.2 禁止持久化

- Prompt；
- Assistant 回复；
- reasoning 正文；
- 工具输入输出；
- base instructions；
- dynamic tool schema；
- first user message；
- preview；
- 完整 JSONL 行；
- 从正文生成的标题或摘要。

### 20.3 日志

错误日志使用 ID、offset、类型和错误码。禁止使用 JSON parser 的完整原始输入作为错误上下文。

---

## 21. 与 Spec 03 的契约

Spec 03 提供：

```text
source_file_id
owning_thread_id candidate
source_area
current_path
chunk_start_offset
observed_size
resume_state
existing_source_fact（非零增量时）
只包含完整行的 reader
```

本 Spec 返回：

```text
RolloutThreadFact
updated_source_fact
逐记录 Owning / ReplayedAncestor / UnknownOwnership 归属
ownership_boundary byte offset 区间与置信状态
final_continuation = OwningLive | Unstable
diagnostics
last_processed_offset
needs_rebuild
```

Spec 03 负责：

- 文件枚举；
- 固定 observed size；
- 半行保留；
- guard 校验；
- 根据 confirmed 来源绑定和 checkpoint 构造非零 offset 的 resume state；
- 为 Skip 来源加载匹配 generation/parser/offset 的 safe fact；缺失或 stale 时从 0 重建；
- metadata checkpoint 事务提交；
- 调度和并发限制。

---

## 22. 与 Spec 04 的契约

Spec 04 可以依赖：

- owning Thread ID 判定规则；
- 每条 rollout 记录的归属分类，以及相应 byte offset 区间和置信状态；
- Thread 父子图和 root Session；
- rollout envelope 分类的安全实现；
- Turn 上下文中模型随时间变化的原始记录格式；
- 独立 usage checkpoint 从 0 开始。

Spec 04 不可以依赖：

- metadata checkpoint；
- `threads.metadata_model` 作为有效事件模型；
- state `tokens_used` 作为 Token 统计来源；
- Spec 02 的 metadata diagnostics 代替 Token 异常模型。

Spec 04 需要重新从 usage checkpoint 读取 `token_count`，并按 v0.2 算法建立账本。
它必须复用并重新验证本 Spec 的记录归属规则：只有 `Owning` 区间可归入当前 Thread，`ReplayedAncestor` 和 `UnknownOwnership` 都不能直接记账。

---

## 23. 实施步骤

### 步骤 1：安全中间类型

- 定义各来源 fact；
- 定义诊断和质量枚举；
- 定义 patch 三态；
- 确保类型中不存在正文承载字段。

### 步骤 2：state adapter

- 只读打开；
- schema 探测；
- 白名单列查询；
- 同事务读取 threads 和 spawn edges；
- 时间和类型转换；
- 缺表缺列降级。

### 步骤 3：session index adapter

- 流式逐行；
- 解析 `thread_name`；
- 每 ID 最新记录；
- 错误隔离和大小限制。

### 步骤 4：rollout adapter

- envelope 分类；
- owning Thread 判断；
- owning `session_meta` 白名单解析，包含 `payload.parent_thread_id`；
- `turn_context` fallback；
- 重放 meta 排除；
- TokenCount 仅分类。

### 步骤 5：关系解析

- 合并 state spawn edge、`payload.parent_thread_id`、nested subagent parent 与受限 `forked_from_id`；
- main/subagent/unknown；
- root 遍历；
- 缺父、多父、环和后代重算。

### 步骤 6：字段合并

- 固定优先级；
- 空值和时间规则；
- 冲突诊断；
- 按 Thread 输出 None 或一个 ResolvedThreadPatch。

### 步骤 7：存储集成

- 使用 Spec 01 `commit_metadata`；
- state/session index 使用无 checkpoint 的 patch-only 提交；
- rollout confirmed owning 来源 binding、完整 safe fact、可选 patch 与 metadata checkpoint 原子提交；
- owning ID 冲突或未确认时不覆盖来源绑定、不推进 checkpoint；
- 验证不会触碰 usage checkpoint；
- 验证 data revision 只在事实变化时增加。

### 步骤 8：测试和文档

- 建立脱敏合成 fixture，并加入本次 3 份真实 rollout 的结构等价 fixture；
- 建立临时 state SQLite fixture；
- 覆盖真实已观察的字段变体；
- 更新 README 的兼容范围。

---

## 24. 测试方案

### 24.1 State schema

- 当前完整 schema 正常读取；
- 只有 `threads.id` 的最小 schema；
- 缺少可选列时降级；
- 缺少 threads 表时 source unavailable；
- 缺 spawn 表时使用 rollout hint；
- spawn 表缺失或 state 不可用且无明确 main 证据时保持 unknown；
- 秒和毫秒时间均正确；
- 禁止列没有出现在 SQL；
- 只读模式下无法写入 Codex DB。

### 24.2 Session index

- 正确解析 `thread_name`；
- 同 ID 新时间覆盖旧时间；
- 旧时间不能覆盖新时间；
- 同时间冲突产生诊断；
- 空标题忽略；
- 无效 JSON 不阻止后续行；
- 半行不作为完整记录；
- 超长行和超长标题被拒绝。

### 24.3 Rollout

- 主 Thread owning meta；
- Subagent owning meta：至少覆盖 legacy `thread_spawn` 形态和 `payload.parent_thread_id` 直接父形态；
- Guardian/other Subagent：`source.subagent.other` + 顶层 `payload.parent_thread_id`，无 nested thread_spawn、无 forked_from；
- 子文件内重放父 `session_meta`；
- 子文件内重放父 `turn_context` 不更新子 Thread 的 cwd 或模型；
- UUIDv7 owning live 边界满足条件时，从该边界恢复 `Owning`；
- owning live 边界无法确认时，后续记录保持 `ReplayedAncestor` 或 `UnknownOwnership`；
- 每条记录都有确定归属，归属区间与置信状态可交付 Spec 04；
- offset 0 且外部 ID confirmed、无 foreign replay 时返回稳定 continuation；
- 非零 offset 从 confirmed `OwningLive` 续读；
- 非零续读首次遇到 foreign meta 时要求从 0 重建且不提交本 chunk；
- `payload.parent_thread_id`、nested parent、forked_from 同值时合并为一个 parent；
- 顶层 direct parent 与 nested/forked 冲突时按优先级选择并标 conflict；
- 文件名 ID 与 owning meta 一致；
- 文件名 ID 与 state path 冲突；
- owning meta 缺失；
- 多个 Turn 模型；
- TokenCount 只分类不解析；
- 正文记录完全忽略；
- unknown 和 malformed 只产生安全诊断。

### 24.4 关系图

- 单个 main；
- 完整 state 关系快照确认无父边时可判定 main；
- state 关系快照不可用且无明确 main 证据时只能判定 unknown；
- owning meta 明确 main 且不存在冲突证据时可判定 main；
- 一层 Subagent；
- 多层 Subagent；
- 父记录晚到；
- state edge 优先于 rollout hint；
- state edge 缺失但 owning `payload.parent_thread_id` 存在时仍解析出 direct parent/root；
- `payload.parent_thread_id` 优先 nested `thread_spawn.parent_thread_id`，nested 优先受限 `forked_from_id`；
- 同 child 多父；
- 自环和多节点环；
- role hint 与 edge 冲突；
- unknown 不生成 root Session。

### 24.5 多来源合并

- state name 优先于 state title；
- state title 优先于 session index；
- rollout cwd 优先于 state cwd；
- 低优先级 null 不覆盖已有值；
- state 暂不可用不清空标题；
- 旧事实不覆盖新事实；
- 增量 fact 保留 cwd/parent/role provenance，parent provenance 至少区分 `session_meta_parent` / `subagent_source` / `forked_from_id`；后到低优先级候选不覆盖已有高优先级候选；
- 同 provenance 使用持久化 record offset 保留第一条可信记录；role 明确 `subagent_source > session_meta_role`，矛盾时标记 conflict；
- 同优先级冲突设置 conflict；
- 一轮同 Thread 只产生一个 patch；
- 无变化不产生 patch。

### 24.6 存储集成

- state/session index patch-only 提交不要求 checkpoint，但有 rollout 的 Thread 必须先具备完整 safe fact 输入；
- Ledger 批量读取只把 generation/parser/offset/binding/owning ID 全匹配的 fact 标为 `Matching`；metadata parser v1 fact 在 v2 下必须 mismatch 并从0重建；
- fact、source binding、`OwningLive` 三个 owning ID 不一致时拒绝续读和提交；
- source commit 显式携带完整 safe fact，不能只提交 patch、binding 和 checkpoint；
- 只有 TokenCount/Ignored 等无元数据变化的新增记录时，`resolved patch=None` 仍推进 safe fact/checkpoint，`data_revision` 不变；
- Thread group ID、patch ID 与组内所有来源 owning ID 任一不同都整组回滚；
- 首次绑定的写入前 ID 允许为空；只有 CAS 通过并写入后，才要求 group/patch/binding/fact/continuation ID 全相等；
- confirmed owning 来源绑定、rollout patch 与 metadata checkpoint 原子提交；
- 已绑定 Thread ID 冲突时不覆盖关联、不推进 offset，并标记 rebuild；
- owning ID 未确认时不建立关联、不提交 rollout patch、不推进 offset；
- metadata offset 前进不改变 usage offset；
- 事务失败不推进 metadata offset；
- 重复 patch 不增加 data revision；
- 事实变化只增加一次 revision；
- 多来源文件归入同一 Thread。

### 24.7 隐私

- fixture 放入正文哨兵字符串；
- fact、patch、数据库和日志中均找不到哨兵；
- state 查询不含 `first_user_message` 或 `preview`；
- JSON 错误不输出原始行；
- 不读取用户项目文件。

---

## 25. 独立验收标准

### 25.1 来源适配

- [ ] `state_5.sqlite` 使用只读白名单查询；
- [ ] schema 差异可降级；
- [ ] `session_index.thread_name` 可正确补充标题；
- [ ] rollout owning Thread 可稳定识别；
- [ ] owning `payload.parent_thread_id` 可作为 direct parent 解析；Guardian/other Subagent 不依赖 nested `thread_spawn` 或 state edge；
- [ ] 每条 rollout 记录均被分类为 `Owning`、`ReplayedAncestor` 或 `UnknownOwnership`；
- [ ] fork 重放的其他 `session_meta` 和 `turn_context` 不污染 owning Thread；
- [ ] owning live 边界无法确认时宁可放弃元数据，也不把父历史归给子 Thread；
- [ ] 记录归属区间与置信状态可供 Spec 04 复用并重新验证；
- [ ] parser 明确返回 `OwningLive` 或 `Unstable` continuation；
- [ ] 非零 offset 只能从 scanner 验证过的 `OwningLive` 恢复，发现迟到 replay 时不提交并从 0 重建；
- [ ] TokenCount 只分类，不计算或保存用量。

### 25.2 元数据正确性

- [ ] 标题、项目、关系、归档、路径、模型和时间遵守固定优先级；
- [ ] null 不覆盖可信值；
- [ ] 旧数据不覆盖新数据；
- [ ] safe fact 的 cwd/parent/role 候选持久化 provenance，parent 至少区分 `session_meta_parent` / `subagent_source` / `forked_from_id`，并按固定 provenance 优先级和 record offset 合并；
- [ ] 三类候选同时持久化 record offset；同 provenance 选择与 role provenance 优先级可重复；
- [ ] 不依赖最后扫描者覆盖；
- [ ] 同优先级冲突可重复地产生相同结果和 conflict 状态；
- [ ] `metadata_model` 不被当作 `models_used`。

### 25.3 Thread 关系

- [ ] Main、Subagent、unknown 判定明确且可重复；
- [ ] 只有完整关系快照确认无父边或 owning meta 明确 main 时，才可在无父关系下判定 main；
- [ ] state 关系来源不可用且无明确 main 证据时保持 unknown；
- [ ] 多层 Subagent 得到相同 root；
- [ ] state spawn edge 缺失时，只要 owning `payload.parent_thread_id` 可信，Subagent 仍能得到 direct parent/root；
- [ ] 缺父时保留 parent 并暂不生成 root；
- [ ] 多父和环不会产生虚假根 Session；
- [ ] unknown 不计为 Session；
- [ ] 后到父记录能触发后代重算。

### 25.4 Checkpoint 和事务

- [ ] 只推进 metadata checkpoint；
- [ ] metadata checkpoint 可以安全越过 TokenCount，因为 usage checkpoint 独立；
- [ ] 不创建或推进 usage checkpoint；
- [ ] state/session index patch-only 事务不伪造 source ID 或 checkpoint；
- [ ] 有 present rollout 但 safe fact 不完整时，state/session-index 不独立覆盖该 Thread；
- [ ] 只接收 Ledger 批量读取判定为 `Matching` 且 owning ID 强一致的 safe fact；
- [ ] 每个 rollout source commit 显式包含完整 safe fact；
- [ ] 无 Thread 字段变化的 chunk 使用 `resolved patch=None` 推进 fact/checkpoint，且不改变 `data_revision`；
- [ ] Thread group ID、patch ID、binding、fact 与 `OwningLive` owning ID 全部一致；
- [ ] 首次 binding 支持 expected None→confirmed；expected previous 陈旧或非空冲突时不写入、不推进 offset；
- [ ] confirmed owning 来源绑定、rollout patch 与 metadata offset 共同提交或回滚；
- [ ] owning ID 冲突时不覆盖已有 `source_files.thread_id`，不推进 offset，并标记 rebuild；
- [ ] owning ID 未确认时不绑定来源、不提交 rollout patch、不推进 offset；
- [ ] 单文件异常不阻塞其他文件；
- [ ] state 暂不可用不清空稳定元数据。

### 25.5 隐私

- [ ] 不保存 Prompt、回复、reasoning、工具正文和完整 JSONL；
- [ ] 不查询 state 正文列；
- [ ] 不从正文生成标题；
- [ ] 诊断不含原始 payload；
- [ ] 不读取项目源码。

### 25.6 工程质量

- [ ] metadata parser version 已从 v1 升级到 v2，旧 v1 safe fact 不被复用；
- [ ] `cargo fmt --check` 通过；
- [ ] `cargo test` 通过；
- [ ] 测试不读取或修改真实 `~/.codex`；
- [ ] 临时 state SQLite 和 JSONL fixture 可独立复现；
- [ ] 失败路径和格式兼容路径有测试；
- [ ] 没有实现 Spec 03 调度或 Spec 04 Token 算法。

---

## 26. 交付物

```text
src/codex/mod.rs
src/codex/state_index.rs
src/codex/session_index.rs
src/codex/rollout.rs
src/codex/metadata.rs
必要的 domain / parent provenance / metadata parser version 类型更新
脱敏 JSONL fixtures（必须包含真实 direct-parent Guardian 结构）
临时 state SQLite fixture 构造代码
来源 adapter、关系图、合并和隐私测试
更新后的 README 兼容说明
```

实际文件可在不破坏模块职责时合并，但不得把所有适配、合并和存储逻辑堆入 `main.rs`。

---

## 27. 最终边界

完成 Spec 02 证明：

> MU 能把多个不稳定的 Codex 本地来源安全地转换为稳定、可解释的 Thread 和 Session 元数据，并且 fork 历史、来源缺失、字段冲突和扫描顺序不会静默污染规范化结果。

它不代表 Token 统计完成。Spec 04 仍必须从独立 usage checkpoint 回放 `token_count`，实现全部 Token 账本与聚合能力；只有美元费用保持占位。


---

## 28. v0.2 修订的强制数据契约摘要

实现和测试不得偏离以下六条：

```text
1. payload.parent_thread_id 是正式 direct-parent 输入字段。
2. state spawn edge 缺失 ≠ 没有 parent。
3. rollout parent 优先级：
   session_meta_parent > subagent_source > forked_from_id。
4. state edge 存在时仍高于 rollout parent；冲突必须显式标 conflict。
5. 新 parent provenance 必须真实持久化，不得伪装成旧 subagent_source。
6. 本次语义改变必须把 metadata parser 从 v1 升到 v2，
   旧 v1 ready safe fact 必须从 offset 0 重建。
```

基于 2026-08-09 真实故障样本，最低真实回归链必须覆盖：

```text
Guardian session_meta
  source.subagent.other
  payload.parent_thread_id
  no nested thread_spawn edge
  no forked_from_id
  no state_5.thread_spawn_edges row
        ↓
metadata parser v2
        ↓
parent_thread_id_hint + root_session_id
        ↓
usage build source 不再因 root 缺失 blocked
        ↓
shadow epoch 可完成 activation
        ↓
active epoch / API / Dashboard 切换到新数据
```
