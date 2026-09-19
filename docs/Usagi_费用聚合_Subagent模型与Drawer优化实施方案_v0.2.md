# Usagi 费用聚合、Subagent 模型归属与 Drawer 优化实施方案

> 版本：v0.2  
> 日期：2026-08-13  
> 代码基线：用户本轮提供的最新 Usagi 代码快照（已完成扫描性能优化）  
> 前置已完成：`Usagi_扫描更新性能优化实施方案_v0.2.md` 对应实现  
> 测试标准：`Usagi_费用聚合_Subagent模型与Drawer优化测试标准_v0.1.md`。本文定义生产方案、施工 ownership、并行顺序与 Gate；正式测试 ID、优先级与完成门以该测试标准为唯一依据。

---

## 1. 本轮目标与范围

本轮同时处理三类已确认问题，并顺手完成一组 Session Drawer 小型 UI 调整：

1. 修复 `codex-auto-review` 无法计费：
   - 保留原始模型标识 `codex-auto-review`；
   - 仅在 pricing 解析层按 `gpt-5.6-luna` 的价格计算；
   - 升级 pricing catalog version，并自动重算历史费用。
2. 修复 subagent 首个 owning `turn_context` 被当作归属边界消费后，模型/推理深度没有进入 `UsageSourceState`，导致后续 Token 被写成 `unknown` 的解析 Bug。
3. 修改费用聚合语义：
   - 不再因为一个子项费用未知就把整个 Session / Dashboard 汇总费用置为 `null`；
   - 聚合结果必须同时表达“已知费用小计”和“费用完整性”；
   - Session 的部分费用用红色数字表示；
   - Dashboard KPI 不改数字颜色，而在「预估费用」标题同一行右上角显示红色圆圈叹号，点击显示说明气泡。
4. Session Drawer 小改动：
   - 顶部合计信息栏由 3 项改为 4 项，新增「合计费用」；
   - `Main` 标题后的模型配置数改为 `Main (x)`，左对齐紧靠 Main；
   - `Subagent` 数量改为 `Subagent (x)`，左对齐紧靠 Subagent；
   - 删除 Drawer 内全部「复制」按钮；
   - Subagent Header 中模型名称（推理深度）与最后活动时间移动到右侧，模型在上、时间在下。

本轮不重新设计扫描性能方案，不新增数据库业务表，不引入 dual-read/fallback，不对无法无损判断真实模型的事件强行猜测模型。

---

## 2. 当前代码基线核对结果

### 2.1 `gpt-5.6` 是 `gpt-5.6-sol` 的官方 alias

当前代码：

```text
src/cost/pricing.rs

GPT_5_6_SOL_ALIASES = ["gpt-5.6"]
```

该设置是正确的，应继续保留。

OpenAI 当前官方 GPT-5.6 Sol 模型页与模型指南均明确说明：

```text
gpt-5.6
→ routes to gpt-5.6-sol
```

因此这里不是 MU 自己发明的兼容别名，而是官方公开模型 alias。

本轮禁止删除：

```text
"gpt-5.6" -> "gpt-5.6-sol"
```

### 2.2 `codex-auto-review` 与上述官方 alias 不是同一类概念

`codex-auto-review` 是 Codex rollout 中出现的功能型模型标识，不应把数据库中所有该字符串直接重写成 `gpt-5.6-luna`。

本轮只建立 **pricing alias / pricing mapping**：

```text
usage_events.model = "codex-auto-review"
        ↓
PricingRepository.resolve(...)
        ↓
使用 GPT-5.6 Luna pricing
```

数据库和 UI 仍可保留：

```text
codex-auto-review
```

这样可以同时满足：

- 原始模型信息不被篡改；
- 费用能正确计算；
- 未来如果 Auto-review 的底层模型再次变化，只需要调整 pricing 映射版本，不需要回写历史模型名称。

OpenAI 当前 Codex changelog 已出现 Guardian / Auto-review 使用 Luna 的迁移记录，因此本轮按用户确认的产品规则将 `codex-auto-review` 计价映射到 Luna。

### 2.3 当前 `unknown` 的主要来源是 MU 自身状态丢失

当前 `src/usage/pipeline.rs::establish_ownership()` 在首次找到 owning 记录时：

```text
Owning turn_context
→ 用来建立 subagent 归属边界
→ 创建 UsageSourceState::default()
→ checkpoint 越过该行
```

但 owning `turn_context` 本身已经包含：

```text
model = gpt-5.6-sol / terra / luna
reasoning_effort = ...
```

当前代码没有把这条记录送入正常 `UsageProcessor`，因此：

```text
active_model = None
active_reasoning_effort = None
```

后续 Token 到达 `processor.rs` 时：

```text
active_model 为空
→ 使用 "unknown" 占位
```

这就是 Sol 调查出的 1,313 条错误 `unknown` 的根因。

### 2.4 另有 253 条属于原始记录顺序问题

这部分与上面的解析 Bug 必须分开：

```text
Token
Token
Token
↓
首个 turn_context(model=...)
```

此时 Token 确实先于模型上下文出现。

已调查结果：

- 143 条：后续上下文与线程元数据一致，可高可信推断；
- 110 条：线程存在 Luna/Sol 等模型切换，现有原始记录无法无损确定早期 Token 属于哪个模型。

**本轮不对这 253 条做启发式模型回填。**

原因：

1. 本轮优先修复 100% 可确定的 MU Bug；
2. 143 条虽然可高可信推断，但仍属于“推断规则”，不是原始记录直接证明；
3. 110 条无法无损恢复，强行归类会制造伪精确数据；
4. 未来如要做 unresolved model inference，应独立定义规则、可信度与测试标准。

因此本轮完成后，数据库仍可能存在少量真正“无法确定模型”的事件；费用聚合必须能够正确承受这种不完整数据，而不能再让整个 Session / Dashboard 失效。

---

## 3. 本轮冻结的费用语义

### 3.1 当前错误语义

当前 `TokenTotals` 的费用合并属于“全有或全无”：

```text
$1 + $2 + —
= —
```

`AggregateRow::into_totals()`、`detail_row()` 等路径也会在：

```text
cost_unknown_count > 0
```

时直接把：

```text
estimated_cost_nanos_usd = None
```

该语义本轮废弃。

### 3.2 新语义：已知费用小计 + 完整性

聚合必须区分三种用户可见状态：

| 状态 | 条件 | `estimated_cost` | UI 含义 |
|---|---|---:|---|
| `complete` | 全部参与事件费用已知，或范围内没有事件 | 已知完整值；空范围为 `0` | 完整费用 |
| `partial` | 同一聚合中既有已知费用也有未知费用 | 已知费用之和 | 费用不完整，但已有部分可计算 |
| `unknown` | 有事件，但全部费用都未知 | `null` | 无法给出任何费用数字 |

示例：

```text
A = $1
B = $2
C = —

聚合：
estimated_cost = $3
status = partial
```

```text
A = —
B = $2
C = $3

聚合：
estimated_cost = $5
status = partial
```

```text
A = —
B = —

聚合：
estimated_cost = null
status = unknown
```

```text
范围内没有 usage event

聚合：
estimated_cost = $0
status = complete
```

### 3.3 绝对禁止把未知费用简单当作 0

实现不得采用：

```text
NULL -> 0
然后直接 SUM
```

因为：

```text
— + —
```

不能显示成：

```text
$0.00
```

数据库中的 `NULL` 仍表示该事件费用无法确定；聚合层只允许在存在至少一条已知费用时显示“已知费用小计”。

---

## 4. 内部费用完整性模型

### 4.1 推荐新增 domain enum

在 `src/usage/aggregate.rs` 增加聚合费用完整性概念，建议：

```rust
pub enum CostCompleteness {
    Empty,
    Complete,
    Partial,
    Unknown,
}
```

其中：

- `Empty` 仅作为内部聚合中性状态；
- API 不暴露 `empty`；
- `Empty` 对外映射成 `complete + $0`。

这样可以解决 `TokenTotals::zero()` 作为累加器时的歧义。

### 4.2 为什么需要内部 `Empty`

如果只有：

```text
complete / partial / unknown
```

那么累加器初始值：

```text
TokenTotals::zero()
```

与真实的“存在已知 $0 事件”都可能表现成：

```text
complete + $0
```

随后与 `unknown` 合并时无法判断应该得到：

```text
unknown
```

还是：

```text
partial
```

因此内部需要 `Empty`，但不需要把它变成用户可见 API 状态。

### 4.3 合并规则

固定如下：

```text
Empty + X = X

Complete + Complete = Complete
Complete + Unknown  = Partial
Unknown  + Complete = Partial

Partial + Complete = Partial
Partial + Unknown  = Partial
Partial + Partial  = Partial

Unknown + Unknown = Unknown
```

费用数值规则：

```text
Complete -> 已知费用和
Partial  -> 已知费用和
Unknown  -> None
Empty    -> Some(0)
```

### 4.4 SQL 聚合结果转换

现有 SQL 已经同时拿到：

```text
SUM(estimated_cost_nanos_usd)
cost_unknown_count
COUNT(*)
```

因此无需 schema migration，也无需新增持久化字段。

统一建立 helper，禁止在多个函数中继续手写不同判断：

```text
cost_aggregate(sum, cost_unknown_count, event_count)
```

转换规则：

```text
event_count == 0
=> Empty + Some(0)

cost_unknown_count == 0
=> Complete + SUM

0 < cost_unknown_count < event_count
=> Partial + SUM

cost_unknown_count == event_count
=> Unknown + None
```

其中 SQLite 的 `SUM(nullable_column)` 会忽略 `NULL`；因此 partial 场景本来就能得到已知 subtotal，当前代码只是主动把它丢掉。本轮应保留该 subtotal。

---

## 5. `codex-auto-review` Pricing 修复

### 5.1 修改位置

主要文件：

```text
src/cost/pricing.rs
src/cost/mod.rs
src/storage/cost.rs       # 复用现有 reprice 机制，不另建第二套
```

### 5.2 Pricing mapping

当前：

```rust
const GPT_5_6_LUNA_ALIASES: &[&str] = &[];
```

修改为：

```rust
const GPT_5_6_LUNA_ALIASES: &[&str] = &["codex-auto-review"];
```

要求：

- `canonical_model_id` 仍为 `gpt-5.6-luna`；
- `codex-auto-review` 只作为 `PricingRepository` 的解析 alias；
- 不修改 `usage_events.model`；
- 不在 parser 层把模型文本改写为 Luna；
- 不影响模型筛选、模型展示、`models_used` 原始标识。

### 5.3 保留 `gpt-5.6` 官方 alias

必须继续保持：

```rust
const GPT_5_6_SOL_ALIASES: &[&str] = &["gpt-5.6"];
```

不得因为增加 `codex-auto-review` 而把两个 alias 机制混淆或互相覆盖。

### 5.4 Pricing catalog version 升级

当前：

```text
PRICING_CATALOG_VERSION = 1
```

本轮改为：

```text
PRICING_CATALOG_VERSION = 2
```

原因：现有 `refresh_usage_costs_if_needed()` 只有版本变化才会重算历史 `usage_events.estimated_cost_nanos_usd`。

如果只增加 alias 不升级 version：

```text
历史 codex-auto-review
estimated_cost_nanos_usd = NULL
```

会继续保持旧值，直到其他原因重建事件。

本轮必须通过 catalog version 升级使历史 active epoch 也能即时 reprice。

### 5.5 不升级 `COST_ALGORITHM_VERSION`

本轮费用公式本身没有变化：

```text
输入 × input rate
+ cached × cached rate
+ cache write × write rate
+ output × output rate
```

变化的是“模型 -> pricing rule”的 catalog 内容，因此：

```text
PRICING_CATALOG_VERSION: 1 -> 2
COST_ALGORITHM_VERSION: 保持 1
```

除非 Luna 施工时发现现有 estimator 本身存在独立公式 Bug，否则不得顺手升级算法版本。

---

## 6. Subagent owning `turn_context` 修复

### 6.1 修改原则

归属边界 `turn_context` 同时也是有效业务上下文。

不能再采用：

```text
用这行建立 ownership
然后丢弃这行的 model / reasoning_effort
```

必须改成：

```text
第一条 Owning TurnContext
        │
        ├─ 建立 ownership 边界
        ├─ 解析 model
        ├─ 解析 reasoning_effort
        ├─ 写入 UsageSourceState
        ├─ 写入对应 durable offset
        └─ checkpoint 才推进到该行之后
```

### 6.2 推荐实现方式：复用正常 processor，不手抄字段

修改：

```text
src/usage/pipeline.rs::establish_ownership()
```

当前对 owning boundary 直接构造：

```text
ProcessResult {
    updated_state: UsageSourceState::default(),
    ...
}
```

应改为：

1. 若 boundary 是 `SessionMeta`：
   - 仍允许空 state；
   - 不伪造模型。
2. 若 boundary 是 `TurnContext`：
   - 使用现有 `CodexRolloutParser` / `normalized_record()` 路径把该行转换成正常 `UsageRecord::TurnContext`；
   - 交给 `UsageProcessor` 处理；
   - 取 `result.updated_state` 作为 ownership commit 的初始 state；
   - 不产生 Token event；
   - 不重复消费该行。

推荐复用正常 processor 的原因：

- `model` 的空字符串过滤规则已经存在；
- `reasoning_effort` 的“缺失即清空”规则已经存在；
- 避免在 `pipeline.rs` 再手写第二套 turn_context 语义；
- 后续 TurnContext 新增上下文字段时更不容易再次出现 ownership boundary 漏同步。

### 6.3 durable offsets

若 owning boundary 的 `turn_context` 中存在模型：

```text
active_model_offset = boundary.start_offset
```

若存在 reasoning effort：

```text
active_reasoning_effort_offset = boundary.start_offset
```

若 reasoning effort 缺失：

```text
active_reasoning_effort = None
active_reasoning_effort_offset = None
```

继续遵守当前正常 TurnContext 的规则：缺失 reasoning effort 是明确上下文边界，不允许继承祖先/上一 Turn 的 effort。

### 6.4 不修改 processor 的 `unknown` 防御兜底

当前 processor 在真正没有 `active_model` 时仍需要有安全处理。

本轮不应简单删除：

```text
"unknown"
```

因为前述 253 条“Token 先于首个模型上下文”的原始数据仍可能进入该路径。

本轮目标是：

```text
不该 unknown 的 1,313 条不再 unknown
```

而不是：

```text
系统永远不允许出现 unresolved model
```

### 6.5 历史数据必须触发 Usage rebuild

只改 pipeline 无法修复数据库里已经写成 `unknown` 的历史事件。

当前代码已有 usage parser version / shadow rebuild 机制：

```text
USAGE_PARSER_VERSION = 4
```

本轮这个修复改变了 rollout 到 canonical usage event 的解析结果，因此应正式升级：

```text
USAGE_PARSER_VERSION: 4 -> 5
```

但本轮**不改变 Token 标准化/差分算法本身**，因此：

```text
USAGE_CANONICAL_ALGORITHM_VERSION: 保持 4
canonical_algorithm_for(5) -> Some(4)
```

Parser version 表示“同一原始 rollout 如何被解释并形成 canonical event”；canonical algorithm version 表示 Token 数值标准化/差分算法。此次 ownership boundary 上下文修复属于前者，不应为了数字相同而机械把两者一起升版。

利用现有 scanner / usage rebuild 链路：

```text
发现 active parser version != binary parser version
→ 建立 build epoch
→ 按 v5 重扫
→ 完成 shadow rebuild
→ 激活新 epoch
```

这样才能使历史 1,313 条错误 `unknown` 重新按原始 `turn_context` 解析为 Sol/Terra/Luna。

**禁止**通过 SQL 直接把历史 `unknown` 批量 UPDATE 成某个模型。真实模型应从 rollout 重新解析获得。

---

## 7. Parser v5 与 Pricing v2 的关系

这两个版本升级分别解决不同问题：

```text
USAGE_PARSER_VERSION 4 -> 5
解决：错误 unknown 模型 + reasoning context ownership boundary

PRICING_CATALOG_VERSION 1 -> 2
解决：codex-auto-review 的历史费用 NULL
```

两者不能互相替代。

### 7.1 为什么仍需要 Pricing v2 reprice

即使 parser v5 最终会重建 usage event，也不能依赖 rebuild 完成后才修复 `codex-auto-review` 费用。

现有 cost refresh 可以直接对 active epoch 做版本化重算：

```text
程序升级
→ pricing v2 reprice
→ 当前 active 数据先获得正确 auto-review 费用
→ parser v5 shadow rebuild 后新 epoch 天然继续使用同一 v2 pricing
```

该顺序允许费用修复与 parser rebuild 各自保持现有事务/epoch 语义。

### 7.2 不新增 schema migration

本轮：

- 不新增 `usage_events` 列；
- 不新增 `app_meta` 列；
- 不新增费用状态持久化列；
- 不新增 unresolved model 表。

费用完整性全部由当前事件的：

```text
estimated_cost_nanos_usd NULL / 非 NULL
```

在查询时派生。

---

## 8. 后端聚合改造

### 8.1 `TokenTotals`

修改：

```text
src/usage/aggregate.rs
```

`TokenTotals` 除现有：

```text
estimated_cost_nanos_usd: Option<i64>
```

外，增加内部费用完整性状态。

建议名称：

```text
cost_completeness
```

禁止复用 `quality_status`、`unknown_count` 等其他概念代替，因为 cache-write unknown、模型 unknown、费用 unknown 是不同维度。

### 8.2 `TokenTotals::zero()`

改成：

```text
estimated_cost_nanos_usd = Some(0)
cost_completeness = Empty
```

API 映射时：

```text
Empty -> complete
```

### 8.3 `TokenTotals::add_assign()`

Token 字段逻辑保持现状。

只重写费用合并：

```text
先根据左右 cost_completeness 计算新状态
再只累加已知 subtotal
```

不能再使用当前：

```rust
match (left_cost, right_cost) {
    (Some(left), Some(right)) => Some(left + right),
    _ => None,
}
```

因为该规则正是 Session Tree 只要一个子项未知就整体 `None` 的来源之一。

### 8.4 `AggregateRow::into_totals()`

当前：

```text
cost_unknown_count > 0
=> estimated_cost = None
```

改成统一 helper 计算 `Empty / Complete / Partial / Unknown`。

### 8.5 `detail_row()`

同样禁止继续：

```text
cost_unknown_count > 0 => None
```

Main model、Subagent、Detail 合计必须与 Summary 使用同一费用完整性规则。

### 8.6 Model 聚合与其他 aggregate paths

当前 `models()`、Session、Detail、Summary 都通过不同 SQL row 转换最终进入 `TokenTotals`。

Luna 必须一次性检索并清理所有：

```text
cost_unknown_count > 0 => None
```

的费用聚合判断，全部改成统一 helper。

不得只修 Dashboard query 或只修 Session query。

### 8.7 `same_totals()`

如果该函数用于判断聚合值相等，新增费用完整性后必须同时比较：

```text
estimated_cost_nanos_usd
cost_completeness
```

否则：

```text
$3 complete
$3 partial
```

会被错误认为完全相同。

---

## 9. API 契约

### 9.1 新增用户可见费用状态

后端 domain 内部允许存在 `Empty`，但 API 固定只暴露：

```text
complete
partial
unknown
```

建议 API 字段：

```text
estimated_cost_status
```

### 9.2 `TokenUsageDto`

由：

```text
estimated_cost: number | null
```

扩展为：

```text
estimated_cost: number | null
estimated_cost_status: "complete" | "partial" | "unknown"
```

### 9.3 `SummaryUsageDto`

同样增加：

```text
estimated_cost_status
```

### 9.4 API 状态映射

固定：

```text
Internal Empty    -> complete
Internal Complete -> complete
Internal Partial  -> partial
Internal Unknown  -> unknown
```

值与状态的合法组合：

| `estimated_cost` | status | 合法 |
|---|---|---|
| number | `complete` | 是 |
| number | `partial` | 是 |
| null | `unknown` | 是 |
| null | `complete` | 否 |
| null | `partial` | 否 |
| number | `unknown` | 否 |

前端 parser 应把非法组合视为 API contract error，而不是静默猜测。

### 9.5 不修改现有费用精度

后端仍保持：

```text
DB/domain: nanos USD integer
API: USD f64
frontend: $xx.xx
```

本轮只改变聚合完整性，不改变显示精度或货币单位。

---

## 10. Session 列表费用展示

修改：

```text
frontend/src/dashboard/session/SessionTableRow.tsx
frontend/src/data/types.ts
frontend/src/data/usagiClient.ts
frontend/src/index.css
```

### 10.1 complete

```text
estimated_cost = $5.00
status = complete
```

显示：

```text
$5.00
```

沿用当前正常费用样式。

### 10.2 partial

```text
estimated_cost = $3.00
status = partial
```

显示：

```text
$3.00
```

但费用数字使用红色。

**不增加 hover / title 文案：**

```text
部分费用无法估算，当前显示已知费用合计
```

用户已明确不要这项说明。

### 10.3 unknown

```text
estimated_cost = null
status = unknown
```

继续显示：

```text
—
```

不把 `—` 改成 `$0.00`。

### 10.4 建议 CSS 状态类

不要通过字符串文本判断是否 partial。

建议：

```text
.session-cost-cell.is-partial
```

由 DTO status 驱动。

---

## 11. Dashboard「预估费用」KPI

### 11.1 complete

保持现状：

```text
预估费用
$123.45
```

数字继续使用现有绿色费用样式。

### 11.2 partial

例如：

```text
estimated_cost = $120.00
estimated_cost_status = partial
```

显示要求：

```text
预估费用                         ⓘ/!
$120.00
```

其中：

- 数字仍保持当前绿色，不改红；
- 在 KPI 卡片顶部标题行的右侧显示红色圆圈叹号图标；
- 图标与「预估费用」文字在同一水平行；
- 图标位于卡片标题区域右侧，不挤压费用数字；
- 只在费用状态非 complete 时显示。

### 11.3 点击气泡

用户要求点击后出现气泡，不使用 hover-only tooltip。

partial 文案固定建议：

```text
有部分费用不完整
```

若 status 为 `unknown`，建议文案：

```text
当前费用无法完整估算
```

如产品希望只保留一条统一文案，也可全部使用：

```text
有费用无法完整估算
```

本文优先采用两态文案，避免全部未知时仍写“部分”。

### 11.4 交互要求

气泡至少支持：

- 点击图标打开；
- 再次点击关闭；
- 点击气泡外关闭；
- Escape 关闭；
- 图标使用 `button`，提供 `aria-label`；
- `aria-expanded` 反映打开状态；
- 不增加全局 toast。

### 11.5 组件边界

当前 `MetricCard` 只接收：

```text
label
value
updated
```

建议最小扩展为一个可选的右上角辅助信息，而不是在 `MetricCard` 内硬编码所有费用规则。

例如概念上：

```text
MetricCard
  label
  value
  updated
  notice?   # 仅 estimated_cost 传入
```

`MetricGrid` 根据：

```text
usage.estimated_cost_status
```

决定是否给「预估费用」卡片传 warning notice。

这样 `MetricCard` 仍是通用 KPI card。

---

## 12. Session Drawer：顶部合计栏

当前：

```text
合计 Token | Main | Subagent
```

本轮改成固定四项：

```text
合计 Token | Main | Subagent | 合计费用
```

其中：

```text
合计 Token = main.inclusive_usage.total_tokens
Main       = main.self_usage.total_tokens
Subagent   = inclusive.total_tokens - self.total_tokens
合计费用   = main.inclusive_usage.estimated_cost
```

### 12.1 合计费用完整性

Drawer 的「合计费用」属于 Session Tree 聚合费用，因此沿用 Session 部分费用视觉规则：

```text
complete -> 正常费用数字
partial  -> 红色费用数字
unknown  -> —
```

这里同样**不增加 hover 解释**。

Dashboard 的红色叹号规则只属于 KPI 卡片，不复制到 Drawer。

### 12.2 四列布局

桌面宽度下：

```text
repeat(4, minmax(0, 1fr))
```

不再让「合计 Token」占 1.25fr。

响应式沿用现有 Drawer 原则；窄宽度下允许 2×2，禁止横向溢出。

Skeleton summary 也从 3 个占位项同步改成 4 个。

---

## 13. Session Drawer：Main / Subagent 标题数量

### 13.1 Main

当前：

```text
Main                                      1 个模型配置
```

改为：

```text
Main (1)
```

要求：

- `(1)` 紧靠 Main；
- 整体左对齐；
- 不再显示「个模型配置」；
- 数字取 `detail.main.model_usage.length`。

概念 DOM：

```text
<h3>
  Main <span>(1)</span>
</h3>
```

或等价可访问结构。

### 13.2 Subagent

当前：

```text
Subagent                                  3
```

改为：

```text
Subagent (3)
```

同样左对齐紧靠标题。

数字取：

```text
detail.subagents.length
```

不新增另一套 subagent count 口径。

### 13.3 CSS

当前 `.session-detail-section-heading` 使用：

```text
justify-content: space-between
```

本轮改为左侧连续布局，建议：

```text
justify-content: flex-start
gap: 4px / 6px
```

具体像素允许 Luna 按现有视觉体系取最小一致值，但不得恢复右对齐。

---

## 14. Session Drawer：删除全部「复制」按钮

当前 Drawer 有至少两处：

1. Session ID 后「复制」；
2. 每个 Subagent ID 后「复制」。

本轮全部删除。

同步删除：

```text
copyText()
.session-detail-copy-button
相关 hover/focus CSS
对应测试断言
```

注意：

- Session ID 本身仍显示；
- Subagent thread ID 本身仍显示；
- 只删除复制按钮和已无引用的 clipboard helper；
- 不影响刷新、关闭按钮。

---

## 15. Session Drawer：Subagent Header 重排

### 15.1 当前结构

当前左侧 identity 内按类似以下顺序：

```text
标题
最后活动时间 + 模型（推理深度）
thread_id
                                  复制按钮
```

### 15.2 新结构

改成：

```text
[展开箭头]  [左侧 identity]              [右侧 meta]
             Subagent 标题                gpt-5.6-sol (medium)
             thread_id                    最后活动时间
```

右侧 meta：

```text
模型名称（推理深度）  # 上
最后活动时间          # 下
```

### 15.3 左侧

保留：

```text
标题
thread_id
```

删除原本左侧的：

```text
last_activity
model(reasoning_effort)
```

### 15.4 右侧

新增独立容器，建议：

```text
.session-detail-subagent-right-meta
```

要求：

- `flex: 0 0 auto`；
- 文本右对齐；
- 模型在上，时间在下；
- 模型允许合理换行但不能覆盖标题；
- 时间使用现有 `formatSessionTime()`；
- 模型继续使用 `formatModelWithReasoningEffort()`；
- `reasoning_effort_mixed` 语义不改。

### 15.5 窄宽度

在现有移动/窄 Drawer breakpoint 下：

- 优先保持标题和模型可读；
- 允许右侧 meta 向下一行 wrap；
- 不允许产生横向滚动；
- 展开按钮仍保持可点击。

---

## 16. 前端 DTO / Client 改造

修改：

```text
frontend/src/data/types.ts
frontend/src/data/usagiClient.ts
```

### 16.1 TypeScript 类型

新增：

```ts
type EstimatedCostStatus = "complete" | "partial" | "unknown";
```

并加入：

```ts
UsageDto.estimated_cost_status
SummaryUsageDto.estimated_cost_status
```

### 16.2 Client validation

解析时必须校验：

```text
estimated_cost_status ∈ complete|partial|unknown
```

并检查值/状态组合。

禁止：

```text
字段缺失时默认 complete
```

因为这会把旧 API 响应静默伪装成完整费用。

本轮前后端同步升级，不保留旧响应 fallback。

---

## 17. `formatCost()` 的职责

当前：

```text
formatCost(number | null)
```

继续只负责：

```text
number -> $xx.xx
null   -> —
```

不要把完整性状态塞进 `formatCost()`。

原因：

- Session 需要 partial -> 红色数字；
- Dashboard 需要 partial -> 数字仍绿色，但出现红色图标；
- Drawer Summary 需要 partial -> 红色数字；

因此 status 属于组件展示状态，不是纯格式化逻辑。

---

## 18. 受影响文件清单

### 18.1 后端核心

必改：

```text
src/cost/pricing.rs
src/cost/mod.rs
src/usage/normalized.rs
src/usage/pipeline.rs
src/usage/aggregate.rs
src/api/query.rs
```

高概率受影响：

```text
src/storage/cost.rs               # 主要为既有 version/reprice 测试确认
src/usage/ledger.rs               # 若公开 aggregate type 新增字段造成构造/映射调整
src/storage/mod.rs                # pricing refresh 相关现有测试 fixture
src/scanner/usage_consumer.rs     # 原则上不改生产逻辑，仅确认 parser version rebuild 自动生效
```

### 18.2 前端

必改：

```text
frontend/src/data/types.ts
frontend/src/data/usagiClient.ts
frontend/src/dashboard/MetricGrid.tsx
frontend/src/dashboard/MetricCard.tsx
frontend/src/dashboard/session/SessionTableRow.tsx
frontend/src/dashboard/session/SessionDetailDrawer.tsx
frontend/src/index.css
```

### 18.3 受影响现有测试文件

预计至少：

```text
src/cost/pricing.rs                 # module tests
src/usage/aggregate.rs              # module tests
src/api/query.rs                    # module tests
src/storage/mod.rs                  # reprice tests
src/usage/pipeline.rs               # ownership boundary tests
frontend/src/dashboard/MetricGrid.test.tsx
frontend/src/dashboard/MetricCard 相关测试（若新增）
frontend/src/dashboard/session/SessionDetailDrawer.test.tsx
frontend/src/dashboard/session/SessionSection.test.tsx / Row 相关测试
frontend/src/data client tests（按当前实际布局）
```

正式测试条目与 ID 已冻结在 `Usagi_费用聚合_Subagent模型与Drawer优化测试标准_v0.1.md`；本节仅列预期受影响测试落点，实际 Gate 以测试标准为准。

---

## 19. Luna 施工顺序：按 Workstream / Wave 并行

本轮不再使用原 v0.1 的 `S1 → S9` 线性编号作为实际调度模型。原因是其中部分步骤虽然画在同一层，但存在明确依赖：

```text
parser v5 trigger
依赖 owning TurnContext 修复已经成立

API estimated_cost_status
依赖 cost completeness aggregate 已冻结

Frontend DTO/client
依赖 API DTO 已冻结

Session/Dashboard/Drawer cost UI
依赖 Frontend DTO/client 已冻结
```

因此 v0.2 改为：

```text
后端三个独立底座并行
→ 历史升级 / Aggregate API 并行
→ Frontend contract 串行冻结
→ 两条 UI Track 并行
→ Integration only
```

### 19.1 Track ownership

#### Track A — Pricing / Reprice

生产 ownership：

```text
src/cost/pricing.rs
src/cost/mod.rs
cost refresh / reprice 的既有必要调用点
```

职责：

1. 保留 `gpt-5.6 -> gpt-5.6-sol` alias。
2. 增加 `codex-auto-review -> Luna pricing`。
3. `PRICING_CATALOG_VERSION: 1 -> 2`。
4. 保持 `COST_ALGORITHM_VERSION = 1`。
5. 验证历史 active usage reprice。
6. 不改 stored model。

测试 ownership：`T-MU04-A01～A02`。

#### Track B — Ownership / Parser v5

生产 ownership：

```text
src/usage/pipeline.rs
usage parser/version mapping 的必要文件
```

职责：

1. owning TurnContext 通过现有 parser/processor 初始化 state。
2. 保存 model / reasoning effort / offsets。
3. effort 缺失时按正常 TurnContext 规则清空。
4. 保留真正 unresolved 的 `unknown` 防御。
5. `USAGE_PARSER_VERSION: 4 -> 5`。
6. canonical algorithm 保持4，`canonical_algorithm_for(5)=Some(4)`。
7. 利用现有 shadow rebuild 修复历史错误 unknown。

测试 ownership：`T-MU04-B01～B03`。

除 correctness 阻塞外，Track B 不修改 scanner 性能优化主 orchestration、worklist/exact-plan 逻辑。

#### Track C — Cost completeness / Aggregate / API

生产 ownership：

```text
src/usage/aggregate.rs
src/api/query.rs
```

职责：

1. 建立内部 cost completeness state。
2. 重写 `TokenTotals::add_assign()` 费用语义。
3. 统一 SQL aggregate/detail completeness helper。
4. 清理所有旧 `cost_unknown_count > 0 => None` 语义。
5. `same_totals()` 纳入 completeness。
6. API 输出 `estimated_cost_status`。

测试 ownership：`T-MU04-C01～C02`，以及 `C03` 的后端部分。

#### Frontend Contract Owner — 唯一 API seam

只有 Track C 后端 DTO 冻结后才执行。

唯一 ownership：

```text
frontend/src/data/types.ts
frontend/src/data/usagiClient.ts
frontend/src/data/usagiClient.test.ts
```

职责：

1. 增加 `EstimatedCostStatus`。
2. 校验 complete/partial/unknown。
3. 校验 value/status 合法组合。
4. 禁止字段缺失默认 complete。
5. 禁止旧 API fallback。

该 seam 完成 `T-MU04-C03`，完成后 D/E UI Track 不再并行修改上述文件。

#### Track D — Session / Dashboard UI

生产 ownership：

```text
frontend/src/dashboard/MetricGrid.tsx
frontend/src/dashboard/MetricCard.tsx
frontend/src/dashboard/session/SessionTableRow.tsx
```

职责：

1. Session partial 数字红色；complete 正常；unknown `—`。
2. 不增加 Session partial hover/title 解释。
3. Dashboard partial 数字保持当前费用色。
4. Dashboard 标题右侧红色圆圈叹号。
5. click bubble / close / Escape / aria。

测试 ownership：`T-MU04-D01～D02`。

#### Track E — Drawer UI

生产 ownership：

```text
frontend/src/dashboard/session/SessionDetailDrawer.tsx
```

职责：

1. Summary 3项→4项，新增合计费用。
2. `Main (x)` / `Subagent (x)` 左对齐紧靠。
3. 删除全部复制按钮。
4. Subagent 左侧 title + thread_id。
5. 右侧 model(reasoning) 在上、last activity 在下。
6. 窄 Drawer 不横向溢出。

测试 ownership：`T-MU04-E01～E03`。

#### Frontend Integration Owner — shared CSS 单一写入者

`frontend/src/index.css` 是 D/E 的共享热点文件。

并行期间：

```text
Track D/E 可以冻结 class 名和样式需求
但不得同时修改相同 shared CSS 区域
```

最终由 Frontend Integration Owner 一次性合并 D/E 新增样式、清理 copy CSS，并执行前端 Gate。

#### Integration Owner

职责：

```text
T-MU04-F01/F02 跨模块 integration
全局 parser/pricing version 断言
确实冲突的历史 fixture 更新
Gate D 静态收口
最终真实数据核对
```

建议新建/独占本轮 integration test 文件，避免多个 Track 同时修改现有大型 integration 文件。

---

## 20. Luna 施工 + 测试 Gate 总图（v0.2）

正式测试 ID、优先级与命令以 `Usagi_费用聚合_Subagent模型与Drawer优化测试标准_v0.1.md` 为准。

### 20.1 总图

```text
Wave 1 — 后端底座（真正并行）

   Track A1: pricing alias
   Track B1: owning TurnContext fix
   Track C1: CostCompleteness state machine
          \        |        /
                 Gate A
                    |

Wave 2 — 历史升级 / 后端契约（并行）

   Track A2: pricing v2 reprice
   Track B2: parser v5 shadow rebuild
   Track C2: aggregate + API status
          \        |        /
          Backend contract frozen
                    |
       Frontend Contract Owner
       types + client + C03
                    |
                 Gate B
                    |

Wave 3 — UI（真正并行）

   Track D: Session + Dashboard
   Track E: Drawer
          \        /
   Frontend Integration Owner
       shared CSS merge
                    |
                 Gate C
                    |

Wave 4 — Integration only

   F01 cross-module fixture
   F02 parser4+pricing1 upgrade
   F03 full regression/static
   existing Performance Gate D
   real-data final check
                    |
                 Gate D
```

### 20.2 Gate A — 底层事实 Gate

必须证明四个局部事实：

```text
codex-auto-review 能按 Luna pricing resolve
gpt-5.6 官方 alias 未回退
owning TurnContext 不再丢 model / effort
cost completeness 状态机能表达 complete / partial / unknown / empty
```

正式条目：

```text
T-MU04-A01
T-MU04-B01
T-MU04-B02
T-MU04-C01
```

建议工程检查：

```bash
cargo fmt --check
cargo check --all-targets
# 对应 Track 的定向 unit/integration subset
```

Gate A 失败不得用 API/UI fallback 绕过。

### 20.3 Gate B — 历史升级 + API/Client 契约 Gate

Gate A PASS 后，A2/B2/C2 可并行施工。

Gate B 必须证明：

```text
pricing v2 能历史 reprice
parser v5 能历史 shadow rebuild
两套升级机制互不冒用版本/checkpoint
Session/Summary/Detail/Model 聚合共用新费用语义
API 输出 estimated_cost_status
Frontend client 只接受合法 value/status 组合
```

正式条目：

```text
T-MU04-A02
T-MU04-B03
T-MU04-C02
T-MU04-C03
复跑 Gate A
```

关键协调点：

```text
Track C 后端 DTO 冻结
→ Frontend Contract Owner 串行修改 types/client
→ C03 PASS
→ 才允许 D/E 两条 UI Track 开始
```

这样避免 Dashboard 与 Drawer 各自发明一套 partial 判定。

### 20.4 Gate C — UI Gate

Gate B 后并行：

```text
Track D = Session / Dashboard
Track E = Drawer
```

两条 Track 不共享主要组件文件；唯一共享热点 `index.css` 由 Frontend Integration Owner 合并。

正式条目：

```text
T-MU04-D01～D02
T-MU04-E01～E03
复跑 T-MU04-C03
```

并执行：

```bash
cd frontend
npm test
npm run check
npm run build
npm run test:browser:gate
```

Gate C 产品行为：

```text
Session partial = 红色已知 subtotal，无新增 hover说明
Dashboard partial = 原费用色数字 + 红色圆圈叹号 + 点击气泡
Drawer = 4项 summary + Main/Subagent (x) + 无复制按钮 + Subagent右侧meta
```

### 20.5 Gate D — Integration / Regression / Performance

Gate C 后不再开新的并行生产 Track；失败只退回对应 owner 修复。

正式条目：

```text
T-MU04-F01
T-MU04-F02
T-MU04-F03
Gate A/B/C 全部重跑
```

工程命令：

```bash
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
git diff --check

cd frontend
npm test
npm run check
npm run build
npm run test:browser:gate
```

还必须复跑现有：

```text
Usagi_扫描更新性能优化测试标准_v0.1.md — Gate D
```

包括真实 CODEX_HOME 三轮无变化 release 性能硬门槛。本文不重新定义阈值。

`clippy -D warnings` 若只剩用户未授权范围的既有 baseline warning：

```text
明确报告 Gate blocker
不得擅自扩大范围
不得移除 clippy Gate
不得降低 -D warnings 制造 PASS
```

### 20.6 最终真实数据核对

所有自动化 Gate PASS 后，核对用户当前真实数据：

1. owning TurnContext Bug 产生的错误 `unknown` 随 parser v5 rebuild 消失；
2. 真正 Token-before-context unresolved 允许继续存在，不要求全部 unknown=0；
3. `codex-auto-review` 获得 Luna 等价费用，stored model 不改名；
4. 原问题 Session 即使仍含真正 unknown cost，也显示已知 subtotal；
5. partial Session/Drawer 数字为红色；
6. Dashboard 数字不改红，标题右侧显示红色圆圈叹号并可点击说明；
7. Drawer 四项 summary、标题数量、copy 删除与 Subagent meta 新布局正确；
8. rebuild 后再次无变化扫描，性能优化 Gate 不回退。

### 20.7 为什么该并行方式优于 v0.1

v0.1 最大问题不是方案语义错误，而是把：

```text
S1/S2/S3
S4/S5/S6
S7/S8/S9
```

画成了过于对称的三层结构，容易让施工者误以为同层步骤都能直接并行。

v0.2 明确区分：

```text
可以并行的生产职责
vs
必须串行冻结的接口 seam
```

具体收益：

1. Pricing、Ownership、CostCompleteness 可从第一分钟并行，互不改同一核心文件。
2. parser v5 只在 ownership 修复后进入历史 rebuild，不会出现“先 bump version、后修语义”的半成品。
3. API DTO 只由 Track C 定义一次，Frontend client 只由单一 owner 接一次。
4. Dashboard 与 Drawer 真正并行，但不争用 `types.ts`、`usagiClient.ts`、`index.css`。
5. 历史 reprice 与 parser rebuild 在实现阶段独立，在 F02 才验证组合，避免两条升级机制被错误耦合。
6. Gate C 后停止新生产扩张，使最后的 performance/real-data 回归只处理确定失败，不再引入新变量。

---

## 21. Luna 施工约束

1. 不回退扫描性能优化代码。
2. 不改变当前 fixed-view / shadow rebuild / epoch 激活的核心机制来“简化”本次修复。
3. 不通过 SQL 猜测并批量修正 253 条原始顺序问题。
4. 不把所有 `unknown` 强制改成 `gpt-5.6-sol`、Luna 或线程最终模型。
5. 不把 `codex-auto-review` 持久化改名为 Luna。
6. 不删除 `gpt-5.6 -> gpt-5.6-sol` 官方 alias。
7. 不新增 schema 字段保存 `partial` 状态；费用完整性是聚合派生值。
8. 不把 unknown cost 直接按 0 参与用户可见完整总额。
9. 不在前端自行把 Session 子项费用重新求和，后端必须提供统一 truth source。
10. 不增加 Session partial 费用 hover 说明。
11. Dashboard partial 费用数字不改红，只显示右上角红色圆圈叹号。
12. 不保留旧 API 契约 fallback / dual-read。
13. 不删除或放宽既有正确测试断言来制造通过；因本轮明确契约变更而失效的断言应迁移到新语义。
14. 如果施工发现必须修改 scanner 主 orchestration，必须先确认是否存在 correctness 阻塞；不得为了方便扩大改动面。

---

## 22. 明确不在本轮范围

以下全部留待后续单独讨论：

1. 对 143 条“高可信可推断”早期 Token 自动回填模型；
2. 对 110 条模型切换线程中的早期 Token 猜测模型；
3. 将数据库字符串 `unknown` 重构为专门的 unresolved-model schema；
4. 给模型数据增加 confidence/provenance；
5. 改变模型筛选中是否显示 unresolved 模型；
6. 为 2026-07-30 之前的历史 Auto-review 单独补 GPT-5.4 时段定价；本轮按用户冻结规则，在当前 MU 支持范围内统一将 `codex-auto-review` 按 Luna 计价；
7. 改变价格公式本身；
8. 增加新的货币单位或费用精度；
9. 重做 Dashboard KPI 卡片布局；
10. 重做 Session Drawer 整体视觉设计；
11. 新增费用详情明细弹窗。

---

## 23. 完成判定

以下全部满足才视为本轮施工完成：

### 数据与解析

- [ ] `gpt-5.6` 仍解析到 `gpt-5.6-sol` pricing。
- [ ] `codex-auto-review` 解析到 Luna pricing，但 stored model 不被改名。
- [ ] `PRICING_CATALOG_VERSION = 2` 并能触发历史 reprice。
- [ ] owning `turn_context` 建立归属时同步保存 model。
- [ ] owning `turn_context` 同步保存/清空 reasoning effort。
- [ ] active model / effort offsets 与 boundary 一致。
- [ ] `USAGE_PARSER_VERSION = 5` 并通过现有 shadow rebuild 修复历史错误 unknown。
- [ ] `USAGE_CANONICAL_ALGORITHM_VERSION` 保持 4，`canonical_algorithm_for(5) = Some(4)`。
- [ ] 无 SQL 猜模型补丁。

### 费用聚合

- [ ] 全已知 -> `complete + 完整费用`。
- [ ] 部分未知 -> `partial + 已知费用小计`。
- [ ] 全未知 -> `unknown + null`。
- [ ] 空范围 -> `complete + 0`。
- [ ] Session、Summary、Detail、Model aggregation 共用统一规则。
- [ ] `same_totals()` 能区分同金额但完整性不同的结果。

### API

- [ ] `estimated_cost_status` 已进入 Token/Summary DTO。
- [ ] 前端 parser 严格校验 status。
- [ ] 无旧 API fallback。

### Session / Dashboard

- [ ] Session partial 合计费用显示红色数字。
- [ ] Session partial 数字无新增 hover 解释。
- [ ] Dashboard partial 数字继续使用当前费用颜色。
- [ ] Dashboard 标题右侧显示红色圆圈叹号。
- [ ] 点击叹号出现费用不完整说明气泡。
- [ ] complete 时不显示警告图标。

### Drawer

- [ ] 顶部 Summary 固定 4 项：合计 Token / Main / Subagent / 合计费用。
- [ ] Drawer partial 合计费用显示红色数字。
- [ ] `Main (x)` 左对齐紧靠。
- [ ] `Subagent (x)` 左对齐紧靠。
- [ ] 所有「复制」按钮已删除。
- [ ] 无残留 clipboard helper / copy CSS。
- [ ] Subagent 左侧为 title + thread_id。
- [ ] Subagent 右侧上方为模型（推理深度），下方为最后活动时间。
- [ ] 窄 Drawer 无横向溢出。

### 工程

- [ ] Gate A PASS。
- [ ] Gate B PASS。
- [ ] Gate C PASS。
- [ ] Gate D PASS。
- [ ] `T-MU04-A01～F03` 已按正式测试标准完成。
- [ ] 已完成真实数据最终核对。

---

## 24. 施工结论

本轮的核心不是“让所有费用都变得可计算”，而是同时完成两个正确性修复：

```text
能确定的模型 / 费用
→ 必须正确恢复并计算

确实无法确定的模型 / 费用
→ 必须保留 unknown，但不能污染整个上层聚合
```

最终数据链路应变成：

```text
Codex rollout
   │
   ├─ owning TurnContext 正确初始化 model / effort
   │      └─ parser v5 shadow rebuild 修复历史错误 unknown
   │
   ├─ codex-auto-review
   │      └─ pricing v2 -> Luna pricing
   │
   └─ usage_events.estimated_cost_nanos_usd
          │
          ├─ 全已知 -> complete total
          ├─ 部分未知 -> partial known subtotal
          └─ 全未知 -> unknown / —
                    │
                    ├─ Session：partial 数字红色
                    ├─ Drawer：partial 合计费用红色
                    └─ Dashboard：数字正常 + 红色警告图标
```

该方案保留真实数据的不确定性，同时避免单个未知事件继续把整个 Session Tree 或 Dashboard 的可用费用信息抹掉。
