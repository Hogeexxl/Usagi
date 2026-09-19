# Usagi Codex 本地数据口径文档

> 版本：v0.2  
> 状态：第一批字段及 Subagent 聚合口径已确定  
> 更新日期：2026-08-05  
> 数据范围：Codex 本地数据，不请求 Codex 服务端 Token 数据  
> 对应页面：Dashboard 汇总卡片、Session 用量与 Session 记录

---

## 1. 文档目标

本文件确定 Usagi（下文简称 MU）第一批数据字段的：

- 标准英文名；
- 标准中文显示名；
- 数据来源；
- 计算公式；
- 时间范围；
- Session 范围；
- 空值与异常处理；
- 字段之间的包含关系。

本版只确定：

1. Dashboard 汇总数据；
2. 按单个 Codex Thread 分组的 Session 数据。

“预估费用”仅预留字段，本版不定义价格来源和计算公式。

---

## 2. 第一版统一原则

### 2.1 Token 数据只来自本地

Token 数据只读取 Codex 本地会话记录中的 `token_count`：

```text
~/.codex/sessions/**/rollout-*.jsonl
~/.codex/archived_sessions/**/rollout-*.jsonl
```

主要原始对象：

```text
token_count.info.last_token_usage
token_count.info.total_token_usage
```

MU 第一版不通过 Codex app-server、Responses API 或其他服务端接口查询单个请求、Turn 或 Thread 的 Token 数据。

### 2.2 统计使用“请求增量”，不累加“累计快照”

每条 `token_count` 可能包含：

- `last_token_usage`：最近一次模型请求的用量；
- `total_token_usage`：当前 Thread 从开始到此刻的累计快照。

正式统计规则只有一套固定优先级：

1. **正常统计来源**：累计去重后的 `last_token_usage` 各字段；
2. **事件级缺失恢复**：只有当前 `token_count` 缺少 `last_token_usage`，且当前与上一条可信 `total_token_usage` 连续、各字段差值均非负时，才使用累计快照差值生成一条恢复事件；
3. **Turn 结束补偿**：Turn 结束时，用“结束累计快照 − 开始累计快照”与本 Turn 已统计增量比较；累计差值更大时只补两者之间的缺失部分，相等时不处理，累计差值更小时标记异常且不自动修正；
4. 同一段 Token 增量只能通过上述一种路径计入，`last_token_usage`、累计差值和 Turn 补偿不得重复累计；
5. 多条 `total_token_usage` 不得直接相加；它只用于去重、恢复、Turn 结束校验和程序重启后的状态恢复；
6. `last_token_usage.total_tokens` 与 `total_token_usage.total_tokens` 只用于一致性校验，不直接作为汇总统计来源；最终 `total_tokens` 始终由 `input_tokens + output_tokens` 计算。

### 2.3 所有数值字段保存原始整数

Token 数量在数据层保存整数，例如：

```json
{
  "input_tokens": 574300000
}
```

`574.3M` 只属于界面格式化结果，不能作为存储值或接口值。

### 2.4 百分比在数据层使用 0～1

例如缓存命中率 97%：

```json
{
  "cache_hit_rate": 0.97
}
```

界面负责显示成：

```text
97%
```

### 2.5 顶部时间筛选同时作用于两部分

Dashboard 的：

- 汇总卡片；
- 模型用量；
- Session 用量；
- Session 记录；

使用同一个时间范围。

第一版前端 Session 列表只展示用户创建的主 Session，不把 Subagent 作为独立行展示。Session 表中的 Token 数值表示：

> 该主 Session 在当前所选时间范围内产生的全部用量，包括主 Thread 自身以及所有层级后代 Subagent 的新增用量。

它不是主 Thread 单独产生的用量，也不是该 Session 从创建以来的终身累计值。

这样可以保证：

```text
当前时间范围内所有根 Session 行的 Input 之和
= 顶部汇总 Input
```

后台仍按原始 Thread 保存主 Agent 与各 Subagent 的自身用量，为后续 Session 详情页展示 Subagent 明细做准备；当前列表读取的是聚合后的 Session 包含用量。

---

## 3. 标准术语与界面命名

设计稿中的部分名称容易与原始字段混淆，本版统一如下。

| 设计稿或旧称 | 标准中文名 | 标准英文名 | 说明 |
|---|---|---|---|
| 总 Token | 总 Token | `total_tokens` | Input 与 Output 的合计 |
| 写入 Token | **输入 Token** | `input_tokens` | 必须改名；“写入”容易与 Cache Write 混淆 |
| 输出 Token | 输出 Token | `output_tokens` | 包含 Reasoning Output |
| 推理 Token | 推理 Token | `reasoning_tokens` | 是 Output 的子集 |
| 缓存读取 Token | 缓存读取 Token | `cached_tokens` | 输入中实际从 Prompt Cache 读取的部分 |
| 缓存写入 Token | 缓存写入 Token | `cache_write_tokens` | 输入中用于建立缓存的部分；可空 |
| 缓存命中率 | 缓存命中率 | `cache_hit_rate` | Cached Input ÷ Input |
| 会话数量 | 会话数量 | `session_count` | 当前范围内有有效用量的不同根 Session 数量；Subagent 不单独计数 |
| 上次更新 | **最后活动时间** | `last_activity_at` | 当前范围内主 Thread 或任一后代 Subagent 的最后有效活动时间 |
| 模型 | 使用模型 | `models_used` | 主 Agent 与全部后代 Subagent 在当前范围内使用过的全部模型 |

### 3.1 设计稿需要调整的文案

建议修改：

1. `写入 Token` → `输入 Token`
2. `缓存输出 Token` / `缓存输入 Token` → `缓存读取 Token`
3. Session 表中重复出现的第二个 `输出 Token` → `推理 Token`
4. “上次更新”建议统一为“最后活动时间”

---

## 4. Token 字段的包含关系

必须遵守以下关系：

```text
Total Tokens
├─ Input Tokens
│  ├─ Cached Tokens（缓存读取）
│  ├─ Cache Write Tokens（可空）
│  └─ Uncached Input Tokens（cache-write 已知时）
└─ Output Tokens
   ├─ Reasoning Tokens
   └─ Other Output Tokens
```

也就是：

```text
Total Tokens = Input Tokens + Output Tokens
```

```text
Uncached Input Tokens
= Input Tokens - Cached Tokens - Cache Write Tokens
（cache-write 未知时为 null）
```

```text
Other Output Tokens
= Output Tokens - Reasoning Tokens
```

注意：

- `cached_tokens` 已包含在 `input_tokens` 中；
- `cache_write_tokens` 已包含在 `input_tokens` 中；
- `reasoning_tokens` 已包含在 `output_tokens` 中；
- 计算总量时不能再次把它们加进去。

错误示例：

```text
错误 Total
= Input + Output + Cached Input + Cache Write + Reasoning
```

正确示例：

```text
正确 Total
= Input + Output
```

---

## 5. 时间范围口径

所有时间范围都使用运行 MU 的电脑当前本地时区。

数据事件以本地 JSONL 记录中的事件时间为准。

优先级：

1. `token_count` 记录自身的时间戳；
2. JSONL 外层记录时间戳；
3. 两者均不存在时，不允许使用文件修改时间冒充精确用量时间。

时间区间统一采用：

```text
[start, end)
```

即包含开始时间，不包含结束时间。

| 范围键 | 中文名 | 开始时间 | 结束时间 |
|---|---|---|---|
| `today` | 今天 | 今天 00:00:00 | 明天 00:00:00 |
| `yesterday` | 昨天 | 昨天 00:00:00 | 今天 00:00:00 |
| `week` | 本周 | 本周一 00:00:00 | 下周一 00:00:00 |
| `month` | 30d | 本月 1 日 00:00:00 | 下个月 1 日 00:00:00 |
| `year` | 今年 | 本年 1 月 1 日 00:00:00 | 明年 1 月 1 日 00:00:00 |

补充规则：

- 一周从星期一开始；
- 跨越多个日期的 Session，按每次 Token 事件实际发生时间拆分；
- 同一个 Session 可以同时为本周、本月和今年贡献数据；
- 正在运行中的 Session，已产生且可信的 Token 可以计入当前范围；
- 时间筛选不按照 Session 创建时间或文件修改时间统计 Token。

---

## 6. 统一的有效用量事件

为了让汇总数据和 Session 数据使用完全相同的口径，MU 内部先生成统一的“有效用量事件”。

建议内部结构：

```json
{
  "event_id": "内部去重标识",
  "occurred_at": "2026-08-05T05:20:00+08:00",
  "thread_id": "产生本次用量的原始 Thread ID",
  "parent_thread_id": "直接父 Thread ID；主 Thread 为 null",
  "root_session_id": "最上层主 Thread ID",
  "agent_role": "main 或 subagent",
  "model": "gpt-5.6",
  "input_tokens": 10000,
  "cached_tokens": 7000,
  "cache_write_tokens": 1000,
  "output_tokens": 1200,
  "reasoning_tokens": 800,
  "total_tokens": 11200
}
```

每个有效用量事件代表一次模型请求的可信增量。Token 原始归属始终保留在 `thread_id`；前端 Session 聚合使用 `root_session_id`。

### 6.1 获取与统计顺序

每条 `token_count` 按以下顺序处理，不允许在实现时自行选择来源：

1. 读取当前 `total_token_usage`，与上一条可信累计快照比较，先判断是否为重复快照；
2. 如果累计快照重复：
   - 不累计当前 `last_token_usage`；
   - 只更新与 Token 无关且确实发生变化的附加状态；
3. 如果累计快照不重复，且 `last_token_usage` 存在：
   - 只使用去重后的 `last_token_usage` 生成正常有效用量事件；
   - 当前累计快照差值只用于校验，不再作为第二份增量加入；
4. 如果累计快照不重复，但 `last_token_usage` 缺失：
   - 仅在上一条与当前 `total_token_usage` 均可信、属于同一累计链、各字段差值均非负时，使用分字段差值生成一条恢复事件；
   - 任一条件不满足时，不生成用量事件，并标记数据异常；
5. 保存当前可信 `total_token_usage`，作为下一次去重、缺失恢复和 Turn 结束校验的基线；
6. Turn 结束时执行一次一致性校验：
   - `Turn 累计差值 = Turn 已统计增量`：校验通过，不新增数据；
   - `Turn 累计差值 > Turn 已统计增量`：只生成数值为“两者差额”的补偿事件；
   - `Turn 累计差值 < Turn 已统计增量`：说明可能存在重复、累计重置或历史重放，标记异常，不自动扣减。

Turn 结束校验只使用已经读取的开始与结束累计快照做减法和比较，不会重新扫描文件，也不会把完整累计差值再次加入。

### 6.2 去重范围

以下情况不能重复计数：

- 连续出现完全相同的累计快照；
- 同一 Thread 同时存在于 `sessions` 和 `archived_sessions`；
- 文件被移动、重命名或归档；
- fork 或 subagent 开头重放父 Thread 历史；
- 文件重新扫描时重复读取已处理区域；
- 同一个物理事件被多个索引入口发现。

### 6.3 主 Thread、Subagent 与根 Session 关系

第一版必须解析并保存 Subagent 的父子关系。

标准字段：

| 字段 | 含义 |
|---|---|
| `thread_id` | 当前原始 Thread 的 ID |
| `parent_thread_id` | 当前 Thread 的直接父 Thread ID；主 Thread 为 `null` |
| `root_session_id` | 沿父链向上追溯得到的最上层主 Thread ID |
| `agent_role` | `main` 或 `subagent` |

关系规则：

1. 用户创建的顶层 Thread 是主 Thread：
   - `thread_id = root_session_id`
   - `parent_thread_id = null`
   - `agent_role = main`
2. Subagent 使用自己的 `thread_id`，并记录直接父 Thread：
   - `parent_thread_id != null`
   - `agent_role = subagent`
3. 多层 Subagent 全部归入同一个最上层 `root_session_id`；
4. Subagent rollout 中复制的父历史必须先去重，只有 Subagent 创建后真正新增的用量才能计入；
5. 同一个有效用量事件只属于一个 `thread_id` 和一个 `root_session_id`；
6. 当前前端列表不为 Subagent 单独生成 Session 行；
7. 后台必须保留每个 Thread 的自身用量，以便后续详情页拆分展示；
8. 父子关系无法解析时标记数据异常，不把该 Subagent 错当成独立主 Session。

### 6.4 异常值

如果出现以下情况，不能直接修正为 0：

- Token 差值为负；
- `cached_tokens > input_tokens`；
- `cached_tokens + cache_write_tokens > input_tokens`（cache-write 已知时）；
- `reasoning_tokens > output_tokens`；
- 原始结构与当前模型规则不匹配。

处理方式：

1. 将该事件标记为数据异常；
2. 不纳入受影响的派生比例；
3. 保留原始记录位置用于调试；
4. 不静默截断成 0。

---

# 第一部分：汇总数据

## 7. 汇总数据的统一范围

汇总数据表示：

> 当前所选时间范围内，所有有效用量事件跨 Thread 汇总后的结果。

汇总不是：

- 所有 Thread 的最新 `total_token_usage` 相加；
- 各 Session 百分比的简单平均；
- 按 Session 创建时间筛选整个 Session 总量。

统一计算方式：

```text
先筛选时间范围内的有效用量事件
→ 再按字段求和
→ 最后计算比例和 Session 数量
```

---

## 8. 汇总字段定义

### 8.1 累计总 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `total_tokens` |
| 标准中文名 | 累计总 Token |
| 数据类型 | 非负整数 |
| 来源类型 | MU 计算 |
| 公式 | `input_tokens + output_tokens` |
| 唯一统计公式 | `input_tokens + output_tokens` |
| 一致性校验字段 | `last_token_usage.total_tokens`、`total_token_usage.total_tokens` |
| 界面示例 | `574.3M` |

计算：

```text
累计总 Token
= Σ有效事件 Input
+ Σ有效事件 Output
```

为了保证字段关系稳定，MU 只以 `Input + Output` 作为最终结果。

原始 `last_token_usage.total_tokens` 用于校验单次请求是否满足：

```text
last_token_usage.total_tokens
= last_token_usage.input_tokens
+ last_token_usage.output_tokens
```

`total_token_usage.total_tokens` 用于校验累计快照是否满足相同的字段关系，以及 Turn 开始、结束累计差值是否与已统计增量一致。校验只做减法和比较，不会产生第二份正常统计数据；只有确认存在缺失增量时，才按第 6.1 节的规则补入差额。

---

### 8.2 累计输入 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `input_tokens` |
| 标准中文名 | 累计输入 Token |
| 数据类型 | 非负整数 |
| 来源类型 | 有效用量事件汇总 |
| 正常统计来源 | 去重后的 `last_token_usage.input_tokens` |
| 缺失恢复规则 | 仅当 `last_token_usage` 缺失且累计链可信时，使用相邻 `total_token_usage.input_tokens` 的非负差值；不得与正常来源重复累计 |
| 界面旧称 | 写入 Token，必须改名 |

计算：

```text
累计输入 Token
= Σ有效事件 input_tokens
```

“有效事件”已经在第 6.1 节确定来源：正常事件来自去重后的 `last_token_usage`；只有缺失恢复事件和 Turn 补偿事件使用累计差值。汇总层不再判断或切换来源。

Input 包含：

- 缓存输入；
- 缓存写入；
- 未缓存输入。

因此 Cached Input 和 Cache Write 不能在总 Token 中再次相加。

---

### 8.3 累计输出 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `output_tokens` |
| 标准中文名 | 累计输出 Token |
| 数据类型 | 非负整数 |
| 来源类型 | 有效用量事件汇总 |
| 正常统计来源 | 去重后的 `last_token_usage.output_tokens` |
| 缺失恢复规则 | 仅当 `last_token_usage` 缺失且累计链可信时，使用相邻 `total_token_usage.output_tokens` 的非负差值；不得与正常来源重复累计 |

计算：

```text
累计输出 Token
= Σ有效事件 output_tokens
```

`output_tokens` 已包含 `reasoning_tokens`。

---

### 8.4 累计推理 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `reasoning_tokens` |
| 标准中文名 | 累计推理 Token |
| 数据类型 | 非负整数 |
| 来源类型 | 有效用量事件汇总 |
| 正常统计来源 | Adapter 后的 `reasoning_tokens` |
| 缺失恢复规则 | 仅当 `last_token_usage` 缺失且累计链可信时，使用相邻 canonical snapshot 的非负差值；不得与正常来源重复累计 |

计算：

```text
累计推理 Token
= Σ有效事件 reasoning_tokens
```

关系：

```text
Reasoning ≤ Output
```

Reasoning 是 Output 的子集，不是额外的一类总量。

---

### 8.5 累计缓存读取 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `cached_tokens` |
| 标准中文名 | 累计缓存读取 Token |
| 数据类型 | 非负整数 |
| 来源类型 | 有效用量事件汇总 |
| 正常统计来源 | Adapter 后的 `cached_tokens` |
| 缺失恢复规则 | 仅当 `last_token_usage` 缺失且累计链可信时，使用相邻 canonical snapshot 的非负差值；不得与正常来源重复累计 |
| 界面含义 | 已命中现有缓存、无需重新处理的输入 Token |

计算：

```text
累计缓存读取 Token
= Σ有效事件 cached_tokens
```

它是 `input_tokens` 的子集。

---

### 8.6 累计缓存写入 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `cache_write_tokens` |
| 标准中文名 | 累计缓存写入 Token |
| 数据类型 | 非负整数或 `null` |
| 来源类型 | 有效用量事件汇总 |
| 正常统计来源 | Adapter 后的 `cache_write_tokens` |
| 缺失恢复规则 | 仅当 `last_token_usage` 缺失且累计链可信时，使用相邻 canonical snapshot 的非负差值；任一端未知时结果保持 `null` |
| 界面含义 | 本次输入中用于建立或更新缓存的 Token |

计算：

```text
累计缓存写入 Token
= Σ有效事件 cache_write_tokens（任一未知时为 null）
```

缺失处理：

- 原始字段明确为 `0`：canonical 结果为 `Some(0)`；
- 原始字段缺失：canonical 结果为 `None`，不得按模型名推断 0；
- 聚合范围内任一事件为 `None`：范围 cache-write 与 uncached input 均为 `null`，界面显示 `—`。

---

### 8.7 累计缓存命中率

| 项目 | 定义 |
|---|---|
| 标准英文名 | `cache_hit_rate` |
| 标准中文名 | 累计缓存命中率 |
| 数据类型 | `0～1` 小数或 `null` |
| 来源类型 | MU 计算 |
| 公式 | `cached_tokens ÷ input_tokens` |
| 界面格式 | 百分比 |

计算：

```text
累计缓存命中率
= 当前范围累计缓存读取 Token
÷ 当前范围累计输入 Token
```

不是：

```text
各 Session 缓存命中率的算术平均
```

也不是：

```text
(Cached Input + Cache Write) ÷ Input
```

Cache Write 表示建立缓存，不属于“命中”。

当：

```text
input_tokens = 0
```

返回：

```json
{
  "cache_hit_rate": null
}
```

界面显示：

```text
—
```

不能显示 `0%`，因为没有输入不等于缓存命中率为零。

---

### 8.8 会话数量

| 项目 | 定义 |
|---|---|
| 标准英文名 | `session_count` |
| 标准中文名 | 会话数量 |
| 数据类型 | 非负整数 |
| 来源类型 | MU 计算 |
| 去重键 | `root_session_id` |

计算：

```text
会话数量
= 当前时间范围内至少包含一条有效用量事件的不同 root_session_id 数量
```

规则：

- 一个主 Thread 及其全部后代 Subagent 合计只算 1 个 Session；
- 同一根 Session 在范围内产生多次请求，只计 1 个 Session；
- 同一根 Session 横跨多个日期，在各自命中的范围中分别计 1 个；
- `sessions` 与 `archived_sessions` 中的同一原始 Thread 不重复计数；
- 只有元数据、没有有效 Token 用量的根 Session 不计入用量会话数量；
- Subagent 不作为独立 Session 计数；
- 只有历史重放、没有新增用量的 Subagent 不增加用量，也不增加会话数量。

---

### 8.9 预估费用

| 项目 | 定义 |
|---|---|
| 标准英文名 | `estimated_cost` |
| 标准中文名 | 预估费用 |
| 当前状态 | 占位 |
| 当前值 | `null` |
| 本版计算 | 不计算 |

本版接口可保留：

```json
{
  "estimated_cost": null
}
```

界面可隐藏或显示 `—`。

---

## 9. 汇总数据建议结构

```json
{
  "period": "today",
  "period_start_at": "2026-08-05T00:00:00+08:00",
  "period_end_at": "2026-08-05T05:30:00+08:00",
  "total_tokens": 574300000,
  "input_tokens": 550000000,
  "cached_tokens": 520000000,
  "cache_write_tokens": 10000000,
  "uncached_input_tokens": 20000000,
  "output_tokens": 24300000,
  "reasoning_tokens": 18100000,
  "other_output_tokens": 6200000,
  "cache_hit_rate": 0.9454545,
  "session_count": 218,
  "estimated_cost": null
}
```

---

# 第二部分：Session 数据

## 10. Session 数据的统一范围

第一版中的 Session 表示：

> 以用户创建的主 Thread 为根，包含该主 Thread 以及所有层级后代 Subagent 的完整 Agent 工作流。

前端 Session 列表只为 `root_session_id` 生成一行，不为 Subagent 单独生成行。

当前所选时间范围内的处理顺序：

```text
筛选时间范围内的有效用量事件
→ 按 thread_id 保留每个原始 Thread 的自身用量
→ 按 root_session_id 汇总主 Thread 与全部后代 Subagent
→ 每个 root_session_id 生成一行
```

因此，Session 层同时保留三个口径：

| 内部口径 | 建议英文名 | 含义 | 当前列表是否直接展示 |
|---|---|---|---:|
| 主 Thread 自身用量 | `self_usage` | 根 Thread 自己产生的用量 | 否 |
| Subagent 用量 | `subagent_usage` | 全部后代 Subagent 真正新增的用量之和 | 否 |
| Session 包含用量 | `inclusive_usage` | `self_usage + subagent_usage` | 是 |

当前 Session 行中的扁平字段：

```text
total_tokens
input_tokens
cached_tokens
cache_write_tokens
uncached_input_tokens
output_tokens
reasoning_tokens
other_output_tokens
cache_hit_rate
```

全部表示 `inclusive_usage`，即主 Agent 与全部后代 Subagent 在当前时间范围内的合计。

一致性关系：

```text
Σ各根 Session 的 inclusive_usage.total_tokens
= 汇总 total_tokens
```

```text
Σ各根 Session 的 inclusive_usage.input_tokens
= 汇总 input_tokens
```

```text
Σ各根 Session 的 inclusive_usage.output_tokens
= 汇总 output_tokens
```

Session 表不是直接展示任一 Thread 最新的 `total_token_usage`，也不是只展示主 Thread 自身用量。

---

## 11. Session 元数据字段

### 11.1 最后活动时间

| 项目 | 定义 |
|---|---|
| 标准英文名 | `last_activity_at` |
| 标准中文名 | 最后活动时间 |
| 数据类型 | 带时区的时间 |
| 来源类型 | 本地事件时间 |
| 当前范围规则 | 当前所选范围内，主 Thread 或任一后代 Subagent 的最后一条有效活动时间 |

计算规则：

1. 收集该 `root_session_id` 下主 Thread 与全部后代 Subagent 在当前范围内的有效活动时间；
2. `last_activity_at` 取其中最大值；
3. 若展示了无 Token 的状态行，可使用对应工作流最后一条生命周期事件时间；
4. 不使用归档移动造成的文件修改时间冒充用户活动时间。

因此，Subagent 在主 Agent 之后继续工作时，Session 行的最后活动时间应随之更新。

建议界面名称从“上次更新”改为“最后活动”。

---

### 11.2 Session ID

| 项目 | 定义 |
|---|---|
| 标准英文名 | `session_id` |
| 标准中文名 | Session ID |
| 数据类型 | 字符串 |
| 来源类型 | 本地原始字段 |
| 第一版取值 | `root_session_id`，即主 Thread 的 `session_meta.id` |
| Thread 级主键 | `thread_id` |

规则：

- 前端 Session 与原始 Thread 不再是完全相同的统计对象；
- `session_id` 固定等于主 Thread ID，用于当前列表的一行和整个工作流聚合；
- 主 Thread 自身满足 `thread_id = session_id`；
- Subagent 保留自己的 `thread_id`，但其 `root_session_id = session_id`；
- 不能使用文件路径作为 Thread 或 Session 主键；
- 普通目录和归档目录中的同一原始 Thread 必须合并；
- 若本地元数据中的 Thread ID 发生冲突，应标记数据异常。

---

### 11.3 标题

| 项目 | 定义 |
|---|---|
| 标准英文名 | `title` |
| 标准中文名 | 标题 |
| 数据类型 | 字符串 |
| 来源类型 | Codex 本地元数据 |
| 是否读取对话正文生成 | 否 |

来源优先级：

1. Codex 主状态索引中的标题或名称；
2. `session_index.jsonl` 中对应 Thread 的名称；
3. 无标题时显示 `未命名 Session`；
4. 可在界面附加短 ID，例如 `未命名 Session · a1b2c3d4`。

MU 不读取首条用户消息来自行生成标题。

标题只读取主 Thread 的元数据。Subagent 的标题、任务名或提示词不写入当前 Session 行，也不覆盖主 Session 标题。

---

### 11.4 所属项目

推荐同时保留两个内部字段：

| 英文字段 | 中文名 | 含义 |
|---|---|---|
| `project_name` | 所属项目 | 界面展示名称 |
| `project_path` | 项目路径 | 本地规范化路径 |

主要来源：

```text
session_meta 中的 cwd / working directory
```

备用来源：

```text
Turn 上下文中的工作目录
```

计算：

```text
project_path = 规范化后的工作目录
project_name = project_path 最后一级目录名
```

示例：

```text
project_path = <repo>
project_name = usagi
```

第一版规则：

- 只使用主 Thread 的初始工作目录确定所属项目；
- Subagent 自己的工作目录不覆盖当前 Session 的 `project_name` 和 `project_path`；
- 路径仅用于本机内部，不上传；
- 没有工作目录时：
  - `project_path = null`
  - `project_name = "未识别项目"`

后续可以增加 Git 仓库根目录识别，但不属于本版已确定口径。

---

### 11.5 Session 内使用模型

| 项目 | 定义 |
|---|---|
| 标准英文名 | `models_used` |
| 标准中文名 | 使用模型 |
| 数据类型 | 字符串数组 |
| 来源类型 | 本地原始上下文 |
| 主要来源 | 各 Thread 的 `turn_context.model` |
| 范围 | 当前时间范围内，主 Agent 与全部后代 Subagent 实际产生有效用量时使用的全部模型 |

一个 Session 工作流可能由主 Agent 与多个 Subagent 使用不同模型，也可能在同一 Thread 内切换模型，因此标准字段不能只保存一个字符串。

示例：

```json
{
  "models_used": [
    "gpt-5.6",
    "gpt-5.6-mini"
  ]
}
```

归集规则：

1. 收集该 `root_session_id` 下主 Thread 与全部后代 Subagent 的有效用量事件；
2. 使用事件发生时对应 `turn_context.model`；
3. 相同模型只保留一次；
4. 保持模型第一次在当前范围中产生有效用量的顺序；
5. Subagent 使用的模型必须包含在 `models_used` 中；
6. 无法确认模型时使用 `unknown`，不能猜测。

标题和项目仍只来自主 Thread；只有模型字段需要合并主 Agent 与 Subagent。

界面展示建议：

- 只有一个模型：显示模型名；
- 多个模型：显示主 Agent 模型，并附加 `+N`；
- 详情或悬浮层展示完整模型列表；
- 后续详情页可再标记每个模型由主 Agent 还是哪些 Subagent 使用。

---

## 12. Session Token 字段

Session Token 字段与汇总字段使用同一套名称、公式和第 6.1 节的固定来源优先级。

处理顺序固定为：

```text
先按 thread_id 计算每个原始 Thread 的自身新增用量
→ 去除 Subagent 文件中复制的父历史
→ 再按 root_session_id 汇总主 Thread 与全部后代 Subagent
```

当前 Session 行中的所有 Token 字段均为 `inclusive_usage`。Session 汇总层只累加已经生成的有效用量事件，不再直接读取或选择 `last_token_usage` 与累计快照差值。

### 12.1 总 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `total_tokens` |
| 标准中文名 | 总 Token |
| 范围 | 当前时间范围内、当前根 Session（主 Agent + 全部后代 Subagent） |
| 公式 | `input_tokens + output_tokens` |

```text
Session 总 Token
= 主 Agent 与全部后代 Subagent 的 ΣInput
+ 主 Agent 与全部后代 Subagent 的 ΣOutput
```

---

### 12.2 输入 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `input_tokens` |
| 标准中文名 | 输入 Token |
| 范围 | 当前时间范围内、当前根 Session（主 Agent + 全部后代 Subagent） |
| 来源 | 当前 `root_session_id` 下主 Agent 与全部后代 Subagent 有效用量事件的 `input_tokens` 之和 |

设计稿中的“写入 Token”统一改为“输入 Token”。

---

### 12.3 输出 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `output_tokens` |
| 标准中文名 | 输出 Token |
| 范围 | 当前时间范围内、当前根 Session（主 Agent + 全部后代 Subagent） |
| 来源 | 当前 `root_session_id` 下主 Agent 与全部后代 Subagent 有效用量事件的 `output_tokens` 之和 |

包含该 Session 的推理 Token。

---

### 12.4 推理 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `reasoning_tokens` |
| 标准中文名 | 推理 Token |
| 范围 | 当前时间范围内、当前根 Session（主 Agent + 全部后代 Subagent） |
| 来源 | 当前 `root_session_id` 下主 Agent 与全部后代 Subagent 有效用量事件的 `reasoning_tokens` 之和 |

它是 Output 的子集，不能与 Output 重复加入 Total。

---

### 12.5 缓存读取 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `cached_tokens` |
| 标准中文名 | 缓存读取 Token |
| 范围 | 当前时间范围内、当前根 Session（主 Agent + 全部后代 Subagent） |
| 来源 | 当前 `root_session_id` 下主 Agent 与全部后代 Subagent 有效用量事件的 `cached_tokens` 之和 |

设计稿中的“缓存输出 Token”统一改为“缓存读取 Token”。

---

### 12.6 缓存写入 Token

| 项目 | 定义 |
|---|---|
| 标准英文名 | `cache_write_tokens` |
| 标准中文名 | 缓存写入 Token |
| 范围 | 当前时间范围内、当前根 Session（主 Agent + 全部后代 Subagent） |
| 来源 | 当前 `root_session_id` 下主 Agent 与全部后代 Subagent 有效用量事件的 `cache_write_tokens` 之和 |
| 可空 | 是 |

缺失时为 `null`，明确的 0 保持为 `0`；不按模型名推断 0。

---

### 12.7 缓存命中率

| 项目 | 定义 |
|---|---|
| 标准英文名 | `cache_hit_rate` |
| 标准中文名 | 缓存命中率 |
| 范围 | 当前时间范围内、当前根 Session（主 Agent + 全部后代 Subagent） |
| 公式 | `cached_tokens ÷ input_tokens` |
| 数据类型 | `0～1` 或 `null` |

```text
Session 缓存命中率
= 主 Agent 与全部后代 Subagent 的累计 Cached Tokens
÷ 主 Agent 与全部后代 Subagent 的累计 Input
```

不是主 Agent 与各 Subagent 命中率的算术平均，也不是各次模型请求命中率的算术平均。

Input 为 0 时返回 `null`，界面显示 `—`。

---

### 12.8 预估费用

| 项目 | 定义 |
|---|---|
| 标准英文名 | `estimated_cost` |
| 标准中文名 | 预估费用 |
| 当前状态 | 占位 |
| 当前值 | `null` |

---

## 13. Session 行建议结构

```json
{
  "last_activity_at": "2026-08-05T05:20:00+08:00",
  "session_id": "01989abc-def0-7000-8000-123456789abc",
  "title": "设计 Usagi 数据口径",
  "project_name": "usagi",
  "project_path": "<repo>",
  "models_used": [
    "gpt-5.6",
    "gpt-5.6-mini"
  ],
  "subagent_count": 2,
  "total_tokens": 312000,
  "input_tokens": 280000,
  "cached_tokens": 225000,
  "cache_write_tokens": 6000,
  "uncached_input_tokens": 49000,
  "output_tokens": 32000,
  "reasoning_tokens": 22000,
  "other_output_tokens": 10000,
  "cache_hit_rate": 0.8035714,
  "estimated_cost": null,
  "usage_breakdown": {
    "self_usage": {
      "total_tokens": 112000
    },
    "subagent_usage": {
      "total_tokens": 200000
    }
  }
}
```

说明：

- 顶层扁平 Token 字段是当前列表直接使用的 `inclusive_usage`；
- `usage_breakdown` 为后续详情页保留，第一版前端可以不展示；
- `subagent_count` 表示当前范围内产生有效新增用量的后代 Subagent Thread 数量。

---

## 14. 汇总与 Session 的一致性约束

对于同一时间范围，所有根 Session 行都使用包含 Subagent 的 `inclusive_usage`，并且必须满足：

```text
summary.total_tokens
= Σsession.total_tokens
```

```text
summary.input_tokens
= Σsession.input_tokens
```

```text
summary.output_tokens
= Σsession.output_tokens
```

```text
summary.reasoning_tokens
= Σsession.reasoning_tokens
```

```text
summary.cached_tokens
= Σsession.cached_tokens
```

```text
summary.cache_write_tokens
= Σsession.cache_write_tokens
```

```text
summary.session_count
= root_session_id 去重后的 Session 行数量
```

缓存命中率不能这样计算：

```text
summary.cache_hit_rate
≠ 平均(session.cache_hit_rate)
```

必须重新使用总分子和总分母：

```text
summary.cache_hit_rate
= Σsession.cached_tokens
÷ Σsession.input_tokens
```

---

## 15. 示例

当前“今天”范围内有两个 Session：

| Session | Input | Cached | Cache Write | Output | Reasoning |
|---|---:|---:|---:|---:|---:|
| A | 1,000 | 900 | 50 | 100 | 60 |
| B | 9,000 | 900 | 100 | 1,000 | 700 |

汇总：

```text
Input = 1,000 + 9,000 = 10,000
Output = 100 + 1,000 = 1,100
Total = 10,000 + 1,100 = 11,100
Reasoning = 60 + 700 = 760
Cached = 900 + 900 = 1,800
Cache Write = 50 + 100 = 150
Session Count = 2
```

缓存命中率：

```text
1,800 ÷ 10,000 = 18%
```

不能计算为：

```text
(90% + 10%) ÷ 2 = 50%
```

---

## 16. 字段总表

### 16.1 汇总字段

| 英文字段 | 标准中文名 | 来源 | 公式或规则 | 可空 |
|---|---|---|---|---:|
| `total_tokens` | 累计总 Token | MU 计算 | `input_tokens + output_tokens` | 否 |
| `input_tokens` | 累计输入 Token | 本地增量汇总 | `Σ input_tokens` | 否 |
| `output_tokens` | 累计输出 Token | 本地增量汇总 | `Σ output_tokens` | 否 |
| `cached_tokens` | 累计缓存读取 Token | 本地增量汇总 | `Σ cached_tokens` | 否 |
| `cache_write_tokens` | 累计缓存写入 Token | 本地增量汇总 | `Σ cache_write_tokens`；任一未知时为 `null` | 是 |
| `uncached_input_tokens` | 未缓存输入 Token | MU 计算 | `input - cached - write`（write 已知时） | 是 |
| `reasoning_tokens` | 累计推理 Token | 本地增量汇总 | `Σ reasoning_tokens` | 否 |
| `other_output_tokens` | 非推理输出 Token | MU 计算 | `output - reasoning` | 否 |
| `cache_hit_rate` | 累计缓存命中率 | MU 计算 | `cached_tokens ÷ input_tokens` | 是 |
| `session_count` | 会话数量 | MU 计算 | `COUNT(DISTINCT root_session_id)`；Subagent 不单独计数 | 否 |
| `estimated_cost` | 预估费用 | 待定 | 当前固定 `null` | 是 |

### 16.2 Session 字段

| 英文字段 | 标准中文名 | 来源 | 公式或规则 | 可空 |
|---|---|---|---|---:|
| `last_activity_at` | 最后活动时间 | 本地事件时间 | 主 Thread 与全部后代 Subagent 的最后有效活动 | 否 |
| `session_id` | Session ID | 主 Thread 的 `session_meta.id` | 等于 `root_session_id` | 否 |
| `title` | 标题 | 主 Thread 的本地状态索引 | 不合并 Subagent 标题；缺失时显示未命名 | 否 |
| `project_name` | 所属项目 | 主 Thread 的 `session_meta.cwd` 派生 | 不被 Subagent 工作目录覆盖 | 否 |
| `project_path` | 项目路径 | 主 Thread 的 `session_meta.cwd` | 规范化本地路径 | 是 |
| `models_used` | 使用模型 | 主 Thread 与全部后代 Subagent 的 `turn_context.model` | 当前范围内全部模型去重列表 | 否 |
| `total_tokens` | 总 Token | MU 计算 | 主 Agent + 全部后代 Subagent；`input_tokens + output_tokens` | 否 |
| `input_tokens` | 输入 Token | 本地增量汇总 | 主 Agent + 全部后代 Subagent | 否 |
| `output_tokens` | 输出 Token | 本地增量汇总 | 主 Agent + 全部后代 Subagent | 否 |
| `cached_tokens` | 缓存读取 Token | 本地增量汇总 | 主 Agent + 全部后代 Subagent | 否 |
| `cache_write_tokens` | 缓存写入 Token | 本地增量汇总 | 主 Agent + 全部后代 Subagent；任一未知为 `null` | 是 |
| `uncached_input_tokens` | 未缓存输入 Token | MU 计算 | `input - cached - write`（write 已知时） | 是 |
| `reasoning_tokens` | 推理 Token | 本地增量汇总 | 主 Agent + 全部后代 Subagent | 否 |
| `other_output_tokens` | 非推理输出 Token | MU 计算 | `output - reasoning` | 否 |
| `cache_hit_rate` | 缓存命中率 | MU 计算 | `(主 + Subagent Cached) ÷ (主 + Subagent Input)` | 是 |
| `subagent_count` | Subagent 数量 | MU 计算 | 当前范围内产生有效新增用量的后代 Subagent Thread 数量 | 否 |
| `self_usage` | 主 Thread 自身用量 | Thread 级有效事件汇总 | 为后续详情页保留，当前列表不直接展示 | 否 |
| `subagent_usage` | Subagent 用量 | 后代 Thread 自身用量汇总 | 为后续详情页保留，当前列表不直接展示 | 否 |
| `estimated_cost` | 预估费用 | 待定 | 当前固定 `null` | 是 |

---

## 17. 当前已确定与暂未确定

### 已确定

- Token 第一版只使用 Codex 本地记录；
- 汇总时间范围：今天、昨天、本周、本月、今年；
- 周起始日为星期一；
- 时间范围同时作用于汇总和 Session 表；
- Session 表按 `root_session_id` 分组，每行表示主 Thread 与全部后代 Subagent 的完整工作流；
- 标准名称使用“输入 Token”，不使用“写入 Token”；
- Total = Input + Output；
- Reasoning 是 Output 子集；
- Cached Tokens（缓存读取）与 Cache Write 是 Input 子项；
- Cache Write 缺失为 `null`，明确 0 保持为 0；
- Cache Hit = Cached Tokens ÷ Input；
- 汇总 Cache Hit 不是 Session 命中率平均值；
- 会话数量按当前范围内有有效用量的不同 `root_session_id` 计数，Subagent 不单独计数；
- 当前 Session 列表不单独展示 Subagent；
- Session 的标题与项目只使用主 Thread 数据；
- Session 的模型列表合并主 Agent 与全部后代 Subagent 使用过的模型；
- Session 的 Input、Output、Reasoning、Cached Tokens、Cache Write 和 Total 均为主 Agent 与全部后代 Subagent 的合计；
- Session Cache Hit 使用合并后的 Cached Tokens ÷ 合并后的 Input；
- 后台保留 Thread 自身、Subagent 合计与 Session 包含用量三个层次，为后续详情页准备；
- 费用字段暂时为 `null`。

### 暂未确定

- 预估费用和模型价格表；
- Session 详情页的终身累计时间口径；
- 模型用量图的最终展示排序；
- Session 用量图的图表口径；
- Git 仓库根目录识别；
- Cache Write 在未知或混合模型范围中的最终界面提示样式；
- 数据异常与部分缺失的具体 UI。

---

## 18. 参考资料

- MU 初版调研文档：《01.调研文档(1).md》
- MU 最新 Dashboard 设计图：`Group 144(1).png`
- Tokei Codex 本地统计实现：
  - `https://github.com/cclank/tokei`
  - `usage.30s.py`
  - `CALCULATION.md`
