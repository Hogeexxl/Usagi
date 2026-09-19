# Usagi Session 记录列表与详情增强实施方案

> 版本：v0.3  
> 日期：2026-08-12  
> 代码基线：用户本轮提供的最新 Usagi 代码快照  
> 已完成前置版本：`Usagi_Dashboard_KPI与条件筛选实施方案_v0.1.md` 及其实现结果  
> 测试标准：`Usagi_Session记录列表与详情增强测试标准_v0.2.md`

---

## 1. 本轮范围

本轮只实现上一版本完成之后新增确认的 Session 需求：

1. 现有 Dashboard 模型/项目条件筛选同时作用于 Session 列表。
2. Session 列表只展示主会话，不把 Subagent 作为独立列表行。
3. 重构 Session 列表字段与统计口径：
   - 模型：Main + 全部后代 Subagent 在当前时间范围内使用过的全部模型；
   - 删除输入 Token、输出 Token、推理 Token 列；
   - 新增「总 Token」= Main 自身 `self_usage.total_tokens`；
   - 新增「合计 Token」= Main + 全部后代 Subagent 的 `inclusive_usage.total_tokens`；
   - 缓存命中率保持合计口径；
   - 「预估费用」仅改名为「合计费用」，继续使用现有 `inclusive_usage.estimated_cost`，本轮不新增费用计算引擎。
4. Session 列表改为全量轻量排序索引 + 按需加载完整 Session Row：
   - 全局排序在前端基于完整轻量索引完成；
   - 完整 `SessionItemDto` 单批最多 60 条；
   - 前端固定每页 15 条；
   - 支持翻页和输入页码直接跳页；
   - 不提供每页条数切换。
5. 表头支持全局排序：最后活动、项目、模型、总 Token、合计 Token、缓存命中率；支持 ASC/DESC 切换并显示方向状态。
6. 当用户进入一个 60 条详情窗口中的第 3 个前端页时，低优先级预取下一组最多 60 条完整 Row。
7. 点击 Session Row 后按需加载 Session 详情；详情缓存必须能随 `data_revision` 更新失效并重取。
8. Session 详情展示 Main 与全部层级 Subagent；数据库树关系继续保留，UI 可平铺 Subagent。
9. Session 列表与详情中所有 Token 数值使用完整整数，例如 `2,019,061`，不使用 K/M/B 缩写。
10. Session Drawer 的数据、视觉、布局、动效、无障碍与细节交互均按本文第 7、10、11 节冻结；实现不得再自行补充另一套 Drawer 语义。

---

## 2. 已完成基线：禁止重复实施

当前代码已经完成以下能力，本轮只能复用或扩展，不得重新建设：

- `project_kind = project | projectless | unknown`；
- Codex Desktop global-state projectless 适配；
- `GET /api/usage/filter-options`；
- Dashboard 模型/项目多选筛选；
- `DashboardFilters`、canonical filter query、filter options 生命周期；
- Summary 模型/项目筛选；
- Dashboard 移除缓存写入 Token 卡片；
- Dashboard 新增推理 Token 卡片；
- 模型筛选激活时隐藏 Dashboard 会话数量；
- `threads.thread_id / parent_thread_id / root_session_id / agent_role` 树关系；
- `usage_events.thread_id / root_session_id` Usage 归属关系；
- `SessionUsageDto` 已包含 `inclusive_usage / self_usage / subagent_usage / subagent_count / models_used`。

本轮**不新增数据库“合计 Token”列**，也不新增主/子会话关联表。

现有事实源已经满足：

```text
Main 自身 Total Token
= self_usage.total_tokens

Main + 全部后代 Subagent Total Token
= inclusive_usage.total_tokens

全部 Subagent Total Token
= subagent_usage.total_tokens
```

`parent_thread_id` 继续保存直接父子树；`root_session_id` 继续用于快速聚合整个 Session Tree。

---

## 3. 本轮固定数据语义

### 3.1 Session 查询范围

Session 查询 scope 由以下三部分组成：

```text
SessionQueryScope =
    range
  + canonical(models)
  + canonical(projects)
```

直接复用当前 Dashboard 已有的 `DashboardFilters` / `UsageFilter` 编码规则：

- 模型同维度：OR；
- 项目同维度：OR；
- 模型与项目：AND；
- 条件筛选与日期范围：AND；
- 空筛选时不发送空参数。

### 3.2 模型筛选：只筛 Session 资格

模型筛选**不得裁剪 Session Row/Detail 内的 Usage 数值**。

资格规则：

```text
在当前 range 内，
该 root Session Tree 的 Main 或任意层级 Subagent
至少存在一笔 usage_events.model 命中选中模型
=> 该 root Session 有资格进入列表
```

进入列表后，Row 和 Detail 均显示该 Session Tree 在当前 `range` 内的完整数据，即包含未选模型产生的 Usage。

示例：

```text
Main(gpt-A)       100K
Subagent(gpt-B)   300K
筛选 model=gpt-A

Dashboard KPI 总 Token：100K        # Summary 仍按 event 过滤
Session 合计 Token：400K            # Session 只做资格筛选
```

该差异是本轮明确的产品语义，不得尝试强行保持 Dashboard KPI 与 Session 合计之和相等。

### 3.3 项目筛选：筛 root Session 资格

项目筛选继续依据 root thread 的现有 normalized 项目归属：

```text
root_session_id
  -> threads(root).project_kind / project_path
```

普通项目、projectless、unknown 的编码规则完全复用现有 Dashboard filter 实现。

项目筛选只决定 root Session 是否进入列表；进入后 Row/Detail 仍显示当前时间范围内完整 Session Tree 数据。

### 3.4 列表只允许主会话成为 Row

Session 列表候选必须显式满足：

```text
threads.agent_role = 'main'
AND threads.root_session_id = threads.thread_id
AND threads.parent_thread_id IS NULL
```

Subagent 永远不能独立成为列表 Row。

Subagent 只允许：

- 贡献 `models_used`；
- 贡献 `inclusive_usage / subagent_usage`；
- 贡献 Session Tree 最后活动时间；
- 出现在 Session Detail 中。

### 3.5 全部后代 Subagent

“Subagent”均指该 root 下全部层级后代，而不仅是 Main 的直接 children。

例如：

```text
Main A
├─ B
│  └─ D
└─ C
```

则 A 的：

```text
subagent_count = B + C + D
inclusive_usage = A + B + C + D
models_used = A/B/C/D 使用模型去重集合
```

数据库继续保留：

```text
B.parent_thread_id = A
D.parent_thread_id = B
B/C/D.root_session_id = A
```

UI 允许将 B/C/D 平铺，不得为了平铺删除或改写树关系。

### 3.6 时间范围

列表和 Drawer 详情全部受当前 Dashboard `range` 约束。

- Session Row 与 Drawer Header 的 `last_activity_at_ms` 使用同一口径：整个 Session Tree 在当前 range 内的 `MAX(occurred_at_ms)`；
- Drawer Header 不切换为 Main 自身最后活动，Detail 加载前后不得发生时间口径变化；
- 每个 Subagent Detail 的 `last_activity_at_ms`：该 Subagent 自身在当前 range 内的最大活动时间；
- 只在当前 range 内存在 Usage 的 Subagent 才进入当前 Detail 的 Subagent 集合和 `subagent_count`。

---

## 4. Session 列表最终字段

列表固定为以下列：

| 列 | 数据口径 | 可排序 |
|---|---|---|
| 最后活动 | 整个 Session Tree 当前 range 内最后活动 | 是 |
| 标题 | root thread title | 否 |
| 项目 | root thread 当前项目显示信息 | 是 |
| 模型 | Main + 全部后代 Subagent 当前 range 内全部模型 | 是 |
| 总 Token | `self_usage.total_tokens` | 是 |
| 合计 Token | `inclusive_usage.total_tokens` | 是 |
| 缓存命中率 | `inclusive_usage.cache_hit_rate` | 是 |
| 合计费用 | `inclusive_usage.estimated_cost` | 否 |

删除现有：

```text
输入 Token
输出 Token
推理 Token
```

不得删除 `SessionItemDto` 中的 `self_usage / subagent_usage / inclusive_usage` 等已有字段；这些字段仍作为后续功能基础保留。

---

## 5. 排序与分页架构

### 5.1 两级数据模型

固定两个不同粒度：

```text
前端显示页：15 条 / 页
完整 Session Row 请求批次：最多 60 条 / 次
```

前端不得提供 page-size 控件。

初始化时后端必须返回：

1. 当前 `SessionQueryScope` 下的 `total_items`；
2. 当前 scope 下**全部符合资格 root Session 的轻量排序索引**；
3. 最多 60 条完整 `SessionItemDto` 作为首批 Row cache。

不能一次返回全部完整 `SessionItemDto`。

### 5.2 轻量排序索引

新增独立 DTO，建议：

```text
SessionSortIndexItem {
    root_session_id
    last_activity_at_ms
    project_sort_key
    model_sort_key
    total_tokens
    combined_total_tokens
    cache_hit_rate
}
```

要求：

- 每个符合资格的 root Session 恰好一条；
- 只包含全局排序所需数据，不包含三套完整 UsageDto；
- `total_tokens = self_usage.total_tokens`；
- `combined_total_tokens = inclusive_usage.total_tokens`；
- `cache_hit_rate = inclusive_usage.cache_hit_rate`；
- `model_sort_key` 使用现有 `models_used` 稳定顺序的第一项；无模型值排最后；
- `project_sort_key` 使用现有 root 项目显示/路径稳定键，不改变上一版本项目归属语义；
- 任意排序值完全相同时，最终以 `root_session_id ASC` 做稳定 tie-break。

### 5.3 排序在前端完成

后端不因用户每次点击表头重新做列表全局排序请求。

前端持有完整 sort index 后：

```text
sort_index
  -> 本地 comparator
  -> 得到全局 root_session_id 顺序
  -> 根据当前 page 取 15 个 ID
  -> 从 Row cache 读取完整数据
```

排序必须针对当前 scope 的**全部 Session**，不得只排序当前已加载的 60 条 Row。

### 5.4 可排序字段与方向

可排序：

```text
last_activity
project
model
total_tokens
combined_total_tokens
cache_hit_rate
```

默认：

```text
sort_by = last_activity
sort_order = desc
```

规则：

- 点击当前排序列：ASC / DESC 二态切换；
- 点击其他排序列：切换到该列的默认方向；
- 文本列 `project/model` 默认 ASC；
- 时间/数值列默认 DESC；
- 当前排序列必须向 UI 暴露明确方向状态，供箭头展示；
- 排序变化**保持当前页码**，因为 total_items/total_pages 不变；
- `cache_hit_rate = null` 无论 ASC/DESC 都排在有效数值之后；
- 空 project/model sort key 均排最后。

### 5.5 分页

固定：

```text
FRONTEND_PAGE_SIZE = 15
ROW_BATCH_LIMIT = 60
```

前端计算：

```text
total_pages = ceil(total_items / 15)
```

UI 状态至少提供：

```text
共 N 条
当前 Y / X 页
上一页 / 下一页
输入页码直接跳页
```

不提供每页多少条设置。

页码输入只接受 `1..total_pages` 的整数；非法值不得改变当前页。

### 5.6 60 条 Row cache 窗口

60 条恰好覆盖 4 个前端页。

按当前全局排序结果定义窗口：

```text
window 0: 排名 1..60   -> page 1..4
window 1: 排名 61..120 -> page 5..8
window 2: 排名 121..180 -> page 9..12
...
```

Row cache 必须按 `root_session_id` 存储，而不是按数组位置存储，因此：

- 切换排序后已缓存 Row 可以继续复用；
- 只需要请求当前排序窗口中尚未缓存的 ID。

### 5.7 跳页

用户可直接从任意页跳到另一页，不得逐页请求。

例如从 page 1 跳到 page 6：

1. sort index 已知 page 6 对应全局排名 76..90；
2. 计算其所属窗口 61..120；
3. 检查该窗口 root IDs 在 Row cache 中的缺口；
4. 一次请求缺失 Row，单次最多 60；
5. 返回后展示 page 6。

### 5.8 第三个页触发下一窗口预取

当用户进入当前 60 条窗口的第 3 个前端页时，低优先级预取下一窗口。

即：

```text
page 3  -> 预取当前排序排名 61..120
page 7  -> 预取 121..180
page 11 -> 预取 181..240
...
```

预取规则：

- 只请求当前 Row cache 缺失 ID；
- 单次仍不得超过 60；
- 不得递归连续预取直至全量加载；
- 同一 scope/revision 同时最多保留一个低优先级预取任务；
- scope 或 revision 变化后旧预取结果不得写入新 snapshot；
- 同一 scope/revision 内排序改变时，已成功取得的 Row 仍可按 ID 复用。

---

## 6. 后端查询与 API 改造

### 6.1 禁止 Schema migration

本轮不增加：

```text
combined_total_tokens 列
session_total_tokens 列
新的 parent/root 关系表
新的 message/session ledger
```

所有新增结果均从现有 `threads + usage_events` 聚合产生。

### 6.2 Session 资格查询与 Row 聚合必须分层

后端查询不得把模型 filter 直接套在最终 Row Usage 聚合上。

必须分成：

```text
A. eligible_roots
   根据 range + filters 找到“有资格出现”的 root_session_id

B. full_range_rows
   对 eligible_roots 在当前 range 内重新聚合全部模型 Usage
```

模型资格层示意：

```sql
EXISTS (
    SELECT 1
    FROM usage_events ue_match
    WHERE ue_match.root_session_id = root.thread_id
      AND ue_match.occurred_at_ms >= :start
      AND ue_match.occurred_at_ms < :end
      AND ue_match.model IN (...selected models...)
)
```

最终 Row 聚合不得继续保留该 `model IN (...)` 条件。

### 6.3 Main-only 资格

`eligible_roots` 必须 JOIN root thread，并显式检查 Main 身份。

不允许仅靠：

```text
GROUP BY usage_events.root_session_id
```

推断列表资格。

### 6.4 Snapshot API

将当前 cursor 型 `/api/usage/sessions` 改造成当前 scope 的 Session 列表 snapshot 接口。

建议请求：

```text
GET /api/usage/sessions
    ?range=today
    &model=...
    &project_path=...
    &include_projectless=1
    &include_unknown_project=1
    &seed_sort_by=last_activity
    &seed_sort_order=desc
```

`seed_sort_*` 只用于选择首批最多 60 条完整 Row，使筛选/日期切换后能直接命中当前前端排序；它不改变 sort index 包含完整结果集的要求。

建议响应：

```json
{
  "range": { ... },
  "data_revision": 123,
  "total_items": 200,
  "sort_index": [ ...200 lightweight items... ],
  "items": [ ...max 60 SessionItemDto... ]
}
```

必须满足：

```text
total_items == sort_index.length
items.length <= 60
items 中所有 root_session_id 必须存在于 sort_index
```

同一响应中的 `data_revision / total_items / sort_index / items` 必须来自同一个 SQLite Deferred read snapshot。

### 6.5 删除 Session cursor/load-more 契约

本轮新分页架构完成后，Session API 不再返回：

```text
next_cursor
```

也不再接受：

```text
cursor
limit 作为“加载更多”游标分页含义
```

清理只限 Session 列表 cursor 路径；不得误删 storage/scanner 内与 Session UI 无关的其他 cursor 概念。

旧 Spec05 中专门验证 Session HTTP cursor 的测试必须按本轮新契约更新，不得通过同时保留旧 cursor API 做 fallback 来维持旧测试。

### 6.6 按 ID 补 Row API

新增只读批量 Row 接口，建议：

```text
GET /api/usage/session-rows
    ?range=...
    &<same filters>
    &expected_data_revision=123
    &root_session_id=A
    &root_session_id=B
    ...
```

规则：

- `root_session_id` 去重后必须为 1..60 个；
- 必须 canonicalize/validate filter；
- 必须验证 `expected_data_revision`；
- root ID 必须仍满足同一 SessionQueryScope 的资格；
- 返回的 Row Usage 仍是当前 range 内完整 Session Tree 数据，不受模型筛选裁剪；
- 返回顺序按请求 ID 顺序，便于前端合并；
- 不存在/不再符合资格的 ID 不得静默返回其他数据。

### 6.7 Revision 错误

cursor 移除后，不再使用 `STALE_CURSOR` 表达普通数据 revision 变化。

新增/改用明确错误：

```text
STALE_DATA_REVISION   HTTP 409
```

用于：

- `session-rows` expected revision 已过期；
- Session Detail expected revision 已过期。

前端遇到后必须刷新当前 Session snapshot，而不是重试旧 ID 集合。

---

## 7. Session Detail 数据接口

### 7.1 按需加载

Session Detail 不进入初始 Session snapshot，也不跟随 60 条 Row 批次预载。

只有点击某一 Session Row 时才请求。

建议：

```text
GET /api/usage/sessions/{root_session_id}/detail
    ?range=...
    &<same filters>
    &expected_data_revision=123
```

filters 在 Detail 请求中的用途是验证该 root 仍属于当前列表 scope；**不得用于裁剪 Detail Usage**。

### 7.2 当前代码基线与 Detail 聚合缺口

当前代码已经具备 Main 按模型展示所需的原始事实，**本轮不需要 Schema migration，也不需要新增 Usage 表或关联表**。

已确认的现状：

```text
usage_events
  thread_id
  root_session_id
  model
  input_tokens
  cached_tokens
  cache_write_tokens
  output_tokens
  reasoning_tokens
  total_tokens
```

其中：

- `reasoning_tokens` 已真实持久化并进入现有 `TokenTotals / TokenUsageDto`；
- `cache_write_tokens` 已支持 `NULL`，现有 canonical 聚合可保留未知态；
- `TokenTotals` 已统一计算 `uncached_input_tokens / other_output_tokens / cache_hit_rate`；
- `TokenUsageDto` 已包含 `reasoning_tokens / estimated_cost`；
- 当前 `estimated_cost` 后端仍为 `None`，因此 Drawer 的费用项本轮只做正式占位并显示 `—`，**不得为了 Drawer 顺手新增费用计算引擎**；
- 当前 `AggregateReader::aggregate_for_root(..., Some(true))` 只得到 Main 全模型合计，`models_for_root()` 只得到模型列表，尚不能表达“Main 每个模型各自的 Usage”。

现有索引已经包含：

```text
usage_events_root_time_idx   (ledger_epoch, root_session_id, occurred_at_ms)
usage_events_thread_time_idx (ledger_epoch, thread_id, occurred_at_ms)
usage_events_model_time_idx  (ledger_epoch, model, occurred_at_ms)
```

Detail 查询继续复用现有索引，本轮不新增数据库索引。

### 7.3 Main 按模型 Usage 聚合

正式实现 Main 多模型 block 前，后端必须先增加 Detail 专用聚合能力。

固定查询语义：

```text
当前 active ledger_epoch
+ 当前 range
+ root_session_id
-> 对整棵 Session Tree 的 usage_events 一次按 thread_id + model 聚合
```

SQL 语义示意：

```sql
SELECT
    thread_id,
    model,
    MIN(occurred_at_ms) AS first_activity_at_ms,
    MAX(occurred_at_ms) AS last_activity_at_ms,
    MIN(event_id) AS first_event_id,
    SUM(input_tokens),
    SUM(cached_tokens),
    SUM(cache_write_tokens),
    SUM(output_tokens),
    SUM(reasoning_tokens),
    SUM(total_tokens),
    SUM(CASE WHEN cache_write_tokens IS NULL THEN 1 ELSE 0 END),
    COUNT(*)
FROM usage_events
WHERE ledger_epoch = :epoch
  AND root_session_id = :root
  AND occurred_at_ms >= :start
  AND occurred_at_ms < :end
GROUP BY thread_id, model;
```

要求：

1. 不为 Main 每个模型单独发一条 SQL；
2. 不为每个 Subagent 单独发一条 Usage SQL；
3. 一次聚合结果在 Rust 内按 `thread_id` 组织，避免 Detail N+1 查询；
4. 每个分组必须复用现有 `TokenTotals` 的 derived-field 规则，不能为 Drawer 另写一套 Token 数学；
5. `SUM(cache_write_tokens)` 仍必须结合 NULL 行计数恢复“任一来源未知 => 聚合值未知”的现有语义；
6. `total_tokens`、`reasoning_tokens`、`cache_hit_rate` 等 invariant 与现有 aggregate 路径一致。

Main 模型稳定顺序继续沿用当前 `models_for_root()` 的原则：

```text
MIN(occurred_at_ms) ASC
-> MIN(event_id) ASC
-> model ASC
```

因此 Drawer 的 `模型 1 / N` 顺序在同一 snapshot 内必须稳定。

### 7.4 Detail DTO

建议固定为：

```text
SessionDetailDto {
    root_session_id
    data_revision
    last_activity_at_ms       # 整棵 Session Tree 当前 range 最后活动
    main: MainSessionDetailDto
    subagents: SubagentDetailDto[]
}

MainSessionDetailDto {
    title
    thread_id
    root_session_id
    models_used
    model_usage: MainModelUsageDto[]
    self_usage
    subagent_count
    inclusive_usage
}

MainModelUsageDto {
    model
    usage
}
```

字段语义：

- `SessionDetailDto.last_activity_at_ms`：整个 Session Tree 当前 range 内的最后活动，和列表 Row 完全一致，Drawer Header 只读该字段；
- `main.models_used`：Main 自身在当前 range 使用过的模型稳定集合；
- `main.model_usage[]`：Main 自身按模型分组后的 Usage，顺序与 `models_used` 一致；
- `main.self_usage`：Main 自身跨全部模型的合计 Usage，继续保留，不得用 `model_usage[]` 替代；
- `main.subagent_count`：当前 range 内有 Usage 的全部层级后代数；
- `main.inclusive_usage`：root tree 当前 range 完整合计。

必须满足：

```text
SUM(main.model_usage[].usage.total_tokens)
= main.self_usage.total_tokens
```

其他基础 Token 字段也应按同一聚合语义满足对应加总关系；`cache_write_tokens` 因存在 unknown/null 语义，不以简单整数求和作为唯一 invariant。

Drawer 已确认展示：

```text
Header
  Session 标题
  Session ID
  最后活动时间              = SessionDetailDto.last_activity_at_ms

合计区
  合计 Token                = inclusive_usage.total_tokens
  Main                      = self_usage.total_tokens
  Subagent                  = subagent_usage.total_tokens

Main 每模型 block
  模型名称
  总 Token
  输入 Token
  输出 Token
  推理 Token
  缓存命中率
  缓存读取
  缓存写入
  预估费用
```

其中：

- Reasoning Token 使用现有真实 `reasoning_tokens`；
- 预估费用保留正式 UI 字段，但本轮后端仍返回 `estimated_cost = null`，前端显示 `—`；
- 前端不得按比例拆分 `self_usage`，不得把 Main 合计值重复填入多个模型 block。

### 7.5 Subagent Detail

`subagents[]` 包含当前 range 内有 Usage 的全部层级后代。

本轮产品范围暂按**每个 Subagent 只使用一个模型**实现，不在 Subagent block 内继续按模型拆分。

每项至少返回：

```text
thread_id
parent_thread_id
root_session_id
title
model                    # 本轮按单模型实现
last_activity_at_ms      # 该 Subagent 自身
usage                    # 该 Subagent 自身完整 TokenUsageDto
```

前端显示：

```text
标题
ID
模型
最后活动
总 Token
输入 Token
输出 Token
推理 Token
缓存读取
缓存写入
缓存命中率
预估费用
```

约束：

- `usage` 是该 Subagent 自身在当前 range 内的完整 Usage；
- 不在 Subagent 内创建 `model_usage[]`，也不渲染嵌套的多模型 block；
- 当前实现和测试 fixture 均按 Subagent 单模型契约执行；
- 若未来真实 Codex 数据确认同一 Subagent 存在多模型，需要另立需求扩展，不在本轮自动拆分、按比例分配或新增隐藏 fallback；
- 树关系通过 `parent_thread_id` 保留在 DTO；当前 Drawer 平铺所有 Subagent，不要求按树渲染。

为了 API 稳定，Subagent 返回顺序固定为：

```text
last_activity_at_ms DESC,
thread_id ASC
```

### 7.6 cache_write_tokens null 与费用占位

账号登录 Codex 可能不披露缓存写入值。

```text
cache_write_tokens = null
```

必须在详情 UI 显示为未知态 `—`，不得转换为 `0`；真实 `0` 必须显示 `0`。

当前费用引擎尚未实现：

```text
estimated_cost = null
```

Drawer 中 Main model block 与 Subagent block 都必须保留「预估费用」位置并显示 `—`。本轮不得为了填充该字段新增费用算法。

---

## 8. 前端 Session Controller 重构

### 8.1 输入参数

当前：

```text
useSessionTableController(range)
```

改为至少接收：

```text
useSessionTableController(range, filters, ...)
```

`DashboardPage` 必须把现有：

```text
view.range
view.filters
```

同时传入 Session controller。

不得新建第二套模型/项目筛选状态。

### 8.2 Query cache key

Session snapshot cache key：

```text
SessionQueryKey =
    range
  + canonical(models)
  + canonical(projects)
```

排序和页码不是数据 scope，不进入 QueryKey。

Snapshot 至少保存：

```text
data_revision
timezone
total_items
sort_index
row_cache: Map<root_session_id, SessionItemDto>
```

### 8.3 UI 状态

Session controller 独立保存：

```text
page
sort_by
sort_order
```

默认：

```text
page = 1
sort_by = last_activity
sort_order = desc
```

行为：

| 事件 | page | sort |
|---|---|---|
| 日期范围变化 | 重置 1 | 保留 |
| 模型/项目筛选变化 | 重置 1 | 保留 |
| 点击排序列 | 保持当前页 | 更新 |
| ASC/DESC 切换 | 保持当前页 | 更新 |
| data_revision 更新 | 尽量保持；超过新 total_pages 时 clamp | 保留 |
| 直接输入页码 | 跳到目标页 | 保留 |

### 8.4 当前页派生

每次 render：

1. 对完整 `sort_index` 本地排序；
2. 计算当前页 15 个 ID；
3. 从 `row_cache` 取 Row；
4. 当前所属 60 条窗口存在缺口时，前台批量加载窗口缺失 Row；
5. 不允许拿其他 QueryKey 的 Row 作为当前 scope 数据。

同一 scope/revision 中 Row 内容与排序无关，因此同一 `root_session_id` 在不同排序下可以复用。

### 8.5 Filter 变化

筛选变化时：

```text
page -> 1
sort_by/order -> 保留
切到新的 SessionQueryKey
```

若该 QueryKey 存在同 revision 成功 snapshot，可直接复用；否则请求新的 Session snapshot。

不得短暂展示旧筛选 scope 的列表作为新 scope 结果。

### 8.6 Revision 更新

收到现有 revision feed 更高 `data_revision`：

- 当前 Session snapshot 标记 stale；
- 重新请求当前 range + 当前 filters 的 Session snapshot；
- 旧 revision 的 Row/detail cache 不得写回新 snapshot；
- 新 total_pages 小于当前 page 时 clamp 到最后有效页；
- 不因为 revision 更新强制回 page 1。

---

## 9. Session 列表前端调整

### 9.1 删除旧 load-more UI

删除 Session UI/controller 中：

```text
next_cursor
has_more
load_more
retry_load_more
SessionTableFooter 的“加载更多”模式
```

改为页码分页状态与控制。

### 9.2 表头排序

表头交互只接 controller 的排序状态：

```text
sort_by
sort_order
select_sort(column)
```

不得在 `SessionTable` 内另存一套排序 state。

可排序列显示当前方向；不可排序列不得伪装成可点击。

### 9.3 完整数值 formatter

新增 Session 专用 formatter，例如：

```text
formatSessionTokenInteger(value)
```

使用 locale-aware 千分位，但不做 K/M/B 缩写。

适用：

- 列表总 Token；
- 列表合计 Token；
- Drawer 中所有 Token 数值。

不得修改 Dashboard KPI 当前通用 formatter，避免扩大显示改动范围。

---

## 10. Session Detail Controller / Cache

### 10.1 Cache key

Detail cache 至少绑定：

```text
range
canonical filters
root_session_id
data_revision
```

即使 Detail 内容不受模型 filter 裁剪，也保留当前列表 scope 作为 cache identity，避免跨 scope 控制状态混用。

### 10.2 点击行为

点击 Row：

1. 检查当前 revision 对应 Detail cache；
2. 命中则直接交给 Drawer；
3. 未命中则发起 Detail request；
4. response revision 必须与当前 Session snapshot 相同；
5. 晚到旧请求不得覆盖当前选择。

### 10.3 数据更新

`data_revision` 变化后：

- 旧 detail cache 自动失效；
- 再次打开必须请求新 revision 数据；
- 如果 Drawer 当前保持打开，controller 必须提供刷新当前详情的能力，具体 UI 是否显示 refreshing 由 Drawer UI 补充 Agent 决定。

---

## 11. Drawer UI / 交互实施规格

本节规定 Session Detail Drawer 的前端视觉、交互和无障碍要求；数据范围、请求时机、revision、cache 和并发语义继续以第 7、8、10 节为准。

### 11.1 视觉基准

Drawer 必须延续现有 Dashboard 的视觉系统，不建立第二套页面风格：

- 以 `1512px` 桌面视口作为主要设计和验收基准；
- `1280px` 及以上桌面 Drawer 宽度固定 `760px`，从视口右侧进入；
- 延用 Dashboard 的 JetBrains Mono 字体、白色 surface、`#f3f4f6` 页面背景、`#e4e4e7` 分隔线和 `8px` 圆角；
- 正文和辅助信息不得小于 `12px`；基础字号层级使用 `12 / 14 / 16 / 18 / 24px`；
- 常规数值使用黑色；预估费用当前为未知态 `—`，未来出现有效费用值时才沿用 Dashboard 现有绿色；
- 不使用装饰渐变、重阴影、胶囊标签或与 Dashboard 不一致的卡片样式；
- Drawer 自身仅使用左边框和克制的左向阴影与页面分层。

响应式行为：

- `1280px` 及以上：宽度 `760px`；
- `641px–1279px`：宽度 `min(90vw, 760px)`；
- `640px` 及以下：Drawer 占满视口宽度和高度，取消左边框；
- Drawer Header 固定在顶部，详情内容在 Drawer 内部独立纵向滚动；页面主体在 Drawer 打开期间锁定滚动；
- Main/Subagent 的 8 项 Usage 网格在 `641px` 及以上使用 `4 列 × 2 行`，在 `640px` 及以下改为 `2 列 × 4 行`，不得通过缩小文字强行维持四列。

### 11.2 打开、关闭与列表联动

点击或键盘激活任一 Session Row 后打开 Drawer，并将该 Row 标记为当前选中行。

选中行使用现有表格 hover/selection 体系，只增加浅色背景和左侧细状态线；不得改变 Row 高度或造成表格位移。

Drawer 打开后遮罩覆盖 Dashboard 主体，正常交互路径下用户不能直接操作后方日期、模型或项目筛选控件；因此本轮不额外定义“Drawer 打开时修改筛选”的状态迁移。

Drawer 支持以下关闭方式：

- Header 右上角关闭按钮；
- `Escape`；
- 点击遮罩；
- 在窄屏全屏模式下仍保留 Header 关闭按钮。

关闭后清除当前行高亮，并把焦点恢复到触发 Drawer 的 Session Row。

### 11.3 Header

Header 只显示：

```text
Session 标题
完整 Session ID + 复制按钮
最后活动时间
刷新当前详情按钮
关闭按钮
```

其中“最后活动时间”固定使用：

```text
SessionDetailDto.last_activity_at_ms
= 整个 Session Tree 当前 range 内 MAX(occurred_at_ms)
```

它必须与 Session 列表的最后活动口径一致；Detail 加载完成后不得切换成 Main 自身最后活动。

明确禁止：

- 不显示 `Main Session` eyebrow/标签；
- 不在 Header 显示模型标签；
- 不截断 Session ID；空间不足时允许 ID 在字符边界自然换行；
- 不用模型、时间或 ID 与标题竞争主层级。

标题使用 `18px / 24px`、`700`；ID 与最后活动使用 `12px / 16px`。长标题允许换行，不设置固定行数截断。刷新和关闭按钮使用与 Dashboard 控件一致的 hover、focus-visible 和圆角状态。

### 11.4 合计信息区

Header 下方第一块内容固定为三列合计信息：

```text
合计 Token = inclusive_usage.total_tokens
Main        = self_usage.total_tokens
Subagent    = subagent_usage.total_tokens
```

禁止在该区域显示：

- Main 占比或任何派生百分比；
- Main 自身缓存命中率；
- 输入、输出、推理或费用等明细。

合计 Token 为本区主值，使用 `24px / 32px`；Main 与 Subagent 使用 `20px / 28px`。三列共用一个 `8px` 圆角、`1px` 边框的 Dashboard 风格 surface，通过内部细分隔线区分，不拆成三个悬浮卡片。

窄屏时合计 Token 独占第一行，Main 与 Subagent 在第二行各占一列。

### 11.5 Main 信息区

Main 区标题为 `Main`，右侧显示当前 Main 使用的模型数量。

Main 有多个模型时，每个模型独立显示一个 Usage block；数据必须来自第 7.3 节的 `main.model_usage[]`。block Header 只显示模型名称和当前序号，例如 `模型 1 / 2`。模型之间垂直间距 `12px`。

每个模型 block 固定显示以下 8 项：

```text
总 Token | 输入 Token | 输出 Token | 推理 Token
缓存命中率 | 缓存读取 | 缓存写入 | 预估费用
```

布局：

- `641px` 及以上：两行四列；
- `640px` 及以下：四行两列。

字段展示规则：

- Token 使用完整千分位整数，不使用 K/M/B；
- Reasoning Token 直接显示该模型真实 `usage.reasoning_tokens`；
- `cache_write_tokens = null` 显示 `—`，真实 `0` 显示 `0`；
- 缓存命中率未知时显示 `—`；
- 本轮 `estimated_cost = null`，因此预估费用显示 `—`；未来有效值才沿用 Dashboard 费用格式和绿色；
- block 使用 `1px` 边框和 `8px` 圆角；模型名位于浅灰 Header，8 项数据位于白色内容区；
- 字段标签使用 `12px / 16px`，数值使用 `15px / 20px`、`500`。

前端不得按比例拆分或伪造模型数据，也不得把 `main.self_usage` 重复填充到每个模型 block。`main.self_usage` 仅作为 Main 跨模型合计与顶部合计区数据源。

### 11.6 Subagent 信息区

Subagent 区按第 7.5 节既定顺序展示，每个 Subagent 使用一个独立 block，不合并不同 Subagent，也不按树缩进。

本轮按**每个 Subagent 只有一个模型**实现。Subagent block 内不再按模型拆二级 Usage block。

每个 Subagent block 的 Header 始终显示：

```text
展开/收起按钮
标题
最后活动时间
完整 ID + 复制按钮
模型
```

标题允许换行；ID 必须完整显示，不得使用省略号或只显示前缀。最后活动时间在桌面端位于标题右侧，窄屏时移动到标题下方。

每个 Subagent 的数据区与 Main 单模型 block 使用相同的 8 项：

```text
总 Token | 输入 Token | 输出 Token | 推理 Token
缓存命中率 | 缓存读取 | 缓存写入 | 预估费用
```

布局同样遵循：

- `641px` 及以上：两行四列；
- `640px` 及以下：四行两列。

字段展示语义与 Main 一致：reasoning 使用真实值；cache-write unknown 显示 `—`、真实 0 显示 `0`；预估费用本轮显示 `—`。

多个 Subagent block 之间保持 `12px` 间距。每个 block 可独立展开或收起，不使用互斥 accordion：

- 初次打开 Drawer 时默认展开最近活动的第一个 Subagent；
- 其余 Subagent 默认收起；
- 用户可以同时展开多个 Subagent；
- 收起时仅隐藏 8 项数据，标题、最后活动、完整 ID 和模型仍然可见；
- 展开状态只属于当前 Drawer/当前 Session，不写入 Detail cache，也不跨 Session 复用。

展开按钮使用右向 chevron；展开时旋转 `90deg`。按钮必须提供动态 `aria-expanded` 和“展开/收起 Subagent 详情”可访问名称。

### 11.7 Loading、Error 与 Refreshing

Detail 首次加载期间先打开 Drawer，再在内容区域显示骨架：

- Header 保留当前 Row 已知的标题、ID 和 Tree-level 最后活动；
- 合计/Main/Subagent 内容区显示与最终布局近似的骨架；
- 不使用全屏 spinner，也不清空 Dashboard。

Detail 加载失败时：

- Drawer 保持打开；
- Header 和当前行高亮保持不变；
- 内容区显示“Session 详情加载失败”和重试按钮；
- 重试复用第 10 节 controller，不创建第二套请求状态。

`data_revision` 变化且 Drawer 保持打开时：

- 继续显示当前详情；
- Header 最后活动旁显示 `正在更新` 和小型 spinner；
- 刷新成功后原位更新内容，不关闭 Drawer、不重置滚动位置；
- 刷新失败时保留旧详情，并在内容顶部显示轻量错误和重试入口；
- 晚到旧 revision response 仍按第 10 节规则丢弃。

### 11.8 动效

- Drawer 打开/关闭：`240ms`，使用快速减速曲线，从右侧平移；
- 遮罩：`180ms` opacity；
- Subagent 展开/收起：`180ms` 高度变化，同时旋转 chevron；
- Row hover/selection、按钮 hover：沿用现有 `120ms`；
- 动效不得改变字段顺序、造成内容横向跳动或阻塞操作；
- `prefers-reduced-motion: reduce` 时禁用平移和高度过渡，状态立即切换。

### 11.9 键盘、焦点与 Dialog 语义

- Session Row 使用 `Enter` 或 `Space` 打开 Drawer；
- Drawer 根容器必须提供模态对话框语义：`role="dialog"`、`aria-modal="true"`，并通过 `aria-labelledby` 关联 Session 标题；
- Drawer 打开并完成首屏渲染后，焦点进入关闭按钮；
- `Tab` 和 `Shift+Tab` 焦点限制在 Drawer 内；
- `Escape` 关闭 Drawer；
- Subagent 展开按钮、复制按钮、刷新按钮、重试按钮和关闭按钮都必须可键盘操作；
- 焦点样式复用现有 `2px #2563eb` focus-visible outline；
- 关闭后焦点恢复到原 Session Row；如果原 Row 已因 revision 更新消失，则恢复到 Session 表格容器。

`role / aria-*` 只定义辅助技术语义，不改变 Drawer 的视觉层级和业务逻辑。

### 11.10 组件边界与验收

推荐组件边界：

```text
SessionDetailDrawer
├── SessionDetailHeader
├── SessionUsageSummary
├── MainModelUsageBlock[]
└── SubagentUsageBlock[]
```

不要为单个字段创建组件；8 项 Usage 网格由 Main model block 和 Subagent block 复用同一个内部展示组件即可。

Drawer UI 完成至少满足：

1. `1512px` 下 Drawer 宽度、字号、边框、圆角和颜色与 Dashboard 一致；
2. Header 不出现 `Main Session` 或模型标签；标题、完整 ID、Tree-level 最后活动可见；
3. 合计区只出现合计 Token、Main、Subagent；
4. Main 每个模型各有一个 8 项 Usage block，数据来自真实按模型聚合；
5. 每个 Subagent 一个 block，按单模型实现，不再在 block 内继续按模型拆分；
6. Main/Subagent 均显示 Reasoning Token；预估费用本轮保留字段并显示 `—`；
7. 每个 Subagent 身份信息完整，ID 不截断，并有相同的 8 项数据；
8. 每个 Subagent 可独立展开/收起，默认只展开第一个；
9. `cache_write_tokens = null` 与真实 `0` 视觉可区分；
10. loading、error、refreshing 不关闭 Drawer，不污染旧 revision；
11. Escape、遮罩、dialog 语义、焦点限制和焦点恢复符合本节规则；
12. `640px`、`900px`、`1512px` 三个宽度无横向溢出、字段重叠或不可操作控件；`640px` 下 Usage 网格为两列。

---

## 12. 推荐实施步骤

### S1：Session 查询模型与资格语义

1. 为 Session 查询复用现有 canonical `UsageFilter`。
2. 建立 `SessionQueryScope` / 后端等价查询参数模型。
3. 将 Session 候选改成显式 Main-only。
4. 实现模型“资格筛选”、项目 root 资格筛选。
5. 确保最终 Row 聚合重新读取当前 range 全部模型 Usage，不被模型 filter 裁剪。
6. 保持现有数据库 schema 不变。

**S1 不改前端。**

### S2：轻量排序索引 + Snapshot 聚合

1. 新增 `SessionSortIndexItem` domain/API DTO。
2. 一次 snapshot 计算：`total_items + full sort_index + seed rows(max 60)`。
3. Row 字段调整为第 4 节口径。
4. `total_tokens` 取 self；`combined_total_tokens` 取 inclusive。
5. 列表最后活动、模型、缓存命中率全部按整棵 tree 口径。
6. 保证 snapshot 同 transaction / revision。

### S3：Row batch API + Detail API + revision 契约

1. 新增 `session-rows` repeated-ID 接口，最大 60。
2. 新增 Session Detail endpoint。
3. 在 `src/usage/aggregate.rs` 增加 Detail 专用的整棵 root `GROUP BY thread_id, model` 聚合；复用现有 `TokenTotals` 与 derived-field 规则，避免 Main/model/Subagent N+1 SQL。
4. 在 Detail domain/API DTO 中新增 `MainModelUsageDto` 与 `main.model_usage[]`；`main.self_usage` 继续保留为 Main 跨模型合计。
5. Detail 顶层返回 Tree-level `last_activity_at_ms`；Header 不使用 Main 自身时间替代。
6. Detail 返回 Main + 当前 range 全部层级 Subagent；Subagent 本轮按单模型 DTO/展示契约实现，不增加 Subagent `model_usage[]`。
7. Reasoning Token 沿用现有真实字段；`estimated_cost` 继续为 `None` 并由 Drawer 显示 `—`，不新增费用引擎。
8. 增加明确 `STALE_DATA_REVISION` 语义。
9. 删除 `/sessions` 的 cursor/next_cursor 契约及仅服务它的 API cursor 代码。
10. 更新受影响旧 Spec05 Session cursor 测试为新契约，不保留 fallback。

**S1–S3 完成后进入 Gate A。**

### S4：Frontend DTO / Client

1. 更新 Sessions snapshot DTO：`total_items / sort_index / items`。
2. 新增 sort-index parser。
3. 新增 batch-row client。
4. 新增 detail client/DTO parser，并支持 `SessionDetailDto.last_activity_at_ms / main.model_usage[] / Subagent model / reasoning_tokens / estimated_cost`。
5. Session query 复用当前 Dashboard filter serializer。
6. 增加 expected revision 参数和 stale error handling。

### S5：Session Controller —— scope / cache / pagination

1. controller 接收 `range + filters`。
2. QueryKey 改为 range + canonical filters。
3. 建立 full sort-index + row-cache snapshot。
4. 固定 15/page，计算 total_pages。
5. 实现上一页/下一页/直接跳页的状态逻辑。
6. filter/range 变化 page=1、sort 保留。
7. revision 更新保持当前页并 clamp。

### S6：Session Controller —— global sort / 60-row window / prefetch

1. 实现本地全量 sort-index comparator。
2. 实现六个可排序字段与稳定 tie-break。
3. 排序变化保持当前页。
4. 计算当前 60-row window 并批量补缺失 Row。
5. 在每个窗口第 3 页触发下一窗口低优先级预取。
6. 去重 in-flight 请求，防止 late response 污染其他 scope/revision。

### S7：Session 列表展示

1. 更新为第 4 节 8 列。
2. 删除输入/输出/推理列。
3. 新增总 Token、合计 Token。
4. 「预估费用」文案改为「合计费用」，不新增成本计算。
5. 接入表头排序状态和 ASC/DESC 指示。
6. 删除“加载更多”，接入固定 15/page 的分页与跳页。
7. Session Token 列使用完整整数 formatter。

**S4–S7 完成后进入 Gate B。**

### S8：Session Detail 数据/controller

1. Row click 触发按需 detail load。
2. 实现 revision-aware detail cache。
3. 映射 Main `model_usage[] / self_usage / inclusive_usage` 与 all-descendant Subagent 数据。
4. Drawer Header 永远使用 Detail 顶层 Tree-level `last_activity_at_ms`。
5. Reasoning Token 使用现有真实值；cache-write null 保持未知态；预估费用 `null` 保持 `—` 占位。
6. Drawer Token 数值接入完整整数 formatter。
7. Subagent 本轮按单模型展示，不在其内部继续拆 model block。
8. 暴露第 11 节 Drawer 所需 component props/state seam。

### S9：Drawer UI / 交互

严格按第 11 节已冻结规格实施：

1. 完成 Header、合计区、Main model blocks、Subagent blocks。
2. 完成 `4×2 -> 2×4` 响应式 Usage 网格与连续 Drawer 宽度规则。
3. 完成 loading/error/refreshing、独立 Subagent 展开状态与动效。
4. 完成遮罩、滚动锁、Escape、focus trap、焦点恢复。
5. 增加 `role="dialog" / aria-modal / aria-labelledby / aria-expanded` 等无障碍语义。
6. 不偏离第 11 节另建视觉或交互规则。

### S10：清理与最终集成

1. 清理旧 Session cursor/load-more frontend state、DTO、API path 和死代码。
2. 保留 scanner/storage 等与本轮 Session UI 无关的 cursor 概念。
3. 确认 Dashboard filters 同时驱动 Summary 与 Session，但二者模型过滤语义分别符合既定规则。
4. 确认 Session 旧 future-facing usage 字段未被误删。
5. 完成静态检查、Gate C 与受影响回归。

**S8–S10 完成后进入 Gate C。**

---

## 13. 明确禁止

1. 不重新实现上一版本 `project_kind/global-state/filter-options/Dashboard filters/KPI`。
2. 不新增数据库合计 Token 列。
3. 不新增新的主/子关联表。
4. 不删除 `self_usage/subagent_usage/inclusive_usage` 等为后续功能保留的现有字段。
5. 不让模型筛选裁剪 Session Row/Detail 内的 Usage。
6. 不让 Subagent 独立出现在 Session 主列表。
7. 不把前端只加载的 60 条 Row 当成全局排序数据集。
8. 不在每次表头排序点击时重新请求完整全局排序结果。
9. 不一次返回当前查询范围全部完整 `SessionItemDto`。
10. 不自动后台预取所有剩余 Session Row。
11. 不保留旧 cursor/load-more 作为 fallback/dual path。
12. 不把 `cache_write_tokens=null` 显示为 0。
13. 不修改 Dashboard KPI 的缩写 formatter 来满足 Session 完整数值展示。
14. 不偏离第 11 节已冻结的 Drawer 规格自行发明第二套视觉、数据或交互语义。
15. 不通过跳过/删除旧测试断言掩盖本轮有意改变的 Session API 契约；旧 cursor 测试应正式迁移为新契约测试。

---

## 14. 本轮完成判定

满足以下条件才视为本轮完成：

1. 现有 Dashboard 模型/项目筛选能够同步改变 Session 列表资格结果。
2. 模型筛选只筛 Session 资格；Row/Detail 为当前 range 内完整 Session Tree Usage。
3. Session 列表只出现 Main。
4. 全部层级 Subagent 正确贡献 models、last_activity、subagent_count、合计 Token。
5. 列表字段与第 4 节一致。
6. 前端固定 15 条/页，后端完整 Row 单批不超过 60。
7. 初始化返回 total_items + 完整轻量 sort index + 最多 60 Row。
8. 六个排序字段均为全局排序，不是当前 Row cache 局部排序。
9. 排序切换不要求重新取得全量索引，且保持当前页。
10. 直接跳页只加载目标 60-row window 的缺失 Row，不逐页请求。
11. 每个 60-row window 的第 3 页能低优先级预取下一窗口，且不会递归拉全量。
12. revision 变化后旧 Row/detail response 不得污染新 snapshot。
13. Session Detail 仅点击后请求，缓存按 revision 正确失效。
14. Detail 包含 Main 与当前 range 内全部层级 Subagent，并保留 `parent_thread_id`。
15. Main `model_usage[]` 来自真实 `usage_events` 的按模型聚合，且 Main 各模型总 Token 合计等于 `self_usage.total_tokens`；前端不存在比例拆分或重复填充。
16. Drawer Header 的最后活动始终使用整棵 Session Tree 口径，加载前后不切换为 Main 自身时间。
17. Drawer 的 Main 与 Subagent 均显示真实 Reasoning Token；预估费用本轮保留位置并显示 `—`。
18. Subagent 本轮按单模型一个 block 实现，不在 Subagent 内继续拆模型 Usage。
19. cache-write unknown 与真实 0 区分。
20. Session Token 数值均使用完整千分位整数，不使用 K/M/B。
21. `640px` 下 Usage 网格为两列，`900px / 1512px` 无 Drawer 宽度断层、横向溢出或控件重叠。
22. Drawer 具备 dialog/aria 语义、focus trap、Escape 关闭与焦点恢复。
23. 旧 Session cursor/load-more API/UI 已清理，不存在双路径。
24. 上一版本已完成功能未被重复改造或回退。
25. 本轮测试标准全部 PASS，既有受影响回归 PASS。
