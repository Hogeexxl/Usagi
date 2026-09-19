# Usagi Dashboard KPI 与条件筛选实施方案

> 版本：v0.1  
> 日期：2026-08-10  
> 代码基线：用户于 2026-08-10 提供的 Usagi 最新代码快照  
> 测试标准：`Usagi_Dashboard_KPI与条件筛选测试标准_v0.1.md`

---

## 1. 本轮范围

本轮只实现以下内容：

1. Dashboard 移除「缓存写入 Token」卡片；只移除前端展示，不删除 `cache_write_tokens` 的数据库、解析、聚合、API DTO 或前端数据解析代码。
2. Dashboard 新增「推理 Token」卡片，直接展示现有 `reasoning_tokens`。
3. 暂不实现「总消息数」「用户消息数」，不得新增 message ledger、消息正文解析或消息计数逻辑。
4. Dashboard 新增模型多选筛选和项目多选筛选；条件筛选只影响 Dashboard KPI，不影响 Session 列表。
5. 日期范围筛选与条件筛选保持独立状态；切换日期范围时保留模型/项目条件。
6. 支持区分：普通项目、Codex Desktop 无项目会话（projectless）、MU 无法识别项目归属（unknown）。
7. 新增全历史筛选选项接口，并设计低频刷新机制；不得在每次打开下拉、切换日期或修改条件时重复请求选项清单。
8. Dashboard 文字层级及模型/项目筛选控件以 Vibe Usage 的实际页面为复刻基准，桌面验收视口宽度固定为 `1512px`；本轮不新增工具/终端筛选，也不新增 Session 表头排序交互。

第 14 节规定本轮已确认的文字、筛选控件和交互复刻契约；未在第 14 节规定的动画、响应式断点及 loading/error/stale 细节仍由本地 Sol 在不改变数据/API/状态语义的前提下补充。

---

## 2. 当前代码基线与直接改动结论

### 2.1 KPI 现状

当前：

- `frontend/src/dashboard/MetricGrid.tsx` 显示 8 张卡片，其中包含 `cache_write_tokens`，不包含 `reasoning_tokens`。
- `frontend/src/data/types.ts` 的 `UsageDto` 已包含 `reasoning_tokens` 和 `cache_write_tokens`。
- `frontend/src/data/usagiClient.ts::parseTokenUsage()` 已解析 `reasoning_tokens`。
- 后端 `SummaryUsageDto`、`TokenTotals`、聚合 SQL 已包含 `reasoning_tokens`。

因此：

- 移除「缓存写入 Token」只改 `MetricGrid` 展示定义和相关前端测试。
- 新增「推理 Token」只增加卡片定义/formatter 分支和测试，不新增后端字段，不修改既有 API response schema。
- `cache_write_tokens` 继续存在于 `UsageDto` 和 Summary response 中，前端继续解析，只是不展示。

### 2.2 当前项目字段现状

当前 MU 的项目路径主要来自：

```text
rollout cwd / state_5.threads.cwd
        ↓
threads.project_path
        ↓
basename(project_path)
        ↓
threads.project_name
```

当前数据库 `threads` 只有：

```text
project_name TEXT
project_path TEXT
```

没有“该 thread 是否属于 Codex Desktop projectless”的独立语义字段。

Codex Desktop 的 projectless thread 仍可能有非空 `cwd`，因此：

```text
project_path IS NULL
```

不能用于判断 projectless。

---

## 3. 项目归属模型

### 3.1 新增 `project_kind`

在 MU normalized thread 层新增稳定字段：

```text
project_kind = project | projectless | unknown
```

语义：

| 值 | 含义 |
|---|---|
| `project` | MU 未发现该 root thread 为 projectless，且存在有效 `project_path` |
| `projectless` | Codex Desktop 明确把该 root thread 标记为无项目会话 |
| `unknown` | MU 无法可靠确定项目归属，或项目归属来源发生冲突 |

必须遵守：

```text
projectless != project_path IS NULL
unknown != projectless
```

`project_path` 继续表示 thread 的真实/归一化工作目录事实，不承担“是否挂载项目”的判定职责。

对于 projectless thread：

- `project_kind = projectless`；
- `project_path` 若存在，必须继续保留；
- `project_name` 若现有解析能够得到，也可继续保留；
- 但 filter-options 不得把该 `project_path` 作为普通项目选项返回。

### 3.2 Codex Desktop projectless 数据源

新增只读适配器读取：

```text
$CODEX_HOME/.codex-global-state.json
```

默认 `$CODEX_HOME` 仍沿用 MU 当前绑定规则；默认路径即 `~/.codex`。

新增建议文件：

```text
src/codex/global_state.rs
```

只解析完成本需求所需的最小字段：

```text
projectless-thread-ids
thread-project-assignments
```

不得读取、保存、日志输出或写入 MU 数据库中的 prompt history、消息正文、preview 等无关字段。

适配器建议输出：

```text
GlobalStateSnapshot {
    status,
    projectless_thread_ids,
    thread_project_assignments,
    diagnostics,
}
```

其中 diagnostics 只能包含错误码、字段名等非敏感信息，不能包含 `.codex-global-state.json` 原始内容。

### 3.3 `project_kind` 判定优先级

仅对 normalized root/thread metadata 做归一化，不根据路径模式猜 projectless。

当 global state 可完整读取时：

1. thread 同时存在于 `projectless-thread-ids` 和 `thread-project-assignments`：
   - `project_kind = unknown`；
   - 记录 metadata conflict diagnostic；
   - 不猜测 Codex Desktop 最终 UI 归属。
2. thread 只存在于 `projectless-thread-ids`：
   - `project_kind = projectless`。
3. thread 不在 `projectless-thread-ids`，且存在有效 `project_path`：
   - `project_kind = project`。
4. thread 不在 `projectless-thread-ids`，且没有有效 `project_path`：
   - `project_kind = unknown`。

不得使用以下规则判定 projectless：

```text
project_path IS NULL
project_path LIKE '~/Documents/Codex/%'
basename(project_path)
某个固定 generated-workspace 路径模式
```

原因：projectless 有真实 cwd；generated cwd 也不能作为稳定身份主键。

### 3.4 global state 不存在或不可读

必须区分“文件不存在”和“文件存在但不可可靠读取”：

- `not_present`：兼容非 Desktop/旧环境。存在有效 `project_path` 时沿用 `project`；无有效路径时为 `unknown`。
- `unavailable/malformed`：不得利用损坏内容重新分类。已有 thread 保留已持久化的 `project_kind`；新 thread 在无法取得 projectless 证据时先记 `unknown`，待后续成功扫描修正。

不得因为一次临时读取失败把已有 `projectless` 批量改写为 `project`。

---

## 4. 数据库与 Domain 改造

### 4.1 Schema migration

当前 `LATEST_SCHEMA_VERSION = 4`。新增：

```text
src/storage/schema/0005_project_kind.sql
```

并将 schema version 升为 5。

`threads` 新增：

```sql
project_kind TEXT NOT NULL
CHECK (project_kind IN ('project', 'projectless', 'unknown'))
```

迁移既有数据时先做兼容 backfill：

```text
project_path 非空 -> project
project_path 为空 -> unknown
```

该 backfill 只保证升级后数据库立即满足约束；下一轮 metadata scan 必须依据 `.codex-global-state.json` 对真实 projectless thread 重新归一化。

不得清空或改写既有 `project_path/project_name`。

### 4.2 Domain

在 `src/domain.rs` 增加类似：

```text
ProjectKind::Project
ProjectKind::Projectless
ProjectKind::Unknown
```

并纳入：

- `ExistingThreadProjection`；
- normalized thread 内部结构；
- `ResolvedThreadPatch`；
- patch validate；
- storage row read/write；
- stable thread equality/postcondition。

`project_kind` 的稳定变化必须被视为 metadata data change。

### 4.3 revision 语义

当前 `src/storage/metadata.rs::commit_metadata()` 已在 normalized `threads` 稳定投影变化时递增 `app_meta.data_revision`。

新增 `project_kind` 后必须保持该机制：

```text
project_kind 改变
    -> stable thread changed
    -> data_revision + 1
    -> 现有 revision publisher / SSE 可观察
```

不新增第二套 project/filter revision。

---

## 5. Scanner / metadata resolver 改造

### 5.1 `CodexMetadata`

当前：

```text
state_index_path
session_index_path
```

扩展为：

```text
state_index_path
session_index_path
global_state_path
```

`CodexMetadata::from_home()`：

```text
state_index_path   = $CODEX_HOME/state_5.sqlite
session_index_path = $CODEX_HOME/session_index.jsonl
global_state_path  = $CODEX_HOME/.codex-global-state.json
```

测试辅助 `with_paths` 同步扩展，禁止测试绕过 global-state 输入。

### 5.2 扫描读取时机

每个 metadata scan round 在解析 normalized thread metadata 前读取一次 global state snapshot，与当前 `state_5` / `session_index` snapshot 一起输入 resolver。

建议扩展：

```text
ResolutionInput {
    state_snapshot,
    session_name_snapshot,
    global_state_snapshot,
    rollout_facts,
    ...
}
```

不需要把 `.codex-global-state.json` 纳入 rollout `source_files`/usage ledger；它是 metadata side-source。

### 5.3 root/subagent 规则

Dashboard 项目筛选按 `usage_events.root_session_id` 对应的 root thread 项目归属过滤。

不得根据 subagent 自己的 cwd/project metadata 决定 Dashboard 项目归属。

因此：

```text
root_session_id -> threads(root).project_kind/project_path
```

是项目筛选唯一 normalized join 入口。

---

## 6. Filter Options API

### 6.1 新接口

新增：

```text
GET /api/usage/filter-options
```

该接口与日期范围无关，返回当前 active usage ledger epoch 的全历史可筛选维度。

建议 response：

```json
{
  "data_revision": 123,
  "models": [
    "gpt-5.6-sol",
    "gpt-5.6"
  ],
  "projects": [
    {
      "kind": "project",
      "project_name": "Usagi",
      "project_path": "<repo>"
    },
    {
      "kind": "projectless"
    },
    {
      "kind": "unknown"
    }
  ]
}
```

特殊项不伪造 `project_path`：

```text
projectless -> UI 文案由前端映射为「无项目会话」
unknown     -> UI 文案由前端映射为「未识别项目」
```

不得用 `__projectless__`、`__unknown__` 等魔法路径冒充真实路径。

### 6.2 模型 options

从当前 `usage_active_epoch` 的 `usage_events` 获取全历史 distinct model：

```text
不加日期范围
不计算 Token SUM
不复用 /api/usage/models
```

只返回存在有效 usage event 的模型。

### 6.3 项目 options

只针对当前 active epoch 中确实存在 usage 的 root session 生成：

```text
usage_events.root_session_id
    JOIN threads.thread_id
```

规则：

- `project_kind=project`：按 `project_path` 去重，返回 `project_name + project_path`；真实 ID 是 `project_path`。
- `project_kind=projectless`：只要存在至少一条对应 usage，返回一个 `projectless` 特殊项。
- `project_kind=unknown`，或 usage root 无法 join 到 thread：返回一个 `unknown` 特殊项。
- projectless thread 的 generated cwd 不得再次以普通项目 option 出现。

### 6.4 snapshot 一致性

`filter-options` 的 `data_revision` 与 options SQL 必须在同一个 SQLite Deferred read transaction 中冻结，模式与现有 `summary_snapshot/models_snapshot` 一致。

---

## 7. Summary 条件筛选

### 7.1 请求参数

扩展：

```text
GET /api/usage/summary
```

保留现有：

```text
range=today|yesterday|week|month|year
```

新增可重复参数：

```text
model=<exact-model>
project_path=<exact-normalized-path>
include_projectless=1
include_unknown_project=1
```

示例：

```text
/api/usage/summary?range=today
  &model=gpt-5.6-sol
  &model=gpt-5.6
  &project_path=%2FUsers%2Fme%2Fdev%2FUsagi
  &include_projectless=1
```

前端没有选中某一维度时，该维度参数必须完全省略；不得发送空字符串参数。

如果 Axum 当前 `Query<T>` 对 repeated values 的解析不能满足契约，后端必须增加明确的 query parser，不得把多选退化成逗号拼接字符串，因为 project path/model 本身不应依赖分隔符转义约定。

### 7.2 筛选逻辑

关系固定：

```text
同一维度内部：OR
不同维度之间：AND
日期范围与条件筛选：AND
```

例：

```text
(model=A OR model=B)
AND
(project_path=P1 OR projectless)
AND
occurred_at_ms in selected range
```

模型必须 exact-match `usage_events.model`，不提供 wildcard/模糊匹配。

### 7.3 聚合 SQL 粒度

Summary 过滤必须在 usage event 聚合前完成：

```text
usage_events ue
LEFT JOIN threads root ON root.thread_id = ue.root_session_id
```

条件：

- model filter 作用于 `ue.model`；
- project path filter 作用于 root `project_kind='project' AND project_path IN (...)`；
- projectless 作用于 root `project_kind='projectless'`；
- unknown 作用于 root `project_kind='unknown'` 或 root thread 缺失；
- root/subagent 的 Token 都继续按 `root_session_id` 归属到 root 项目。

不得先按 Session 聚合后再做 model filter，否则会把同一 Session 中未选模型的 Token 一并带入。

### 7.4 `session_count`

后端继续返回 `session_count`，其计算必须与当前筛选后的 usage event 集合一致：

```text
COUNT(DISTINCT root_session_id)
```

前端是否展示由第 10 节决定。

### 7.5 查询参数归一化

进入 aggregation 层前：

- repeated model 去重并排序；
- repeated project path 去重并排序；
- 空值/控制字符等非法值返回明确 API error；
- 前端 cache key 使用同一 canonical 顺序。

禁止 `[A,B]` 与 `[B,A]` 产生两份语义相同的 snapshot/cache entry。

---

## 8. Aggregate / Ledger 改造

### 8.1 新查询结构

建议新增：

```text
UsageFilter {
    models: Vec<String>,
    project_paths: Vec<String>,
    include_projectless: bool,
    include_unknown_project: bool,
}

SummaryQuery {
    range: TimeRange,
    filter: UsageFilter,
}
```

保持 `sessions()` 和 `/api/usage/sessions` 不接收这些条件；本轮条件筛选不得扩散到 Session 列表。

### 8.2 Filter options snapshot

在 `UsageLedger` 增加类似：

```text
filter_options_snapshot()
```

返回：

```text
UsageSnapshot<FilterOptions>
```

复用当前 active epoch + data revision frozen snapshot 模式。

### 8.3 既有 `/usage/models`

不删除、不修改其既有用途和日期范围语义。

新筛选器 options 不通过 `/usage/models?range=...` 获取。

---

## 9. 前端数据层

### 9.1 DTO

新增 typed DTO：

```text
ProjectFilterOption =
  | { kind: "project"; project_name: string; project_path: string }
  | { kind: "projectless" }
  | { kind: "unknown" }

FilterOptionsResponse {
  data_revision: number
  models: string[]
  projects: ProjectFilterOption[]
}
```

新增筛选状态：

```text
DashboardFilters {
  models: string[]
  projects: ProjectSelection[]
}
```

`ProjectSelection` 必须保留 typed special values，禁止把 special option 编码成假 path。

### 9.2 Client

`usagiClient` 新增：

```text
filterOptions(signal)
summary(range, filters, signal)
```

已有无筛选调用必须保持等价：

```text
summary(range, emptyFilters)
```

产生的 URL 只包含 `range`。

继续解析 Summary 中的 `cache_write_tokens` 和 `reasoning_tokens`；不得因卡片隐藏删除 parser 字段。

---

## 10. Dashboard controller 状态与行为

### 10.1 状态分离

日期与条件筛选保持独立：

```text
range: RangeKey
filters: DashboardFilters
```

不能把日期范围塞进 `DashboardFilters`。

切换 `range`：

- 保留当前 `filters`；
- 请求新 `range + filters` Summary。

修改 filters：

- 保留当前 range；
- 请求新 `range + filters` Summary。

`clear_filters()`：

- 清空全部模型/项目条件；
- 不改变 range。

### 10.2 active 判定

```text
modelFilterActive = filters.models.length > 0
projectFilterActive = filters.projects.length > 0
anyFilterActive = modelFilterActive || projectFilterActive
```

「清除筛选」的可见性只依赖 `anyFilterActive`。

### 10.3 Summary snapshot cache

当前 controller 以 `RangeKey` 保存 snapshot，改为 canonical query key：

```text
DashboardQueryKey = range + canonical(models) + canonical(projects)
```

必须满足：

```text
(today, [A,B], [P1])
==
(today, [B,A], [P1])
```

不同筛选条件不得互相拿 snapshot 作为回退数据。

切换条件时延续当前既有原则：

- 目标 query key 有成功 snapshot：可保留该 query 自己的旧值并显示 loading；
- 没有：显示 skeleton；
- 不允许短暂展示其他条件组合的数据。

### 10.4 请求竞态

筛选变化必须复用/扩展当前 Summary AbortController + generation 机制：

- 新 query 取消旧 Summary request；
- 旧响应晚到不得覆盖新筛选状态；
- revision 触发重取时使用“当前 range + 当前 filters”，不能退回无筛选请求。

---

## 11. Filter options 获取与刷新策略

### 11.1 首次获取

Dashboard mount 时请求一次：

```text
GET /api/usage/filter-options
```

可与首次 Summary/status 并行。

### 11.2 明确禁止的请求触发

以下行为不得重新请求 filter options：

- 打开模型筛选控件；
- 打开项目筛选控件；
- 切换日期范围；
- 选择/取消模型；
- 选择/取消项目；
- 点击「清除筛选」；
- 单纯重复渲染组件。

### 11.3 revision 标脏 + scan 终态刷新

保留：

```text
filterOptionsRevision = 最近成功 options.data_revision
pendingOptionsRevision = 已观察到但尚未刷新 options 的最大 data_revision
```

收到现有 revision/SSE：

```text
new data_revision > filterOptionsRevision
    -> 只标记 options dirty
    -> 不立即请求
```

当一次 scan 生命周期到达终态且没有 active/queued follow-up 后：

```text
pendingOptionsRevision > filterOptionsRevision
    -> 请求 filter-options 一次
```

同一 scan 周期无论产生多少次 `data_revision` 变化，最多刷新一次 options。

scan completed 和 failed 都属于可刷新终态：如果扫描在失败前已经提交稳定 metadata/usage change，不能永久保留旧 options。

### 11.4 options 请求失败

- 不影响已成功加载的 KPI Summary；
- 已有旧 options 时继续保留，不清空；
- 标记 options error/stale，后续显式 retry 或下一次满足刷新条件时重试；
- 不能因为 options 请求失败自动清除当前已选 filters。

### 11.5 已选值从新 options 消失

刷新 options 后，如果某个已选模型/项目不再出现在全历史 options：

- 不得静默删除用户当前选择；
- 当前 Summary 查询语义保持不变，可能返回 0；
- 直到用户手动移除或执行 `clear_filters()`。

具体 UI 如何呈现 stale selection 由本地 Sol 补充。

---

## 12. KPI 展示规则

### 12.1 默认/无模型筛选

Dashboard KPI 固定为：

1. 预估费用
2. 总 Token
3. 输入 Token
4. 输出 Token
5. 会话数量
6. 缓存命中率
7. 缓存读取 Token
8. 推理 Token

不显示「缓存写入 Token」。

### 12.2 模型筛选激活

是否隐藏「会话数量」只由：

```text
modelFilterActive
```

决定。

当至少选中一个模型：

显示：

- 预估费用
- 总 Token
- 输入 Token
- 输出 Token
- 缓存读取 Token
- 缓存命中率
- 推理 Token

隐藏：

- 会话数量

即使同时选中了项目，只要 `modelFilterActive=true`，会话数量仍隐藏。

### 12.3 仅项目筛选激活

只选项目、未选模型时：

- 会话数量继续显示；
- 值为当前日期范围 + 当前项目条件下的 distinct root session count。

### 12.4 reasoning/cache-write

- `reasoning_tokens` 用既有 integer formatter。
- `cache_write_tokens` 继续存在于 API/DTO/解析链，但 MetricGrid 永远不展示该卡片。

---

## 13. 项目筛选语义

### 13.1 普通项目

UI：

- 主显示：`project_name`；
- 真实筛选 ID：`project_path`；
- hover 可显示完整 `project_path`，具体组件由本地 Sol 补充。

同名不同路径必须作为两个不同 option，不得按 `project_name` 合并。

### 13.2 无项目会话

UI 语义：

```text
无项目会话
```

后端语义：

```text
threads(root).project_kind = 'projectless'
```

不得使用 generated cwd 作为该特殊选项 ID。

### 13.3 未识别项目

UI 语义：

```text
未识别项目
```

后端语义：

```text
threads(root).project_kind = 'unknown'
OR root thread projection missing
```

这类数据与 projectless 严格区分。

---

## 14. UI 复刻规格（1512px 基准）

### 14.1 基准与字体资源

所有本节参数均以 Vibe Usage 在 `1512px` 浏览器内容视口宽度下的实际计算样式为准。实现和 browser visual regression 必须使用同一宽度；不得用其他宽度下的响应式结果替代桌面验收结果。

全局字体栈固定为：

```css
"JetBrains Mono", "JetBrains Mono Fallback", ui-monospace,
SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono",
"Courier New", "PingFang SC", "Microsoft YaHei", monospace
```

当前 `JetBrainsMono-Regular.woff2` 不能继续被声明为同时覆盖 400–700。前端必须打包具有真实 400/500/700 字形的 JetBrains Mono 静态字体，或打包覆盖这些字重的官方 variable font，并保留现有 OFL 许可文件。`font-synthesis: none` 继续保留；标题和 KPI 数值不得使用 Regular 字形、文字描边、阴影或浏览器伪粗体模拟 700。

### 14.2 文字参数

除表中明确例外，所有文字均使用第 14.1 节字体栈、正常字形和 `letter-spacing: normal`。

| 分区 | 字号 | 字重 | 行高 | 字距 | 颜色 |
|---|---:|---:|---:|---:|---|
| Dashboard 标题 | 30px | 700 | 36px | normal | `#09090b` |
| 「同步数据」按钮 | 12px | 500 | 16px | normal | `#52525b` |
| 时间筛选选项 | 12px | 400 | 16px | normal | 未选 `#52525b`；选中 `#ffffff` |
| KPI 标签 | 14px | 400 | 20px | normal | `#52525b` |
| KPI 数值 | 24px | 700 | 32px | normal | 默认 `#09090b`；预估费用 `#34d399` |
| Session记录标题 | 16px | 500 | 20px | normal | `#52525b` |
| Session 列表表头 | 12px | 500 | 16px | 0.6px | `#52525b` |
| 最后活动、标题、项目、模型正文 | 14px | 400 | 20px | normal | `#09090b` |
| 输入 Token、输出 Token、推理 Token、缓存命中率正文 | 14px | 500 | 20px | normal | `#09090b` |
| 预估费用正文 | 14px | 400 | 20px | normal | `#34d399` |

Dashboard 标题必须移除当前 `-0.02em` 字距。Session 表头继续保持左侧文字列左对齐、数值列右对齐和 tabular numbers；本轮只复刻文字层级，不新增可点击表头、排序状态或上下箭头。

### 14.3 时间与条件筛选栏布局

时间筛选组与条件筛选组位于同一流式行：

```css
display: flex;
flex-wrap: wrap;
align-items: center;
gap: 8px;
```

`8px` 是 flex item 之间的 gap，不是固定坐标、固定栏宽或额外 margin。空间不足时按 flex-wrap 自然换行。

条件筛选组只包含「模型」「项目」两个入口，顺序固定为模型在前、项目在后；不得新增 Vibe Usage 的工具或终端筛选。

### 14.4 模型/项目筛选触发器

两个触发器复用 Vibe Usage 的胶囊控件：

- 高度 `28px`，`padding: 6px 12px`，内部 `gap: 6px`，全圆角；
- 文字 `12px / 400 / 16px`；
- 模型使用 `12px` 芯片图标，项目使用 `12px` 文件夹图标，末尾使用 `10px` 下拉箭头；
- 无选择时背景 `#e2e3e7`，主标签/图标 `#52525b`，状态文字「全部」为 `#71717a`；
- 有选择时背景 `#18181b`，主标签/图标为 `#ffffff`，状态文字显示「N 项」且为 `#a1a1aa`；
- 展开时箭头旋转 180°，收起时恢复；
- hover/focus/disabled 必须沿用同一色阶，不引入新的强调色。

### 14.5 多选弹层与交互

模型和项目使用同一种多选弹层：

- 位于触发器下方 `6px`；宽度至少 `192px`、最大 `288px`、最大高度 `288px`；
- 白色背景、`1px #e4e4e7` 边框、`8px` 圆角、与 Vibe Usage 一致的下拉阴影，纵向溢出时滚动；
- option 高度 `32px`，`padding: 8px 12px`，文字 `12px / 400 / 16px / #52525b`；
- option 左侧为 `14px × 14px` 复选框；未选为透明底加边框，选中为 `#18181b` 底和白色勾；
- 点击 option 后立即更新当前 filters 和 Summary，弹层保持打开；点击外部或按 `Escape` 关闭；
- 触发器状态同步显示「全部」或「N 项」；同一筛选器支持连续选择/取消多个 option；
- 模型弹层提供可展开的「GPT」父级，子模型左侧缩进到 `28px`；父级支持全选/取消全部子模型，并正确显示未选、部分选择、全选三种状态；
- 项目弹层平铺普通项目、「无项目会话」和「未识别项目」；普通项目主文字仍使用 `project_name`，完整 `project_path` 只作为真实筛选 ID 和 hover 信息；
- options 刷新后已选值消失时，继续在弹层中保留同样的已选行和查询语义，直到用户主动取消或执行 `clear_filters()`。

任一条件激活时，在条件筛选组末尾显示「清除筛选」按钮：`padding: 4px 8px`，文字 `12px / 400 / 16px`，颜色 `#fb7185`，透明背景、无边框。点击后只清空模型/项目并重新请求当前日期范围 Summary，不改变日期范围，也不请求 filter options。

### 14.6 Session 列表边界

本轮只调整第 14.2 节规定的 Session 标题、表头和正文文字样式。Session API、服务端分页、cursor、默认最后活动顺序和现有列表交互全部保持不变；表头仍为静态文本，不实现点击排序。

---

## 15. 推荐实施顺序

### 分批测试规则

S1–S9 按三个开发批次连续实施。每个批次内必须先完成全部阶段及静态检查，再集中运行一次对应 Gate；禁止在完成一个小点、一个文件或单个 S 阶段后立即运行正式测试。Gate 失败时只修复当前批次并重跑该 Gate，不提前运行后续 Gate。

| 开发批次 | 阶段组合 | 批次完成后运行的测试条目 | 进入下一批条件 |
|---|---|---|---|
| Gate A | S1–S3 | `T-S01-001`～`T-S03-002` | Gate A 全部 PASS |
| Gate B | S4–S6 | `T-S04-001`～`T-S06-002` | Gate B 全部 PASS |
| Gate C | S7–S9 | `T-S07-001`～`T-S09-003` | Gate C 全部 PASS |
| Gate D | S10 | `T-S10-001`～`T-S10-002`，并重跑前三批全部条目及 Spec01–06 受影响回归 | Gate D 全部 PASS |

### S1：Schema + Domain

1. 新增 schema v5 `project_kind`。
2. 完成 migration/backfill。
3. Domain/patch/storage projection 全链路加入 `ProjectKind`。
4. 保证 project_kind stable change 推进 `data_revision`。

### S2：Codex global-state adapter

1. 新增 `src/codex/global_state.rs`。
2. typed parse `projectless-thread-ids`、`thread-project-assignments`。
3. 增加 status/diagnostic，不泄露原始内容。
4. 扩展 `CodexMetadata` 和测试 fixture。

### S3：Metadata resolver

1. `ResolutionInput` 接收 global state snapshot。
2. 按第 3 节规则归一化 `project_kind`。
3. conflict/unavailable 行为固定。
4. projectless 不清除 cwd/project_path。

S3 完成即到达 Gate A；此前不单独测试 S1、S2 或 S3。

### S4：Aggregate filter model

1. 新增 `UsageFilter/SummaryQuery`。
2. Summary totals/session_count SQL 同时支持 model + project 条件。
3. 项目条件始终通过 root thread join。
4. 保持 Session 列表查询完全不受影响。

### S5：Filter options aggregate/API

1. 新增 active-epoch 全历史 option query。
2. 新增 snapshot + `GET /api/usage/filter-options`。
3. 添加 typed response/parser。
4. 普通项目按 path 去重；projectless/unknown 为特殊 typed option。

### S6：Summary API 参数

1. 支持 repeated model/project path + two special flags。
2. canonicalize/validate。
3. 无 filter 参数时保持现有 Summary 行为和 response schema。

S6 完成即到达 Gate B；此前不单独测试 S4、S5 或 S6。

### S7：Frontend data/controller

1. 新增 FilterOptions DTO/client parser。
2. 新增独立 `filters` state。
3. Summary 请求带 filter。
4. snapshot cache key 改为 query key。
5. 实现 clear filters、range 保留 filters。
6. 实现 options mount-once + dirty-on-revision + terminal refresh-once。

### S8：KPI 调整

1. 从 MetricGrid 移除缓存写入卡片。
2. 新增推理 Token 卡片。
3. 依据 `modelFilterActive` 控制会话数量卡片。
4. 不改变其他 KPI formatter 和 Token 口径。

### S9：UI 组件补充

按第 14 节实现已确认的 Vibe Usage 复刻规格：

- 真实 JetBrains Mono 400/500/700 字重资源和分区文字参数；
- 模型/项目多选触发器、弹层、复选框、分组与计数状态；
- hover project path；
- 「清除筛选」按钮布局；
- loading/error/stale option 的具体视觉表达；
- 响应式细节。

S9 不得改变本文确定的数据/API/状态语义，不得增加工具/终端筛选或 Session 表头排序。

S9 完成即到达 Gate C；此前不单独测试 S7、S8 或 S9。

### S10：Gate D 与最终回归

只在 Gate A、Gate B 和 Gate C 全部通过后进入 S10。严格执行 `T-S10-001`～`T-S10-002`，然后重跑本轮 20 条全部测试及既有 Spec01–06 受影响回归。S10 是唯一的全量测试点；不得通过删除断言、跳过测试、增加 fallback/dual-read 来制造通过。

---

## 16. 明确禁止

1. 不删除 `cache_write_tokens` 后端/数据库/DTO/parse 链。
2. 不新增消息数量统计。
3. 不让条件筛选影响 Session 列表。
4. 不按 `project_name` 作为真实项目 ID。
5. 不把 projectless 等价为 `project_path IS NULL`。
6. 不按 `~/Documents/Codex/...` 等路径模式猜 projectless。
7. 不用魔法字符串伪造特殊 project path。
8. 不在前端拉 Session 分页后自行计算 KPI。
9. 不在每次下拉点击/日期变化/筛选变化时请求 filter options。
10. 不把 filter options 塞入 Summary response。
11. 不删除或改变现有 `/api/usage/models` 语义。
12. 不对模型做 wildcard/substring 匹配。
13. 不在项目归属源暂时不可读时批量覆盖已有可靠 `project_kind`。
14. 不通过旧逻辑 fallback、dual-read 或兼容旧字段长期并存绕过正式迁移。
15. 不新增工具/终端筛选，不新增 Session 表头排序参数、排序游标、可点击表头或上下箭头。
16. 不用 Regular 字形冒充 500/700，也不用描边、阴影或伪粗体代替真实 JetBrains Mono 字重资源。

---

## 17. 验收标准

以下全部满足才可验收：

1. Dashboard 永久不显示「缓存写入 Token」卡片，但 `cache_write_tokens` 原有后端与前端解析链完整保留。
2. Dashboard 正确显示现有 `reasoning_tokens`，无需新增/重算 reasoning 数据。
3. 本轮代码不存在总消息数/用户消息数的新统计实现。
4. 模型/项目均可形成多选查询；同维度 OR、跨维度 AND。
5. 条件筛选只改变 KPI Summary；Session 列表请求、分页、内容均不受影响。
6. 切换日期范围保留模型/项目条件；「清除筛选」不改变日期。
7. 只要模型筛选激活就隐藏会话数量；仅项目筛选时会话数量继续显示且值正确。
8. 普通项目以 `project_path` 为真实 ID，`project_name` 仅用于显示；同名不同路径不合并。
9. Codex Desktop projectless 能由明确 metadata 身份筛出，即使其 `project_path/cwd` 非空。
10. 「无项目会话」与「未识别项目」完全分离；generated cwd 不作为 projectless 判定依据。
11. projectless thread 的真实 `project_path` 不因分类而被清除。
12. filter-options 为 active epoch 全历史维度，不受当前日期范围影响。
13. 首次进入 Dashboard 获取一次 options；打开下拉、切日期、选择筛选、点击「清除筛选」不会额外请求 options。
14. data revision 变化只标脏 options；同一扫描周期终态后最多刷新一次 options。
15. 项目归属 metadata 变化会推进现有 `data_revision` 并能触发后续 options 更新。
16. Summary filter 在 usage-event 粒度过滤，模型筛选不会混入同 Session 中其他模型的 Token。
17. project filter 按 root session 项目归属过滤，subagent Token 仍正确归属于 root project。
18. 无筛选时 Summary 与当前版本语义完全一致。
19. 旧请求晚到不能覆盖当前 range/filter 的新状态。
20. 新增 migration、Rust/TS 单元与 integration/browser 测试全部通过，且既有受影响 Spec01–06 回归通过。
21. `1512px` 下 Dashboard 标题、同步按钮、时间筛选、KPI、Session 标题/表头/正文的字体、字号、字重、行高、字距和颜色符合第 14.2 节；标题使用真实 JetBrains Mono 700 字形。
22. 条件筛选栏只出现模型和项目，按 `8px` flex gap 流式排列；触发器、弹层、复选框、计数和清除按钮符合第 14.3–14.5 节。
23. 模型父级和子项多选、项目平铺多选、弹层保持打开、外部点击/Escape 关闭及立即刷新 Summary 的行为与 Vibe Usage 一致。
24. Session 列表仅改变文字样式；API、分页、cursor、默认顺序和表头静态行为保持现状，不出现新排序交互。

本轮新增/变更行为的唯一测试条目依据为：

```text
Usagi_Dashboard_KPI与条件筛选测试标准_v0.1.md
```
