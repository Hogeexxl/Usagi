# Usagi Spec 06-01：前端框架与 Dashboard 界面

> 版本：v0.2  
> 状态：当前契约修订版  
> 更新日期：2026-08-09  
> 依赖：`Spec_05_查询API与更新通知_v0.2.md`  
> 测试标准：`Usagi_测试标准_Spec01-06_v0.17.md`

---

## 1. 范围

实现无侧边栏、无顶部导航的单页 Dashboard：前端工程、全局视觉规范、时间范围、8 张 KPI 卡片、同步数据按钮、revision 更新与异常状态。

不实现设置按钮、两个中间图表及其空占位、费用计算、Session 列表（见 Spec 06-02）、路由、暗色模式或移动端导航。

---

## 2. 前端模块与接口

保留现有 React 19 + TypeScript strict + Vite 6 + Tailwind CSS 4。不引入 router、全局状态库、请求缓存库、UI 组件库和图表库。Axum 继续托管 `frontend/dist`。

建议文件：

```text
frontend/src/
  App.tsx
  index.css
  dashboard/
    DashboardPage.tsx
    RangeSelector.tsx
    MetricGrid.tsx
    MetricCard.tsx
    SyncButton.tsx
    useDashboardController.ts
    format.ts
    types.ts
  data/
    usagiClient.ts
```

`useDashboardController` 是页面状态 seam，对 view 只暴露：

```text
DashboardViewModel {
  range
  metrics
  load_state       // initial | loading | ready | error
  refresh_state    // idle | requesting | running | failed | tracking_error | source_changed
  error_code?
  select_range(range)
  retry_load()
  request_refresh()
  retry_refresh_status()
}
```

HTTP、AbortController、SSE、60 秒轮询、revision 去重和过期响应丢弃都隐藏在 hook/data 模块内。view 不直接 `fetch`，不重算 Token 口径。

controller 内部为每个 range 保存最后一份成功 `{range,data_revision,usage}` snapshot，但 view 只能看到当前 range 对应的一份；不允许把其他 range 的数值当作回退。

---

## 3. 1512px 视觉基准

### 3.1 页面分区

```text
左留白区 84px | 流体内容区 calc(100vw - 168px) | 右留白区 84px
                         └─ 固定 padding: 32px 16px 64px
```

- 内容区不设 `width`/`max-width` 常量，由外层留白后的剩余宽度得到；
- 内容区始终使用 `box-sizing:border-box; padding:32px 16px 64px`，该 padding 不随断点改变；
- `>=1280px`：外层左右留白各 84px；`768..1279px`：各32px；`<768px`：外层留白隐藏为0，仅保留内容区左右16px padding；
- 页面没有侧边栏占位、顶部导航高度或隐藏抽屉；
- 1512px 时外层内容区宽1344px，内部可用宽1312px，组件左边缘为 `84+16=100px`；内容区不小于视口高度。

### 3.2 视觉 token

| token | 值 |
|---|---|
| 页面背景 | `#f3f4f6` |
| 表面 | `#ffffff` |
| 主文字 | `#09090b` |
| 次文字 | `#52525b` |
| 边框 | `#e4e4e7` |
| 选中背景 | `#18181b` |
| 选中文字 | `#ffffff` |
| 辅助底色 | `#e2e3e7` |
| 圆角 | 卡片/按钮 `8px`，segmented control `9999px` |
| 阴影 | 无 |

全局字体栈与 Vibe Usage 实际 computed style 一致：

```css
"JetBrains Mono", "JetBrains Mono Fallback", ui-monospace,
SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono",
"Courier New", "PingFang SC", "Microsoft YaHei", monospace
```

`JetBrains Mono` 作为本地 webfont 通过 `@font-face` 引入并使用 `font-display: swap`。实施时将官方可再分发的 WOFF2 文件和许可声明一并纳入仓库，不从 CDN 加载，不依赖用户系统已安装该字体。

### 3.3 字号与间距

- `Dashboard`：30/36px、700；
- KPI 标签：14/20px、500；数值：24/32px、700；
- segmented control 和同步按钮：12/16px；
- 标题行后 32px 放时间范围，范围后 32px 放 KPI；
- 所有常规区域基于 8px 节奏，禁止为单个卡片加例外 margin。

---

## 4. 页面结构

### 4.1 标题与操作行

同一 flex row 左侧为 `Dashboard`，右侧只有“同步数据”。没有 Logo、面包屑、分享、成就或设置。

按钮默认高 30px，padding `6px 12px`，透明背景、1px 边框、8px 圆角。小于 480px 仍保持同行；标题可缩，按钮不折行。

### 4.2 时间范围

顺序固定：

```text
今天 | 昨天 | 本周 | 本月 | 今年
```

整组高 28px，底色 `#e2e3e7`、胶囊圆角、padding 4px。单项 padding `2px 12px`；选中项近黑背景白字，其他为次文字。

用 `role=group` + `aria-label="时间范围"`，每项暴露 `aria-pressed`。点击后立即更新选中样式，取消旧请求，再请求 `/api/usage/summary?range=...`。

### 4.3 KPI 网格

桌面/平板卡片固定 `237×106px`、24px 内边距、白背景、1px 边框、8px 圆角、无阴影。布局：

```css
grid-template-columns: repeat(auto-fit, 237px);
column-gap: 16px;
row-gap: 24px;
justify-content: space-between;
```

`auto-fit` 根据可用宽度自动从 5 列降为 4/3/2 列；`space-between` 只分配网格剩余水平空间，同一 grid 的第二行仍对齐前一行列起点。1512px 下网格可用宽1312px，5 列实际列间距 31.75px。

`<768px` 完全切换为 Vibe Usage 移动规则：两列 `minmax(0,1fr)`、8px gap、16px 内边距、86px 最小高度；卡片宽度随内容区变化。页面不得出现水平滚动。

8 张卡片顺序和字段固定：

| 位置 | 标签 | Summary 字段 | 空/未知展示 |
|---:|---|---|---|
| 1 | 预估费用 | `estimated_cost` | `null` 显示 `—`，不显示 `$0` |
| 2 | 总 Token | `total_tokens` | `0` |
| 3 | 输入 Token | `input_tokens` | `0` |
| 4 | 输出 Token | `output_tokens` | `0` |
| 5 | 会话数量 | `session_count` | `0` |
| 6 | 缓存命中率 | `cache_hit_rate` | `null` 显示 `—` |
| 7 | 缓存写入 Token | `cache_write_tokens` | `null` 显示 `—` |
| 8 | 缓存读取 Token | `cached_tokens` | `0` |

不在前端反推任何 Token 卡片。`cache_write_tokens=null` 直接由 formatter 显示未知占位，不猜测为 0，也不另外显示状态文案。

### 4.4 数值格式

- Token/Session 小于 1000 用十进制整数；从 1000 起使用 `K/M/B`、一位小数并移除 `.0`；
- compact 数值通过 `title` 和可访问名称保留完整整数；
- 命中率为 `ratio*100`，最多一位小数并加 `%`；
- 费用非空时预留 `$` + 两位小数格式，本版本必须为 `—`；
- formatter 必须是纯函数，不接受字符串 Token，不修改 API 值。

---

## 5. 同步数据与更新流

### 5.1 初始加载与切换

1. 默认 range 为 `today`；页面 mount 并行请求 summary 和 status。
2. 首次等待时显示 8 个同尺寸 skeleton，不显示伪造 0。
3. 切换 range 时中止旧 AbortController，只接受最新 request id 的响应。目标 range 有成功 snapshot 时可保留该 range 自己的旧值并标记 loading；没有时显示同尺寸 skeleton，不得显示切换前 range 的值。
4. 成功响应一次性替换 8 个值和 `data_revision`，禁止逐卡片刷新。
5. 失败时只保留当前 range 自己的成功 snapshot（若有），在范围控件下显示固定错误文案和“重试”。controller 分别记录 summary、普通 status 和 revision 的失败标记；`retry_load()` 重试当前仍失败的全部普通依赖，不重请已成功且未过期的依赖。

### 5.2 同步按钮

1. 只在“已成功取得 status、`source_binding_status=ready`、无 requesting、无 active refresh target、无 active scan、无 queued follow-up”时允许点击。此时 `scan_state=idle` 或 `failed` 都可重新同步；`running`、`source_changed` 或 `followup.state=queued` 必须禁用。
2. 点击后立即进入 `requesting`、禁用按钮，调用 `POST /api/refresh` 并携带 `X-Usagi-Request: 1`。
3. `202 started`：保存 `refresh_target={scan_id,kind:started}`；`200 coalesced`：保存 `refresh_target={scan_id,kind:followup}`，该 ID 必须是排队 follow-up，不是当前 active scan。两者均进入“同步中…”，`status_revision` 只用于单调新鲜度，不单独作为完成判据。
4. 存在 target 时所有 status 请求都携带 `target_scan_id`，只根据 `target_scan.state` 完成它。当前 scan_state、last-finished 投影或单纯 revision 增加都不是 target 终态证据。target 终止后，如 data revision 变化则重取当前 summary。
5. scan target 执行失败或 follow-up `start_failed`：显示“同步失败”并保留旧 KPI。清除 active target 后，只要 binding ready、无 active/queued 且当前 scan state 为 idle/failed，按钮恢复可点。
6. `409 SOURCE_CHANGED`：状态为 `source_changed`，文案“数据源已变化”，按钮禁用至 status 恢复。
7. 快速重复点击不能发出并发 refresh；前端禁用与 Spec 03/05 coalescing 共同保护。
8. `requesting/running` 期间用于跟踪扫描的 status 请求失败时进入 `tracking_error`，保留 target 和旧 KPI 并显示“同步状态获取失败”；`retry_refresh_status()` 只重试 status，绝不再次 POST refresh。

不显示设置入口、扫描进度百分比、文件数或 `scan_id`。

用户可见错误固定映射，不直接显示 API `message`：

| 条件 | 文案 | 后续操作 |
|---|---|---|
| summary、revision 或非同步跟踪的 status 失败 | `数据加载失败` | `retry_load()` |
| refresh `403` | `无法发起同步` | 恢复按钮 |
| refresh `409 SOURCE_CHANGED` | `数据源已变化` | 按钮保持禁用，等待 status 恢复 |
| refresh POST 其他失败 | `同步失败` | 保留旧数据，恢复按钮 |
| 同步跟踪 status 失败 | `同步状态获取失败` | `retry_refresh_status()` |

### 5.3 SSE 与轮询

1. mount 后连接 `/api/events`；收到 revision 只触发 status/当前 range 重取，不把 SSE payload 当用量。
2. `status_revision` 变化重取 status；`data_revision` 变化重取 summary；相同 tuple 不重复请求。
3. SSE error 后启用每 60 秒 `/api/revision` 断线恢复轮询；EventSource 重连并收到事件后停止轮询。
4. unmount 时 close EventSource、clear interval、abort fetch；React StrictMode 重复 effect 不得留下第二个连接或 timer。
5. `/api/revision` 失败会设置 revision 失败标记；`retry_load()` 立即重试它。成功后把返回 tuple 与本地已接受 tuple 比较，按需重取 status 和当前 range summary，然后清除失败标记。只要 EventSource 尚未恢复，60 秒降级轮询就继续运行，一次手动重试成功不得停止 timer。

### 5.4 status 目标追踪与竞态

1. summary、status 各自使用独立 AbortController 和单调 request generation。status 响应只在 generation 最新且 `status_revision >= last_accepted_status_revision` 时接受。
2. POST requesting 期间，status 可更新缓存但不得结束 requesting。POST 响应到达后用 `scan_id + disposition` 建立 target，再立即用最新缓存 status 归约；旧 POST 响应由 refresh generation 丢弃。
3. target 存在时，status URL 固定附加 `target_scan_id=target.scan_id`；合法 target 如返回 `SCAN_NOT_FOUND`视为协议/数据库错误，不得猜测已完成。
4. `target_scan.state` 唯一映射：queued → “同步等待中…”；running → “同步中…”；completed → 清 target 并重取 summary；failed/start_failed → 清 target、保留 KPI 并显示“同步失败”。
5. 页面 mount 先请求无 target 的 status：`followup.state=queued` 时采用其 ID 为 target 并立即携 ID 重取；`followup.state=start_failed` 时也采用其 ID 重取持久化 target，随后按 start_failed 映射为可重试的“同步失败”；否则有 active scan 时可采用 active ID 为 target 或只显示全局同步中。内存已有 target 的断网/SSE 恢复必须继续携带其 ID。
6. `retry_refresh_status()` 保留同一 target，只发起新 status generation。不依赖收到每个中间 revision；即使多轮 scan 在一次 status 查询前完成，持久化 target row 仍能结束对应 refresh。
7. 无 target/requesting 时，普通 status 映射 Startup/Scheduled 扫描。无 active、无 queued、binding ready 且 scan_state 为 idle/failed 时按钮可用。

| `target_scan.state` | 页面状态 | 是否清 target |
|---|---|---|
| queued | 同步等待中 | 否 |
| running | 同步中 | 否 |
| completed | 同步完成，重取 summary | 是 |
| failed / start_failed | 同步失败，保留 KPI | 是 |

POST 网络中断后前端不猜测是否接受；下一次 status 若看到 active scan 或 queued follow-up 就显示同步中。用户重试 POST 时服务端只会 Started 或复用同一 queued follow-up，不会并发扫描。

---

## 6. 异常、动效与可访问性

- `active epoch=0` 按 API 合法零值展示；费用和无分母命中率仍为 `—`。
- `cache_write_tokens=null` 时缓存写入卡显示 `—`，不变成 0；明确的 `0` 仍显示 `0`。
- running/rebuild/failed 期间保留旧稳定 KPI；页面不闪回全 0。
- 数值只在新 snapshot commit 到 React state 时做 120ms opacity 过渡；不做数字滚动、缩放或弹跳。
- hover 只改变按钮背景/边框，卡片不浮起；`prefers-reduced-motion` 时移除所有 transition。
- 所有按钮可键盘操作，保留 2px focus-visible outline，颜色对比满足 WCAG AA。
- loading/error/status 通过 `aria-live=polite` 通知；skeleton 本身 `aria-hidden=true`。

---

## 7. 实施步骤

### 步骤 1：整理前端工程

1. 保留现有 package/toolchain，新增 Vitest + jsdom + React Testing Library 作为最小组件测试依赖。
2. 增加 `test` script；`check` 仍为 `tsc --noEmit`，`build` 仍先 typecheck 再 Vite build。
3. 移除现有深色脚手架页和 Inter 全局样式，建立 3.2 的 CSS token 与字体资源。
4. Vite dev proxy 保留 `/api -> http://127.0.0.1:3210`，设置 `changeOrigin:true`，并在 proxy `configure` 的 `proxyReq` 钩子中把每个转发请求的 `Origin` 固定改写为 `http://127.0.0.1:3210`。这一规则同时覆盖 GET、SSE 和 refresh，使 Host/Origin 符合 Spec 05 本机防护；production 只使用同源相对 URL。

### 步骤 2：实现 data 模块与 controller

1. 为 Spec 05 summary/status/revision/refresh/error 建立精确 TypeScript DTO；不使用 `any`。
2. 对每个 fetch 检查 HTTP status 和必需字段；本机同源数据仍在 data seam 转换为固定结果/错误。
3. 实现按 range 绑定的 snapshot cache、summary request id/AbortController，保证 range/revision 并发时只有当前 range 的最新响应入 state。
4. 按 5.3/5.4 实现依赖失败标记、独立 status/refresh generation、requesting 期间 status 缓冲、Started/follow-up target 归约和 reducer 状态转换，再接 SSE 和轮询清理；不把连接对象存入可渲染 state。

### 步骤 3：实现布局与 KPI

1. 按 3.1 实现页面留白变量和无导航单页 shell。
2. 实现标题行、RangeSelector、MetricGrid 和数值 formatter；卡片定义为一个固定配置数组，确保顺序/字段不分散。
3. 用真实 1512/1280/768/390 viewport 验证列数、间距、字符溢出和水平滚动。
4. 不创建图表文件、组件、占位 DOM 或 mock data。

### 步骤 4：实现同步与状态

1. 把 SyncButton 只绑定 controller 的 `request_refresh`，按 5.2 映射按钮状态。
2. 连接 SSE/revision 轮询，用 fake EventSource/timer 验证去重与清理。
3. 补齐 loading、partial stale data、error、failed、tracking_error 和 source_changed 视觉状态，并将两类重试按钮分别绑定到公开 seam。
4. 构建 `frontend/dist`，通过 Axum 实际路由检查静态首页、API 与SPA 刷新路由回退。

---

## 8. 独立验收标准

> **测试标准唯一来源**：本节只定义 Spec 06-01 的功能与交付完成边界，不再定义测试方案、测试用例、优先级或执行清单。Spec 06-01 的测试条目、P0/P1/P2 分类、Gate、测试代码落点、执行命令及与最终完整测试的关系，**唯一以 `Usagi_测试标准_Spec01-06_v0.17.md` 为准**。
>
> 本 Spec 其他章节中出现的“验证”“测试”“检查”等文字仅属于实施说明或风险提示，不构成独立测试标准；如与上述 v0.17 测试标准存在差异或冲突，以 v0.17 为准。不得以本节勾选项替代 S06-01 Gate。

- [ ] 使用现有 React/TypeScript/Vite/Tailwind 工程，无 router、状态库、UI/图表库；
- [ ] 单页无侧边栏、顶部导航和设置入口；两个图表及占位完全不渲染；
- [ ] 1512px 下外层左右各84px、内容区流体为1344px且固定 padding `32px 16px 64px`；外层留白按 84/32/0px 断点切换；
- [ ] 桌面卡片固定237×106px且随宽度自动换列；移动端两列流体并无水平滚动；
- [ ] 8 张卡片的顺序、标签、canonical Summary 字段和 null/unknown 展示与 4.3 一致；
- [ ] 预估费用占位保留但始终显示 `—`，前端没有价格表或费用计算；
- [ ] 时间范围只有今天/昨天/本周/本月/今年，切换不受过期响应覆盖；
- [ ] 成功 KPI snapshot 与 range/data revision 绑定，新 range 失败时不展示其他 range 的值；
- [ ] 同步数据按钮正确处理 Started/Coalesced/running/failed/source_changed，不产生并发扫描；
- [ ] status 与 refresh 使用独立 generation/revision 竞态保护；requesting 期间响应只缓冲，不提前结束 refresh；
- [ ] Started 跟踪直接 scan ID；Coalesced 跟踪排队 follow-up ID，当前 scan 终态不会提前恢复按钮或刷新未最终数据；
- [ ] queued → active → terminal/start_failed 全程可确定归约；Busy 不离开 queued，非重试错误才进入 start_failed；页面重载、断网、多次 Coalesced 不丢目标；
- [ ] failed 在 binding ready 且无 active/queued/target 时允许新 refresh；running/source_changed/queued 禁止；
- [ ] SSE 仅驱动重取，断线后 60 秒轮询可恢复，所有资源在 unmount 清理；
- [ ] `retry_load()` 重试所有当前失败的普通依赖，revision 成功后追平 tuple 且不提前停止 SSE 降级轮询；
- [ ] 通过 Vite dev server 访问 summary/SSE/refresh 时 Host 和改写后 Origin 均符合 Spec 05；
- [ ] loading/error/rebuild 不改变布局尺寸，不清空旧稳定 KPI；
- [ ] 本 Spec 的测试与工程验收已按 `Usagi_测试标准_Spec01-06_v0.17.md` 中 S06-01 完成门执行；本节不另设测试清单或通过口径。

---

## 9. 完成定义

通过本 Spec 后，页面已具备稳定的单页 shell、KPI 和同步数据流。Spec 06-02 只在 KPI 网格下方增加 Session 记录，不修改本 Spec 的全局视觉、range 或 revision 接口。
