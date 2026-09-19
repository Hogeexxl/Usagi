# Usagi 预估费用模块实施方案 v0.5

> 代码基线：用户于 2026-08-12 提供的最新 Usagi 源码快照 `8701431b-11c4-4127-a130-ad3f334a9422.zip`  
> 定价基线：OpenAI 官方公开 Standard API 价格，核验日期 2026-08-12  
> Reasoning Effort 数据源基线：Codex rollout `turn_context` 的 request/turn context effort，核验日期 2026-08-12  
> 本版合并三项工作：**预估费用模块** + **Subagent `agent_path` 标题适配修复** + **Drawer 模型 Reasoning Effort 展示与 Main `(model, effort)` 分组**。三项工作同版交付；Metadata、Usage Context、Cost 三个业务职责保持分离，仅在 usage event、数据库迁移、聚合/API、最终集成与发布顺序上按本文规定汇合。  
> **本文不定义测试条目。测试范围、用例、断言与门禁由后续独立测试标准文档统一规定。** 历史测试中与本版生产行为冲突的旧占位断言，不得反向约束本方案的生产实现。

---

## 1. 本版实施范围

本版必须完成三个闭环。

### Workstream A — Subagent 标题修复

目标链路：

```text
Codex state_5.threads.agent_path
              │
              ├──────────────┐
              │              │
              ▼              ▼
        StateThreadFact   rollout session_meta
              │              │
              │       payload.agent_path
              │              +
              │       thread_spawn.agent_path
              │              │
              │              ▼
              │       RolloutThreadFact
              │              │
              │       durable safe fact
              │              │
              └───────┬──────┘
                      ▼
             ThreadMetadataResolver
                      │
                仅 Subagent fallback
                      │
                      ▼
                threads.title
                      │
                      ▼
            Detail API / Drawer
```

必须解决：

```text
/root/gate_b_rereview → Gate b rereview
/root/package_snapshot → Package snapshot
/root/launch_mu → Launch mu
```

不得使用 `agent_nickname` 作为任务标题。

### Workstream B — 预估费用模块

目标链路：

```text
CodexRawTokenUsage
        │ Adapter
        ▼
NormalizedTokenUsage
        │
        │ + model
        │ + occurred_at_ms
        │ + usage event granularity
        ▼
PricingRepository
        │
        ▼
ModelPricing
        │
        ▼
CostEstimator
        │
        ▼
usage_events.estimated_cost_nanos_usd
        │
        ▼
SQL aggregate
        │
        ▼
API estimated_cost
        │
        ▼
Dashboard / Session List / Drawer
```

必须支持：

- uncached input；
- cached input；
- cache write；
- output；
- Short / Long Context；
- request/event 级计算后再向上聚合；
- 旧数据 reprice/backfill；
- 当前已经预留费用字段的所有生产展示位置接通。


### Workstream C — Reasoning Effort usage 维度与 Drawer 展示

目标链路：

```text
Codex rollout turn_context
        │
        ├─ model
        └─ effort
             │
             ▼
      Codex usage parser
             │
             ▼
       UsageProcessor
   active_model + active_reasoning_effort
             │
             ▼
        usage_events
   model + reasoning_effort + tokens + cost
             │
       ┌─────┴─────────────┐
       │                   │
       ▼                   ▼
Main Drawer             Subagent Drawer
GROUP BY                保持单 block
(model, effort)         显示 effort 摘要
       │                   │
       ▼                   ▼
gpt-5.6-sol (high)      gpt-5.6-luna (high)
gpt-5.6-sol (medium)
gpt-5.6-terra (max)
```

必须满足：

- Reasoning Effort 的历史主数据源使用 rollout `turn_context`，不使用 `state_5.threads.reasoning_effort` 反推历史；
- Main Drawer 的分组键从 `model` 升级为 `(model, reasoning_effort)`；
- 同一模型但不同 effort 必须拆成不同 Main block；
- Subagent 继续遵守“一个 Subagent 一个 usage block”的既有范围，不在本版按模型或 effort 继续拆 block；
- effort 缺失时保持 Unknown，不根据模型默认值猜测；
- Reasoning Effort 只作为 usage 统计/展示维度，不进入价格表，也不改变 Token 单价。

---

## 2. 当前代码基线：实施时不得偏离

### 2.1 当前 Schema 与 parser 版本

最新代码当前为：

```text
LATEST_SCHEMA_VERSION = 5
METADATA_PARSER_VERSION = 2
USAGE_PARSER_VERSION = 3
USAGE_CANONICAL_ALGORITHM_VERSION = 3
```

本版预留迁移编号：

```text
0006_subagent_agent_path.sql
0007_usage_context_and_estimated_cost.sql

最终：LATEST_SCHEMA_VERSION = 7
```

版本变化固定为：

```text
METADATA_PARSER_VERSION
2 → 3

USAGE_PARSER_VERSION
3 → 4

USAGE_CANONICAL_ALGORITHM_VERSION
3 → 4
```

原因必须区分：

```text
agent_path
→ metadata parser 能力变化

reasoning_effort
→ usage parser 新增 turn_context 字段
→ usage event canonical payload / event identity 增加维度
→ 需要历史 usage rebuild

estimated cost
→ canonical usage 之上的 derived metric
→ 本身不要求 usage parser/canonical bump
```

除非实施时使用的代码基线已经不是本文指定的最新快照，否则不得自行改 migration 编号、拆出第三个 migration，或把费用与 reasoning-effort 分成两个 schema 版本。

### 2.2 Token canonical 已定型

`src/usage/normalized.rs`：

```rust
pub struct NormalizedTokenUsage {
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
}
```

费用模块只允许使用以上 canonical 字段。

Raw → canonical：

```text
CodexRawTokenUsage                  NormalizedTokenUsage
---------------------------------------------------------
input_tokens                    -> input_tokens
cached_input_tokens             -> cached_tokens
cache_write_input_tokens        -> cache_write_tokens
output_tokens                   -> output_tokens
reasoning_output_tokens         -> reasoning_tokens
```

不得在 CostEstimator 中重新引入：

```text
cached_input_tokens
cache_write_input_tokens
reasoning_output_tokens
```

### 2.3 当前 usage event 语义

`src/usage/processor.rs` 当前有：

```text
Normal
Recovered
TurnCompensation
```

当前含义固定为：

```text
Normal
  = 直接使用 token_count.last_token_usage

Recovered
  = 当前 token_count 缺少 last_usage 时，
    使用 current_total - previous_total 恢复该 token_count 边界的 usage

TurnCompensation
  = Turn 结束时根据累计快照补齐仍未记账的缺口，
    可能包含不止一个缺失 request
```

费用模块不得修改 processor 的 Token 恢复算法。

### 2.4 当前费用链路断点

当前生产代码已存在费用 DTO/UI，占位主要断在后端：

```text
src/usage/aggregate.rs
  UsageSummary.estimated_cost = None

src/api/query.rs
  map_totals() estimated_cost = None

usage_events
  尚无 estimated_cost 列
```

前端已有：

```text
Dashboard KPI            预估费用
Session List             合计费用
Drawer Main model        预估费用
Drawer Subagent          预估费用
formatCost()              number/null → $x.xx / —
```

本版不得重做费用 UI；应让真实后端数据进入现有 UI。

### 2.5 当前 Subagent 标题链路断点

当前 `StateThreadFact` 没有：

```text
agent_path
```

`THREAD_ALLOWLIST` 也没有：

```text
agent_path
```

当前 rollout `SessionMetaAllowed` 未解析：

```text
payload.agent_path
payload.source.subagent.thread_spawn.agent_path
```

当前标题 resolver 为：

```text
state.name
→ state.title
→ session_index.thread_name
→ None
```

因此 `name/title/session_index` 均为空时，Subagent 最终只能得到 `threads.title=NULL`。

---


### 2.6 当前 Reasoning Effort 链路断点

当前 `src/codex/usage.rs`：

```rust
pub struct TurnContextRecord {
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub occurred_at_ms: Option<i64>,
}
```

当前只读取 `turn_context.payload.model`，没有把 Codex rollout 中的：

```text
turn_context.payload.effort
```

带入 usage pipeline。

当前 `UsageSourceState` 只有：

```text
active_model
```

没有：

```text
active_reasoning_effort
```

当前 `SourceStateProof` / storage source state 只有：

```text
active_model
active_model_offset
```

没有：

```text
active_reasoning_effort
active_reasoning_effort_offset
```

当前 `TurnState` / `turns` 只持久化模型归属状态：

```text
model_state
single_model
unresolved_model_seen
```

没有对应的 reasoning-effort Turn 状态。因此如果只给 event/source state 增加 effort，而不扩展 open Turn 的 durable state，跨 chunk / restart 后的 `TurnCompensation` 会丢失此前已经观察到的 effort 组合，可能把补偿量错误归给最后一次 effort。

当前 `UsageEvent` / `UsageEventWrite` / `usage_events` 只有：

```text
model
```

没有：

```text
reasoning_effort
```

另外，`src/usage/rebuild.rs::active_state_fingerprint()` 当前只覆盖 existing source-state 字段；reasoning effort 成为 canonical context 后，active effort 与其 offset 也必须进入 state proof fingerprint。

因此当前 `mu.sqlite3` 无法可靠回答：

```text
gpt-5.6-sol (high)   用了多少 Token / 费用
gpt-5.6-sol (medium) 用了多少 Token / 费用
```

本版必须从 rollout 重新建立该维度，并同时完成：

```text
turn_context parser
→ processor active context
→ open Turn effort state
→ source-state durable persistence
→ Turn durable persistence
→ state proof fingerprint
→ usage event canonical identity
→ aggregate/API/Drawer
```

不得从 Thread 级 `state_5.threads.reasoning_effort` 回填历史 usage。

### 2.7 本次实施可执行性复核后的文件边界修正

对当前代码基线重新做文件级依赖复核后，必须按以下事实施工：

```text
rollout_metadata_facts:
读取/部分写入位于 src/storage/source.rs
完整 metadata commit 写入/读取/offset 校验还位于 src/storage/metadata.rs

RolloutMetadataFact struct literal:
除 rollout/domain/storage 外，
src/scanner/pipeline.rs 也有生产/测试 helper 构造依赖

Reasoning Effort durable state:
src/usage/ledger.rs 负责 pipeline ↔ storage DTO 映射
src/storage/usage.rs 负责 source state / turns / event SQL
src/usage/rebuild.rs 负责 active state fingerprint / rebuild proof

Frontend Session Detail:
frontend/src/data/usagiClient.ts 有运行时 DTO 校验，
不能只改 types.ts 与 Drawer JSX
```

因此 Part E 的文件 ownership 以本文 v0.5 为唯一施工依据；此前遗漏的文件清单不得继续作为限制。

# Part A — Subagent `agent_path` 标题适配

## 3. `agent_path` 数据模型

### 3.1 State 输入

修改：

```text
src/codex/state_index.rs
```

`StateThreadFact` 增加：

```rust
pub agent_path: Option<String>,
```

`THREAD_ALLOWLIST` 增加：

```text
agent_path
```

读取规则与当前动态列机制保持一致：

```text
PRAGMA table_info(threads)
→ 只选择实际存在且在 allowlist 中的列
```

所以旧 Codex DB 不存在 `agent_path` 时必须自然得到：

```text
StateThreadFact.agent_path = None
```

不得把 `agent_path` 变成 state DB 的必需列。

本版不读取、不传播：

```text
agent_nickname
```

### 3.2 Rollout 输入

修改：

```text
src/codex/rollout.rs
```

`SessionMetaAllowed` 增加两个解析来源：

```text
payload.agent_path
payload.source.subagent.thread_spawn.agent_path
```

两者描述的是同一类 Subagent 任务路径事实。

新增 provenance：

```rust
pub enum AgentPathProvenance {
    SessionMeta,
    ThreadSpawn,
}
```

优先级固定：

```text
payload.agent_path                       priority 2
thread_spawn.agent_path                  priority 1
```

如果两者值相同：正常合并。

如果两者值不同：

```text
保留高优先级 payload.agent_path
+
标记 CandidateConflict / has_conflict
```

不得静默忽略冲突。

### 3.3 `RolloutThreadFact`

增加：

```rust
pub agent_path: Option<Candidate<AgentPathProvenance>>,
```

必须像 `cwd`、`parent_thread_id_hint`、`agent_role_hint` 一样进入 parser 的 candidate merge 体系；不要额外发明一套临时变量。

---

## 4. `agent_path` 规范化

增加专用 helper，建议放在 `src/codex/rollout.rs` 可被 state/metadata 共用的合适位置；如果跨模块共享更合理，可提取到 `src/codex` 内部公共 helper，但不得放到 frontend。

推荐接口：

```rust
fn normalize_agent_path(raw: &str) -> Option<String>
```

规则：

1. trim；
2. 空值返回 `None`；
3. 拒绝 control characters；
4. 必须是绝对路径；
5. 做 lexical normalization，不访问文件系统；
6. 必须处于 `/root` 命名空间；
7. `/root` 与 `/root/` 自身不生成任务名；
8. 末段为空时返回 `None`。

禁止：

```text
fs::canonicalize
fs::metadata
exists()
```

`agent_path` 是 Codex metadata，不是要求真实存在的本机路径。

有效示例：

```text
/root/package_snapshot
/root/gate_b_rereview
/root/group/gate_a_backend
```

最终标题只取**规范化路径最后一段**。

---

## 5. Rollout durable safe fact

本版必须让 rollout `agent_path` 进入现有 durable safe-fact 链路，不允许只存在于一次 parser 调用的临时内存中。

当前链路：

```text
RolloutThreadFact
→ to_safe_fact()
→ domain::RolloutMetadataFact
→ rollout_metadata_facts
→ query_metadata_fact()
→ RolloutMetadataFact
→ from_safe_fact()
→ RolloutThreadFact
```

`agent_path` 必须完整走通。

### 5.1 Domain

修改：

```text
src/domain.rs
```

增加 domain provenance：

```rust
pub enum AgentPathProvenance {
    SessionMeta,
    ThreadSpawn,
}
```

字符串值固定：

```text
session_meta
thread_spawn
```

`RolloutMetadataFact` 增加：

```rust
pub agent_path: Option<String>,
pub agent_path_provenance: Option<AgentPathProvenance>,
pub agent_path_record_offset: Option<i64>,
```

`validate()` 增加：

```text
agent_path_record_offset >= 0
```

以及三元组一致性：

```text
三者全部 NULL
或
三者全部非 NULL
```

### 5.2 Parser safe-fact conversion

修改：

```text
src/codex/rollout.rs
```

同步补齐：

```text
RolloutThreadFact::to_safe_fact()
RolloutThreadFact::from_safe_fact()
RolloutThreadFact::empty()
```

不得出现：

```text
首次 full parse 能看到 agent_path
但 resume/skip 后恢复出的 RolloutThreadFact 丢失 agent_path
```

### 5.3 Storage round-trip 与编译依赖

`RolloutMetadataFact` 的生产读写并不只在一个 storage 文件中。A1 必须同时修改：

```text
src/storage/source.rs
src/storage/metadata.rs
src/scanner/pipeline.rs
```

其中：

```text
src/storage/source.rs
→ source scan state / safe fact 读取与部分写入路径

src/storage/metadata.rs
→ metadata commit 的 read_fact()/write_fact()
→ rollout_metadata_facts 显式 SELECT / INSERT / UPDATE
→ validate_fact_offsets()

src/scanner/pipeline.rs
→ RolloutMetadataFact 相关 struct literal / helper 初始化
```

所有 `rollout_metadata_facts` 的显式列清单必须加入：

```text
agent_path
agent_path_provenance
agent_path_record_offset
```

`src/storage/metadata.rs::validate_fact_offsets()` 必须把：

```text
agent_path_record_offset
```

纳入与现有 provenance offset 相同的边界校验，至少保持：

```text
0 <= agent_path_record_offset <= resolved_through_offset
```

`src/scanner/pipeline.rs` 只允许为新增字段补齐 struct literal/helper 初始化，不得借此修改 scanner planning、checkpoint、rebuild 或 consumer 调度行为。

不得只改 struct 不改 SQL、只改写入不改读取，或只保证首次 full parse 正常而让 safe-fact resume 丢字段。

## 6. Schema v6：Subagent agent_path

新增：

```text
src/storage/schema/0006_subagent_agent_path.sql
```

本 migration 只负责 metadata safe-fact schema，不混入费用字段。

推荐沿用 `0004_metadata_parent_v2_cleanup.sql` 的严格表重建方式，重建：

```text
rollout_metadata_facts
```

新表在原字段基础上新增：

```sql
agent_path TEXT,
agent_path_provenance TEXT CHECK (
    agent_path_provenance IS NULL
    OR agent_path_provenance IN ('session_meta', 'thread_spawn')
),
agent_path_record_offset INTEGER CHECK (
    agent_path_record_offset IS NULL OR agent_path_record_offset >= 0
),
```

增加整体 CHECK：

```sql
CHECK (
    (agent_path IS NULL
        AND agent_path_provenance IS NULL
        AND agent_path_record_offset IS NULL)
    OR
    (agent_path IS NOT NULL
        AND agent_path_provenance IS NOT NULL
        AND agent_path_record_offset IS NOT NULL)
)
```

迁移旧事实时三列统一填 `NULL`。

必须完整保留当前表已有：

- 外键；
- existing CHECK；
- `rollout_metadata_facts_thread_idx`；
- 其他全部已有字段。

不要顺手清理其他 schema。

---

## 7. Metadata parser version

本次 rollout metadata 解析结果发生变化，因此：

```text
METADATA_PARSER_VERSION = 2 → 3
```

修改 authority：

```text
src/codex/rollout.rs
```

目的：让已有 metadata checkpoint/fact 在新版本下进入既有 parser-version rebuild 机制，从头重新解析历史 rollout 的 `session_meta`，补齐历史 Subagent 的 `agent_path`。

只升级：

```text
METADATA_PARSER_VERSION
```

费用功能和 `agent_path` 修复都不得因此修改：

```text
USAGE_PARSER_VERSION
USAGE_CANONICAL_ALGORITHM_VERSION
```

---

## 8. `agent_path → title` 格式化

在 metadata/codex 层实现纯函数，例如：

```rust
fn subagent_title_from_agent_path(agent_path: &str) -> Option<String>
```

转换规则固定：

```text
1. 使用已经 normalize 的 agent_path
2. 取最后一个 path component
3. '_' 替换为 ' '
4. 合并连续空格
5. 首个字符大写
6. 其余字符保持原样
```

示例：

```text
/root/package_snapshot
→ package_snapshot
→ package snapshot
→ Package snapshot

/root/gate_b_rereview
→ Gate b rereview

/root/final_acceptance
→ Final acceptance
```

不要实现额外英文 Title Case，不要擅自把：

```text
Gate b rereview
```

改成：

```text
Gate B Rereview
```

本版目标是复现当前 Codex 任务名语义，而不是做自然语言美化。

---

## 9. 标题 resolver

修改：

```text
src/codex/metadata.rs
```

建议将现有内联 title 选择拆成独立 helper，避免 `resolve_thread()` 继续膨胀，例如：

```rust
resolve_thread_title(...)
```

### 9.1 Main Session

Main Session 行为完全不变：

```text
state.name
→ state.title
→ session_index.thread_name
→ None
```

`agent_path` 永远不得为 Main Session 生成标题。

### 9.2 Subagent

优先级固定：

```text
1. state.name
2. state.title
3. session_index.thread_name
4. state.agent_path 派生名称
5. rollout.agent_path 派生名称
6. None
```

即：

```text
明确标题来源
> agent_path fallback
```

任何已有明确 `name/title/session_index` 都不能被 `agent_path` 覆盖。

### 9.3 State 与 rollout agent_path

若：

```text
state.agent_path 有效
rollout.agent_path 有效
```

则 state 优先。

如果两者不同，不因为低优先级 rollout 不同而自动把 thread 标记 conflict；resolver 直接采用 state。

rollout 内部同等级 candidate 冲突仍由 rollout parser 自己标记 conflict。

### 9.4 State source 不完整时的保护

当前 resolver 在 state 不完整时会避免重写大量 state 权威字段。标题 fallback 也必须遵守“不能用低优先级来源破坏已有明确值”的原则。

规则：

```text
state source unavailable/incomplete
+
existing.title 已有值
+
当前只能得到 rollout.agent_path fallback

→ 保持 existing.title
```

只有：

```text
role == Subagent
existing.title == None
rollout.agent_path 有效
```

时，才允许 rollout fallback 填充派生标题。

不要因为 state 暂时不可用，把已经存储的明确标题降级成 `agent_path` 派生标题。

### 9.5 Metadata quality

`agent_path` 成功派生标题后：

```text
title != None
```

因此 title 缺失不再导致这部分 partial。

但其他 metadata 缺失仍按当前规则决定：

```text
complete / partial / conflict
```

不得为了本功能放宽整体 quality 判定。

---

## 10. 最终标题输出层

本版不在 public Thread schema 增加：

```text
threads.agent_path
threads.agent_nickname
```

最终 canonical 输出仍然是：

```text
threads.title = "Gate b rereview"
```

因此以下生产链路原则上无需增加 fallback：

```text
threads.title
→ aggregate/session detail
→ Detail API title
→ frontend formatSessionTitle()
```

禁止：

```text
API 层解析 agent_path
aggregate.rs 生成标题
Drawer 解析 /root/...
sessionFormat.ts 新增 agent_path fallback
```

---

# Part B — 预估费用模块

## 11. Cost 模块结构

新增：

```text
src/cost/
├── mod.rs
├── pricing.rs
└── estimator.rs
```

最终由 crate module root 注册：

```text
cost
```

职责必须保持：

```text
PricingRepository
  = 查价格

ModelPricing
  = 一份模型定价规则

CostEstimator
  = 使用 NormalizedTokenUsage + ModelPricing 算费用
```

不得让 PricingRepository 参与 Token 解析，也不得让 CostEstimator 直接读取 DB / rollout JSON。

---

## 12. Cost domain

### 12.1 金额内部单位

DB 与聚合层统一使用整数：

```text
USD nanodollar
1 USD = 1,000,000,000 nanos
```

字段统一：

```text
*_nanos_usd
```

禁止用 `f64` 在 event、DB、aggregate 层累计金额。

API 边界才转：

```rust
nanos as f64 / 1_000_000_000.0
```

Estimator 内部乘法/加法使用 checked `i128`，最后安全转换为非负 `i64`。

### 12.2 `TokenRates`

```rust
pub struct TokenRates {
    pub input_nanos_per_token: i64,
    pub cached_input_nanos_per_token: i64,
    pub cache_write_nanos_per_token: Option<i64>,
    pub output_nanos_per_token: i64,
}
```

`cache_write_nanos_per_token` 保持 `Option`：没有可靠价格时不得猜测。

### 12.3 `LongContextPolicy`

```rust
pub struct LongContextPolicy {
    pub threshold_input_tokens: i64,
    pub rates: TokenRates,
}
```

阈值属于 `ModelPricing`，不属于 Codex parser。

### 12.4 `ModelPricing`

```rust
pub struct ModelPricing {
    pub canonical_model_id: &'static str,
    pub aliases: &'static [&'static str],
    pub effective_from_ms: i64,
    pub effective_to_ms: Option<i64>,
    pub short_context: TokenRates,
    pub long_context: Option<LongContextPolicy>,
}
```

### 12.5 Context tier

```rust
pub enum ContextTier {
    Short,
    Long,
}
```

### 12.6 Event granularity

Cost 模块不要直接依赖 `usage::processor::EventKind`。

增加费用领域自己的输入枚举：

```rust
pub enum UsageCostGranularity {
    RequestScoped,
    AggregateCompensation,
}
```

usage ledger 映射：

```text
Normal            → RequestScoped
Recovered         → RequestScoped
TurnCompensation  → AggregateCompensation
```

这样 CostEstimator 只知道“这份 usage 能否视为单 request”，不需要知道 MU processor 的业务枚举。

### 12.7 Outcome

```rust
pub enum UnknownCostReason {
    UnknownModel,
    MissingCacheWriteRate,
    AmbiguousLongContextGranularity,
}

pub enum CostEstimateOutcome {
    Known(EstimatedCost),
    Unknown(UnknownCostReason),
}
```

算术溢出不是 Unknown；必须作为真正错误返回。

---

## 13. PricingRepository

接口：

```rust
pub trait PricingRepository {
    fn resolve(
        &self,
        model: &str,
        occurred_at_ms: i64,
    ) -> Option<&ModelPricing>;
}
```

v1 实现：

```text
BundledPricingRepository
```

本版：

- 不联网；
- 不接 OpenRouter；
- 不做用户 override；
- 不从 frontend 获取价格；
- 不对未知模型 fallback。

匹配顺序：

```text
exact canonical model id
→ exact alias
→ occurred_at_ms 命中 pricing effective range
→ 找不到则 UnknownModel
```

禁止：

```text
contains
starts_with
模糊 family 匹配
未知模型自动套 GPT-5.5 / GPT-5.6
```

---

## 14. v1 GPT-5.6 Standard pricing catalog

本版费用口径固定使用 OpenAI API **Standard** 价格；不得混入 Batch / Flex / Fast mode。

当前官方 Standard 价格，单位 USD / 1M tokens：

| Model | Context | Input | Cached input | Cache write | Output | Threshold |
|---|---|---:|---:|---:|---:|---:|
| `gpt-5.6-sol` | Short | 5.00 | 0.50 | 6.25 | 30.00 | 272,000 |
| `gpt-5.6-sol` | Long | 10.00 | 1.00 | 12.50 | 45.00 | >272,000 |
| `gpt-5.6-terra` | Short | 2.00 | 0.20 | 2.50 | 12.00 | 272,000 |
| `gpt-5.6-terra` | Long | 4.00 | 0.40 | 5.00 | 18.00 | >272,000 |
| `gpt-5.6-luna` | Short | 0.20 | 0.02 | 0.25 | 1.20 | 272,000 |
| `gpt-5.6-luna` | Long | 0.40 | 0.04 | 0.50 | 1.80 | >272,000 |

alias：

```text
gpt-5.6 → gpt-5.6-sol
```

对应 nanos/token：

```text
gpt-5.6-sol Short :  5000 /  500 /  6250 / 30000
gpt-5.6-sol Long  : 10000 / 1000 / 12500 / 45000

gpt-5.6-terra Short: 2000 / 200 / 2500 / 12000
gpt-5.6-terra Long : 4000 / 400 / 5000 / 18000

gpt-5.6-luna Short : 200 / 20 / 250 / 1200
gpt-5.6-luna Long  : 400 / 40 / 500 / 1800
```

顺序均为：

```text
input / cached input / cache write / output
```

其他模型只有在有可靠官方价格数据时才能加入 catalog；无法可靠定价则保持 Unknown，不得为了让 UI 有数字而猜价。

`PRICING_CATALOG_VERSION` 从：

```text
1
```

开始。

---

## 15. CostEstimator 公式

### 15.1 cache write

费用算法内部：

```text
effective_cache_write_tokens
= usage.cache_write_tokens.unwrap_or(0)
```

注意：这只是费用估算假设。

Canonical：

```text
cache_write_tokens=None
```

必须继续保持 `None`，不得改成 `Some(0)`。

### 15.2 uncached input

费用内部：

```text
billable_uncached_input_tokens
= usage.input_tokens
- usage.cached_tokens
- effective_cache_write_tokens
```

必须 checked subtraction。

不得调用/修改 canonical `uncached_input_tokens()` 来偷偷改变其 `None` 语义。

### 15.3 公式

选定 rates 后：

```text
input_cost
= billable_uncached_input_tokens × input_rate

cached_input_cost
= cached_tokens × cached_input_rate

cache_write_cost
= effective_cache_write_tokens × cache_write_rate

output_cost
= output_tokens × output_rate

total_cost
= input_cost
+ cached_input_cost
+ cache_write_cost
+ output_cost
```

### 15.4 Reasoning

`reasoning_tokens` 是 `output_tokens` 的子集。

因此：

```text
reasoning_tokens
```

保留统计，但**不额外收费**。

禁止：

```text
output_tokens × output_rate
+
reasoning_tokens × output_rate
```

### 15.5 cache-write rate 缺失

```text
effective_cache_write_tokens == 0
→ 不要求 pricing 有 cache-write rate
→ cache_write_cost = 0

effective_cache_write_tokens > 0
且 cache_write rate == None
→ Unknown(MissingCacheWriteRate)
```

这样当前账号登录 Codex 即使不给 cache-write 数据也能估算；未来 raw 开始返回真实 `cache_write_input_tokens` 后，Adapter 写入 `cache_write_tokens`，Estimator 自动开始纳入费用，不需要改公式。

---

## 16. Short / Long Context 判定

### 16.1 判断单位

Long/Short 的正确判断单位是：

```text
一次模型 request 的完整 prompt input tokens
```

不是：

```text
Session total
User Turn total
时间 bucket
Dashboard aggregate
累计 total_token_usage
```

判断字段：

```text
NormalizedTokenUsage.input_tokens
```

这是 gross input。

对于存在 `LongContextPolicy` 的模型：

```text
input_tokens <= threshold → Short
input_tokens >  threshold → Long
```

当前 GPT-5.6 threshold：

```text
272,000
```

所以：

```text
272,000 → Short
272,001 → Long
```

一旦进入 Long，**完整 request** 使用 long rates，不是只对超过阈值的部分加价。

### 16.2 Normal

```text
Normal
→ token_count.last_token_usage
→ RequestScoped
```

可直接按该 event 的 `input_tokens` 判断 Short/Long。

### 16.3 Recovered

当前 processor 的 Recovered 是在一个明确 `token_count` 边界缺少 `last_usage` 时，以：

```text
current_total - previous_total
```

恢复该边界 usage。

本版映射：

```text
Recovered → RequestScoped
```

不得把它改成 Session 累计，也不得重写 recovery 算法。

### 16.4 TurnCompensation

TurnCompensation：

```text
end_total - start_total - accounted_usage
```

可能覆盖一个或多个未正常记账的 request。

映射：

```text
TurnCompensation → AggregateCompensation
```

对有 LongContextPolicy 的模型：

```text
compensation.input_tokens <= threshold
→ 可以安全按 Short

compensation.input_tokens > threshold
→ 无法区分：
   一个 Long request
   或多个 Short request
→ Unknown(AmbiguousLongContextGranularity)
```

对没有 LongContextPolicy、只有唯一线性 rates 的模型，可按唯一 rates 正常计算。

### 16.5 聚合层禁止重新判断

费用只能：

```text
Event A → Cost A
Event B → Cost B
Event C → Cost C

Aggregate Cost = SUM(A, B, C)
```

禁止：

```text
SUM(tokens)
→ 再判断 >272K
→ 再乘模型价格
```

---

## 17. Schema v7：Usage Context + Estimated Cost

新增：

```text
src/storage/schema/0007_usage_context_and_estimated_cost.sql
```

本 migration 是 **Reasoning Effort durable state 与 Cost Workstream 的唯一共享 schema migration**。不得再另建 `0008_reasoning_effort.sql`、`0008_turn_effort.sql` 或 `0008_estimated_cost.sql`。

### 17.1 `usage_events`

新增：

```sql
reasoning_effort TEXT,
estimated_cost_nanos_usd INTEGER
```

并保持：

```text
estimated_cost_nanos_usd IS NULL
或
estimated_cost_nanos_usd >= 0
```

语义：

```text
reasoning_effort = TEXT
→ 该 event 对应 rollout turn_context effort 已知

reasoning_effort = NULL
→ 原始 rollout 未给出或当前链路无法可靠归属
→ 不允许根据模型默认值猜测

estimated_cost_nanos_usd >= 0
→ 该 event 费用可可靠估算

estimated_cost_nanos_usd = NULL
→ 该 event 费用无法可靠估算
```

`reasoning_effort` 必须保留为独立列，不允许拼进：

```text
model = "gpt-5.6-sol-high"
```

正确形式：

```text
model = "gpt-5.6-sol"
reasoning_effort = "high"
```

### 17.2 `usage_source_states`

为支持非零 checkpoint 增量续读，增加：

```text
active_reasoning_effort TEXT
active_reasoning_effort_offset INTEGER NULL CHECK >= 0
```

并保持：

```text
active_reasoning_effort IS NULL
→ active_reasoning_effort_offset 必须为 NULL

active_reasoning_effort 非 NULL
→ active_reasoning_effort_offset 必须非 NULL
```

SQLite 不能通过普通 `ALTER TABLE ... ADD CONSTRAINT` 给已有表补跨列 CHECK，因此 `0007` 必须按项目现有 migration 风格重建 `usage_source_states`，完整保留所有既有字段、CHECK、PK、FK 与数据。

不能只把 effort 写到 `usage_events` 而不持久化 active state。否则 Scanner 从非零 offset 恢复时，可能在下一个 `turn_context` 之前生成 usage event，从而丢失本应继承的 effort。

### 17.3 `turns`

Reasoning Effort 还必须进入 open Turn durable state。否则一个 Turn 跨 batch / restart 后，`TurnCompensation` 无法知道前半段已经观察过哪些 effort。

`turns` 增加：

```text
reasoning_effort_state TEXT NOT NULL
    CHECK IN ('none', 'single', 'mixed')

single_reasoning_effort TEXT

unresolved_reasoning_effort_seen INTEGER NOT NULL
    CHECK IN (0, 1)
```

保持：

```text
reasoning_effort_state = 'single'
⇔
single_reasoning_effort IS NOT NULL
```

旧 v3 Turn 行迁移到 v7 时初始化为：

```text
reasoning_effort_state = 'none'
single_reasoning_effort = NULL
unresolved_reasoning_effort_seen = 0
```

旧 active epoch 仍可在 v4 rebuild 完成前继续提供稳定 Token/费用查询；这些旧 Turn 状态不得被拿来伪造历史 effort。

由于新增跨列 CHECK，`0007` 同样应重建 `turns`，并完整保留现有：

```text
start/last/accounted token state
model_state
compensation blocks
quality/status
PK/FK/CHECK
```

所有 `turns` 的 read/write/upsert/carry/compatibility SQL 都必须同步携带新增三列。

### 17.4 `app_meta`

新增：

```sql
cost_algorithm_version INTEGER NOT NULL DEFAULT 0 CHECK >= 0
pricing_catalog_version INTEGER NOT NULL DEFAULT 0 CHECK >= 0
```

代码常量：

```rust
pub const COST_ALGORITHM_VERSION: i64 = 1;
pub const PRICING_CATALOG_VERSION: i64 = 1;
```

费用版本继续与 parser/canonical 版本独立。

### 17.5 canonical identity 与 derived cost

`reasoning_effort` 是 usage event 的 canonical context 字段，必须参与：

```text
UsageEvent equality
canonical duplicate/conflict comparison
event_id deterministic encoding
```

因此本版需要 usage parser/canonical v4。

`estimated_cost_nanos_usd` 仍是 derived metric，禁止参与：

```text
event_id
canonical fingerprint
Token equality
canonical conflict comparison
```

价格更新后 event identity 不变。

## 18. 新 usage event 写入费用

当前 processor event 转 storage event 的明确边界在：

```text
src/usage/ledger.rs
source_commit()
```

当前这里把：

```text
processor::UsageEvent
→ storage::usage::UsageEventWrite
```

本版应在这个边界接入费用计算。

### 18.1 `UsageEventWrite`

修改：

```text
src/storage/usage.rs
```

增加：

```rust
pub reasoning_effort: Option<String>,
pub estimated_cost_nanos_usd: Option<i64>,
```

### 18.2 `source_commit()` 映射

对每个 processor event：

```text
event.kind
→ UsageCostGranularity

event.model
+ event.occurred_at_ms
→ PricingRepository.resolve()

event.usage
+ ModelPricing
+ UsageCostGranularity
→ CostEstimator
```

结果：

```text
Known(cost)
→ Some(cost.total_nanos_usd)

Unknown(...)
→ None
```

再构造 `UsageEventWrite`。

不要让 storage INSERT SQL 自己做 pricing 计算。

### 18.3 所有 event copy/carry SQL

必须搜索所有：

```text
INSERT INTO usage_events
INSERT INTO usage_events (...) SELECT ... FROM usage_events
```

凡是复制既有 canonical event 到其他 epoch 的路径，都必须同时复制：

```text
reasoning_effort
estimated_cost_nanos_usd
```

其中：

```text
reasoning_effort
→ canonical event 字段，参与 canonical compare

estimated_cost_nanos_usd
→ derived 字段，canonical compare 继续忽略
```

---

## 19. 历史费用 reprice/backfill

只给新 event 算费用不够。升级后已有 `usage_events` 也必须得到当前 pricing catalog 下的估算值。

### 19.1 触发位置

当前真正的 DB `Ledger::open()` 位于：

```text
src/storage/mod.rs
```

当前顺序：

```text
open SQLite
→ configure
→ migrate
→ validate_schema
→ bind_codex_home
→ read_revision_tuple
→ init watch channel
```

改为：

```text
open SQLite
→ configure
→ migrate
→ validate_schema
→ bind_codex_home
→ refresh_usage_costs_if_needed
→ read_revision_tuple
→ init watch channel
```

费用 refresh 必须在初始 revision 被发布之前完成。

### 19.2 触发条件

```text
app_meta.cost_algorithm_version != COST_ALGORITHM_VERSION
OR
app_meta.pricing_catalog_version != PRICING_CATALOG_VERSION
```

### 19.3 refresh

新增内部 storage/cost refresh 函数；具体文件可放：

```text
src/storage/usage.rs
```

或职责清晰的：

```text
src/storage/cost.rs
```

如果新建 `storage/cost.rs`，只允许做：

```text
读取 usage_events
调用 CostEstimator
回写 cost/version/revision
```

不得复制 pricing 公式。

单个原子事务内：

```text
1. 读取所有 usage_events 计费所需列
2. 还原 NormalizedTokenUsage
3. event_kind → UsageCostGranularity
4. 调用同一个 PricingRepository + CostEstimator
5. Known → 写 nanos
6. Unknown → 写 NULL
7. 全部成功后更新两个 version
8. data_revision += 1 一次
9. commit
```

任何 canonical row 非法或算术溢出，整个 refresh 失败，不允许留下部分新价格、部分旧价格。

### 19.4 后续价格更新

未来 bundled pricing 改动：

```text
更新 pricing catalog
→ PRICING_CATALOG_VERSION + 1
→ Ledger::open()
→ 自动 reprice
```

不需要 bump Token parser，不需要重建 Token event。

---

## 20. Aggregate

修改：

```text
src/usage/aggregate.rs
```

### 20.1 `TokenTotals`

当前所有 usage scope 统一通过 `TokenTotals` 传递 Token 数据，因此本版将聚合费用也放入该聚合承载对象：

```rust
pub estimated_cost_nanos_usd: Option<i64>,
```

费用仍然是 derived metric，不改变 `NormalizedTokenUsage`。

### 20.2 空范围

```text
无 usage event
→ estimated_cost_nanos_usd = Some(0)
```

API：

```text
estimated_cost = 0.0
```

### 20.3 聚合 nullable 规则

```text
所有 event cost 都 Known
→ SUM(cost)

只要存在一个 event cost == NULL
→ 整个范围 cost = NULL
```

不得返回“已知部分费用”冒充总费用。

SQL 需要同时得到：

```text
SUM(estimated_cost_nanos_usd)
cost_unknown_count
```

必须与现有 cache-write unknown 统计分开命名，不能复用一个 unknown_count。

### 20.4 所有聚合路径统一接通

必须沿现有 aggregate 架构逐一接通：

```text
Summary
Model
Session self
Session subagent
Session inclusive
Session Detail Main `(model, reasoning_effort)` model_usage[]
Session Detail Subagent usage
所有依赖 TokenTotals 的 filter/sort/session row 路径
```

原则：

```text
只 SUM event cost
```

不得在任一 aggregate scope 上重新调用 PricingRepository。

### 20.5 `UsageSummary`

当前存在独立：

```rust
UsageSummary.estimated_cost: Option<f64>
```

建议删除这个重复 source，统一由：

```text
UsageSummary.totals.estimated_cost_nanos_usd
```

提供费用。

不要同时维护：

```text
UsageSummary.estimated_cost
+
TokenTotals.estimated_cost_nanos_usd
```

两套可分叉的费用真相。

---

## 21. API

修改：

```text
src/api/query.rs
```

外部 JSON contract 保持：

```rust
estimated_cost: Option<f64>
```

统一在 API 边界：

```text
Some(nanos)
→ Some(nanos / 1_000_000_000.0)

None
→ null
```

语义：

```text
Known cost   → number
Unknown cost → null
Empty usage  → 0.0
```

删除生产代码中代表“费用未实现”的硬编码：

```rust
estimated_cost: None
```

真实 Unknown 仍允许返回 `None`。

API 不读取 pricing catalog，不重新算费用。

---

## 22. 前端费用显示接通

本版原则上不改费用 UI 结构。

现有位置继续使用现有字段：

### Dashboard

```text
MetricGrid.tsx
usage.estimated_cost
```

### Session List

```text
SessionTableRow.tsx
item.inclusive_usage.estimated_cost
```

### Drawer Main

每个：

```text
model_usage[].usage.estimated_cost
```

Main block 在 Part C 中改为按 `(model, reasoning_effort)` 分组。因此费用也必须自然随该分组聚合；只显示该模型配置 block 自己的 event cost，不得把 Main 总费用复制到每个 block。

### Drawer Subagent

```text
subagent.usage.estimated_cost
```

显示该 Subagent 自身 usage event 的费用聚合。

继续使用现有：

```text
formatCost()
number → $x.xx
null   → —
```

禁止：

```text
frontend pricing table
frontend cost formula
frontend self + subagent 二次求和
```

如果生产前端已经完全按上述字段读取，则无需为了本功能制造无意义的前端改动。

---

# Part C — Reasoning Effort usage 维度与 Drawer 展示

## 23. 数据来源与字段语义

本功能的历史主数据源固定为 Codex rollout `turn_context`。

当前适配优先读取：

```text
payload.effort
```

若实际 rollout 版本同时存在兼容字段：

```text
payload.reasoning_effort
```

可仅在 `payload.effort` 缺失时作为兼容 fallback；不得反向覆盖 `payload.effort`。

本版不建立固定 allowlist：

```text
low / medium / high / xhigh / max / ultra ...
```

因为 effort 能力由模型版本决定。MU 只做安全字符串规范化：

```text
trim
→ 空字符串 = None
→ 非空值统一 lowercase
→ 禁止 control characters
```

不得把未知 effort 强制映射成 `medium`、`high` 或任何模型默认值。

`state_5.threads.reasoning_effort` 不作为 usage 历史来源。本版不需要为了 Drawer 分组扩展 state metadata reader 去回填该字段，因为 Thread 级当前值无法证明历史每个 usage event 的 effort。

---

## 24. Codex usage parser

修改：

```text
src/codex/usage.rs
```

`TurnContextRecord` 改为：

```rust
pub struct TurnContextRecord {
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub occurred_at_ms: Option<i64>,
}
```

`parse_turn_context()`：

```text
payload.model
→ model

payload.effort
→ normalize_reasoning_effort()
→ reasoning_effort
```

若实现兼容 fallback：

```text
payload.effort
→ 有值：使用
→ 无值：再读取 payload.reasoning_effort
```

不得在 parser 中把 effort 拼接进 model。

---

## 25. Usage pipeline、Turn state 与 durable context

修改：

```text
src/usage/processor.rs
src/usage/pipeline.rs
src/usage/ledger.rs
src/storage/usage.rs
src/usage/rebuild.rs
```

### 25.1 `UsageRecord::TurnContext`

从：

```rust
TurnContext {
    ownership,
    model,
}
```

改为：

```rust
TurnContext {
    ownership,
    model,
    reasoning_effort,
}
```

### 25.2 `UsageSourceState` / `SourceStateProof`

`UsageSourceState` 新增：

```rust
pub active_reasoning_effort: Option<String>,
```

`SourceStateProof` 新增：

```rust
pub active_reasoning_effort_offset: Option<u64>,
```

Storage state 对应新增：

```rust
pub active_reasoning_effort: Option<String>,
pub active_reasoning_effort_offset: Option<i64>,
```

pipeline 像现有 `active_model_offset` 一样记录 effort 来源 `turn_context` 的起始 offset。

关键更新规则：

```text
收到 owning turn_context：

model=Some(v)
→ 沿用现有 active_model 更新规则

reasoning_effort=Some(v)
→ active_reasoning_effort=Some(v)
→ active_reasoning_effort_offset=该 turn_context start offset

reasoning_effort=None
→ active_reasoning_effort=None
→ active_reasoning_effort_offset=None
```

新的 `turn_context` 没有 effort 时，不得继续沿用前一个 Turn 的已知 effort。

`src/usage/ledger.rs` 必须完整负责：

```text
storage UsageSourceStateWrite
↔ SourceStateProof
↔ UsageSourceState
```

的双向映射，不能只把字段加进 processor/pipeline。

### 25.3 Usage event

`UsageEvent` 新增：

```rust
pub reasoning_effort: Option<String>,
```

Normal / Recovered event 创建时同时取：

```text
state.active_model
state.active_reasoning_effort
```

最终 canonical event：

```text
thread/root/turn
model
reasoning_effort
NormalizedTokenUsage
estimated_cost（后续 derived）
```

### 25.4 TurnCompensation effort 归属

当前 processor 已对 Turn 内模型使用 `TurnModelState` 防止混合模型补偿被错误归类。Reasoning Effort 必须建立同等但独立的 Turn 状态：

```rust
pub enum TurnReasoningEffortState {
    None,
    Single(String),
    Mixed,
}
```

`TurnState` 同时增加：

```text
reasoning_effort_state
unresolved_reasoning_effort_seen
```

每个已记账 event 都观察其 effort：

```text
event.reasoning_effort = Some(v)
→ None → Single(v)
→ Single(v) + same v → Single(v)
→ Single(other) → Mixed
→ Mixed 保持 Mixed

event.reasoning_effort = None
→ unresolved_reasoning_effort_seen = true
```

TurnCompensation：

```text
unresolved_reasoning_effort_seen = true
→ reasoning_effort=None

否则 reasoning_effort_state=Single(v)
→ reasoning_effort=Some(v)

Mixed / None
→ reasoning_effort=None
```

这样：

```text
high + high        → compensation high
high + medium      → compensation Unknown
high + Unknown     → compensation Unknown
只有 Unknown       → compensation Unknown
```

不得把 mixed/unknown Turn 的补偿量强行归到最后一次 effort。

### 25.5 Open Turn 持久化与 carry

`TurnReasoningEffortState` 不是临时 processor 状态。它必须与现有 `TurnModelState` 一样进入：

```text
UsageTurnWrite
turns row decode
turns INSERT / UPSERT
open Turn restore
closed Turn write
carry_turn()
carry seed compatibility comparison
```

否则 batch/restart/carry 会改变 compensation 的 effort 归属。

Reasoning effort 缺失不阻止 Token compensation 本身；它只使 compensation event 的：

```text
reasoning_effort = NULL
```

因此不得新增一个“effort unknown 就禁止 compensation”的 Token block。

### 25.6 Rebuild state proof

`src/usage/rebuild.rs::active_state_fingerprint()` 必须加入：

```text
active_reasoning_effort
active_reasoning_effort_offset
```

并把 fingerprint domain tag 从现有：

```text
usage-source-state-proof-v1
```

升级为新的明确版本，例如：

```text
usage-source-state-proof-v2
```

reasoning effort 已进入 canonical context 后，旧 proof 不能继续代表相同的完整 source state。

该修改只影响 usage rebuild/carry proof，不改变 metadata safe fact。

## 26. Usage parser / canonical v4

因为本版不只是增加 UI 字段，而是改变 canonical usage event 的上下文维度，必须：

```text
USAGE_PARSER_VERSION
3 → 4

USAGE_CANONICAL_ALGORITHM_VERSION
3 → 4
```

修改：

```text
src/usage/normalized.rs
```

映射固定为：

```rust
canonical_algorithm_for(4) == Some(4)
```

新 binary 不继续把 parser v3 视为当前 canonical algorithm。

`event_id()`：

- deterministic encoder tag 升级，例如 `usage-event-v1 → usage-event-v2`；
- 在 model 后加入 `optional reasoning_effort`；
- 同一原始 request 重放必须稳定得到同一 v4 event id；
- 相同 model/token、不同 effort 的 canonical event 不得被视为同一个 event payload。

这里的版本 bump 是 reasoning-effort 功能要求，不是费用模块要求。

`NormalizedTokenUsage` 六个 Token 字段不新增 effort，Token 数值口径保持不变。

---

## 27. Storage event、source state、Turn 与 SQL 链路

`UsageEventWrite` 最终固定为：

```text
event_id
kind
occurred_at_ms
thread_id
root_session_id
turn_key
model
reasoning_effort
NormalizedTokenUsage
estimated_cost_nanos_usd
```

必须搜索所有：

```text
INSERT INTO usage_events
INSERT INTO usage_events (...) SELECT ... FROM usage_events
usage event row decode
canonical row compare
carry / rebuild / local replay copy
```

规则：

```text
reasoning_effort
→ canonical event 字段
→ INSERT / copy / compare 必须携带

estimated_cost_nanos_usd
→ derived 字段
→ INSERT / copy 必须携带
→ canonical compare 继续忽略
```

此外必须同步搜索：

```text
usage_source_states 的 SELECT / INSERT / UPSERT / proof
turns 的 SELECT / INSERT / UPSERT / carry / compatibility compare
active_state_fingerprint
```

其中：

```text
active_reasoning_effort + offset
→ source state durable 字段

reasoning_effort_state
single_reasoning_effort
unresolved_reasoning_effort_seen
→ Turn durable 字段
```

不得只修改主 event INSERT，遗漏 source-state resume、open Turn restore、carry copy 或 rebuild proof。

## 28. Main Drawer 聚合：按 `(model, reasoning_effort)` 分组

只修改 Session Detail Main 的 block 维度。

当前：

```text
GROUP BY model
```

改为：

```text
GROUP BY model, reasoning_effort
```

`MainModelUsage`：

```rust
pub struct MainModelUsage {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub usage: TokenTotals,
}
```

排序继续按首次实际活动确定，不按字符串字母顺序重排：

```text
MIN(occurred_at_ms)
→ MIN(event_id)
→ model
→ reasoning_effort
```

示例：

```text
gpt-5.6-sol / high   → block 1
gpt-5.6-sol / medium → block 2
gpt-5.6-terra / max  → block 3
```

同一 `(model, effort)` 的所有 event 合并到同一 block。

`reasoning_effort=NULL` 必须作为独立 Unknown bucket：

```text
gpt-5.6-sol / NULL
```

不得与 `high`、`medium` 等已知 bucket 合并。

费用也随同一 SQL bucket 聚合：

```text
(model, effort) block cost
= SUM(该 bucket 的 event estimated_cost)
```

不得在分组后重新调用 CostEstimator。

### 保持其他模型统计口径不变

本需求只改变 Drawer Main `model_usage[]`。

以下继续以 **model-only** 为维度：

```text
Dashboard 模型筛选
全局 Models API
Session models_used
Session Table 的 model filter/sort 语义
```

不得因为 Drawer 需要 effort，就把 Dashboard 的模型筛选项扩展成 `model + effort`。

---

## 29. Subagent Drawer：保持单 block，仅补 effort 摘要

此前已经冻结的范围继续有效：

```text
一个 Subagent
→ 一个 usage block
→ 本版不继续按 model/effort 拆 block
```

因此 Subagent Detail 需要给单 block 生成一个明确的 effort summary。

Aggregate domain 建议增加：

```rust
pub enum ReasoningEffortSummary {
    Unknown,
    Single(String),
    Mixed,
}
```

判断基于该 Subagent 当前查询时间范围内的 usage events：

```text
没有已知 effort
→ Unknown

所有 event 都有值，且 DISTINCT effort = 1
→ Single(value)

DISTINCT effort > 1
或 已知 effort 与 NULL event 混合
→ Mixed
```

`SubagentDetail` 增加该 summary。

API 不直接输出 Rust enum 字符串，固定结构化为：

```rust
pub reasoning_effort: Option<String>,
pub reasoning_effort_mixed: bool,
```

映射：

```text
Unknown
→ reasoning_effort=null
→ reasoning_effort_mixed=false

Single("high")
→ reasoning_effort="high"
→ reasoning_effort_mixed=false

Mixed
→ reasoning_effort=null
→ reasoning_effort_mixed=true
```

这样前端不需要猜 `null` 到底是 unknown 还是 mixed。

---

## 30. API 与 Drawer 文案

### 30.1 Main API

`MainModelUsageDto`：

```rust
pub struct MainModelUsageDto {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub usage: TokenUsageDto,
}
```

JSON 示例：

```json
{
  "model": "gpt-5.6-sol",
  "reasoning_effort": "high",
  "usage": { "...": "..." }
}
```

### 30.2 Main Drawer

block title 从：

```text
gpt-5.6-sol
```

改为：

```text
gpt-5.6-sol (high)
```

Unknown：

```text
gpt-5.6-sol (—)
```

Main section 当前：

```text
N 个模型
```

必须改成：

```text
N 个模型配置
```

因为：

```text
gpt-5.6-sol (high)
gpt-5.6-sol (medium)
```

是两个 block，但不是两个不同模型。

### 30.3 Subagent Drawer

当前：

```text
gpt-5.6-luna
```

改为：

```text
Single → gpt-5.6-luna (high)
Unknown → gpt-5.6-luna (—)
Mixed → gpt-5.6-luna (mixed)
```

只修改模型文案，不新增第二层 Subagent usage 分组。

前端建议新增一个纯 formatter：

```text
formatModelWithReasoningEffort(model, effort, mixed)
```

不得在 formatter 中维护 effort allowlist、默认值或模型能力矩阵。

---

## 31. 历史 Reasoning Effort 重建

不能用 SQL backfill 给旧 `usage_events.reasoning_effort` 猜值。

正确历史恢复机制：

```text
USAGE_PARSER_VERSION 3 → 4
        │
        ▼
现有 usage rebuild 机制启动 shadow epoch
        │
        ▼
从历史 rollout 重新读取 turn_context
        │
        ├─ model
        └─ effort
        │
        ▼
重新生成 v4 usage_events
        │
        ▼
Main Drawer 获得历史 (model, effort) 分组
```

历史 rollout 中确实没有 effort 的 event：

```text
reasoning_effort=NULL
```

不得使用：

```text
state_5 当前 reasoning_effort
模型默认 reasoning effort
同 Session 相邻 Turn effort
```

去猜测历史值。

---

# Part D — 三个 Workstream 的共享集成

## 32. Migration 注册策略

本版只有两个 migration：

```text
Workstream A — Metadata
→ 0006_subagent_agent_path.sql

Usage Context + Cost
→ 0007_usage_context_and_estimated_cost.sql
```

`0007` 同时承载：

```text
usage_events.reasoning_effort
usage_events.estimated_cost_nanos_usd

usage_source_states.active_reasoning_effort
usage_source_states.active_reasoning_effort_offset

turns.reasoning_effort_state
turns.single_reasoning_effort
turns.unresolved_reasoning_effort_seen

app_meta.cost_algorithm_version
app_meta.pricing_catalog_version
```

`src/storage/migrations.rs` 属于共享集成文件，只由 Integration Owner 在 **Wave 1.5** 统一注册：

```text
v5
→ v6 metadata agent_path
→ v7 usage context + cost

LATEST_SCHEMA_VERSION = 7
```

不得为了 reasoning effort 再增加 Schema v8。

---

## 33. Version 关系

本版最终版本变化固定为：

```text
Schema
5 → 7

METADATA_PARSER_VERSION
2 → 3

USAGE_PARSER_VERSION
3 → 4

USAGE_CANONICAL_ALGORITHM_VERSION
3 → 4

COST_ALGORITHM_VERSION
new = 1

PRICING_CATALOG_VERSION
new = 1
```

原因：

```text
agent_path
→ metadata parser/rebuild

reasoning_effort
→ usage parser + canonical usage event/rebuild

estimated cost
→ derived metric + 独立 cost/pricing version
```

不要因为 cost 与 effort 同在 migration v7，就让 cost 参与 usage canonical identity。

---

## 34. 历史数据升级顺序

安装本版后：

```text
Ledger::open
│
├─ migration v6
│    └─ rollout_metadata_facts 增加 agent_path durable fields
│
├─ migration v7
│    ├─ usage_events 增加 reasoning_effort + estimated cost
│    ├─ usage_source_states 增加 active reasoning effort + offset
│    ├─ turns 增加 reasoning-effort Turn durable state
│    └─ app_meta 增加 cost/pricing versions
│
├─ validate_schema
│
├─ bind_codex_home
│
├─ cost version 检查
│    └─ 对当前 active epoch 已有 usage_events 做 reprice/backfill
│
└─ Ledger open 完成
```

Scanner 启动后，两条 rebuild 独立进行：

```text
Metadata:
checkpoint metadata parser=2
!= METADATA_PARSER_VERSION=3
→ metadata rebuild
→ 历史 agent_path/title 补齐
```

```text
Usage:
active usage parser=3
!= USAGE_PARSER_VERSION=4
→ shadow usage epoch rebuild
→ 历史 turn_context effort 补齐
→ 新 v4 event 在写入时直接计算 estimated cost
→ rebuild 完成后激活 v4 epoch
```

这里允许 DB open 先给旧 active v3 event 做 cost backfill，因为 usage rebuild 可能不是瞬时完成；旧 epoch 在切换前仍然是查询真相，应该具备费用显示。

v4 新 event 必须在生成时直接带费用，因此 v4 epoch 激活后不依赖再次全表 reprice 才能显示费用。

不得把：

```text
metadata rebuild
usage rebuild
cost reprice
```

合并成同一个重建状态机。

---

# Part E — 并行开发组织、文件所有权与测试 Gate

## 施工总原则

本 Part 必须与独立测试标准：

```text
Usagi_预估费用模块测试标准_v0.2.md
```

配套执行。

本文不重复定义测试断言，只规定：

```text
谁修改哪些生产文件
哪些工作可以并行
何时发生文件 ownership handoff
哪些 Gate 必须先 PASS
```

同一个文件允许**跨 Wave 顺序移交 ownership**，但同一 Wave 内不得由两个 Subagent 并行编辑。

---

## 35. 施工 + 测试 Gate 总图

```text
Wave 0 — Integration Owner
冻结 contract
+ 预注册 cost module skeleton
        │
        ▼
┌────────────────────────────────────────────────────────┐
│ Wave 1：四路并行                                      │
│ A1 Metadata │ B1 Effort durable │ C1 Cost Core │ F1 UI │
└────────────────────────────────────────────────────────┘
        │
        ▼
Gate 1 — 纯逻辑/局部编译 Gate
        │
        ▼
Wave 1.5 — Integration Owner 串行基础集成
合并 A1/B1/C1/F1 基础结果
注册 migration v6/v7
LATEST_SCHEMA_VERSION=7
更新全局 schema/version 断言
        │
        ▼
Gate 1.5 — durable/schema Gate
T-MU03-A01
T-MU03-C02
T-MU03-S01
        │
        ├──────────────┬────────────────┐
        ▼              ▼                ▼
Wave 2 A2         Wave 2 D1         Wave 2 E1
Title Resolver    Cost Runtime      Aggregate/API
        │              │                │
        └──────────────┴────────────────┘
                       │
                       ▼
Gate 2 — 模块闭环 Gate
                       │
                       ▼
Wave 3 — Integration Owner
最终合并 A2/D1/E1
接通 F1
处理 shared imports / legacy tests
                       │
                       ▼
Gate 3 — 跨模块集成 Gate
                       │
                       ▼
Wave 4 — Production cleanup
                       │
                       ▼
Gate 4 — 历史升级 E2E + 全量回归
                       │
                       ▼
                     完成
```

---

## 36. Wave 0 — Integration Owner 串行冻结接口

开始编码前固定：

```text
Schema v6 = agent_path metadata
Schema v7 = usage context + Turn effort state + estimated cost
LATEST_SCHEMA_VERSION = 7

METADATA_PARSER_VERSION = 3
USAGE_PARSER_VERSION = 4
USAGE_CANONICAL_ALGORITHM_VERSION = 4

usage event:
reasoning_effort

usage source state:
active_reasoning_effort
active_reasoning_effort_offset

Turn durable state:
reasoning_effort_state
single_reasoning_effort
unresolved_reasoning_effort_seen

storage cost:
estimated_cost_nanos_usd

Main Detail:
GROUP BY (model, reasoning_effort)

Subagent:
单 block + effort summary
```

新增类型名固定：

```text
AgentPathProvenance
UsageCostGranularity
ReasoningEffortSummary
TurnReasoningEffortState
```

### Wave 0 必须提前解决 cost module 编译入口

当前 crate root `src/lib.rs` 尚无 `cost` module。如果仍等到 Wave 3 才注册，C1 的 module tests 与后续 D1 `crate::cost` 引用都无法形成可编译 Gate。

因此由 Integration Owner 在 Wave 0：

```text
src/lib.rs
→ 注册最小可见性的 cost module

src/cost/mod.rs
src/cost/pricing.rs
src/cost/estimator.rs
→ 创建可编译 skeleton
```

这里只建立 module 边界，不写 pricing/estimator 业务逻辑。Wave 0 完成后，`src/cost/*` ownership 移交给 C1。

---

## 37. Wave 1 — 四路并行

### Subagent A1 — Metadata ingestion + durable fact

负责：

```text
src/codex/state_index.rs
src/codex/rollout.rs
src/domain.rs
src/storage/source.rs
src/storage/metadata.rs
src/scanner/pipeline.rs
src/storage/schema/0006_subagent_agent_path.sql
```

完成：

```text
StateThreadFact.agent_path
state allowlist/read
normalize_agent_path

SessionMetaAllowed 两个 agent_path 来源
AgentPathProvenance
RolloutThreadFact.agent_path

RolloutMetadataFact durable fields
to_safe_fact/from_safe_fact

source.rs safe-fact read/write
metadata.rs read_fact/write_fact/offset validation
scanner/pipeline.rs 必要 struct literal/helper

METADATA_PARSER_VERSION 2→3
```

边界：

```text
src/storage/metadata.rs
✓ 只改 agent_path durable fact 所需 SQL/row decode/offset invariant
✗ 不改 title resolver / parent-root 语义

src/scanner/pipeline.rs
✓ 只补 RolloutMetadataFact 字段初始化
✗ 不改 planning/checkpoint/rebuild/consumer 调度
```

A1 不改：

```text
src/codex/metadata.rs
usage/cost/frontend
src/storage/migrations.rs
```

### Subagent B1 — Reasoning Effort canonical + durable usage context

B1 必须形成一个**可连续持久化的 usage-context 闭环**，不能再拆成“processor 先改、storage 以后再补”，否则 `UsageSourceState` / `SourceStateProof` struct 变化会直接让当前 `usage/ledger.rs` 无法编译。

负责：

```text
src/codex/usage.rs
src/usage/processor.rs
src/usage/pipeline.rs
src/usage/ledger.rs
src/usage/rebuild.rs
src/usage/normalized.rs
src/storage/usage.rs
src/storage/schema/0007_usage_context_and_estimated_cost.sql
```

完成：

```text
TurnContextRecord.reasoning_effort
normalize_reasoning_effort
UsageRecord::TurnContext.reasoning_effort

UsageSourceState.active_reasoning_effort
SourceStateProof.active_reasoning_effort_offset
storage source-state round-trip

UsageEvent.reasoning_effort
TurnReasoningEffortState
TurnState effort state
turns durable round-trip / carry compatibility

event_id v2
canonical event compare/copy 携带 effort

active_state_fingerprint v2
并包含 active effort + offset

USAGE_PARSER_VERSION 3→4
USAGE_CANONICAL_ALGORITHM_VERSION 3→4
canonical_algorithm_for(4)
```

B1 同时负责在 `0007` 建好本文冻结的最终 schema 形状，包括：

```text
reasoning effort columns
Turn effort columns
estimated_cost_nanos_usd column
cost/pricing version columns
```

但 B1 **不实现费用计算**：

```text
不调用 CostEstimator
不做 reprice
不写 pricing catalog
新 cost 字段保持 NULL / version 默认 0
```

这样 `0007` 只有一个 owner，不会在 B1 与 D1 之间二次改 migration。

### Subagent C1 — Cost Core

负责：

```text
src/cost/mod.rs
src/cost/pricing.rs
src/cost/estimator.rs
```

完成：

```text
TokenRates
LongContextPolicy
ModelPricing
PricingRepository
BundledPricingRepository
UsageCostGranularity
ContextTier
EstimatedCost / UnknownCostReason
cache-write 公式
Short/Long 规则
GPT-5.6 pricing catalog
COST_ALGORITHM_VERSION
PRICING_CATALOG_VERSION
```

C1 不读取/使用 `reasoning_effort` 参与定价。

### Subagent F1 — Session Detail 前端 contract + Drawer

负责：

```text
frontend/src/data/types.ts
frontend/src/data/usagiClient.ts
frontend/src/dashboard/session/SessionDetailDrawer.tsx
必要的 session formatter 文件
对应 frontend unit/component tests
```

完成：

```text
Main DTO reasoning_effort
Subagent DTO reasoning_effort + reasoning_effort_mixed
runtime response validation

Main model + effort title
Main “N 个模型配置”
Subagent single/unknown/mixed effort title
```

`usagiClient.ts` 必须同步修改，因为当前 Session Detail response 不是只依赖 TypeScript 类型，还经过运行时 parser 校验。

F1 不实现费用公式，不改变 Dashboard model filter，不自行设计 API 字段。

---

### Wave 1 Gate — 纯逻辑/局部 Gate

Wave 1 结束时还没有正式注册 v6/v7 migration，因此此 Gate **不执行依赖 fresh Ledger latest schema 的完整 DB round-trip**。

执行：

```text
B1
→ T-MU03-C01
→ T-MU03-C03
→ T-MU03-C04

C1
→ T-MU03-B01
→ T-MU03-B02
→ T-MU03-B03

A1
→ agent_path parser / normalize / safe-fact conversion 的局部 unit subset

F1
→ 受影响 frontend unit/component tests
```

同时至少执行一次：

```text
cargo check --all-targets
```

若新增 struct 使未授权文件出现编译错误，必须回到当前 work package 修正 ownership 清单或必要 struct literal；不得把编译错误推迟到项目最后。

---

## 38. Wave 1.5 — Integration Owner 串行基础集成

Wave 1 四路完成后，先做一次基础集成，不直接进入 A2/D1/E1。

Integration Owner：

```text
1. 合并 A1 / B1 / C1 / F1 的基础结果
2. src/storage/migrations.rs 注册 0006 / 0007
3. LATEST_SCHEMA_VERSION 5→7
4. 更新 migrations.rs / storage/mod.rs / scanner 内仅由全局版本变化导致的旧 schema-version 断言
5. 解决 crate/module/re-export/import 编译问题
6. 不改变 A/B/C 已冻结业务语义
```

### Gate 1.5 — durable/schema Gate

正式执行：

```text
T-MU03-A01
T-MU03-C02
T-MU03-S01

cargo fmt --check
cargo check --all-targets
```

只有 Gate 1.5 PASS 后：

```text
A2 可以开始
D1 可以开始
E1 可以开始
```

这一步保证下游开发面对的已经是真正可打开的 Schema v7，而不是“代码字段已经存在、migration 还没注册”的半成品状态。

---

## 39. Wave 2 — 三路并行

### Subagent A2 — Metadata title resolver

负责：

```text
src/codex/metadata.rs
```

依赖：

```text
A1 + Gate 1.5
```

完成：

```text
subagent_title_from_agent_path()
Main title 路径保持原样
Subagent 标题优先级
state.agent_path > rollout.agent_path
state 不完整时 existing.title 保护
```

### Subagent D1 — Cost runtime + reprice

B1 已经完成 reasoning-effort storage/context plumbing；此时以下文件 ownership 从 B1 **顺序移交**给 D1：

```text
src/storage/usage.rs
src/usage/ledger.rs
```

D1 另外负责：

```text
src/storage/mod.rs
可选新增 src/storage/cost.rs
```

依赖：

```text
B1 + C1 + Gate 1.5
```

完成：

```text
UsageEventWrite.estimated_cost_nanos_usd
source_commit() 调 CostEstimator
所有 event INSERT/COPY/CARRY 携带 derived cost
Ledger::open() cost refresh
cost/pricing version 检查
历史 cost reprice/backfill
```

D1 不再修改 `0007` schema；schema 已由 B1 按 Wave 0 contract 一次性完成。

D1 不得修改：

```text
reasoning effort canonical identity
Turn effort state
usage/rebuild.rs proof semantics
CostEstimator 公式
```

### Subagent E1 — Aggregate + API

负责：

```text
src/usage/aggregate.rs
src/api/query.rs
```

依赖：

```text
B1 reasoning contract
C1 cost contract
Gate 1.5 schema
```

可以与 D1 并行编码。

完成：

```text
TokenTotals cost nanos
cost unknown propagation
Main model_usage GROUP BY (model, reasoning_effort)
MainModelUsage.reasoning_effort
Subagent ReasoningEffortSummary

MainModelUsageDto.reasoning_effort
SubagentDetailDto reasoning_effort + reasoning_effort_mixed

API cost nanos → f64
删除生产 estimated_cost 固定 None
保持 models_used / global Models API 为 model-only
```

E1 不修改 frontend、pricing、storage INSERT。

---

### Wave 2 Gate — 模块闭环 Gate

执行：

```text
A2
→ T-MU03-A02

D1
→ T-MU03-B04
→ T-MU03-B05

E1
→ T-MU03-B06 的 backend aggregate/API 部分
→ T-MU03-C05 的 backend aggregate/API 部分
```

Gate 2 不要求真实 Drawer 全链路；F1 与 E1 的最终 DTO 联调留到 Wave 3。

---

## 40. Wave 3 — Integration Owner 最终合并

顺序：

```text
1. 合并 A2
2. 合并 D1
3. 合并 E1
4. 接通已完成的 F1
5. 解决 shared imports / re-export
6. 检查 usage_events 所有显式 SQL 列
7. 检查 usage_source_states active effort round-trip
8. 检查 turns effort state read/write/carry
9. 检查 active_state_fingerprint 已包含 effort
10. 检查 Main Detail (model, effort)
11. 检查 Subagent title / effort 两条链路
12. 检查所有生产 estimated_cost 占位已接真实 cost
```

### 旧测试代码 ownership

现有仓库有多处历史测试硬编码：

```text
schema version = 5
usage parser/canonical = 3
estimated_cost = null
Main Detail GROUP BY model 的旧 DTO
```

处理规则：

```text
各 Workstream
→ 只修改自己新增行为的局部测试

Integration Owner / Wave 3
→ 统一处理跨 Workstream / 全局版本 / 旧 API contract 的测试更新
```

重点包括但不限于：

```text
src/storage/migrations.rs 内 migration tests
src/storage/mod.rs 内 schema-version tests
src/scanner/mod.rs 内 current schema/parser expectations
tests/spec04_usage_integration.rs
tests/spec05_api_integration.rs
tests/spec06_frontend_browser.rs
frontend 现有 Session Detail contract tests
```

注意：未知模型导致 `estimated_cost=null` 仍是合法新行为。不得机械删除所有 null 断言；只删除“费用永远未实现”这种旧占位语义。

### Gate 3 — 跨模块集成 Gate

执行：

```text
T-MU03-A03
T-MU03-B06
T-MU03-C05
T-MU03-C06
```

同时重新执行共享改动最容易破坏的核心项：

```text
T-MU03-B02
T-MU03-B03
T-MU03-C03
```

Gate 3 PASS 后才能进入清理。

---

## 41. Wave 4 — 生产代码清理

只处理本版产生的旧占位和冗余生产路径。

### Metadata

- 不留下 Drawer `agent_path` fallback；
- 不使用 `agent_nickname`；
- canonical `threads.title` 是唯一标题事实。

### Usage context

- 不保留 Main Detail 旧 `GROUP BY model` 第二套路径；
- 不把 effort 拼入 model；
- 不用 state_5 Thread 级 effort 覆盖 rollout event effort；
- 不保留 parser v3 作为当前算法 fallback；
- 不遗漏 open Turn effort durable state 或 state-proof v2。

### Cost

- 移除代表“费用未实现”的生产 `estimated_cost: None`；
- 真实 Unknown 继续允许 `None`；
- 前端无费用计算。

### 本阶段不做

```text
不重写历史 Spec
不增加与本版无关功能
不为了旧测试断言保留错误生产行为
不做 scanner 单次读取优化
```

### Gate 4 — 最终测试

执行：

```text
T-MU03-S02
T-MU03-S03

T-MU03-F01
T-MU03-F02
T-MU03-F03
```

并执行测试标准规定的全量命令。

Gate 4 不代替 Gate 1 / 1.5 / 2 / 3；执行记录缺少任一前置 Gate，不得仅凭最终全量 PASS 判定施工流程完整。

# Part F — 禁止偏离事项

## 42. Subagent 标题禁止项

1. 不使用 `agent_nickname` 生成任务标题；
2. 不让 Main Session 使用 `agent_path` fallback；
3. 不覆盖已有 `state.name/title/session_index.thread_name`；
4. 不把 `agent_path` fallback 放到 frontend；
5. 不在 API/aggregate 层重新生成标题；
6. 不让 state 暂时不可用导致已有明确 title 被低优先级 rollout fallback 覆盖；
7. 不访问文件系统验证 `agent_path` 是否真实存在；
8. 不把 `agent_path` 加入 public `threads` 表；
9. 不只解析 rollout 而忽略 safe-fact round-trip；
10. 不因该修复修改 usage parser/canonical version。

## 43. Reasoning Effort 禁止项

1. 不使用 `state_5.threads.reasoning_effort` 回填历史 usage event；
2. 不把 `reasoning_effort` 拼进 `model` 字符串；
3. 不硬编码 effort allowlist 或模型默认 effort；
4. rollout 缺失 effort 时不继承前一个 Turn 的旧 effort；
5. 不把 Unknown effort 猜成 `medium/high/max`；
6. Main 必须按 `(model, reasoning_effort)` 分组，不能仍只按 model；
7. `reasoning_effort=NULL` 必须作为独立 Main bucket；
8. Subagent 本版不按 effort 拆多个 usage block；
9. Subagent mixed effort 不得显示成最后一次 effort；
10. 不改变 Dashboard model filter、全局 Models API 和 `models_used` 的 model-only 语义；
11. reasoning effort 不参与 PricingRepository / CostEstimator 的价格选择；
12. 不在 parser v3/canonical v3 下静默改变 event canonical payload。

## 44. 费用禁止项

1. 不在 `NormalizedTokenUsage` 增加 `estimated_cost`；
2. CostEstimator 不使用 raw `cache_write_input_tokens`；
3. 不把 canonical `cache_write_tokens=None` 改成 `Some(0)`；
4. 不修改 canonical `uncached_input_tokens()` 的 nullable 语义；
5. reasoning token 不重复收费；
6. reasoning effort 不改变单位 Token 价格；
7. 不用 Session/User Turn/aggregate token 判断 272K；
8. 不对聚合 Token 重新定价；
9. 不把 272K 写死在 Codex parser/Adapter；
10. unknown model 不 fallback；
11. frontend 不保存价格、不计算价格；
12. DB/aggregate 不使用 `f64` 累加；
13. cost 不进入 event identity；
14. 不实现 OpenRouter/联网更新/用户 override；
15. 不为了让所有 UI 都有数字而猜价格。

## 45. 并行实施禁止项

1. 两个 Subagent 不得在同一 Wave 同时修改 `src/storage/migrations.rs`；
2. migration v7 由 B1 按 Wave 0 冻结 contract 一次性完成最终 schema，D1 不再二次修改；
3. `src/storage/usage.rs` 与 `src/usage/ledger.rs` 在 Wave 1 由 B1 ownership，Wave 2 明确顺序移交给 D1；不得并行编辑；
4. `src/usage/rebuild.rs` 的 reasoning-effort proof 由 B1 完成，D1 不接管；
5. 不得让 C1/D1 各自复制一套 cost formula；
6. 不得让 Metadata 与 Cost 建立业务依赖；
7. 不得让 Reasoning Effort 进入 Metadata title resolver；
8. 不得在并行分支自行改 Wave 0 冻结字段名并要求其他分支适配；
9. 发现未列出的“新增 struct 字段导致现有生产 struct literal 无法编译”时，允许最小补充 ownership，但必须只做编译/字段传播所需修改，并由 Integration Owner 记录；
10. 发现 shared-file 冲突时交 Integration Owner 统一处理，禁止后提交覆盖先提交。

---

# Part G — 最终生产结构

## 46. 三个功能的最终关系

```text
                                   Usagi 本版本
                                          │
            ┌─────────────────────────────┼─────────────────────────────┐
            │                             │                             │
            ▼                             ▼                             ▼
     Metadata Workstream          Usage Context Workstream         Cost Workstream
            │                             │                             │
 state/rollout agent_path          rollout turn_context             NormalizedTokenUsage
            │                     model + reasoning effort                   │
            ▼                             │                         model/time/granularity
     durable safe fact                    ▼                             │
            │                      UsageProcessor                        ▼
            ▼                             │                      PricingRepository
      title resolver                      ▼                             │
            │                       usage_events                         ▼
            ▼                  model + effort + tokens             ModelPricing
      threads.title                        │                             │
            │                             │                             ▼
            │                             └──────────────┐        CostEstimator
            │                                            │             │
            │                                            ▼             ▼
            │                                  event estimated_cost ◄───┘
            │                                            │
            └───────────────────────┬────────────────────┘
                                    ▼
                                Aggregate
                                    │
                       ┌────────────┴────────────┐
                       ▼                         ▼
             Main (model, effort)        Subagent single block
             usage + cost blocks         title + effort + usage + cost
                       │                         │
                       └────────────┬────────────┘
                                    ▼
                               Detail API
                                    │
                                    ▼
                              Session Drawer
```

三条职责边界：

```text
Metadata
→ “这个 Subagent 叫什么”

Usage Context
→ “这个 usage event 用了什么 model / reasoning effort”
→ 并保证 source/open Turn 在 checkpoint/restart/carry 后仍保留该 context

Cost
→ “这个 usage event 按该模型价格值多少钱”
```

不得交叉承担职责。

---

## 47. 实施完成后的生产行为

### 47.1 Subagent title

真实 Codex metadata：

```text
name = NULL
title = NULL
session_index = 无
agent_path = /root/gate_b_rereview
agent_role = subagent
```

最终：

```text
threads.title = Gate b rereview
```

Detail API / Drawer 直接使用 canonical title。

### 47.2 Main model + effort

假设 Main 历史 event 为：

```text
gpt-5.6-sol / high
gpt-5.6-sol / high
gpt-5.6-sol / medium
gpt-5.6-terra / max
```

Drawer 输出三个模型配置 block：

```text
gpt-5.6-sol (high)
gpt-5.6-sol (medium)
gpt-5.6-terra (max)
```

每个 block 独立聚合：

```text
Token
Reasoning Token
Cache
Estimated Cost
```

### 47.3 Subagent model + effort

典型单一 effort：

```text
gpt-5.6-luna (high)
```

缺失：

```text
gpt-5.6-luna (—)
```

同一 Subagent 查询范围存在多个 effort：

```text
gpt-5.6-luna (mixed)
```

仍只有一个 Subagent usage block。

### 47.4 Estimated cost

每个可计价 usage event：

```text
NormalizedTokenUsage
+
model pricing
+
request granularity
→ estimated_cost_nanos_usd
```

`reasoning_effort` 不进入公式。

上层只做 event cost 求和；Main Drawer 因 `(model, effort)` 分组，自然得到对应配置的独立费用。

当前：

```text
cache_write_tokens=None
→ CostEstimator 内部按 0 write 估算
→ canonical 继续保持 None
```

未来 Codex 返回真实 cache-write：

```text
raw cache_write_input_tokens
→ Adapter
→ cache_write_tokens=Some(...)
→ 现有 CostEstimator 自动计入 cache-write cost
```

---

## 48. 与测试标准文档的关系

本文只定义生产实施方案、数据口径、模块职责、迁移策略、版本关系、执行依赖和并行分工。

**本文不重复定义测试条目、测试用例或测试断言；Part E 仅引用 `Usagi_预估费用模块测试标准_v0.2.md` 中的测试 ID 作为施工 Gate。**

本版配套测试标准已经独立定义为 `Usagi_预估费用模块测试标准_v0.2.md`。测试标准以本文最终生产行为为依据，不得用历史“`estimated_cost` 恒为 null”或 Main “只按 model 分组”等旧断言反向约束生产实现。
