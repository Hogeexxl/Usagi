# Usagi Spec 06-02：Session 记录列表

> 版本：v0.2  
> 状态：当前契约修订版  
> 更新日期：2026-08-09  
> 测试标准：`Usagi_测试标准_Spec01-06_v0.17.md`  
> 直接依赖：`Spec_05_查询API与更新通知_v0.2.md`、`Spec_06_01_前端框架与Dashboard界面_v0.2.md`  
> 数据口径：`Usagi_Codex本地数据口径_v0.2.md`  
> 上游实现：Spec 01～04  
> 当前版本说明：用量图表不在本版本范围内；本 Spec 只增加 Session 记录列表

---

## 1. 实现范围

在 Spec 06-01 已完成的单页 Dashboard 中，直接在 KPI 网格下方增加 `Session记录` 区域，并消费 Spec 05 已定义的：

```text
GET /api/usage/sessions
```

本 Spec 完成以下能力：

1. Session 表格结构、列定义、视觉布局与响应式行为；
2. 当前时间范围对应的 Session 首屏加载；
3. 基于 `next_cursor` 的“加载更多”分页；
4. `data_revision` 变化后的整表重取；
5. `STALE_CURSOR` / `INVALID_CURSOR` 的自动恢复；
6. range 切换、分页、revision 更新并发时的竞态保护；
7. loading、refreshing、empty、首屏失败、分页失败状态；
8. 标题、项目、模型、时间、Token、命中率和费用的固定展示规则；
9. Session API DTO、运行时校验和组件测试；
10. 与 Spec 01～06-01 的数据口径、revision、分页和隐私契约保持一致。

页面结构固定为：

```text
Dashboard
├─ 标题 / 同步数据
├─ 时间范围
├─ KPI 网格
└─ Session记录
   └─ SessionTable
```

中间不插入模型用量图、Session 用量图、空图表卡片或 mock 图表占位。

---

## 2. 前端模块与 seam

### 2.1 文件布局

在 Spec 06-01 的现有结构上增加：

```text
frontend/src/
  dashboard/
    DashboardPage.tsx
    useDashboardController.ts
    session/
      SessionSection.tsx
      SessionTable.tsx
      SessionTableRow.tsx
      SessionTableSkeleton.tsx
      SessionTableFooter.tsx
      useSessionTableController.ts
      sessionFormat.ts
      sessionTypes.ts
  data/
    usagiClient.ts
```

职责固定：

| 文件 | 职责 |
|---|---|
| `SessionSection.tsx` | 区域标题、table surface、状态组合 |
| `SessionTable.tsx` | `<table>`、表头、tbody、空状态 |
| `SessionTableRow.tsx` | 单行字段映射与可访问性 |
| `SessionTableSkeleton.tsx` | 首屏无缓存时的同高 skeleton 行 |
| `SessionTableFooter.tsx` | 加载更多、分页失败重试 |
| `useSessionTableController.ts` | range snapshot、首屏请求、分页、revision、竞态 |
| `sessionFormat.ts` | 时间、模型、fallback 文案；复用 06-01 数值 formatter |
| `sessionTypes.ts` | Session DTO 与 view model |
| `usagiClient.ts` | 增加 sessions 请求与 DTO 校验 |

不得在 React view 中：

- 直接拼 HTTP URL；
- 解析 cursor；
- 重算 Token；
- 重算 Session/root/Subagent 关系；
- 根据 `self_usage` 或 `subagent_usage` 重新得到 `inclusive_usage`；
- 根据 title/project/model 猜测后端缺失事实。

### 2.2 复用 06-01 的 revision 链

Spec 06-01 已经建立 SSE `/api/events` + 60 秒 `/api/revision` 断线恢复轮询。06-02 不允许再建立第二条 EventSource 或第二个 polling timer，也不修改 `DashboardViewModel`、range 接口或服务端 revision 契约。

实施时把 06-01 现有 revision transport 从 controller 内部抽成 data 层共享模块：

```text
frontend/src/data/revisionFeed.ts
```

推荐 seam：

```text
RevisionTuple {
  data_revision: number
  status_revision: number
}

RevisionFeed {
  subscribe(listener) -> unsubscribe
  get_snapshot() -> RevisionTuple | null
  retry_now()
}
```

实现要求：

1. `revisionFeed` 在一个页面进程内只有一个 transport 实例；
2. 第一个订阅者出现时建立 EventSource；
3. 最后一个订阅者释放时关闭 EventSource、清 timer、abort revision fetch；
4. SSE error 后只建立一个 60 秒断线恢复轮询 timer；
5. EventSource 恢复并收到有效 tuple 后停止断线恢复轮询 timer；
6. `retry_now()` 复用同一个 `/api/revision` 请求 seam，不创建新的长期 timer；
7. tuple 按两个分量分别单调接受：新 tuple 必须满足 `data_revision >= last.data_revision` 且 `status_revision >= last.status_revision`；至少一个分量更大才发布，乱序旧响应直接丢弃；
8. `useDashboardController` 改为订阅该 feed，并保持 06-01 原有 summary/status/refresh 行为；
9. `useSessionTableController` 订阅同一个 feed，但只关心 `data_revision`；
10. 这是 06-01 revision transport 的内部提取，不改变其公开 view model、用户状态文案或 HTTP 接口。

调用关系：

```text
revisionFeed
  ├─ useDashboardController()
  │    └─ 按 06-01 规则重取 summary/status
  │
  └─ useSessionTableController({ range })
       └─ data_revision 增长时重取 Session 第一页
```

Session controller 把 feed 的 `data_revision` 作为“有更新需要追平”的单调提示：

- `feed.data_revision > Session snapshot.data_revision` → 重取第一页；
- `feed.data_revision == Session snapshot.data_revision` → 不请求；
- `feed.data_revision < Session snapshot.data_revision` → 不倒退重取，因为 Session 请求可能已经冻结到更晚的 SQLite snapshot。

---

## 3. Session API 契约与前端 DTO

### 3.1 请求

首屏：

```text
GET /api/usage/sessions?range=<range>&limit=50
```

下一页：

```text
GET /api/usage/sessions?range=<range>&limit=50&cursor=<opaque cursor>
```

固定规则：

```text
range = today | yesterday | week | month | year
limit = 50
```

第一版前端固定 `limit=50`，不提供每页数量设置。

cursor 对前端完全 opaque：

- 不 decode；
- 不修改；
- 不持久化到 localStorage/sessionStorage；
- 不放入 URL；
- 不跨 range 复用；
- 页面 reload 后从第一页开始。

### 3.2 TypeScript DTO

`usagiClient.ts` 增加精确 DTO：

```text
UsageDto {
  input_tokens: number
  cached_tokens: number
  cache_write_tokens: number | null
  uncached_input_tokens: number | null
  output_tokens: number
  reasoning_tokens: number
  other_output_tokens: number
  total_tokens: number
  cache_hit_rate: number | null
  estimated_cost: number | null
}

SessionItemDto {
  root_session_id: string
  title: string | null
  project_name: string | null
  project_path: string | null
  last_activity_at_ms: number
  models_used: string[]
  subagent_count: number
  inclusive_usage: UsageDto
  self_usage: UsageDto
  subagent_usage: UsageDto
}

SessionPageDto {
  range: {
    key: "today" | "yesterday" | "week" | "month" | "year"
    start_ms: number
    end_ms: number
    timezone: string
  }
  data_revision: number
  items: SessionItemDto[]
  next_cursor: string | null
}
```

运行时校验至少检查：

1. `range.key` 与请求 range 一致；
2. `data_revision` 是非负安全整数；
3. `root_session_id` 是非空字符串；
4. `last_activity_at_ms` 是安全整数；
5. `models_used` 是字符串数组；
6. `subagent_count` 是非负安全整数；
7. Token required 字段是 `0..=Number.MAX_SAFE_INTEGER` 的整数；
8. `cache_hit_rate` 为 `null` 或 `0..1` 的有限数值；
9. `next_cursor` 为 `null` 或非空字符串；
10. 当前 v1 `estimated_cost` 必须为 `null`；
11. 同一 response 内不得出现重复 `root_session_id`。

DTO 校验失败统一归入前端安全错误，不把原始 response body 显示给用户。

### 3.3 列表只使用 inclusive usage

Session 表格所有 Token/比例/费用列只读取：

```text
item.inclusive_usage
```

第一版表格不直接展示：

```text
self_usage
subagent_usage
subagent_count
root_session_id
project_path
cache_write_tokens
cached_tokens
uncached_input_tokens
other_output_tokens
total_tokens
```

其中：

- `self_usage` / `subagent_usage` 保留给后续 Session 详情；
- `subagent_count` 当前不新增独立列；
- `root_session_id` 只作为 React key 和内部身份；
- `project_path` 只可作为项目名称的本机辅助 title/tooltip；
- 其他未显示 Token 字段仍由 API 返回，但本表不自行增加设计外列。

---

## 4. Session 表列定义

### 4.1 列顺序

固定为：

| 顺序 | 表头 | 数据来源 | 展示规则 |
|---:|---|---|---|
| 1 | 最后活动 | `last_activity_at_ms` | 本机时间格式 |
| 2 | 标题 | `title` | null → `未命名 Session` |
| 3 | 项目 | `project_name` | null/空 → `未识别项目` |
| 4 | 模型 | `models_used` | 单模型原样；多模型首项 + `+N` |
| 5 | 输入 Token | `inclusive_usage.input_tokens` | compact integer |
| 6 | 输出 Token | `inclusive_usage.output_tokens` | compact integer |
| 7 | 推理 Token | `inclusive_usage.reasoning_tokens` | compact integer |
| 8 | 缓存命中率 | `inclusive_usage.cache_hit_rate` | 百分比；null → `—` |
| 9 | 预估费用 | `inclusive_usage.estimated_cost` | 当前固定 `—` |

设计稿中的以下旧文案在实现时使用统一数据口径名称：

```text
上次更新       → 最后活动
第二个“输出 Token” → 推理 Token
```

本表使用“输入 Token”，不使用“写入 Token”。

### 4.2 排序

前端不提供表头排序按钮。

展示顺序完全接受 Spec 05 服务端顺序：

```text
last_activity_at DESC,
root_session_id ASC
```

前端不得：

- 按标题重新排序；
- 按 Token 大小重新排序；
- 将不同页分别排序后再拼接；
- 将模型、项目或费用作为客户端排序键。

### 4.3 标题

```text
title != null && trim(title) != ""
  → 原样显示
else
  → 未命名 Session
```

规则：

- 单行显示；
- 超出列宽使用 ellipsis；
- `title` attribute / accessible name 保留完整标题；
- 不从 `root_session_id`、项目路径或对话正文生成新标题；
- 不把 Subagent 名称拼进标题。

### 4.4 项目

```text
project_name != null && trim(project_name) != ""
  → project_name
else
  → 未识别项目
```

`project_path` 非空时：

- 可作为该单元格的 `title`；
- 不单独增加路径列；
- 不在错误日志、analytics 或远程请求中发送。

项目名称单行 ellipsis。

### 4.5 模型

`models_used` 按 API 给出的稳定顺序展示。

规则：

```text
[]                 → unknown
["gpt-5.6"]        → gpt-5.6
["gpt-5.6","x"]    → gpt-5.6 +1
["a","b","c"]      → a +2
```

完整模型数组放入 `title` / accessible description，使用 `, ` 连接。

这里的首项只表示 API 返回的“当前范围第一次产生有效用量的模型顺序首项”，06-02 不把它声称为“主 Agent 模型”。当前 API 没有提供可证明的主 Agent 模型字段。

### 4.6 Token

输入、输出、推理全部复用 Spec 06-01 的整数 formatter：

```text
0..999  → 原始十进制整数
>=1000  → K/M/B，一位小数，移除 .0
```

例如：

```text
999       → 999
1_000     → 1K
1_250     → 1.3K
574_300_000 → 574.3M
```

每个 compact 数值必须：

- `title` 保留完整十进制整数；
- accessible name 能读出完整整数；
- 不把 compact 字符串回写 data state。

### 4.7 缓存命中率

```text
null → —
否则 → ratio * 100，最多一位小数 + %
```

例如：

```text
0.97      → 97%
0.9454545 → 94.5%
```

不显示为 Session 内各 Thread 命中率平均值；前端只格式化 API 已返回的结果。

### 4.8 费用

当前版本：

```text
estimated_cost = null
```

因此每一行固定显示：

```text
—
```

formatter 仍复用 06-01 的费用预留逻辑，但 06-02 不引入价格表或本地费用计算。

### 4.9 最后活动时间

输入只使用：

```text
last_activity_at_ms
response.range.timezone
```

所有行按该 Session page response 的 IANA `range.timezone` 格式化，保证显示日历边界与服务端 range 完全一致。

显示格式：

```text
在该 timezone 的同一自然日      HH:mm
在该 timezone 的同一年其他日期  MM-DD HH:mm
跨年                            YYYY-MM-DD HH:mm
```

完整时间放入 `title`，格式：

```text
YYYY-MM-DD HH:mm:ss
```

实现使用 `Intl.DateTimeFormat(..., { timeZone: response.range.timezone })`，formatter 不自行换算 UTC offset。

约束：

- 不使用文件 mtime；
- 不使用当前 render 时间推算“3分钟前”一类相对文案；
- 不启动每分钟 timer；
- 不使用 `Date.parse` 解析后端字符串，因为 API 已给毫秒整数；
- timezone formatter 构造失败视为前端数据格式错误，不悄悄改用另一时区。

---

## 5. 视觉与布局

### 5.1 区域位置

`Session记录` 紧接 KPI 网格。

固定垂直关系：

```text
KPI 网格结束
  ↓ 32px
Session记录标题
  ↓ 12px
Session table surface
```

不为本版本未实现的两个用量图保留垂直空白。

### 5.2 区域标题

```text
Session记录
```

样式：

- 16px / 24px；
- font-weight 500；
- 主文字色；
- 不显示总条数徽标；
- 不增加筛选器、搜索框、导出按钮或设置按钮。

### 5.3 Table surface

外层：

```text
background: #ffffff
border: 1px solid #e4e4e7
border-radius: 8px
box-shadow: none
overflow: hidden
```

内部增加一个专门的横向滚动容器：

```text
overflow-x: auto
overflow-y: visible
```

页面 body 自身仍不得产生水平滚动。

### 5.4 表格宽度

使用语义化 `<table>`，第一版固定：

```text
width: 100%
min-width: 1232px
table-layout: fixed
```

推荐 `colgroup`：

```text
最后活动      128px
标题          auto（1512px 基准下约 300px）
项目          150px
模型          150px
输入 Token    120px
输出 Token    120px
推理 Token 136px
缓存命中率    112px
预估费用       96px
```

当内容区宽度小于 `1232px`：

- 只允许 table wrapper 局部横向滚动；
- 不压缩 Token 表头到多行；
- 不隐藏列；
- 不把桌面表格改成卡片列表；
- 不造成页面级水平滚动。

### 5.5 表头

`thead`：

- 高度 40px；
- 左右 padding 16px；
- 12px / 16px；
- font-weight 500；
- 次文字色；
- 下边框 `#e4e4e7`；
- 白色背景；
- 不 sticky；
- 无排序图标。

文本列左对齐：

```text
最后活动 / 标题 / 项目 / 模型
```

数值列右对齐：

```text
输入 Token / 输出 Token / 推理 Token / 缓存命中率 / 预估费用
```

### 5.6 数据行

普通数据行：

- 最小高度 48px；
- 左右 padding 16px；
- 13px / 20px；
- 主文字色；
- 行间使用 1px 下边框；
- 最后一行不重复画底边框；
- hover 只允许使用非常轻的辅助底色；
- 不使用阴影、位移、缩放。

Token/比例/费用使用：

```css
font-variant-numeric: tabular-nums;
```

标题、项目、模型单元格：

```css
white-space: nowrap;
overflow: hidden;
text-overflow: ellipsis;
```

### 5.7 小屏幕

`<768px` 时：

- 保留 06-01 页面 16px 内容 padding；
- Session 区域占满内容宽；
- table 仍为 1232px 最小宽；
- 仅 table wrapper 横向滚动；
- 标题不横向滚动；
- “加载更多”footer 固定在 table 内容宽度之外的 surface 容器宽度中，始终可见，不要求横向滚到最右侧才能操作。

---

## 6. Session controller 状态模型

### 6.1 按 range 保存 snapshot

controller 为每个 range 最多保存一份当前进程内成功 snapshot：

```text
SessionRangeSnapshot {
  range
  timezone
  data_revision
  rows
  next_cursor
}
```

只保存在 React/controller 内存，不写 localStorage、IndexedDB 或文件。

切回一个已加载 range 时：

1. 可立即展示该 range 自己的旧 snapshot；
2. 同时请求该 range 第一页追平当前数据；
3. 不得展示其他 range 的 rows。

### 6.2 状态

```text
SessionLoadState =
  initial
  | loading
  | ready
  | refreshing
  | error

SessionPageState =
  idle
  | loading
  | error
```

view model：

```text
SessionTableViewModel {
  rows
  timezone
  load_state
  page_state
  has_more
  error_code?
  page_error_code?
  retry_load()
  load_more()
  retry_load_more()
}
```

`has_more` 固定：

```text
next_cursor != null
```

### 6.3 request generation

首屏/整表刷新使用：

```text
first_page_generation
first_page_abort_controller
```

分页使用：

```text
load_more_generation
load_more_abort_controller
```

range 改变或要求整表重取时：

1. `first_page_generation += 1`；
2. abort 旧首屏请求；
3. `load_more_generation += 1`；
4. abort 旧分页请求；
5. 清除当前 active `next_cursor` 的使用资格；
6. 发出新 range 的第一页请求。

任何响应只有同时满足以下条件才能进入 state：

```text
response.range.key == current range
request generation == current generation
request 没有被逻辑取消
```

### 6.4 首屏加载

首次进入一个从未成功加载的 range：

```text
load_state = loading
rows = []
GET first page
```

成功：

```text
snapshot = {
  range,
  timezone: response.range.timezone,
  data_revision: response.data_revision,
  rows: response.items,
  next_cursor: response.next_cursor
}
load_state = ready
page_state = idle
```

失败：

```text
load_state = error
rows = []
```

### 6.5 有缓存时刷新

当前 range 已有成功 snapshot：

```text
load_state = refreshing
旧 rows 保持显示
GET first page
```

成功后一次性替换：

```text
rows
timezone
data_revision
next_cursor
```

不得把新第一页 append 到旧 rows。

失败时：

- 保留旧 rows；
- `load_state = error`；
- 在表格底部显示轻量错误与“重试”；
- 不清空旧表；
- 不把旧 snapshot 标记为新 revision。

### 6.6 revision 触发

Session controller 订阅共享 `revisionFeed`。

当：

```text
feed.data_revision exists
AND
current Session snapshot exists
AND
feed.data_revision > snapshot.data_revision
```

触发第一页重取。

如果 Session 自己刚取得更高 revision：

```text
snapshot.data_revision > feed.data_revision
```

不重取，不倒退。

如果当前没有 Session snapshot，controller 正常执行首次加载，不等待 revisionFeed 首个 tuple。

同一个 feed revision 只触发一次正在进行的追平；不能因为 React render 或多个订阅通知重复请求。

### 6.7 加载更多

只有以下条件全部满足才允许：

```text
load_state == ready
page_state == idle
next_cursor != null
没有 first-page refreshing
```

点击：

```text
page_state = loading
GET /sessions?...&cursor=<current next_cursor>
```

成功前保存本次请求使用的：

```text
base_range
base_data_revision
base_next_cursor
```

响应接受条件：

```text
current range == base_range
current snapshot.data_revision == base_data_revision
current snapshot.next_cursor == base_next_cursor
response.data_revision == base_data_revision
generation 仍为最新
```

满足后：

```text
rows = old rows + response.items
next_cursor = response.next_cursor
page_state = idle
```

追加前验证：

- 新页不得包含当前 rows 已有的 `root_session_id`；
- 新页内部不得重复 `root_session_id`。

重复表示协议异常，不能静默 dedupe 后继续。

### 6.8 分页 cursor 失效恢复

以下两种分页错误：

```text
409 STALE_CURSOR
400 INVALID_CURSOR
```

都视为“当前已加载多页 snapshot 无法继续安全追加”。

处理：

1. 不显示“cursor 无效”给用户；
2. abort/失效当前分页 generation；
3. 保留旧 rows 作为 stale 内容；
4. 自动从第一页重新请求当前 range；
5. 成功后原子替换为新第一页并得到新 cursor；
6. 自动恢复最多执行一次；
7. 若第一页重取失败，再进入普通 `load_state=error` 并提供“重试”。

原因是 cursor 可能因：

- `data_revision` 变化；
- range 边界变化；
- 本地服务进程重启导致 HMAC 密钥变化；

而失效。前端不得尝试修复 cursor。

### 6.9 普通分页失败

除 `STALE_CURSOR` / `INVALID_CURSOR` 以外的 load-more 失败：

```text
旧 rows 保持
旧 next_cursor 保持
page_state = error
```

显示：

```text
加载更多失败    重试
```

`retry_load_more()` 使用同一 snapshot 的当前 `next_cursor` 发起新请求。

如果重试前 `revisionFeed` 已收到更高 `data_revision`，优先执行整表第一页刷新，不再继续旧 cursor。

### 6.10 与同步按钮的关系

06-02 不直接读取 refresh target，也不自己调用 `/api/status`。

同步完成的数据更新链固定为：

```text
Spec 06-01 追踪 refresh target 完成
→ 服务端 data_revision 变化并由 SSE / 断线恢复轮询暴露
→ revisionFeed 接受更高 tuple
├─ 06-01 重取 summary
└─ 06-02 重取 Session 第一页
```

扫描进行中、rebuild 中或扫描失败时：

- Session 表可以继续展示旧 stable snapshot；
- 不显示伪造空表；
- 不根据 `scan_state` 清空 rows。

---

## 7. Loading、Empty 与错误展示

### 7.1 首次 loading

表头立即渲染。

tbody 渲染 6 行 skeleton：

- 每行 48px；
- 与真实列宽一致；
- skeleton `aria-hidden=true`；
- table surface 尺寸稳定；
- 不填充假 Token 或假标题。

table wrapper：

```text
aria-busy=true
```

### 7.2 Empty

首屏成功且：

```text
items.length == 0
next_cursor == null
```

显示单个跨 9 列状态行：

```text
当前时间范围暂无 Session 记录
```

状态区最小高度 192px，使空表不会塌成只有表头。

不显示：

- `0 个 Session`；
- 假行；
- 引导创建 Session；
- 同步按钮副本。

### 7.3 Refreshing

有旧 rows 时刷新第一页：

- 旧 rows 保持；
- table `aria-busy=true`；
- 不覆盖 skeleton；
- 可在 `Session记录` 标题右侧显示小型纯文本 `更新中…`；
- 不改变 table 高度；
- 不禁用页面顶部时间范围控件；
- 禁用“加载更多”，直到新第一页完成。

### 7.4 首屏失败

没有旧 snapshot：

```text
Session 记录加载失败    重试
```

错误状态仍保留表头，tbody 状态区最小高度 192px。

`retry_load()` 只重取当前 range 第一页。

### 7.5 有旧数据时刷新失败

保留 rows，在 footer 显示：

```text
Session 记录更新失败    重试
```

不弹 toast，不清空行，不把整个 Dashboard 变成错误页。

### 7.6 加载更多 footer

`next_cursor != null` 且无错误：

```text
[ 加载更多 ]
```

loading：

```text
加载中…
```

分页失败：

```text
加载更多失败    [ 重试 ]
```

无 `next_cursor`：

- footer 不显示“没有更多”；
- 不额外占用一整行高度。

按钮沿用 06-01：

- 30px 高；
- 1px border；
- 8px radius；
- 12/16px；
- focus-visible 2px outline；
- loading 时 disabled。

---

## 8. 可访问性与交互细节

1. 使用原生 `<table>`、`<thead>`、`<tbody>`、`<th scope="col">`、`<tr>`、`<td>`；
2. `Session记录` 标题通过 `aria-labelledby` 关联 table section；
3. loading/错误/empty 状态使用 `aria-live="polite"`；
4. compact Token 的可访问名称包含完整整数；
5. 模型 `+N` 的 accessible description 包含完整模型列表；
6. ellipsis 单元格必须可通过 title 或等价方式读取完整值；
7. 横向滚动 wrapper 可键盘聚焦，并提供可访问名称 `Session 记录表格，可横向滚动`；
8. 不把整行做成 click target，因为本版本没有 Session 详情路由；
9. 不使用 hover-only 才能获得关键数据；
10. `prefers-reduced-motion` 下不增加新的 transition；
11. 200% 缩放时页面 body 不横向溢出，table wrapper 自己滚动；
12. loading-more 期间再次点击不会产生第二个分页请求。

---

## 9. 实施步骤

### 步骤 1：扩展 data DTO

1. 在 `sessionTypes.ts` 定义 `UsageDto`、`SessionItemDto`、`SessionPageDto` 和 view model 类型；
2. 与 06-01 已存在的 Usage 类型重复时，抽到共享 `dashboard/types.ts`，不要维护两套字段定义；
3. `usagiClient.ts` 新增：

```text
getSessions({
  range,
  limit,
  cursor?,
  signal
}) -> Promise<SessionPageDto>
```

4. URL 使用 `URLSearchParams` 构造，cursor 不手工 escape；
5. 继续使用同源相对 `/api/...`；
6. 检查 HTTP status；
7. 解析 Spec 05 固定 `{error:{code}}`；
8. 增加 DTO runtime validation；
9. 不记录 response body；
10. 单测覆盖 null、unknown cache write、空模型数组、非法安全整数和重复 session ID。

### 步骤 2：抽取共享 revisionFeed

1. 把 06-01 已有 EventSource + `/api/revision` 断线恢复轮询 transport 提取到 `data/revisionFeed.ts`；
2. 使用订阅/快照 seam，让 Dashboard 与 Session controller 共用同一 tuple；
3. 用引用计数或等价生命周期保证页面内最多一个 EventSource 和一个断线恢复轮询 timer；
4. 保留 06-01 的 EventSource error、60 秒轮询、`retry_load()` 立即重试和恢复停止 timer 语义；
5. `useDashboardController` 公共 view model 不增加字段；
6. 不改变 summary/status/refresh 请求时机与 target 归约；
7. 增加两个订阅者、StrictMode 重挂载、SSE error/recover 的资源泄漏测试；
8. 确认旧的 KPI、refresh、status 测试全部继续通过。

### 步骤 3：实现 Session controller 基础状态

1. 创建 `useSessionTableController`；
2. 建立五个 range 的内存 snapshot map；
3. 建立 first-page 与 load-more 两套独立 generation；
4. 建立两套 AbortController；
5. mount 对当前 range 请求第一页；
6. range 切换时按 6.3 取消旧请求；
7. 已缓存 range 立即显示旧 snapshot 并刷新；
8. 无缓存 range 显示 skeleton；
9. unmount abort 所有 fetch；
10. React StrictMode 下不得留下重复请求链或 stale setState。

### 步骤 4：接 revision 追平

1. 订阅 `revisionFeed`；
2. 只处理 `feed.data_revision > snapshot.data_revision`；
3. 相同 revision 去重；
4. feed revision 较小不回退；
5. revision 到达时如正在 load more，先取消 load more，再刷新第一页；
6. revision 到达时如已在刷新第一页，不重复发同一追平请求；
7. 首屏 response 比 feed 更新时直接接受，不额外倒退请求；
8. 用单测覆盖 SSE/session 请求乱序。

### 步骤 5：实现 cursor 分页

1. 首屏固定 `limit=50`；
2. `next_cursor` 非空才显示“加载更多”；
3. 点击后冻结 base range/revision/cursor；
4. 请求期间 disabled；
5. 成功页必须和 base revision 相同才能 append；
6. 新页与已有行做 root ID 唯一性验证；
7. append 后原子更新 rows 和 next cursor；
8. `STALE_CURSOR` / `INVALID_CURSOR` 进入一次自动首屏重取；
9. 普通失败保留旧 rows/cursor 并提供分页重试；
10. range/revision 改变时废弃旧分页结果。

### 步骤 6：实现 formatter

在 `sessionFormat.ts`：

1. 标题 fallback；
2. 项目 fallback；
3. 模型单项 / `+N`；
4. 完整模型 tooltip 文本；
5. last activity 本机时间格式；
6. 完整时间 title；
7. 复用 06-01 compact integer；
8. 复用 06-01 ratio；
9. 复用 06-01 cost placeholder；
10. formatter 全部为纯函数。

如果 06-01 formatter 当前不可复用，先把共同函数移动到：

```text
dashboard/format.ts
```

再由 KPI 与 Session 同时 import；禁止复制一套相似但边界不同的 formatter。

### 步骤 7：实现表格组件

1. `SessionSection` 接收 view model；
2. 添加 `Session记录` heading；
3. 创建 table surface 和横向 wrapper；
4. 使用固定 9 列 `colgroup`；
5. 表头使用本 Spec 4.1 的 canonical 文案；
6. `SessionTableRow` 只映射正式字段；
7. 文本列 ellipsis；
8. 数值列右对齐；
9. Token 使用 tabular nums；
10. 费用当前全部显示 `—`；
11. 不给 `<tr>` 增加点击行为；
12. row key 固定 `root_session_id`。

### 步骤 8：实现 loading / empty / error / footer

1. 首屏无缓存时 6 行 skeleton；
2. 空成功时单状态行；
3. 有缓存 refresh 保留 rows；
4. 首屏失败显示重试；
5. refresh 失败保留 rows + footer 重试；
6. 分页 loading 按钮 disabled；
7. 分页失败单独重试；
8. 无更多页时移除 footer；
9. 所有状态不改变表头列宽；
10. `aria-busy` / `aria-live` 正确。

### 步骤 9：接入 DashboardPage

1. 从 06-01 controller 获取现有 `range`；
2. 调用 `useSessionTableController({ range })`，controller 内部订阅共享 `revisionFeed`；
3. KPI 网格后 `32px` 插入 SessionSection；
4. 不增加图表 DOM；
5. 不预留图表高度；
6. 确保同步完成后共享 revision tuple 同时驱动 summary 与 Session 追平；
7. 确保 range 切换同时驱动 KPI 与 Session，但两者请求彼此独立失败，不互相清空成功数据。

### 步骤 10：浏览器与构建验证

至少验证 viewport：

```text
1512
1280
1024
768
767
390
```

检查：

1. 1512px 表格完整可见；
2. 小于 table min-width 时只有 wrapper 横滚；
3. body 无横向滚动；
4. 长标题/项目/模型不撑宽；
5. 50 行时滚动性能正常；
6. load-more 后 100/150/200 行不明显卡顿；
7. range 快速切换不混行；
8. SSE/revision 更新时旧页不会与新第一页混合；
9. Vite dev proxy 下 sessions GET 通过 Spec 05 Host/Origin 防护；
10. `npm run test`、`npm run check`、`npm run build` 全部通过。

---

## 10. 联合审核

### 10.1 与数据口径 v0.2

审核结果：通过。

| 契约 | 06-02 实现 |
|---|---|
| Session 按 `root_session_id` 一行 | row key 与 API item 都按 root Session |
| Subagent 不单独成行 | 不生成子行 |
| Token 为 inclusive usage | 所有可见 Token 只读 `inclusive_usage` |
| 标题/项目只用主 Thread | 直接消费 API resolved 字段，不二次合并 |
| 模型包含主 + Subagent | 直接消费 `models_used` |
| `输入 Token` canonical 名称 | 表头使用“输入 Token” |
| 第二个输出列应为推理 | 表头使用“推理 Token” |
| 上次更新改为最后活动 | 表头使用“最后活动” |
| Cache Hit 为合并后的 cached/input | 只格式化 API 值 |
| 费用暂为 null | 固定显示 `—` |

补充处理：

数据口径中的“多个模型显示主 Agent 模型 +N”属于界面建议，但 Spec 05 当前只提供按首次有效事件稳定排序后的 `models_used`，没有提供“主 Agent 当前模型”的可证明字段。06-02 因此显示 `models_used[0] +N`，并明确不把首项声称为主 Agent 模型，不新增后端猜测。

### 10.2 与 Spec 01

审核结果：通过。

06-02：

- 不直接访问 SQLite；
- 不读取 source/checkpoint；
- 不创建新持久化表；
- 只消费 Spec 05 HTTP DTO；
- `root_session_id` 只作为业务行身份，不使用文件路径；
- 不改变 `data_revision` / `status_revision` 持久化规则。

### 10.3 与 Spec 02

审核结果：通过。

06-02：

- 不解析 state/session-index/rollout；
- 不自行推导标题、项目、parent/root；
- 不读取 Prompt/Assistant/tool 正文；
- `title` / `project_name` / `project_path` 使用已解析结果；
- 不用 Subagent metadata 覆盖根 Session 标题或项目。

### 10.4 与 Spec 03

审核结果：通过。

06-02：

- 不触发独立扫描；
- 不实现 polling；
- 不理解 active/follow-up scan；
- 不改变手动刷新互斥；
- 扫描期间保留旧 stable Session rows；
- 更新只通过既有 revision 链追平。

### 10.5 与 Spec 04

审核结果：通过。

06-02：

- 不重算 `last_token_usage` / total delta；
- 不重算 Subagent replay；
- 不重算 Session aggregation；
- 可见 Token 只来自 `inclusive_usage`；
- 不把 `self_usage` / `subagent_usage` 相加；
- 不平均 Session 内 cache hit；
- 接受 `last_activity_at`、`models_used` 和服务端排序；
- 不改变 summary = Σ Session 的聚合不变量。

### 10.6 与 Spec 05

审核结果：通过。

06-02 完整遵守：

```text
GET /api/usage/sessions
range
limit=50
opaque cursor
data_revision
next_cursor
last_activity_at DESC, root_session_id ASC
STALE_CURSOR
INVALID_CURSOR
```

分页不混合 revision；cursor 失效只重取第一页；前端不 decode cursor。

06-02 不新增 API，不要求 Spec 05 修改响应字段。

### 10.7 与 Spec 06-01

审核结果：通过。

保留：

- React 19 + TypeScript strict + Vite 6 + Tailwind 4；
- 无 router；
- 无全局状态库；
- 无请求缓存库；
- 无 UI/图表库；
- 原页面留白、字体、颜色、圆角和按钮 token；
- 原 range selector；
- 原同步按钮；
- 原 SSE + 60 秒断线恢复轮询；
- 原 refresh target 语义。

06-02 只把 06-01 已有 revision transport 内部提取为共享 `revisionFeed`，`DashboardViewModel`、range、refresh target 和服务端 revision 接口均不改变；Dashboard 与 Session 共享同一 EventSource / 断线恢复轮询 timer。

用量图表本版本不实现，也不预留 placeholder。

### 10.8 联审结论

本 Spec 不要求修改 Spec 01～05 的数据库、扫描器、聚合或 HTTP 契约。

06-02 与现有正式契约之间不存在阻塞性冲突，可以直接进入实施。

06-01 与 06-02 已统一 canonical Token 中文口径：`input_tokens` 固定显示“输入 Token”，`cached_tokens` 固定显示“缓存读取 Token”，`cache_write_tokens` 固定显示“缓存写入 Token”。不存在继续沿用“写入 Token”指代 `input_tokens` 的兼容文案。

---

## 11. 独立验收标准

> **测试标准唯一来源**：本节只定义 Spec 06-02 的功能与交付完成边界，不再定义测试方案、测试用例、优先级或执行清单。Spec 06-02 的测试条目、P0/P1/P2 分类、Gate、测试代码落点、执行命令、S06-01 回归要求及与最终完整测试的关系，**唯一以 `Usagi_测试标准_Spec01-06_v0.17.md` 为准**。
>
> 本 Spec 其他章节中出现的“验证”“测试”“检查”等文字仅属于实施说明或风险提示，不构成独立测试标准；如与上述 v0.17 测试标准存在差异或冲突，以 v0.17 为准。不得以本节勾选项替代 S06-02 / S06 总 Gate。

- [ ] KPI 网格后直接出现 `Session记录`，中间无图表和图表占位；
- [ ] 只请求 `/api/usage/sessions`，不访问 SQLite/rollout；
- [ ] 首屏固定 `limit=50`；
- [ ] 9 列顺序与 4.1 完全一致；
- [ ] 使用“最后活动 / 输入 Token / 推理 Token”canonical 文案；
- [ ] 不展示 Subagent 独立行；
- [ ] 所有可见用量来自 `inclusive_usage`；
- [ ] title/project null 有固定 fallback；
- [ ] 多模型使用稳定首项 +N，完整列表可访问；
- [ ] Token、ratio、cost formatter 与 06-01 一致；
- [ ] cost 当前全部为 `—`；
- [ ] range 切换不会混入旧 range rows；
- [ ] 同 range 可保留自己的旧 snapshot；
- [ ] revision 更高时重取第一页，不把新页 append 到旧 revision；
- [ ] next cursor 支持逐页加载；
- [ ] `STALE_CURSOR` / `INVALID_CURSOR` 自动从第一页恢复；
- [ ] 普通分页失败保留已加载 rows；
- [ ] 旧请求晚到不会覆盖新 range/revision；
- [ ] 首次 loading 为 6 行 skeleton；
- [ ] empty、first-page error、refresh error、load-more error 均有稳定布局；
- [ ] 小屏只有 table wrapper 横向滚动，body 不横向滚动；
- [ ] table 使用语义化标签并满足键盘、aria、200% zoom；
- [ ] Dashboard 与 Session 共用一个 `revisionFeed`，页面内最多一个 EventSource 和一个 revision polling timer；
- [ ] 不创建新 API、新持久化表或浏览器持久化 cache；
- [ ] 本 Spec 的测试、S06-01 回归与工程验收已按 `Usagi_测试标准_Spec01-06_v0.17.md` 中 S06-02 / S06 总完成门执行；本节不另设测试清单或通过口径。

---

## 12. 完成定义

通过本 Spec 后，当前版本 Dashboard 的前端交付面为：

```text
Dashboard shell
+ 时间范围
+ 8 张 KPI
+ 同步 / revision / 异常状态
+ Session 记录列表
```

Session 列表能够在同一 range 内稳定展示根 Session 包含用量，并在分页、同步、revision 变化、进程重启 cursor 失效和请求乱序场景下保持“不混 range、不混 revision、不清空旧 stable 数据”的一致性。
