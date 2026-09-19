# Usagi 多终端支持分析报告

> 本报告基于当前工作区代码和本机实际数据目录分析。本次只读检查，没有修改代码。当前 Git 工作区本身已有多处未提交修改，因此结论以当前 working tree 为准，不完全等同于最近一次 commit。

## 一、结论先行

### 总结

- **Usagi 当前不是通用的会话采集框架，而是围绕 Codex rollout JSONL 构建的一条固定流水线。**
- **Antigravity 和 Maka 都可以映射到 Usagi 的统一会话/Token 数据语义，但不能直接复用当前 Codex 文件扫描器。**
- **Maka 的字段对齐度更高，建议先实现 Maka。**
- **Antigravity 的 `.db` 数据可以实现，但需要先解决两个口径问题：**
  1. `inputTokens` 与 `cacheReadTokens` 的包含关系；
  2. `gen_metadata` 中缺少时间戳，需要和 `steps.metadata` 关联。
- Antigravity 的旧 `.pb` / RPC 数据不建议作为第一阶段范围。
- 前端需要的不只是加一个筛选按钮，还需要让 terminal 维度贯穿：
  - 数据库；
  - UsageFilter；
  - 查询 SQL；
  - API DTO；
  - Dashboard 查询 key；
  - Session 列表；
  - Drawer 详情；
  - 图表和 KPI。

### 可行性判断

| 终端 | Session 元数据 | Token 字段 | 项目 | 时间 | 父子关系 | 结论 |
|---|---:|---:|---:|---:|---:|---|
| Codex | 完整 | 完整 | 完整 | 完整 | 完整 | 当前基线 |
| Maka | 较完整 | 较完整 | 可获取 | 较完整 | 基本没有 | **高可行** |
| Antigravity `.db` | 完整 | 可获取但需归一化 | 可获取 | 需关联 | 部分可获取 | **中高可行** |
| Antigravity `.pb` | 依赖 RPC | 依赖 RPC | 依赖 RPC | 依赖 RPC | 不稳定 | 第一阶段跳过 |

---

# 二、当前 Usagi 代码现状

## 1. 数据采集链路是 Codex 专用的

当前主要链路如下：

```text
Codex 文件目录
  ↓
scanner::Discovery
  ↓
chunk_reader
  ↓
Codex rollout metadata parser
  ↓
usage consumer
  ↓
NormalizedTokenUsage
  ↓
usage_events / turns
  ↓
AggregateReader
  ↓
HTTP API
  ↓
React Dashboard
```

关键位置：

| 模块 | 当前职责 |
|---|---|
| `src/main.rs` | 启动 Codex scanner、Codex quota、HTTP 服务 |
| `src/scanner/discovery.rs` | 固定扫描 `sessions` 和 `archived_sessions` |
| `src/scanner/mod.rs` | 固定加载 `state_5.sqlite`、`session_index.jsonl`、`.codex-global-state.json` |
| `src/codex/rollout.rs` | 解析 Codex rollout JSONL |
| `src/codex/usage.rs` | 解析 Codex `token_count` |
| `src/scanner/usage_consumer.rs` | 基于 JSONL offset 增量消费 Usage |
| `src/usage/normalized.rs` | 统一 Token 结构 |
| `src/usage/aggregate.rs` | Summary、Session、Model、Project 聚合 |
| `src/api/query.rs` | 内部 API DTO 和查询参数 |
| `frontend/src/data/types.ts` | 前端 API 类型 |
| `frontend/src/dashboard/FilterControls.tsx` | 模型/项目筛选 |
| `frontend/src/dashboard/session/SessionTable.tsx` | Session 表格 |
| `frontend/src/dashboard/session/SessionDetailDrawer.tsx` | Session 详情抽屉 |

当前 `ScanConfig` 只有一个 `codex_home`，`CodexMetadata` 也固定指向 Codex 的三个元数据源。

`Discovery` 的语义也是固定的：

```text
$CODEX_HOME/sessions
$CODEX_HOME/archived_sessions
```

并且默认只接受：

```text
rollout-*.jsonl
```

所以 Antigravity、Maka 不能简单通过增加两个目录路径接入。

---

## 2. 当前数据库已经有项目字段，但没有终端字段

当前 `threads` 已经包含：

```text
thread_id
parent_thread_id
root_session_id
agent_role
title
project_name
project_path
project_kind
metadata_model
created_at_ms
updated_at_ms
archived
```

因此：

- **项目字段不需要重新设计一套；**
- Antigravity 和 Maka 只需要把项目路径/名称归一化写入现有字段；
- 但当前 `threads` 没有 `terminal`；
- 当前 Session API 也没有返回 terminal。

当前 `usage_events` 已经是统一 Token 账本，核心字段包括：

```text
thread_id
root_session_id
occurred_at_ms
model
input_tokens
cached_tokens
cache_write_tokens
output_tokens
reasoning_tokens
total_tokens
estimated_cost_nanos_usd
```

从 Token 结构上看，新增终端可以复用 `usage_events`，但前提是各终端适配器先把原始字段转换成当前的 canonical 语义。

---

## 3. 当前查询筛选只有模型和项目

后端的 `UsageFilter` 当前只有：

```text
models
project_paths
include_projectless
include_unknown_project
```

当前 SQL 主要通过：

```text
usage_events
LEFT JOIN threads
```

再根据模型、项目进行筛选。

前端的 `DashboardFilters` 也是：

```ts
{
  models: string[],
  projects: ProjectSelection[]
}
```

没有 terminal。

因此终端筛选需要同时改：

- `UsageFilter`
- `parse_summary_params`
- `parse_session_query_params`
- Summary 查询
- Session 查询
- Model Distribution
- Project Distribution
- Skills 查询
- 前端 `DashboardFilters`
- `dashboardQueryKey`
- `sessionParams`
- 所有查询缓存 key

如果只在 Session 表格里加 terminal，而不贯穿 KPI 和图表，会出现页面各区域统计口径不一致。

---

## 4. 当前 Session 列表和 Drawer 的字段状态

当前列表 `SessionItemDto` 已经有：

```text
root_session_id
title
project_name
project_path
last_activity_at_ms
models_used
subagent_count
inclusive_usage
self_usage
subagent_usage
data_status
error_code
```

所以列表端：

- 项目字段已经有；
- 只需要增加 `terminal`；
- UI 增加终端列。

当前 Drawer 的 `SessionDetailResponse` / `MainSessionDetailDto` 只有：

```text
title
thread_id
root_session_id
models_used
model_usage
self_usage
subagent_count
inclusive_usage
```

Drawer 当前只展示：

- 标题；
- Session ID；
- 最后活动时间；
- Token 汇总；
- Main；
- Subagent。

Drawer 没有：

- terminal；
- project_name；
- project_path。

所以 Drawer 需要扩展 API DTO 和前端展示，而不是只从当前列表行临时拼接。建议详情 API 自己返回完整的 terminal/project 信息。

---

# 三、Antigravity 本机数据分析

## 1. 发现的目录

本机存在以下主要路径：

```text
~/.gemini/antigravity/
~/.gemini/antigravity/conversation_summaries.db
~/.gemini/antigravity/conversations/*.db
```

另外还发现：

```text
~/.gemini/antigravity-ide/conversations/*.db
~/.gemini/antigravity-ide/implicit/*.pb
~/.gemini/antigravity/annotations/*.pbtxt
~/.gemini/antigravity/agyhub_summaries_proto.pb
```

`antigravity-cli` 当前目录不存在：

```text
~/.gemini/antigravity-cli/conversations
```

macOS Application Support 下的：

```text
~/Library/Application Support/Antigravity
~/Library/Application Support/Antigravity IDE
```

主要是 IDE 运行状态和日志，不是最适合读取 Token 的主数据源。

当前应优先支持：

```text
~/.gemini/antigravity/conversations/*.db
~/.gemini/antigravity-ide/conversations/*.db
```

## 2. Antigravity 每个会话是一个 SQLite DB

每个 conversation DB 的表结构基本一致：

```text
battle_mode_infos
executor_metadata
gen_metadata
parent_references
steps
trajectory_meta
trajectory_metadata_blob
```

当前现场的 standalone Antigravity 大约有 10 个会话 DB，IDE 目录下还有 2 个会话 DB。运行中的 DB 会同时存在：

```text
.db
.db-shm
.db-wal
```

因此读取时不能只复制 `.db` 主文件，否则可能读不到 WAL 中的最新数据。

## 3. 可以提取的字段

### 会话 ID

文件名本身就是 cascade/conversation ID：

```text
<conversation-id>.db
```

可以作为 Antigravity 原生 session ID。

### 标题、项目和关系

`conversation_summaries.db` 的 `conversation_summaries` 表包含：

```text
conversation_id
title
step_count
last_modified_time
workspace_uris
status
project_id
parent_conversation_id
nesting_depth
```

当前现场能看到：

- 标题；
- workspace URI；
- 项目路径；
- projectless 会话；
- 父 conversation 字段；
- 会话状态；
- 最后修改时间。

例如项目路径可以从：

```text
file:///Users/.../ProjectName
```

转换成：

```text
project_path = /Users/.../ProjectName
project_name = ProjectName
```

没有 workspace 的会话可以归到：

```text
project_kind = projectless
```

### Token

`gen_metadata.data` 是 protobuf，不是 JSON，但可以离线解析。

关键字段：

```text
chatModel.usage.inputTokens
chatModel.usage.outputTokens
chatModel.usage.cacheReadTokens
chatModel.usage.thinkingOutputTokens
chatModel.usage.responseId
chatModel.responseModel
chatModel.modelDisplayName
```

可以映射到：

| Usagi 字段 | Antigravity 来源 |
|---|---|
| `model` | `modelDisplayName`，否则 `responseModel` |
| `output_tokens` | `outputTokens` |
| `reasoning_tokens` | `thinkingOutputTokens` |
| `cached_tokens` | `cacheReadTokens`，但需要重新确认口径 |
| `cache_write_tokens` | 当前没有明确来源，应该是 `NULL` |
| `event_id` | `responseId` |
| `thread_id` | cascade/conversation ID |
| `root_session_id` | 默认等于自身，除非父关系明确 |

## 4. Antigravity 当前存在两个重要口径问题

### 问题一：`cacheReadTokens` 不一定是 `inputTokens` 的子集

当前现场实际数据中出现了类似：

```text
inputTokens = 3070
cacheReadTokens = 16279
```

这直接写入 Usagi 当前 canonical 结构会违反：

```text
cached_tokens <= input_tokens
```

因此不能直接这样映射：

```text
input_tokens = inputTokens
cached_tokens = cacheReadTokens
```

需要先确认 Antigravity 的字段语义。

目前较合理的候选公式是：

```text
uncached_input = inputTokens
cached_tokens = cacheReadTokens
input_tokens = inputTokens + cacheReadTokens
cache_write_tokens = NULL
```

但这个公式必须用真实版本 fixture 和 Antigravity 语义再次确认，不能直接凭字段名决定。

### 问题二：`gen_metadata` 当前通常没有可靠时间戳

参考 parser 对当前 `.db` 读取时，很多 `gen_metadata` 记录的内部 timestamp 是空的。

不过同一个 DB 的：

```text
gen_metadata.idx
steps.idx
```

现场存在对应关系，而且 `steps.metadata` 中有：

```text
createdAt
source = user/model
```

因此可以尝试：

```text
gen_metadata.idx
  ↔ steps.idx
  ↔ steps.metadata.createdAt
```

用 `steps.metadata` 的时间作为 Token event 时间。

但这需要增加版本兼容测试，确认不同 Antigravity 版本中 `idx` 是否始终稳定对应。不能使用文件 mtime 代替事件时间，否则会违反 Usagi 当前“按真实事件时间统计”的数据口径。

## 5. Antigravity 父子关系不如 Codex 稳定

目前可以从以下位置尝试获得关系：

```text
conversation_summaries.parent_conversation_id
parent_references
trajectory_meta
```

但现场很多主会话的 `parent_conversation_id` 为空，`parent_references` 也不是所有会话都有。

因此第一阶段建议：

- Antigravity 每个 cascade 作为一个独立主 Session；
- `agent_role = main`；
- `root_session_id = thread_id`；
- 只有明确解析到父关系时才建立 Subagent 关系；
- 不要因为 `has_subtrajectory` 或某些 protobuf 字段就直接推断成 Codex Subagent。

否则可能把 Antigravity 内部 trajectory 误当成 Usagi 的主/子 Session 树。

## 6. Antigravity 结论

Antigravity 的判断是：

> **可以对齐到 Usagi 的统一 Session/Usage 数据模型，但不能直接对齐到当前 Codex 的 JSONL checkpoint 模型。**

第一阶段建议只支持：

```text
Antigravity .db
```

暂不支持：

```text
旧 .pb
依赖 Language Server RPC 的历史数据
```

旧 `.pb` 数据如果要支持，需要启动 Antigravity Language Server 并实现 RPC 读取，不适合放入第一阶段的本地静态扫描器。

---

# 四、Maka 本机数据分析

## 1. Maka 不是目录式 JSONL，而是单个 SQLite 数据库

本机主要数据源是：

```text
~/Library/Application Support/Maka/session-experience.sqlite
```

它不是：

```text
~/.maka/sessions/<id>/...
```

这种目录树，而是一个集中式 SQLite 数据库。

表结构：

```text
authorities
outbox
outbox_attachments
sessions
transcripts
```

其中最重要的是：

```text
sessions
transcripts
```

`SharedStorage`、`DIPS` 和 `Partitions/maka-browser` 更偏向浏览器/运行时状态，不建议作为 AI Session 用量来源。

## 2. Maka Session metadata 很适合映射

`sessions.summary` 是 JSON，当前可以看到：

```text
id
cwd
projectId
activityAt
name
lastMessageAt
status
backend
model
thinkingLevel
permissionMode
collaborationMode
orchestrationMode
```

可以直接映射：

| Usagi 字段 | Maka 来源 |
|---|---|
| `thread_id` | `sessions.session_id` |
| `title` | `summary.name` |
| `project_path` | `summary.cwd` |
| `project_name` | `cwd` 的 basename |
| `model` | `summary.model` 或 transcript 中的 `modelId` |
| `reasoning_effort` | `summary.thinkingLevel`，需要注意它可能是 Session 级别 |
| `last_activity_at_ms` | `activityAt` / `lastMessageAt` / transcript message timestamp |
| `agent_role` | 默认 `main` |

当前 `cwd="/"` 的会话应当归类为：

```text
project_kind = projectless
```

不能把 `/` 当作普通项目路径。

## 3. Maka transcript 里存在完整 Token usage

`transcripts.snapshot` 的结构大致是：

```text
{
  sessionId,
  generation,
  durableThrough,
  durable: [...]
}
```

其中完整的 Token message 形态是：

```text
message.type = "token_usage"
```

同时包含：

```text
id
turnId
ts
input
output
cacheHitInput
cacheMissInput
cacheWriteInput
cacheRead
cacheCreation
reasoning
total
```

附近的 assistant message 通常包含：

```text
id
modelId
ts
turnId
```

所以可以通过：

```text
turnId / 相邻 message / session summary
```

关联模型。

Maka 的字段和 Usagi canonical 结构基本一致：

```text
input_tokens       = input
cached_tokens      = cacheHitInput
cache_write_tokens = cacheWriteInput
output_tokens      = output
reasoning_tokens   = reasoning
total_tokens       = input + output
```

需要注意：

- `cacheRead` 和 `cacheHitInput` 可能是同一语义的不同字段，不能重复相加；
- `cacheCreation` 和 `cacheWriteInput` 也需要选择一个；
- 应优先使用完整 `token_usage` 消息，不要把 `inputTokens/outputTokens` 这类上下文估算字段当作第二份 Usage 事件；
- Usage 事件可以使用 message 的稳定 `id` 去重。

## 4. Maka 的不足

现场当前有 6 条 Session metadata，但只有部分 Session 有 transcript 数据。当前只看到了 3 条 transcript，其中 2 条包含完整 token usage。

这不一定代表 Maka 产品永远只有这部分数据，可能是：

- transcript 尚未落盘；
- 历史 transcript 被清理；
- 当前 DB 只保留 durable snapshot；
- Session 已存在，但没有产生完整模型调用；
- 数据正在 WAL 中更新。

因此 Maka 适配器必须允许：

```text
有 Session metadata，但没有 Usage
```

这种情况，不应当把它当成数据库错误。

Maka 当前没有明确的主/子 Session 关系字段。虽然有：

```text
collaborationMode
orchestrationMode
runningTurnIds
```

但不能直接推断出 Codex 那样的 `parent_thread_id` 树。

第一阶段建议所有 Maka Session 都作为独立 Main：

```text
agent_role = main
root_session_id = thread_id
subagent_count = 0
```

后续如果确认 Maka 的 transcript/runtimeSteps 中存在稳定的子任务 ID，再补充父子关系。

## 5. Maka 增量读取不能直接复用 Codex offset

Maka 是一个会被持续更新的 SQLite + WAL：

```text
session-experience.sqlite
session-experience.sqlite-wal
session-experience.sqlite-shm
```

它不是 append-only JSONL，所以不能直接使用：

```text
source_start_offset
source_end_offset
resolved_through_offset
```

更适合使用：

```text
session_id
transcript.generation
durableThrough
message.id
snapshot digest
sessions.updated_at
transcripts.updated_at
```

建议以：

```text
(terminal, native_session_id, message_id)
```

作为 Usage 事件去重和增量处理依据。

---

# 五、能否对齐当前 Codex 数据库

## 可以对齐的部分

三类终端都可以统一成：

```text
threads
usage_events
turns
```

统一的 Session 维度：

```text
thread_id
root_session_id
title
project_name
project_path
project_kind
agent_role
created_at_ms
updated_at_ms
```

统一的 Usage 维度：

```text
occurred_at_ms
model
input_tokens
cached_tokens
cache_write_tokens
output_tokens
reasoning_tokens
total_tokens
estimated_cost
```

这样做后，现有的：

- Dashboard KPI；
- Model Distribution；
- Project Distribution；
- Session 聚合；
- Token 统计；
- 费用估算；

都可以继续复用。

## 不能直接复用的部分

当前以下设计是 Codex 专用的：

1. `source_files.source_area` 只有：

   ```text
   sessions
   archived_sessions
   ```

2. `source_files` 假设一个物理文件对应一个 rollout；
3. `rollout_metadata_facts` 是“一文件一条”；
4. `usage_source_states` 也是“一文件一状态”；
5. checkpoint 是 JSONL byte offset；
6. `app_meta.codex_home_fingerprint` 假设只有一个 CODEX_HOME；
7. `DiscoverySnapshot` 只有两个 Codex 区域；
8. scanner worker 固定使用 Codex metadata parser；
9. `source_files.current_path` 全局唯一；
10. `threads.thread_id` 是全局主键，没有 terminal 维度。

尤其 Maka 是一个数据库里包含多个 Session：

```text
一个 Maka DB
  ├─ Session A
  ├─ Session B
  ├─ Session C
```

所以不能简单把一个 Maka 数据库当成一个 `source_files.thread_id`。

---

# 六、数据库改造建议

## 1. 增加 Terminal 枚举

建议统一使用：

```text
codex
antigravity
maka
```

不要让前端自行维护任意字符串。

至少需要增加：

```sql
terminal TEXT NOT NULL
CHECK (terminal IN ('codex', 'antigravity', 'maka'))
```

建议放在：

```text
threads
```

和来源表中。

已有 Codex 数据迁移为：

```text
terminal = 'codex'
```

## 2. 处理 Session ID 冲突

不能只给 `threads` 增加 terminal 字段而不处理主键。

不同终端理论上可能出现相同的 UUID。当前 `threads.thread_id` 是全局主键，因此建议采用内部 canonical ID：

```text
codex:<native_id>
antigravity:<native_id>
maka:<native_id>
```

同时保留：

```text
native_session_id
terminal
```

例如：

```text
thread_id          = maka:c66af207-...
native_session_id  = c66af207-...
terminal           = maka
```

`parent_thread_id` 和 `root_session_id` 也必须使用 canonical ID。

另一种方案是复合主键：

```text
PRIMARY KEY (terminal, native_session_id)
```

但当前所有外键、SQL、聚合和事件表都使用 `thread_id`，改成复合键的迁移成本更高。

## 3. 不建议强行把所有来源都塞入 `source_files`

建议保留当前 Codex source model，同时新增更通用的来源状态结构，例如：

```text
terminal_sources
terminal_session_states
```

大致包含：

```text
terminal
source_kind
source_path
native_session_id
thread_id
parser_version
source_revision
cursor_or_digest
last_seen_at_ms
status
```

来源类型可以是：

```text
codex_rollout_jsonl
antigravity_cascade_db
maka_session_sqlite
```

这样：

- Codex 继续使用现有 JSONL offset；
- Antigravity 使用 DB 文件 + response ID + idx；
- Maka 使用 generation/message ID/digest；
- 三者最终都写入统一的 `threads` 和 `usage_events`。

## 4. 现有 project 字段可以复用

数据库不需要新建一套 MakaProject 或 AntigravityProject 字段。

统一写入：

```text
project_name
project_path
project_kind
```

来源映射：

- Codex：现有 resolver 逻辑；
- Antigravity：workspace URI；
- Maka：summary.cwd；
- 无项目：`project_kind = projectless`；
- 无法判断：`project_kind = unknown`。

## 5. 需要新增 Migration

当前：

```text
LATEST_SCHEMA_VERSION = 10
```

建议新增一个或多个 migration：

```text
0011_terminal_dimension.sql
0012_terminal_source_state.sql
```

至少要完成：

- 旧 Codex threads 回填 `terminal='codex'`；
- 新增 terminal 约束；
- 新增 native session ID；
- 新增 terminal source state；
- 新增 terminal 相关索引；
- 迁移/校验所有父子关系和 Usage 外键。

---

# 七、后端 API 和聚合改造

## 1. UsageFilter 增加终端

建议：

```rust
pub struct UsageFilter {
    terminals: Vec<Terminal>,
    models: Vec<String>,
    project_paths: Vec<String>,
    include_projectless: bool,
    include_unknown_project: bool,
}
```

语义：

```text
同一维度内 OR
terminal + model + project 之间 AND
```

例如：

```text
terminal=codex OR maka
AND model=gpt-5.6-luna
AND project=/Users/hogee/Desktop/Usagi
```

## 2. 查询参数

建议使用重复参数：

```text
?terminal=codex
&terminal=antigravity
```

需要修改：

- `parse_summary_params`
- `parse_session_query_params`
- `validate_filter_value`
- Summary SQL
- Session eligibility SQL
- Model Distribution SQL
- Project Distribution SQL
- Skills SQL

## 3. API DTO

`SessionUsageDto` 增加：

```text
terminal
```

详情 DTO 建议增加：

```text
MainSessionDetailDto {
    terminal
    project_name
    project_path
}
```

也可以在 `SessionDetailResponse` 根部增加：

```text
terminal
project_name
project_path
```

这样 Drawer 不依赖列表行数据，也能独立完成展示。

## 4. 费用状态需要重新审视

Antigravity 当前没有明确的 cache write 字段，Gemini 模型也未必在现有价格表中。

Maka 的 `gpt-5.6-luna` 也可能属于当前价格表之外的路由模型。

因此扩展后很可能出现：

```text
Token 数据有效
费用未知或部分未知
```

当前 `SessionDataStatus` 会受到费用完整性影响，可能把这种 Session 显示成“数据不完整”。

建议后续拆开：

```text
usage_quality
cost_quality
```

而不是用一个 `data_status` 同时表达两者。

不要：

- 用 Codex 价格估算 Gemini；
- 把未知 cache write 默认为 0；
- 把未知价格默认为 0；
- 因为费用未知就丢弃 Token。

---

# 八、前端改造清单

## 1. Dashboard 过滤器

`DashboardFilters` 增加：

```ts
type Terminal = "codex" | "antigravity" | "maka";

type DashboardFilters = {
  terminals: Terminal[];
  models: string[];
  projects: ProjectSelection[];
};
```

需要修改：

- `frontend/src/data/types.ts`
- `canonicalDashboardFilters`
- `dashboardQueryKey`
- `sessionParams`
- `scope.ts`
- `useDashboardController`
- `useSessionTableController`
- `useSessionDetailController`
- `useDashboardChartsController`
- 相关测试 fixture

建议终端筛选器默认：

```text
全部
```

多选逻辑：

```text
同一 terminal 维度 OR
与模型/项目/时间范围 AND
```

终端选项：

```text
Codex
Antigravity
Maka
```

## 2. Session 列表新增终端列

当前表格已有：

```text
最后活动
标题
项目
模型
合计 Token
Sub 数量
缓存命中率
合计费用
```

建议增加：

```text
终端
```

推荐放在：

```text
标题之后，项目之前
```

显示友好名称：

```text
Codex
Antigravity
Maka
```

而不是直接显示小写枚举值。

## 3. Drawer 增加终端和项目

当前 Drawer header 可以扩展为：

```text
标题
终端：Maka
项目：PeekFlow
项目路径：/Users/hogee/Desktop/PeekFlow
Session ID
最后活动时间
```

建议项目路径使用 tooltip 或折叠展示，避免长路径撑开抽屉。

## 4. Codex Quota 的特殊处理

当前 Dashboard 固定加载：

```text
Codex Quota
```

但 Antigravity 和 Maka 没有接入 Codex quota。

因此：

- 终端选择仅 Maka/Antigravity 时，Codex quota 卡片应隐藏或显示“不适用”；
- 选择全部终端时，Quota 仍只能标记为 Codex quota，不能误认为所有终端的配额；
- 不要把三类终端 quota 混成一个指标。

## 5. 模型筛选器需要放宽 Provider 分组

当前前端模型分组固定为：

```text
OpenAI
Route-models
```

加入 Antigravity 后会出现：

```text
Gemini
gemini-3.x
```

加入 Maka 后也可能出现新的路由模型。

建议：

- 后端返回 provider；
- 前端不再硬编码只有两个 provider；
- 未知 provider 归入“其他”；
- 终端筛选可以先限制模型选项范围，避免同名模型产生混淆。

---

# 九、推荐实施顺序

## Phase 0：先冻结数据口径

先做两个 fixture 适配验证：

### Maka fixture

验证：

- `token_usage` 识别；
- `message.id` 去重；
- `turnId` 和 model 关联；
- `input = cacheHitInput + cacheMissInput + cacheWriteInput`；
- WAL 下读取；
- transcript rewrite；
- Session metadata 缺失 transcript；
- `cwd="/"` 的 projectless 处理。

### Antigravity fixture

验证：

- protobuf 解码；
- `responseId` 去重；
- `gen_metadata.idx` 与 `steps.idx` 的时间关联；
- `inputTokens/cacheReadTokens` 正确归一化；
- model fallback；
- workspace URI；
- `.db-wal` 快照；
- 缺失 timestamp 时的降级策略。

如果 Antigravity 时间和 cache 语义没有冻结，不建议直接进入正式数据库。

## Phase 1：数据库和通用身份

先完成：

- terminal enum；
- canonical session ID；
- `native_session_id`；
- `threads.terminal`；
- terminal source state；
- Codex 数据回填；
- 聚合层 terminal filter。

## Phase 2：先接入 Maka

原因：

- 字段最接近 Usagi canonical schema；
- 有稳定的 message ID；
- 有明确的 `ts`；
- 缓存字段完整；
- 项目和 Session metadata 清晰；
- 不需要立即解决 Antigravity 的 protobuf/RPC/时间问题。

## Phase 3：接入 Antigravity `.db`

第一阶段只支持：

```text
~/.gemini/antigravity/conversations/*.db
~/.gemini/antigravity-ide/conversations/*.db
```

暂不支持：

```text
旧 .pb
Language Server RPC
```

## Phase 4：后端 API 和前端

接入：

- terminal filter；
- terminal column；
- Drawer terminal/project；
- KPI/图表统一筛选；
- cost unknown 状态；
- Codex quota 条件显示。

---

# 十、最终建议

最稳妥的目标架构不是：

```text
把 Antigravity/Maka 伪装成 Codex rollout
```

而是：

```text
CodexAdapter
AntigravityAdapter
MakaAdapter
        ↓
统一 Session Metadata / Normalized Usage Event
        ↓
threads / usage_events / turns
        ↓
统一 AggregateReader / API / Dashboard
```

最终判断：

1. **Maka：可以较直接地对齐现有数据库的统一字段，建议优先实现。**
2. **Antigravity：可以对齐，但必须在适配器层处理 cache 语义和事件时间，不应把原始 protobuf 数值直接写入现有 `usage_events`。**
3. **数据库只增加一个 terminal 字段是不够的。** 还需要解决：
   - 多终端 Session ID 冲突；
   - Maka 一个 SQLite 包含多个 Session；
   - 不同终端不同增量游标；
   - 当前 `CODEX_HOME` 单一 source binding；
   - 当前 Codex 专用 source/checkpoint 表。
4. **项目字段本身已经存在，重点是让新适配器填充它，并把它暴露到 Drawer 详情 API。**
5. **终端筛选应当作为全局 Dashboard 维度，而不是只作用于 Session 列表。**
