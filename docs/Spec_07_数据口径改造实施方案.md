# Spec07 数据口径改造实施方案

- 文档版本：v0.1
- 基线源码：`Usagi-69be2a4`（commit `69be2a4d5095679192fa7c1667a0a7e39ab7234e`）
- 数据口径基线：
  - `normalizedTokenUsage数据口径.md` v0.2
  - `codex rollout数据口径.md` v0.2
- 改造性质：**替换式改造，不做旧方案兼容兜底**
- 当前状态：**已完成的历史实施方案；不作为当前 Luna 执行入口。后续真实 Codex metadata 适配与冗余清理由 Spec08 接管。**
- 当前支持范围：**OpenAI / Codex rollout，GPT-5.6+ 口径**
- 当前不实现：Responses API 直连 Adapter、Chat Completions Adapter、Anthropic、Gemini、GPT-5.6 以前 cache-write 兼容

---

## 1. 改造完成后的唯一目标结构

### 1.1 目录结构

本轮完成后必须至少形成：

```text
src/
├─ usage/
│  ├─ mod.rs
│  ├─ normalized.rs
│  ├─ adapters/
│  │  ├─ mod.rs
│  │  └─ openai/
│  │     ├─ mod.rs
│  │     └─ codex.rs
│  ├─ aggregate.rs
│  ├─ processor.rs
│  ├─ pipeline.rs
│  ├─ ledger.rs
│  ├─ rebuild.rs
│  └─ carry.rs
├─ codex/
│  └─ usage.rs
├─ storage/
│  ├─ migrations.rs
│  ├─ schema/
│  │  ├─ 0001_initial.sql
│  │  ├─ 0002_usage_ledger.sql
│  │  └─ 0003_normalized_token_usage.sql
│  └─ usage.rs
└─ api/
   └─ query.rs
```

当前不要创建空文件：

```text
responses.rs
chat_completions.rs
anthropic.rs
gemini.rs
```

### 1.2 唯一 canonical Token 类型

`src/usage/normalized.rs` 必须定义：

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedTokenUsage {
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
}
```

Adapter 之后，MU 所有 Token 算术、账本、SQLite v3、聚合、API DTO、前端 DTO 都使用这套名称。

### 1.3 canonical 不变量

必须由 `NormalizedTokenUsage::new/validate` 强制：

```text
input_tokens >= 0
cached_tokens >= 0
cache_write_tokens == null OR cache_write_tokens >= 0
output_tokens >= 0
reasoning_tokens >= 0
total_tokens >= 0

cached_tokens <= input_tokens
reasoning_tokens <= output_tokens
total_tokens == input_tokens + output_tokens

cache_write_tokens != null
    => cached_tokens + cache_write_tokens <= input_tokens
```

算术必须使用 checked arithmetic，溢出必须返回错误，不允许 saturating、wrapping 或静默归零。

### 1.4 派生值

统一规则：

```text
uncached_input_tokens =
    cache_write_tokens != null
        ? input_tokens - cached_tokens - cache_write_tokens
        : null

other_output_tokens =
    output_tokens - reasoning_tokens

cache_hit_rate =
    input_tokens > 0
        ? cached_tokens / input_tokens
        : null
```

聚合时必须先累计 token，再计算比例：

```text
aggregate.cache_hit_rate
= Σcached_tokens / Σinput_tokens
```

禁止平均逐事件/逐 Session 的命中率。

---

## 2. 本轮必须删除的旧定义

这是替换式改造。以下定义不得保留为运行时 fallback、alias、兼容字段或双写字段。

| 旧定义 | 处理 |
|---|---|
| `TokenVector` | 删除，由 `NormalizedTokenUsage` 完全替代 |
| `CacheWriteStatus` | 删除 |
| `cache_write_status` | 从 Domain、Processor、Storage v3、Aggregate、API、Frontend 删除 |
| `UnsupportedZero` | 删除 |
| `UnknownMissing` | 删除；未知由 `cache_write_tokens: None` 表达 |
| `cached_input_tokens` | Adapter 前可作为 Codex raw 字段存在；Adapter 后全部改为 `cached_tokens` |
| `cache_write_input_tokens` | Adapter 前可作为 Codex raw 字段存在；Adapter 后全部改为 `cache_write_tokens` |
| `reasoning_output_tokens` | Adapter 前可作为 Codex raw 字段存在；Adapter 后全部改为 `reasoning_tokens` |
| `reported_total_tokens` | 删除；raw `total_tokens` 校验后 canonical 只保留 `total_tokens` |
| `derived_total_tokens` | 删除；canonical `total_tokens` 本身固定等于 `input + output` |
| `cache_tokens` | **彻底删除，不迁移、不改名、不保留派生字段** |
| `CACHE_WRITE_CAPABILITY_CONFLICT` | 删除，不再产生该 anomaly |
| model capability → `UnsupportedZero` 推断 | 删除 |

禁止采取以下做法：

```text
旧字段 + 新字段同时存在
serde alias 接受旧 API 字段
SQL 同时读新旧列
API 同时返回新旧字段
Frontend 遇到新字段失败后退回旧字段
cache_tokens 改名成 cached_tokens
缺失 cache_write_tokens 时默认成 0
```

### 2.1 唯一允许看到旧列名的位置

`0002_usage_ledger.sql` 是已经存在的历史 migration，不能为了“看起来干净”改写。

新 `0003_normalized_token_usage.sql` 在执行 v2→v3 数据搬迁时也必须读取旧列名。

因此旧列名只允许存在于：

1. `src/storage/schema/0002_usage_ledger.sql`；
2. `src/storage/schema/0003_normalized_token_usage.sql` 的迁移输入部分；
3. Codex raw parser / raw Adapter 输入及其 raw fixture；
4. 专门验证“旧字段已删除”的测试字符串/断言。

这不是运行时兜底。v3 运行时表、Rust Domain、API、Frontend 不得继续依赖旧字段。

---

## 3. 第一步：建立 `NormalizedTokenUsage`

### 3.1 新建 `src/usage/normalized.rs`

从当前 `src/domain.rs::TokenVector` 搬出真正仍需要的能力，并按新字段重写。

必须实现：

```rust
impl NormalizedTokenUsage {
    pub fn zero() -> Self;
    pub fn new(...) -> Result<Self, DomainError>;
    pub fn validate(&self) -> Result<(), DomainError>;
    pub fn checked_add(&self, other: &Self) -> Result<Self, DomainError>;
    pub fn checked_sub(&self, previous: &Self) -> Result<Self, DomainError>;
    pub fn uncached_input_tokens(&self) -> Option<i64>;
    pub fn other_output_tokens(&self) -> i64;
    pub fn fingerprint(&self) -> [u8; 32];
}
```

`zero()` 固定为：

```text
input_tokens       = 0
cached_tokens      = 0
cache_write_tokens = Some(0)
output_tokens      = 0
reasoning_tokens   = 0
total_tokens       = 0
```

### 3.2 `checked_add` 的 cache-write 规则

```text
Some(a) + Some(b) => Some(a + b)
None    + anything => None
anything + None    => None
```

其余五个整数正常 checked-add。

结果重新经过 canonical validation。

### 3.3 `checked_sub` 的 cache-write 规则

```text
Some(current) - Some(previous)
    => current >= previous 时 Some(delta)
    => current < previous 时返回 cache-write negative-delta 错误

None - anything
anything - None
    => cache_write_tokens = None
```

其余 required 字段任一出现负 delta 都按现有“total chain reset / negative delta”语义处理。

**不得再产生“capability conflict”。**

### 3.4 fingerprint

新 fingerprint 输入只包含：

```text
USAGE_CANONICAL_ALGORITHM_VERSION
input_tokens
cached_tokens
cache_write_tokens 的 Some/None tag
cache_write_tokens value（Some 时）
output_tokens
reasoning_tokens
total_tokens
```

删除 status byte，删除 `reported_total_tokens`。

本轮 canonical 算法改变，因此：

```rust
USAGE_PARSER_VERSION = 3;
USAGE_CANONICAL_ALGORITHM_VERSION = 3;
```

`canonical_algorithm_for(3) == Some(3)`。

不要让新代码继续宣称 parser/canonical v2。

### 3.5 Token 版本常量归属

建议把：

```text
USAGE_PARSER_VERSION
USAGE_CANONICAL_ALGORITHM_VERSION
canonical_algorithm_for
```

从 `src/domain.rs` 移到 `src/usage/normalized.rs`，再由 `usage/mod.rs` 统一 re-export。

目的：Token canonical 版本由 Token canonical 模块所有，不再污染通用 Domain。

### 3.6 删除 `src/domain.rs` 的旧代码

完成新类型后，从 `src/domain.rs` 删除：

```text
CacheWriteStatus
TokenVector
add_cache_write
subtract_cache_write
与旧三态相关的 tests
```

保留通用 `DomainError` 和非 Token domain 类型。

---

## 4. 第二步：建立 OpenAI/Codex Adapter

### 4.1 新建模块入口

`src/usage/adapters/mod.rs`：

```rust
pub mod openai;
```

`src/usage/adapters/openai/mod.rs`：

```rust
pub mod codex;
```

`src/usage/mod.rs` 增加：

```rust
pub mod adapters;
pub mod normalized;
```

并按项目需要 re-export `NormalizedTokenUsage`。

### 4.2 新建 `src/usage/adapters/openai/codex.rs`

定义 Codex raw Token 结构：

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexRawTokenUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_write_input_tokens: Option<i64>,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}
```

raw 字段必须保持 Codex rollout 原名。

实现：

```rust
pub struct CodexRolloutAdapter;

impl CodexRolloutAdapter {
    pub fn normalize(
        raw: CodexRawTokenUsage,
    ) -> Result<NormalizedTokenUsage, ...>;
}
```

唯一映射：

```text
Codex input_tokens                 → input_tokens
Codex cached_input_tokens          → cached_tokens
Codex cache_write_input_tokens     → cache_write_tokens
Codex output_tokens                → output_tokens
Codex reasoning_output_tokens      → reasoning_tokens
Codex total_tokens                 → total_tokens
```

Adapter 必须让 `NormalizedTokenUsage::new` 完成最终不变量校验。

### 4.3 GPT-5.6+ 的 cache-write 规则

当前版本不再传模型能力矩阵。

```text
raw cache_write_input_tokens 存在且合法
    → Some(value)

raw cache_write_input_tokens 缺失
    → None
```

`Some(0)` 与 `None` 必须严格区分。

删除：

```text
MODEL_CAPABILITY_VERSION
CACHE_WRITE_UNSUPPORTED_MODELS
with_test_unsupported_models
exact_model 参与 cache-write normalization
SnapshotKind::Single 对 unsupported model 的特殊分支
```

---

## 5. 第三步：把 `src/codex/usage.rs` 变成纯 Codex rollout parser

当前 `UsageRawAdapter` 同时做 JSON parse 和 canonical normalize。本轮拆开。

### 5.1 建议改名

```text
UsageRawAdapter → CodexRolloutParser
```

如果 Luna 为降低一次性改名风险选择暂时保留 `UsageRawAdapter` 类型名，**仅允许保留类型名，不允许保留旧 Token 字段/三态逻辑**。优先仍是改名。

### 5.2 parser 的职责

parser 只做：

1. 完整 JSONL 行解析；
2. record type 分类；
3. 时间戳提取；
4. raw Token 字段类型读取；
5. 构造 `CodexRawTokenUsage`；
6. 调用 `CodexRolloutAdapter::normalize(raw)`；
7. 把成功结果放入 `TokenCountInfo`。

parser 不再做：

```text
模型能力判断
UnsupportedZero 推断
CacheWriteStatus 生成
canonical 字段命名决定
```

### 5.3 `parse_line` 参数

当前：

```rust
parse_line(&line, exact_model)
```

其中 `exact_model` 只服务旧 cache-write capability 推断。

删除该参数，改成：

```rust
parse_line(&line)
```

pipeline 中的 `active_model` 仍保留，因为 usage event 仍需要归属 model；只是不能再把它传给 Token parser 做 cache-write 判定。

### 5.4 `total_token_usage` / `last_token_usage`

保持现有业务语义：

```text
total_token_usage → current_total_usage / cumulative total
last_token_usage  → optional last usage
```

raw 读取后都必须经过同一个 Codex Adapter。

---

## 6. 第四步：Processor / Pipeline / Ledger 全面切换 canonical

### 6.1 类型替换

以下所有 `TokenVector` 改为 `NormalizedTokenUsage`：

```text
UsageValue::Valid(...)
UsageEvent.usage
UsageEvent.previous_total
UsageEvent.current_total
TurnState.start_total
TurnState.last_total
TurnState.accounted
UsageSourceState.previous_total
Storage UsageSnapshot.vector
UsageEventWrite.usage
UsageTurnWrite.accounted
```

只要语义是“一个 Token snapshot/vector”，统一使用 canonical 类型。

### 6.2 Processor 删除 capability conflict 分支

从 `src/usage/processor.rs` 删除：

```text
ProcessorError::CacheWriteCapabilityConflict
AnomalyCode::CacheWriteCapabilityConflict
所有 match 分支
所有 unsupported/known capability conflict 测试
```

保留仍有效的：

```text
CacheWriteChainDecrease
TurnCacheWriteDeltaNegative
```

但触发规则改为：

```text
只有 current.cache_write_tokens 和 previous.cache_write_tokens 都是 Some 时，
才检查是否 decrease。
任一为 None 时，该维度不可判定，不得伪造 decrease/capability conflict。
```

### 6.3 accounted 聚合

`add_accounted` 必须使用 `NormalizedTokenUsage::checked_add`。

一旦任意 event 的 `cache_write_tokens == None`：

```text
turn.accounted.cache_write_tokens == None
```

直到该 Turn accounted 被重新从完整数据构建。

不得把 unknown 归零。

### 6.4 compensation / recovered delta

当 cumulative required 字段可相减，但 cache-write 任一侧未知：

```text
recovered usage 的 required token delta 正常产生
cache_write_tokens = None
```

是否允许 event 继续作为 partial usage，沿用当前业务规则；但不得为了 cache-write 未知把整个 required token delta 丢弃，除非其他现有 required 不变量本身失败。

### 6.5 serialization / stable digest

`processor.rs` 中任何手工序列化 Token vector 的 digest/fingerprint helper，都同步改为 canonical 六字段 + Option tag。

不得留下 status byte、reported total。

---

## 7. 第五步：SQLite schema 升级到 v3

### 7.1 migration 原则

新增：

```text
src/storage/schema/0003_normalized_token_usage.sql
```

更新：

```rust
LATEST_SCHEMA_VERSION = 3
```

`MIGRATIONS` 注册 v3。

**不改写 `0002_usage_ledger.sql`。**  
`0002` 是已发布 migration 历史；保留它不是兼容兜底。

v3 migration 必须是一次性替换：

```text
v2 旧表
→ 创建 v3 临时表
→ 校验并复制数据
→ 删除旧表
→ v3 表改为正式名
→ 重建 index / FK
→ user_version = 3
```

迁移结束后 SQLite 正式 schema 不得存在旧 Token 列。

### 7.2 `usage_events` v3

正式 Token 列必须变成：

```sql
input_tokens INTEGER NOT NULL
cached_tokens INTEGER NOT NULL
cache_write_tokens INTEGER
output_tokens INTEGER NOT NULL
reasoning_tokens INTEGER NOT NULL
total_tokens INTEGER NOT NULL
```

保留业务列：

```text
ledger_epoch
event_id
event_kind
occurred_at_ms
thread_id
root_session_id
turn_key
model
quality_status
source provenance
created_at_ms
```

删除：

```text
cached_input_tokens
cache_write_input_tokens
cache_write_status
reasoning_output_tokens
```

v2→v3 数据搬迁：

```text
cached_input_tokens      → cached_tokens
cache_write_input_tokens → cache_write_tokens
reasoning_output_tokens  → reasoning_tokens
total_tokens             → total_tokens
```

v3 CHECK：

```text
cached_tokens <= input_tokens
reasoning_tokens <= output_tokens
total_tokens = input_tokens + output_tokens
cache_write_tokens IS NULL
    OR cached_tokens + cache_write_tokens <= input_tokens
```

如果继续保留 `quality_status`：

```text
complete / partial
```

不得再通过 `cache_write_status` 判断；写事件时：

```text
cache_write_tokens.is_some() → complete
cache_write_tokens.is_none() → partial
```

它只是事件质量投影，不是 cache-write 第三套状态。

### 7.3 `usage_event_occurrences`

本表无 Token 字段，但因为 FK 指向 `usage_events`，v3 重建 parent 时必须同步安全重建/重绑定 FK。

迁移测试必须证明：

```text
occurrence 数量不变
(ledger_epoch,event_id) 仍全部可解析到 usage_events
source FK 不丢
```

### 7.4 `turns` v3

三组 snapshot 均只保留 canonical 后缀。

`start_total_*`：

```text
start_total_input_tokens
start_total_cached_tokens
start_total_cache_write_tokens
start_total_output_tokens
start_total_reasoning_tokens
start_total_total_tokens
start_total_fingerprint
```

`last_total_*` 同理。

`accounted_*`：

```text
accounted_input_tokens
accounted_cached_tokens
accounted_cache_write_tokens
accounted_output_tokens
accounted_reasoning_tokens
accounted_total_tokens
accounted_fingerprint
```

删除所有：

```text
*_cached_input_tokens
*_cache_write_input_tokens
*_reasoning_output_tokens
*_reported_total_tokens
*_derived_total_tokens
*_cache_write_status
```

迁移时：

```text
*_cached_input_tokens      → *_cached_tokens
*_cache_write_input_tokens → *_cache_write_tokens
*_reasoning_output_tokens  → *_reasoning_tokens
*_derived_total_tokens     → *_total_tokens
```

`reported_total_tokens` 和 `derived_total_tokens` 在 v2 schema 本来必须相等；迁移前/复制时依赖 v2 CHECK，不再复制两个值。

### 7.5 `usage_source_states` v3

`previous_total_*` 改为：

```text
previous_total_input_tokens
previous_total_cached_tokens
previous_total_cache_write_tokens
previous_total_output_tokens
previous_total_reasoning_tokens
previous_total_total_tokens
previous_total_fingerprint
previous_total_offset
```

删除：

```text
previous_total_cached_input_tokens
previous_total_cache_write_input_tokens
previous_total_reasoning_output_tokens
previous_total_reported_total_tokens
previous_total_derived_total_tokens
previous_total_cache_write_status
```

### 7.6 `ingest_anomalies`

生产代码不再定义或写入：

```text
CACHE_WRITE_CAPABILITY_CONFLICT
```

v3 migration 建议删除历史该 anomaly：

```sql
DELETE FROM ingest_anomalies
WHERE anomaly_type = 'CACHE_WRITE_CAPABILITY_CONFLICT';
```

原因：该 anomaly 来自本轮明确废弃的模型 capability 推断语义，继续保留会把旧错误语义带入新版本。

其他 anomaly 不动。

### 7.7 parser/canonical 版本与旧 active epoch

v3 migration **不要把旧 v2 usage 数据伪装成 parser v3 数据**。

即：

- 不要直接把 `app_meta.usage_parser_version` 从 2 改成 3；
- 不要把旧 `usage_source_states.canonical_algorithm_version=2` 改成 3；
- 旧 active ledger 的数值可迁移并继续只读展示；
- 新二进制以 `USAGE_PARSER_VERSION=3` 发现 parser/canonical mismatch 后走现有 rebuild 机制；
- 新 build 完成后才激活 parser v3/canonical v3。

这是版本可信度要求，不是运行时旧字段 fallback。

### 7.8 migration 原子性

v2→v3 migration 与 `PRAGMA user_version=3` 必须继续处于 migration runner 的同一个 `BEGIN IMMEDIATE` transaction。

任何建表、复制、DROP、RENAME、index/FK 失败：

```text
user_version 仍为 2
旧 v2 schema 和数据完整
不得留下半迁移 v3 表
```

---

## 8. 第六步：重写 `src/storage/usage.rs`

### 8.1 import

删除：

```rust
CacheWriteStatus
TokenVector
```

使用：

```rust
NormalizedTokenUsage
```

### 8.2 `UsageSnapshot`

改成：

```rust
pub(crate) struct UsageSnapshot {
    pub vector: NormalizedTokenUsage,
    pub fingerprint: Vec<u8>,
}
```

### 8.3 SQL column list

全面同步 v3 列名。

重点搜索并逐一修改：

```text
usage_events INSERT / compare / duplicate check
turns SELECT / INSERT / UPSERT / CAS compare
usage_source_states SELECT / INSERT / UPSERT
carry / rebuild 复制语句
snapshot_columns
event row decoding
turn row decoding
source-state row decoding
```

### 8.4 `snapshot_columns`

当前返回：

```text
input
cached_input
cache_write_input
output
reasoning_output
reported_total
derived_total
cache_write_status
fingerprint
```

改为只返回：

```text
input
cached
cache_write
output
reasoning
total
fingerprint
```

不要构造任何 status。

### 8.5 event quality

当前 `write_event` 根据 `CacheWriteStatus::UnknownMissing` 设置 partial。

改为：

```rust
let quality = if event.usage.cache_write_tokens.is_some() {
    "complete"
} else {
    "partial"
};
```

### 8.6 旧 fingerprint

读取 v2→v3 迁移来的 old-active row 时 fingerprint 作为旧 algorithm 的 opaque proof 存在；新 parser v3 不能把它当 v3 proof 使用。现有 parser/canonical version gate 必须阻止这种误用。

---

## 9. 第七步：Aggregate 改造

### 9.1 `TokenTotals`

改成 canonical 名称：

```rust
pub struct TokenTotals {
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,

    pub uncached_input_tokens: Option<i64>,
    pub other_output_tokens: i64,
    pub cache_hit_rate: Option<f64>,
}
```

`estimated_cost` 继续放在现有 Summary/API 层，不必塞进 `TokenTotals`。

删除：

```text
cache_write_status
cache_tokens
cached_input_tokens
cache_write_input_tokens
reasoning_output_tokens
```

### 9.2 `zero`

```text
input=0
cached=0
cache_write=Some(0)
output=0
reasoning=0
total=0
uncached=Some(0)
other_output=0
cache_hit_rate=None
```

### 9.3 聚合 cache-write

SQL/内存聚合统一：

```text
如果任何参与聚合的 row.cache_write_tokens IS NULL
    aggregate.cache_write_tokens = None
否则
    aggregate.cache_write_tokens = SUM(cache_write_tokens)
```

不得 `COALESCE(NULL,0)` 后继续求和。

### 9.4 派生字段重算

每次累计结束调用统一 `recompute_derived()`：

```text
total_tokens = input + output
other_output_tokens = output - reasoning

if cache_write Some:
    uncached = input - cached - cache_write
else:
    uncached = None

if input == 0:
    cache_hit_rate = None
else:
    cache_hit_rate = cached / input
```

所有整数运算 checked。

### 9.5 Session scope

建议把 `SessionUsageRow.inclusive` 改名为：

```text
inclusive_usage
```

最终结构：

```rust
pub struct SessionUsageRow {
    ...
    pub inclusive_usage: TokenTotals,
    pub self_usage: TokenTotals,
    pub subagent_usage: TokenTotals,
    ...
}
```

逐基础字段必须满足：

```text
inclusive = self + subagent
```

对于 `cache_write_tokens`：

```text
self Some + subagent Some → inclusive Some(sum)
任一 None                 → inclusive None
```

派生字段分别从各自 scope 的 aggregate base tokens 重算，禁止派生字段相加/平均。

---

## 10. 第八步：Query API 改造

### 10.1 `TokenUsageDto`

改成：

```rust
pub struct TokenUsageDto {
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: Option<i64>,
    pub uncached_input_tokens: Option<i64>,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub other_output_tokens: i64,
    pub total_tokens: i64,
    pub cache_hit_rate: Option<f64>,
    pub estimated_cost: Option<f64>,
}
```

删除 API 字段：

```text
cached_input_tokens
cache_write_input_tokens
cache_write_status
reasoning_output_tokens
cache_tokens
```

### 10.2 `SummaryUsageDto`

与 `TokenUsageDto` 使用相同 Token 字段，再增加：

```text
session_count
```

不要再手写两份语义不同的字段集合；可以保留两个 struct，但映射来源必须同一 helper。

### 10.3 `map_totals`

安全整数检查至少覆盖：

```text
input_tokens
cached_tokens
cache_write_tokens Some(value)
uncached_input_tokens Some(value)
output_tokens
reasoning_tokens
other_output_tokens
total_tokens
```

`cache_hit_rate` 保持 `[0,1]` finite 校验。

删除 status string 映射。

### 10.4 Session API

`inclusive_usage`、`self_usage`、`subagent_usage` 三套 DTO 都返回完全相同的 canonical Token 字段。

不得为 Session 单独保留旧字段名。

---

## 11. 第九步：Frontend 改造

### 11.1 `frontend/src/data/types.ts`

`UsageDto` 改为：

```ts
export type UsageDto = {
  input_tokens: number;
  cached_tokens: number;
  cache_write_tokens: number | null;
  uncached_input_tokens: number | null;
  output_tokens: number;
  reasoning_tokens: number;
  other_output_tokens: number;
  total_tokens: number;
  cache_hit_rate: number | null;
  estimated_cost: number | null;
};
```

删除：

```text
cached_input_tokens
cache_write_input_tokens
cache_write_status
reasoning_output_tokens
cache_tokens
```

未来 Session DTO 只能复用 `UsageDto`，不要重新定义 Token 字段。

### 11.2 `usagiClient.ts`

`parseUsage` 直接要求 canonical API 字段。

不得：

```text
检测旧字段
兼容 old response
cache_write_tokens null → 0
```

对：

```text
cache_write_tokens
uncached_input_tokens
cache_hit_rate
estimated_cost
```

分别使用 nullable validator。

### 11.3 `MetricGrid.tsx`

卡片字段改为：

```text
estimated_cost
total_tokens
input_tokens
output_tokens
session_count
cache_hit_rate
cache_write_tokens
cached_tokens
```

中文固定：

```text
预估费用
总 Token
输入 Token
输出 Token
会话数量
缓存命中率
缓存写入 Token
缓存读取 Token
```

修正当前错误：

```text
input_tokens: “写入 Token” → “输入 Token”
```

删除 `cache_write_status` 判断：

```ts
case "cache_write_tokens":
    return formatInteger(usage.cache_write_tokens);
```

`null` 由现有 formatter 显示未知占位，不允许 frontend 猜 0。

### 11.4 前端测试 fixture

全部 fixture 改 canonical 名称。

禁止为了让旧测试继续过而在 fixture 同时放新旧字段。

---

## 12. 第十步：文档与测试代码清理

### 12.1 必须同步的工程文档

至少检查并更新：

```text
Spec_04_Token账本与聚合
Spec_05_查询API与更新通知
Spec_06_01_前端框架与Dashboard
Spec_06_02_Session记录列表
Usagi_测试标准_Spec01-06_v0.17.md
各 Spec04/05/06 测试代码布局
```

规则：

- 所有 MU canonical 层旧字段改为新字段；
- Codex raw 示例继续保留 Codex 原始字段；
- 删除 `cache_tokens` 定义；
- 删除 `cache_write_status` 三态；
- `cache_hit_rate` 保留；
- `input_tokens` 中文统一“输入 Token”；
- `cached_tokens` 中文统一“缓存读取 Token”；
- `cache_write_tokens` 中文统一“缓存写入 Token”。

### 12.2 代码测试 fixture

JSONL raw fixture 仍必须使用：

```text
cached_input_tokens
cache_write_input_tokens
reasoning_output_tokens
```

因为它模拟 Codex rollout。

API / aggregate / database-v3 / frontend fixture 必须使用：

```text
cached_tokens
cache_write_tokens
reasoning_tokens
```

不要机械全局替换 raw JSON。

---

## 13. 推荐实际执行顺序

Luna 按以下顺序做，不要跳步。

### S1：先建立 canonical 类型

1. 新增 `usage/normalized.rs`；
2. 实现校验、add/sub、derived helper、fingerprint；
3. 单测通过；
4. parser/canonical version 升 3。

**此时不要先改前端。**

### S2：建立 Adapter

1. 新增 adapters 目录；
2. 新增 `CodexRawTokenUsage`；
3. 实现 Codex → Normalized 映射；
4. 完成 adapter 单测；
5. 删除 unsupported model capability 分支。

### S3：切 parser

1. `codex/usage.rs` raw parse 与 normalization 分离；
2. `parse_line` 删除 exact_model 参数；
3. 更新 pipeline 调用；
4. raw parser tests 通过。

### S4：切 Processor / Ledger 内存模型

1. 全部 `TokenVector` → `NormalizedTokenUsage`；
2. 删除 `CacheWriteStatus`；
3. 删除 capability conflict；
4. 更新 fingerprint/digest；
5. processor/pipeline/ledger tests 通过。

### S5：做 SQLite v3 migration

1. 新增 `0003_normalized_token_usage.sql`；
2. schema version → 3；
3. 重建三类 Token 表；
4. 保持 occurrences FK；
5. 删除旧 capability anomaly；
6. migration test 完整通过；
7. 确认 v3 `pragma_table_info` 不存在旧列。

### S6：切 storage runtime SQL

1. `storage/usage.rs` 全部换 v3 列；
2. snapshot tuple 简化；
3. load/write/carry/rebuild 全部编译；
4. storage + Spec04 integration 通过。

### S7：切 aggregate

1. `TokenTotals` canonical；
2. SQL 聚合 null 传播；
3. derived fields；
4. Session 三 scope；
5. aggregate tests 通过。

### S8：切 API

1. DTO 全改 canonical；
2. 删除旧 API 字段；
3. 增加 derived 字段；
4. API integration/stress 通过。

### S9：切 Frontend

1. types；
2. runtime parser；
3. MetricGrid；
4. fixtures；
5. Vitest + build + browser gate。

### S10：删除残留并做最终 Gate

完成全文搜索和全测试；发现旧字段不得用 alias/fallback 解决，必须追到源头删掉。

---

## 14. 最终静态清理 Gate

### 14.1 生产运行时代码中必须为 0 命中

排除历史 migration `0002`、v3 migration 的输入旧列、Codex raw parser/adapter 后，以下旧语义必须为 0：

```text
TokenVector
CacheWriteStatus
UnsupportedZero
UnknownMissing
cache_write_status
cache_tokens
reported_total_tokens
derived_total_tokens
CacheWriteCapabilityConflict
CACHE_WRITE_CAPABILITY_CONFLICT
```

### 14.2 Adapter 后必须为 0 命中

以下 raw 字段不得出现在 canonical runtime 层：

```text
cached_input_tokens
cache_write_input_tokens
reasoning_output_tokens
```

重点检查：

```text
src/usage/aggregate.rs
src/usage/processor.rs
src/usage/pipeline.rs
src/usage/ledger.rs
src/usage/rebuild.rs
src/storage/usage.rs
src/api/
frontend/src/
```

### 14.3 必须存在

```text
src/usage/normalized.rs
src/usage/adapters/mod.rs
src/usage/adapters/openai/mod.rs
src/usage/adapters/openai/codex.rs
src/storage/schema/0003_normalized_token_usage.sql
```

---

## 15. 验收标准

本 Spec 只有“全部通过”才算完成。

### A. 架构验收

- [ ] `NormalizedTokenUsage` 是 Adapter 后唯一 Token snapshot 类型。
- [ ] Codex raw 字段只存在 Adapter 边界之前。
- [ ] 当前只实现 `adapters/openai/codex.rs`。
- [ ] 没有 Responses / Chat Completions / Anthropic / Gemini 空壳实现。
- [ ] `TokenVector` 已删除。
- [ ] `CacheWriteStatus` 已删除。

### B. 数据语义验收

- [ ] `total_tokens == input_tokens + output_tokens`。
- [ ] `cached_tokens <= input_tokens`。
- [ ] `reasoning_tokens <= output_tokens`。
- [ ] cache-write `Some(0)` 与 `None` 严格区分。
- [ ] cache-write 已知时 `uncached = input - cached - write`。
- [ ] cache-write 未知时 `uncached = null`。
- [ ] `cache_hit_rate = cached / input`，input=0 时 null。
- [ ] 聚合命中率不是逐项平均。
- [ ] `cache_tokens` 完全不存在于新数据模型。

### C. Processor / Ledger 验收

- [ ] add/sub 对 `Option<cache_write_tokens>` 的传播符合本文。
- [ ] unknown cache-write 不再产生 capability conflict。
- [ ] known cache-write 下降仍可检测。
- [ ] Turn accounted、recovered、compensation 统一 canonical。
- [ ] fingerprint 使用 canonical v3。
- [ ] parser/canonical version 均升 3。

### D. SQLite 验收

- [ ] `LATEST_SCHEMA_VERSION == 3`。
- [ ] fresh DB 最终 user_version=3。
- [ ] v2 DB 可原子升级 v3。
- [ ] `usage_events` 只存在 canonical Token 列。
- [ ] `turns` 的 start/last/accounted 后缀全部 canonical。
- [ ] `usage_source_states.previous_total_*` 后缀全部 canonical。
- [ ] v3 正式表不存在 `*_cache_write_status`、`*_reported_total_tokens`、`*_derived_total_tokens`。
- [ ] v2 数值迁移无损。
- [ ] occurrence FK 和数量保持。
- [ ] migration failure 回滚到完整 v2。
- [ ] 旧 v2 active parser 不被伪装成 v3，后续会进入 rebuild。

### E. Aggregate / Session 验收

- [ ] Summary、Model、Session 全部输出 canonical TokenTotals。
- [ ] 任一聚合成员 cache-write unknown → aggregate cache-write null。
- [ ] Session `inclusive_usage = self_usage + subagent_usage` 对基础字段成立。
- [ ] Session cache-write 的 null 传播正确。
- [ ] 三个 Session scope 的 derived 值各自重算。

### F. API 验收

- [ ] API 不返回任何旧字段。
- [ ] API 返回 `cached_tokens`。
- [ ] API 返回 `cache_write_tokens` nullable。
- [ ] API 返回 `reasoning_tokens`。
- [ ] API 返回 `uncached_input_tokens` nullable。
- [ ] API 返回 `other_output_tokens`。
- [ ] API 保留 `cache_hit_rate`。
- [ ] JSON safe integer / ratio 校验保持。

### G. Frontend 验收

- [ ] `UsageDto` 无旧字段。
- [ ] client 不兼容旧 response。
- [ ] cache-write null 不转 0。
- [ ] Dashboard 显示“输入 Token”。
- [ ] Dashboard 显示“缓存读取 Token”。
- [ ] Dashboard 显示“缓存写入 Token”。
- [ ] Dashboard 8 卡顺序保持既定设计。
- [ ] frontend unit/typecheck/build/browser gate 通过。

### H. 回归验收

- [ ] Rust 全量测试通过。
- [ ] Spec01～05 已有核心存储/扫描/账本/API 测试无功能退化。
- [ ] Spec06-01 Dashboard 测试通过。
- [ ] 数据口径改造专项测试全部通过。
- [ ] 静态旧字段清理 Gate 通过。
- [ ] 未通过任何“旧字段 fallback”让测试变绿。

---

## 16. 完成后 Luna 必须提交的验收报告

只报告以下内容，不写长篇总结：

```text
1. 新增文件清单
2. 修改文件清单
3. 删除的旧类型/字段/分支清单
4. SQLite v3 migration 结果
5. parser/canonical version
6. 专项测试逐项 PASS/FAIL
7. cargo test 结果
8. frontend test/check/build/browser 结果
9. 旧字段 rg Gate 结果
10. 尚未完成项（必须为 0 才能申请验收）
```
