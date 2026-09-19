# Usagi Antigravity Adapter 数据事实冻结报告

> 调查范围：本机真实 Antigravity 数据；不修改任何代码，不提出完整多终端架构方案。
>
> 本机安装版本：standalone Antigravity `2.15.0`；Antigravity IDE `2.5.5`。
>
> 调查路径：
>
> - `~/.gemini/antigravity/conversation_summaries.db`
> - `~/.gemini/antigravity/conversations/*.db`
> - `~/.gemini/antigravity/annotations/*.pbtxt`
> - `~/.gemini/antigravity-ide/conversations/*.db`
>
> 当前本机现场包含 standalone 10 个 conversation DB、IDE 2 个 conversation DB。运行中的会话会持续变化，本文中的数量是本次调查时的快照。

## 已验证事实

### 1. `steps` 表、`step_type` 与 `steps.metadata`

当前 standalone 和 IDE 的 `steps` 表 schema 相同：

```sql
CREATE TABLE steps (
    idx integer PRIMARY KEY,
    step_type integer NOT NULL DEFAULT 0,
    status integer NOT NULL DEFAULT 0,
    has_subtrajectory numeric NOT NULL DEFAULT false,
    metadata blob,
    error_details blob,
    permissions blob,
    task_details blob,
    render_info blob,
    step_payload blob,
    step_format integer NOT NULL DEFAULT 0
)
```

当前真实样本中，和 Usage 直接相关的稳定组合是：

| `step_type` | `steps.metadata.source` | 现场语义证据 |
|---:|---:|---|
| `15` | `2` | 所有带有效 Usage `responseId` 的 step 都唯一匹配到该组合；是当前版本的模型生成/模型调用步骤 |
| `14` | `4` | 用户步骤；在所有大样本中位于模型步骤之前或模型调用之间 |
| `132` | `2` | 模型来源的轨迹/输出步骤，但当前样本没有对应 Usage `responseId`；不能作为 Usage 事件计数 |
| `101`、`17`、`23`、`98`、`99` 等 | `5` | 其他系统/轨迹步骤；当前没有 Usage authoritative record |

本次全量扫描中，真正带 Token counter 的 `gen_metadata` Usage 记录，其 `responseId` 全部能在唯一一个 `step_payload` 中找到；这些匹配到的 step 全部是：

```text
step_type = 15
metadata.source = 2
```

`steps.metadata` 的 protobuf 中，当前能被真实数据稳定验证、且与 Usage 相关的字段只有：

| protobuf 路径 | 现场验证语义 |
|---|---|
| `steps.metadata` field `1` | 嵌套 Timestamp；包含 seconds，部分记录包含 nanos；用于步骤事件时间 |
| `steps.metadata` field `3` | source enum；当前 `2=模型`、`4=用户`、`5=其他/系统类`，由实际步骤顺序和 `step_payload`/Usage 关联验证 |

当前 `steps.metadata` 没有发现以下 Usage counter：

```text
inputTokens
cacheReadTokens
outputTokens
thinkingOutputTokens
reasoningTokens
responseId
```

其他 metadata protobuf tag 在不同步骤中存在，但本次没有足够本地证据为它们冻结稳定语义；它们不应作为 Usage 字段读取。

### 2. `gen_metadata.data` 的 Usage 字段及实测关系

当前真实 `gen_metadata.data` 的结构为：

```text
GeneratorMetadata
└─ chatModel
   └─ usage
```

在真实记录中观察到的 Usage 字段包括：

| protobuf 路径 | 当前实测事实 |
|---|---|
| `chatModel.usage` field `2` | `inputTokens`；在 cache 命中后通常变小，表现为本次未从 cache 读取的新输入部分 |
| `chatModel.usage` field `3` | `outputTokens`；当前记录中包含 thinking/reasoning 输出 |
| `chatModel.usage` field `5` | `cacheReadTokens`；缓存读取量，经常大于 field 2 的 `inputTokens` |
| `chatModel.usage` field `9` | `thinkingOutputTokens`；thinking/reasoning 输出子集 |
| `chatModel.usage` field `10` | 当前 879 条完整 Usage 中满足 `outputTokens - thinkingOutputTokens`，可视为非 thinking 的 output 部分；不应再次加到 output |
| `chatModel.usage` field `11` | `responseId`；当前完整 Usage 的稳定事件关联 ID |
| `chatModel.usage` field `7` | `bot-<uuid>` 形式的 per-call 字符串；当前无法冻结为 Usage event ID |
| `chatModel.usage` field `8` | 33 字节左右的二进制字段；当前无法冻结为 Usage counter 或 event ID |
| `chatModel.usage` field `1`、`6` | 在当前完整记录中分别表现为高度固定值，不能解释为 canonical Token counter；不纳入 Usage 映射 |

当前样本中出现了以下真实序列：

```text
inputTokens = 3070
cacheReadTokens = 16279

inputTokens = 3921
cacheReadTokens = 16276

inputTokens = 6673
cacheReadTokens = 16281
```

这不是偶发的单条坏数据：本次跨 standalone/IDE 样本统计中，约 883 条带 `chatModel.usage` 的记录里，719 条出现：

```text
cacheReadTokens > inputTokens
```

同时，完整记录都满足：

```text
thinkingOutputTokens <= outputTokens
```

除去没有 Token counter 的 placeholder，当前真实记录中 `outputTokens = thinkingOutputTokens + field10` 成立。

因此，`cacheReadTokens` 不能被视为 `inputTokens` 已包含的子集；现场数据证明 field 2 和 field 5 是两个并列输入组成部分。当前最符合实际数据且能满足 Usagi canonical 不变量的公式是：

```text
uncached_input_tokens = inputTokens
cached_tokens         = cacheReadTokens 或 0
input_tokens          = inputTokens + cacheReadTokens
cache_write_tokens    = NULL
output_tokens         = outputTokens
reasoning_tokens      = thinkingOutputTokens 或 0
total_tokens          = input_tokens + output_tokens
```

其中：

```text
cached_tokens <= input_tokens
reasoning_tokens <= output_tokens
total_tokens = input_tokens + output_tokens
```

当前 `gen_metadata` Usage protobuf 中没有观察到可以稳定证明为 cache-write 的独立 counter。`field10` 已被真实算术关系证明是 output 的非 thinking 部分，不是 cache write。

### 3. Usage authoritative source

#### 3.1 两张表不是同粒度的一一对应表

当前真实数据证明：

```text
gen_metadata.idx != steps.idx 的可靠关联键
```

表面上两者都从 `0` 开始，但在实际 DB 中：

- 一个 `gen_metadata` Usage 行的 `idx=0` 可以对应后续 `steps.idx=1` 的模型步骤；
- `steps` 中存在大量没有 Usage 的步骤；
- `step_type=132` 也有 `metadata.source=2`，但没有 Usage `responseId`；
- `steps` 的数量通常显著大于 `gen_metadata`；
- 小会话还存在 `gen_metadata` placeholder 行没有 Token counter 的情况。

#### 3.2 当前真实数据的关联方式

当前本机样本中：

- 约 884 个 `gen_metadata` 行；
- 其中 1 个没有 `chatModel.usage`；
- 4 个 `chatModel.usage` 只包含非 Token/placeholder 字段，没有 input/output/cache/reasoning counter；
- 879 个完整非零 Usage 记录具有非空 `responseId`；
- 这 879 个 `responseId` 均能在 `steps.step_payload` 中唯一找到；
- 找到的 step 全部是 `step_type=15`、`metadata.source=2`；
- 所有这些匹配 step 都有可解析的 `steps.metadata` 时间戳。

因此当前真实版本应使用：

```text
gen_metadata.usage.responseId
    ↔ steps.step_payload 中的 responseId
```

而不是：

```text
gen_metadata.idx
    ↔ steps.idx
```

#### 3.3 authoritative 定义

推荐冻结为：

```text
gen_metadata.data
    = Usage、model、responseId 的唯一 authoritative source

steps.step_payload
    = responseId 到模型步骤的关联索引

steps.metadata
    = 关联后的 role/source 和事件时间来源
```

`steps.metadata` 不应单独生成 Usage 事件。

这样可避免：

```text
gen_metadata 统计一次
steps 再统计一次
```

造成双重计数。

### 4. 稳定 identity 与事件时间

#### 4.1 event identity

当前真实数据中，最稳定的 Usage event identity 是：

```text
(conversation_id, responseId)
```

原因：

- `responseId` 出现在 `gen_metadata.usage`；
- 同一个 `responseId` 在对应 conversation 的 `step_payload` 中唯一出现；
- 当前完整 Usage 没有观察到 conversation 内重复 responseId；
- `gen_metadata.idx` 只是在当前 DB 版本中的行索引，不能作为跨 rewrite/compaction 的事件 ID；
- `steps.idx` 是 trajectory step 索引，已被实际样本证明不能直接和 `gen_metadata.idx` 对齐。

当前 4 个没有 responseId 的 `chatModel.usage` 行没有 input/output/cache/reasoning counter，属于 placeholder，不构成可计费 Usage 事件。

如果未来出现“有完整 Token counter 但无 responseId”的记录，当前本地数据没有证明一个稳定替代 ID。该情况应进入 quarantine/partial，而不是静默用 `idx` 冒充稳定 event ID。

#### 4.2 event time

Usage event 的真实时间应取：

```text
与 responseId 唯一匹配的 step_payload 所在 steps 行
  → steps.metadata field 1 Timestamp
```

当前 879 条完整 Usage 均可通过该方式取得时间。

`gen_metadata` 内部的 `chatStartMetadata.createdAt` 在当前 `.db` 样本中没有提供可用 Usage 时间。

`conversation_summaries.last_modified_time` 比最后一个模型 step 时间晚约零到几十秒，代表会话索引更新/持久化时间，不是每个模型调用的 event time。

禁止使用：

```text
.db 文件 mtime
.db-wal mtime
conversation_summaries.last_modified_time
```

代替单次模型调用的事件时间。

### 5. SQLite + WAL 和增量读取

#### 5.1 当前本机状态

全部 12 个 standalone/IDE conversation DB 的 SQLite journal mode 都是：

```text
wal
```

现场存在：

```text
<conversation>.db
<conversation>.db-wal
<conversation>.db-shm
```

当前 WAL 文件在调查时多数已经被 checkpoint 成 `0` 字节，但 `-shm` 文件仍存在；这不代表运行期间不会产生 WAL 内容。

对同一个运行中的 standalone conversation 使用两个 SQLite read-only 连接同时读取，两个连接获得了相同的：

```text
gen_metadata count
steps count
parent_references count
max(idx)
```

说明只读 SQLite 事务能够获得一致的 DB+WAL 视图；读取时应保留 SQLite 的一致性事务语义，不应自行拼接主库和 WAL 字节。

#### 5.2 新模型调用的实际表现

在本次调查期间，正在运行的 conversation：

```text
49e69e84-d3f2-4f56-8c17-4ea8ded58b31
```

其 `gen_metadata` / `steps` 行数在连续观察中增长，最新观察到：

```text
gen_metadata = 133
steps        = 269
max(gen_metadata.idx) = 132
max(steps.idx)        = 268
```

这说明新模型调用会以新的 DB 行体现，并且当前观察到的主库/WAL checkpoint 行为会把 WAL 内容合并回主库。

#### 5.3 UPDATE、rewrite、compaction

本次静态/短时运行观察中：

- 没有观察到已存在 Usage 行被 UPDATE 成另一条模型调用；
- 没有观察到 conversation DB 文件重命名；
- 没有观察到删除或归档目录；
- SQLite 表使用 `idx` 主键，数据库存在 freelist，说明 SQLite 层面不能假设永远 append-only；
- 没有足够本地证据冻结 Antigravity 是否会对历史 `gen_metadata` 行做 rewrite、删除或 compaction。

因此，`idx` 只能作为本次扫描的辅助 cursor，不能作为永久 event identity。

#### 5.4 当前可冻结的 checkpoint 依据

对于完整 Usage，建议冻结为：

```text
source identity = conversation_id
usage identity  = responseId
observed cursor  = max(idx) / file revision / data_version，仅作为重新扫描提示
```

重启后应：

1. 在一个一致 SQLite read transaction 中读取 `gen_metadata`、`steps`；
2. 按 `responseId` 重新关联 step；
3. 以 `(conversation_id,responseId)` 幂等去重；
4. 即使重新读取已有 idx，也不能重复计数；
5. 不依赖“只读取 max idx 之后的行”作为唯一正确性依据；
6. 如果出现完整 Usage 无 responseId，标记 partial/quarantine，而不是猜测 ID。

这样可以防止：

```text
重启漏读
重复读取重复计数
idx rewrite 后错误跳过
```

对于 conversation 删除、归档和重命名：本机当前样本没有提供足够事实冻结具体行为，只能在完整目录枚举后把“文件不再存在”标为候选 missing，不能仅根据一次目录不可读就推断删除。

### 6. 各来源职责和优先级

| 数据源 | 当前真实职责 | authoritative/fallback 判断 |
|---|---|---|
| `conversation_summaries.db` | standalone 会话清单、title、workspace URI、status、last_modified、last_user_input、project_id | standalone 的 title/workspace/session updated authoritative metadata source |
| `conversations/<id>.db` | 单个 cascade 的 steps、gen_metadata、trajectory metadata、parent references | Usage/model/responseId authoritative；workspace 可作 summary 缺失时 fallback |
| `gen_metadata.data` | 每次模型调用的 Usage、model、responseId | Usage 唯一主数据源 |
| `steps.step_payload` | 包含 responseId 的模型步骤关联点 | 用于 Usage 与 step/time 的 join，不独立计数 |
| `steps.metadata` | step source 和 step Timestamp | 关联后的 role/time source，不独立计数 |
| `annotations/*.pbtxt` | 当前包含 title 或 last_user_view_time | title fallback/UI annotation；不含 Usage/project/模型 event time |
| `antigravity-ide/conversations/*.db` | IDE 的 per-conversation DB | schema 与 standalone 相同；当前没有对应 `conversation_summaries.db`，title/index 更新时间来源尚未冻结 |

在当前 standalone 数据中：

- 10 个 `conversation_summaries` ID 与 10 个 conversation DB 文件完全对应；
- summary 的 workspace URI 与 DB 内 `trajectory_metadata_blob` workspace 在有 workspace 的 8 个会话中一致；
- summary title 与对应 annotation title 在有 title 的会话中一致；
- annotation 只有 title 或 last_user_view_time，没有 Usage。

推荐优先级：

```text
standalone title/workspace/time
    conversation_summaries.db

standalone Usage/model/event identity
    conversation DB gen_metadata

Usage event time/role
    responseId 匹配后的 steps.metadata

annotation
    只作 title/UI fallback

IDE title/updated
    当前尚无本机 summary index 证据，暂不冻结
```

### 7. 父子关系

#### `parent_conversation_id`

当前 standalone `conversation_summaries.db` 的 10 条记录中，`parent_conversation_id` 全部为空，`nesting_depth` 全部为 `0`。

因此本机真实数据没有证明它当前能提供可用的父会话关系。

#### `parent_references`

当前只有一个 standalone DB：

```text
effa6389-921a-497e-87e0-5a2962526c07.db
```

包含 4 条 `parent_references`。

这些 protobuf 中包含 UUID-like 字符串和当前 cascade ID 的编码引用，但这些 UUID 没有和当前 `conversation_summaries.parent_conversation_id` 建立可验证的一一关系；本次没有找到足够本地证据证明其字段含义就是 Usagi 的直接 parent conversation。

因此不能直接映射为：

```text
parent_thread_id
root_session_id
agent_role=subagent
```

#### `trajectory_meta` / `has_subtrajectory`

当前 12 个 `.db` 都有一条 `trajectory_meta`，其中：

```text
trajectory_type = 4
source = 1
cascade_id = 当前 conversation ID
```

但当前 12 个 DB 的：

```text
steps.has_subtrajectory != 0
```

数量全部为 0。

因此当前本机数据没有证明 trajectory_meta 或 subtrajectory 是可直接映射成 Session 父子树的关系。

第一阶段数据契约应冻结为：

```text
每个 Antigravity conversation 默认：
agent_role = main
parent_thread_id = NULL
root_session_id = 自身
```

只有未来获得明确、可重复验证的 parent relation fixture 后，才允许建立 Subagent 树。

### 8. standalone 与 IDE schema

本机安装版本：

```text
standalone Antigravity = 2.15.0
Antigravity IDE        = 2.5.5
```

本机 10 个 standalone DB 和 2 个 IDE DB 的 SQLite table schema hash 完全一致，均包含：

```text
battle_mode_infos
executor_metadata
gen_metadata
parent_references
steps
trajectory_meta
trajectory_metadata_blob
```

因此以下部分可以共用一个 DB/protobuf parser：

```text
steps
steps.metadata
steps.step_payload
gen_metadata
gen_metadata.usage
trajectory_metadata_blob
trajectory_meta
```

但两者不能直接共用 summary/index 层面的职责：

- standalone 有 `conversation_summaries.db`；
- IDE 当前本机没有对应 summary index；
- IDE 的 title、updated time、project fallback 尚未冻结。

## 尚未验证事实

以下事实本次本机数据没有足够证据确认，不应在 Adapter 契约中假定：

1. Antigravity 未来版本是否保持 `step_type=15`、`source=2` 的数值稳定性。
2. Antigravity 是否会 UPDATE 已存在的 `gen_metadata` Usage 行。
3. SQLite compaction/vacuum 后 `idx` 是否会被重排或复用。
4. conversation 删除后 summary row、conversation DB、annotation 的先后和最终状态。
5. conversation 归档是否使用独立目录、status 字段，或只是 summary 状态变化。
6. conversation 重命名是否只更新 summary/annotation，还是会重写 cascade DB。
7. `parent_references` protobuf 各字段的正式 schema 语义。
8. `trajectory_type=4`、`source=1` 的完整枚举定义。
9. Antigravity 是否存在真实的单会话多模型切换样本；本机当前完整 Usage 观测到的模型主要是 `gemini-3.8-flash`，不能据此证明多模型场景不存在。
10. IDE 数据库的 title、created time、updated time 是否存在另一个本机索引文件。
11. `gen_metadata.usage` field `1`、`6`、`7`、`8` 的正式 schema 名称；本次只确认它们不是 Usagi 当前需要的 canonical counter/稳定 event ID。
12. 没有 responseId 但拥有完整 Token counter 的未来版本记录应使用什么稳定 ID；当前现场没有这种记录。

## 推荐的数据契约

### Usage authoritative contract

```text
主 Usage 来源：gen_metadata.data.chatModel.usage
```

只对包含以下有效 counter 的记录生成 Usage：

```text
inputTokens
outputTokens
或 cacheReadTokens/thinkingOutputTokens 中至少有实际非零 Usage
```

placeholder/no-counter 的 `gen_metadata` 行不生成 Usage。

### Model step join contract

```text
gen_metadata.usage.responseId
    必须在某个 steps.step_payload 中唯一出现

匹配后的 step 必须满足：
step_type = 15
metadata.source = 2
```

如果完整 Usage 无法完成这个唯一 join，标记 partial/quarantine，不静默使用 `gen_metadata.idx`。

### Canonical Token contract

```text
cached_tokens         = cacheReadTokens 或 0
input_tokens          = inputTokens + cached_tokens
cache_write_tokens    = NULL
output_tokens         = outputTokens
reasoning_tokens      = thinkingOutputTokens 或 0
total_tokens          = input_tokens + output_tokens
```

并验证：

```text
cached_tokens <= input_tokens
reasoning_tokens <= output_tokens
total_tokens = input_tokens + output_tokens
```

`field10` 仅作为实测的 `other_output_tokens = outputTokens - thinkingOutputTokens`，不再加入 `output_tokens`。

### Event identity contract

```text
event_id = (terminal='antigravity', conversation_id, responseId)
```

`gen_metadata.idx` 只能作为扫描提示/诊断信息，不能单独作为 event identity。

### Event time contract

```text
occurred_at_ms = matched steps.metadata.field1 Timestamp
```

禁止：

```text
.db mtime
.db-wal mtime
conversation_summaries.last_modified_time
```

替代单次模型调用时间。

### Session metadata contract

standalone：

```text
title           = conversation_summaries.title
project/workspace = conversation_summaries.workspace_uris
updated_at      = conversation_summaries.last_modified_time
last_user_time  = conversation_summaries.last_user_input_time
```

DB fallback：

```text
workspace = trajectory_metadata_blob
```

IDE 当前只冻结：

```text
workspace = trajectory_metadata_blob
Usage/model/time = DB 内 steps/gen_metadata
```

IDE title/updated time 暂不作为已冻结字段。

### Relationship contract

第一阶段：

```text
agent_role = main
parent_thread_id = NULL
root_session_id = conversation_id
```

除非未来 fixture 证明 `parent_conversation_id` 或 `parent_references` 的正式关系语义，否则不能生成 subagent。

## 必须阻塞实施的问题

以下问题解决前，不应把 Antigravity Adapter 标记为“生产数据契约已冻结”：

1. **需要冻结删除、归档、重命名、rewrite/compaction 的行为。**
   当前真实现场没有覆盖这些生命周期变化，单纯使用 max idx 会有漏读或错误跳过风险。
2. **需要将 responseId-to-step_payload 关联写入 fixture。**
   不能实现成 `gen_metadata.idx = steps.idx`。
3. **需要明确完整 Usage 无 responseId 时的处理。**
   推荐阻塞/隔离，而不是猜测 ID。
4. **需要验证未来版本是否保持 `step_type=15/source=2`。**
   当前结论只覆盖本机 standalone 2.15.0 和 IDE 2.5.5 的真实数据。
5. **如果第一阶段要求支持 IDE 的 title/updated time，需要先找到 IDE 对应的 authoritative index。**
6. **如果第一阶段要求父子 Session，需要先解码并验证 `parent_references` 正式语义。**
7. **需要保留一个真实活跃 WAL 快照 fixture。**
   当前短时读取证明只读事务可以一致读取，但还没有覆盖 WAL 中未 checkpoint、同时写入期间的重启场景。

## 非阻塞、可以后续支持的问题

以下问题不影响第一阶段先读取独立 Main Session 的 Usage：

1. Antigravity `.pb` / Language Server RPC 历史数据。
2. IDE title 的额外索引来源。
3. 多模型切换场景；当前 adapter 可以按每条 Usage 的 responseModel/displayName 继续扩展。
4. parent/subtrajectory 完整树关系。
5. cache write 如果未来版本提供稳定字段，可再增加；当前使用 `NULL`。
6. Gemini 价格目录和费用估算；Token 数据可以先独立入库，费用保持 unknown/partial。
7. annotation 的 last_user_view_time；它不是 Usage event time。

## Fixture 清单

| Fixture | 来源 | 必须验证的不变量 |
|---|---|---|
| `standalone_project_complete.db` | 当前 `96d66dc2-...` | 364 条 gen metadata、项目 workspace、step 关联、cacheRead/input 关系、全量 event time |
| `standalone_parent_references.db` | 当前 `effa6389-...` | 4 条 parent references 不被误判成 parent_thread_id；projectless 处理 |
| `standalone_active_wal.db` | 当前活跃 `49e69e84-...` 连同 `.db/.db-wal/.db-shm` | 只读一致快照、行数增长、重启重复读取不重复计数 |
| `ide_complete.db` | 当前 `antigravity-ide/5d0f0f9a-...` | 与 standalone 相同 schema、workspace fallback、无 summary index 时的 metadata 状态 |
| `step_type_matrix.db` | 当前 `49e69e84-...` | `14/4=user`、`15/2=model Usage`、`132/2=non-authoritative model trajectory`、其他 type 不计 Usage |
| `response_id_join.db` | 当前包含 responseId 的大型 DB | responseId 在 step_payload 中唯一匹配，不能使用相同 idx 假设 |
| `placeholder_no_usage.db` | 当前 `8c59510d-...` | gen_metadata 无 Usage counter 时不生成事件 |
| `usage_without_response_id.db` | 当前小 DB 的无 counter 行；另需构造完整 counter/no-responseId 副本 | 当前 no-counter 行忽略；未来完整 counter/no-ID 必须 quarantine |
| `annotation_title.pbtxt` | 当前 annotations | 有 title 时与 summary title 一致；只有 last_user_view_time 时不得生成 title/Usage |
| `lifecycle_deleted_copy` | 从真实 DB 副本构造 | 删除后不漏历史、不重复恢复、summary/file 缺失状态可识别 |
| `lifecycle_rename_copy` | 从真实 DB 副本构造 | conversation ID/event ID 不随文件名/title 改变 |
| `lifecycle_rewrite_compaction_copy` | 从真实 DB 副本构造 | idx 重排/重写时 responseId 幂等去重，不漏、不重 |

所有 fixture 都应保留：

```text
.db
.db-wal（存在时）
.db-shm（存在时）
conversation_id
summary index row（存在时）
```

原始提示词、step payload 和模型输出只作为专家离线验证数据保留，不应进入 Usagi 正式 normalized 数据库。
