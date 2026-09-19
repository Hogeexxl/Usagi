# Usagi v0.2.0 BeUI 整改实施方案 — Spec01：全局基础与 Header

> 施工分支：`feat/v0.2.0-beui-redesign`  
> 本 Spec 只处理：全局 Theme 首帧、Header、Header 使用的 BeUI primitives、Dashboard 一级分区 32px 间距。  
> 筛选、KPI、Table、Drawer、图表内部视觉均不在本 Spec 改造范围内。

---

# 1. Spec01 范围

## 1.1 必须完成

1. Theme 首帧初始化，消除默认 Dark 启动时的 Light → Dark 闪烁。
2. Theme Toggle 恢复 BeUI 官方实现与官方调用方式：
   - `variant="circle-blur"`
   - `start="bottom-up"`
   - Circle origin = `50% 100%`
3. Header 三个文字操作按钮恢复 BeUI 官方 `StatefulButton`：
   - 检查更新 / 版本升级
   - 同步数据
   - 停止服务
4. 三个 Header 文字按钮全部改为 BeUI `size="sm"`。
5. 上次同步时间恢复 BeUI 官方 `ActionSwapText` Blur。
6. 删除 UpdateButton 右侧额外“发现新版本 …”文本。
7. Animated Toast Stack 恢复 BeUI 官方实现；停止服务 loading → success/error 必须更新同一个 toast。
8. success / error Toast 不再硬编码 `4200ms`，继承 BeUI 默认 duration。
9. Dashboard 一级区域统一由父级 `gap-8` 控制 32px 垂直间距：
   - Header → 筛选区
   - 筛选区 → KPI
   - KPI → 图表
   - 图表 → Session
10. 删除会与父级 `gap-8` 叠加的一级 section `margin-top`。

## 1.2 本 Spec 明确不做

- 不修改筛选区内部 Tabs / Checkbox / MorphPopover。
- 不修改 KPI 卡片。
- 不修改 Table 内部。
- 不修改 Drawer。
- 不修改图表内部。
- 不修改后端 API、Rust 数据模型、数据库、扫描器。
- 不新增 System Theme。
- 不重做 BeUI Neutral 色板；现有基础 Theme token 继续保留。
- 不建立新的 MU Button / Toast / ActionSwap / ThemeToggle primitive。

唯一允许触碰后续区域文件的原因，是**删除其一级 section 外部 `mt-*`**，以完成全局 32px section rhythm；不得顺手修改这些区域内部视觉。

---

# 2. BeUI 组件来源与固定参数

## 2.1 BeUI 源码同步规则

当前分支没有 `components.json`，TypeScript 也没有 `@/` alias。Spec01 **不授权为了跑 shadcn CLI 而执行 `shadcn init`、重写 Theme、增加全局 alias 或重构目录**。

因此本 Spec 的 BeUI 同步规则固定为：

1. 以 BeUI 当前官方 Registry / 官方组件页面源码为唯一基线。
2. 若现有仓库环境可直接执行对应 Registry Install 且不会触发 `shadcn init` / Theme 重写，则优先 Install。
3. 否则使用同一 BeUI 官方页面的 **Manual 完整源码原样同步**。
4. Manual 同步后只允许：
   - 把 `@/...` import 改为 MU 当前相对路径；
   - Theme Toggle 把官方 `next-themes` 桥接替换为现有 `ThemeProvider`；
   - 保留本文明确批准的 destructive semantic override。
5. 除上述三类适配，不得修改官方 DOM、class、variant、size、hover、press、spring、easing、duration、width morph、icon transition、reduced-motion。
6. 禁止以当前 MU primitive 为基础继续“修得更像 BeUI”。

## 2.2 本 Spec 必须恢复的官方 primitive

| 用途 | 官方 BeUI 来源 | 固定调用 |
|---|---|---|
| Header Stateful Button | `@beui/button-stateful` | 使用官方 Button + StatefulButton 源码 |
| 上次同步文字切换 | `@beui/action-swap-blur` | `ActionSwapText` + `animation="blur"` |
| Theme Toggle | `@beui/theme-toggle` | `variant="circle-blur"` + `start="bottom-up"` |
| Theme Toggle 图标 | BeUI `ActionSwapIcon` | 保留官方 Sun / Moon blur swap |
| Toast | `@beui/animated-toast-stack` | 官方 `AnimatedToastStack` + hook |

## 2.3 Header 固定视觉参数

### `Usagi`

```text
font-family: JetBrains Mono
font-size: 30px
font-weight: 700
line-height: 36px
letter-spacing: normal
color: foreground
```

不加标题动效。

### Header 三个文字按钮

全部：

```text
size = sm
ripple = false
```

BeUI 官方 `sm` 由组件内部负责，调用层不得复制其 class。当前官方 `sm` 视觉基线为：

```text
h-8
px-3
text-xs
gap-1.5
rounded-full
```

这里只作为验收参考，代码必须使用 `size="sm"`。

### Theme Toggle

使用 BeUI 官方 Theme Toggle Preview 的按钮外观，不保留当前 MU 的圆形透明按钮样式。

固定调用：

```tsx
<ThemeToggle
  variant="circle-blur"
  start="bottom-up"
  className="rounded-xl border border-border bg-background p-2.5"
  iconClassName="h-5 w-5"
/>
```

禁止保留当前：

```text
h-10 w-10
rounded-full
bg-transparent
transition-colors
hover:bg-primary/5
```

这套 MU 自定义视觉。

---

# 3. 文件范围

## 3.1 预计直接修改

```text
frontend/index.html

frontend/src/theme/ThemeProvider.tsx
frontend/src/theme/ThemeProvider.test.tsx
frontend/src/theme/theme.ts                  # 原则上只读；仅 storage key / type 需要同步时允许改

frontend/src/dashboard/DashboardPage.tsx
frontend/src/dashboard/DashboardPage.test.tsx
frontend/src/dashboard/UpdateButton.tsx
frontend/src/dashboard/SyncButton.tsx
frontend/src/dashboard/ServiceButton.tsx
frontend/src/dashboard/ServiceButton.test.tsx

frontend/src/index.css

frontend/src/dashboard/charts/ChartSection.tsx
frontend/src/dashboard/session/SessionSection.tsx

frontend/src/ui/beui/theme-toggle.tsx
frontend/src/ui/beui/action-swap.tsx         # 或官方同步产生的等价 Action Swap 文件
frontend/src/ui/beui/animated-toast-stack.tsx
frontend/src/ui/beui/button/base.tsx
frontend/src/ui/beui/button/stateful.tsx
frontend/src/ui/beui/button/index.ts
```

## 3.2 BeUI 同步时允许机械更新

仅在官方源码确实依赖时允许：

```text
frontend/src/ui/lib/ease.ts
frontend/src/ui/lib/cn.ts
```

若官方源码没有要求，不要为了“统一”主动重写。

## 3.3 禁止扩大

除测试文件外，本 Spec 不应修改：

```text
frontend/src/dashboard/FilterControls.tsx
frontend/src/dashboard/RangeSelector.tsx
frontend/src/dashboard/MetricGrid.tsx
frontend/src/dashboard/session/SessionTable*
frontend/src/dashboard/session/SessionDetailDrawer.tsx
frontend/src/dashboard/charts/DistributionDonutCard.tsx
frontend/src/dashboard/charts/SkillsUsageChart.tsx
src/**
```

`ChartSection.tsx` 与 `SessionSection.tsx` 只允许删除一级 `mt-4`，不得做其它视觉整改。

---

# 4. 实施顺序

# S01-1：先恢复 Header 使用的 BeUI 官方 primitives

施工顺序必须先 primitive、后业务调用层，禁止反过来调业务 class 去“仿官方”。

同步：

```text
button-stateful
action-swap-blur
animated-toast-stack
theme-toggle
```

完成后先做源码检查：

- `StatefulButton` 的逐字 slot / blur / width morph 保留。
- `Button` 官方 variant / size class 保留。
- `ActionSwapText` 官方 width measurement / width transition 保留。
- `ActionSwapIcon` 官方 blur swap 保留。
- Toast status morph / swipe / layout animation 保留。
- Theme Toggle View Transition / reduced-motion / fallback 保留。

Theme Toggle 唯一允许的源码级业务适配：

```text
官方 next-themes
→ MU ThemeProvider 的 theme / setTheme
```

不得因此重写 ThemeToggle 动画。

若 Registry/Manual 同步带入 `next-themes` dependency，本项目不使用它；删除该依赖并使用现有 ThemeProvider bridge。

---

# S01-2：修 Theme 首帧 bootstrap

## 当前问题

`frontend/index.html` 当前没有首帧主题 class。React 挂载后 `ThemeProvider` 才应用 `.dark`，存在启动闪烁。

## 实施

`<html>` 默认直接带 Dark：

```html
<html lang="zh-CN" class="dark">
```

在 `<head>` 内、React module script 之前增加同步内联 bootstrap：

```html
<script>
  (() => {
    let theme = "dark";
    try {
      const stored = window.localStorage.getItem("usagi.theme");
      if (stored === "dark" || stored === "light") theme = stored;
    } catch {}
    document.documentElement.classList.toggle("dark", theme === "dark");
  })();
</script>
```

固定规则：

```text
合法 dark  → 首帧 dark
合法 light → 首帧 light
无记录     → 首帧 dark
非法记录   → 首帧 dark
Storage 读取失败 → 首帧 dark
```

不得：

- 等 `useEffect` 后再决定首帧；
- 读取 `prefers-color-scheme`；
- 引入 `system`；
- 新增第三种 Theme 状态。

`ThemeProvider` 保留现有：

```text
Theme = "dark" | "light"
THEME_STORAGE_KEY = "usagi.theme"
DEFAULT_THEME = "dark"
```

Provider 挂载后的 theme 必须与 bootstrap 结果一致，不允许第一次 render 再反向切换。

---

# S01-3：修 Header Theme Toggle

`DashboardPage.tsx`：

当前：

```text
variant="circle-blur"
start="top-right"
+ MU 自定义 rounded-full / transparent / hover
```

改为：

```tsx
<ThemeToggle
  variant="circle-blur"
  start="bottom-up"
  className="rounded-xl border border-border bg-background p-2.5"
  iconClassName="h-5 w-5"
/>
```

要求：

- Header 最右侧位置不变。
- Reveal 起点固定屏幕底部中间。
- Circle Blur duration / easing 使用官方值，不在 MU 调用层传 duration。
- reduced-motion：官方逻辑直接切换。
- 浏览器不支持 View Transition：官方 fallback 直接切换。
- Sun / Moon 继续使用官方 `ActionSwapIcon`。
- 不额外套 Tooltip。
- 不增加第三套 ThemeToggle CSS。

---

# S01-4：修 UpdateButton

`UpdateButton.tsx` 固定参数：

```tsx
<StatefulButton
  state={state}
  variant="primary"
  size="sm"
  ripple={false}
  loadingText="检查中…"
  successText="已是最新"
  errorText="检查失败"
>
```

业务状态保持：

```text
idle      → 检查更新
loading   → 检查中…
success   → 已是最新
error     → 检查失败
upgrade   → 版本升级
```

必须删除当前按钮右侧：

```text
发现新版本 v...
```

及其承载 `<span>`。

最终 Header 左侧只能是：

```text
Usagi  [检查更新 / 版本升级]
```

不得新增 Badge、版本号、Tooltip 或其它辅助控件。

`useUpdateController` 业务逻辑不重写。

---

# S01-5：修上次同步 + SyncButton

## 上次同步

最终结构：

```text
上次同步：HH:mm:ss
```

整行：

```text
text-sm
text-muted-foreground
whitespace-nowrap
```

时间值使用：

```tsx
<ActionSwapText
  value={syncText}
  animation="blur"
>
  {syncText}
</ActionSwapText>
```

删除时间值当前单独的：

```text
text-foreground
```

不得：

- 改成 `text-xs`；
- 自定义 opacity；
- 用 native CSS transition 替换 ActionSwapText；
- 删除官方 width morph。

## SyncButton

固定：

```tsx
<StatefulButton
  variant="outline"
  size="sm"
  ripple={false}
  ...
>
  同步数据
</StatefulButton>
```

状态保持：

```text
idle      同步数据
loading   同步中…
success   同步完成
error     同步失败
```

现有成功态 1600ms 业务反馈窗口可保留；本 Spec 不重新设计 controller 状态机。

---

# S01-6：修停止服务按钮与 Toast

## Stop Service Button

固定：

```tsx
<StatefulButton
  variant="outline"
  size="sm"
  ripple={false}
  loadingText="停止中…"
  ...
>
  停止服务
</StatefulButton>
```

唯一批准的视觉例外固定为：

```text
border-destructive/35
text-destructive
hover:bg-destructive/10
hover:text-destructive
```

不得增加：

- destructive 专属 Button 分叉；
- 自定义 press scale；
- 自定义 duration；
- 自定义 radius；
- 自定义 icon。

## Toast

继续使用同一 toast update 流程：

```text
点击停止服务
→ loading：正在停止服务
→ success：同一个 toast 更新为 服务已停止
→ error：同一个 toast 更新为 停止服务失败
```

loading：

```ts
{
  status: "loading",
  title: "正在停止服务",
  duration: 0,
  dismissible: false
}
```

success update：

```ts
{
  status: "success",
  title: "服务已停止",
  dismissible: true
}
```

error update：

```ts
{
  status: "error",
  title: "停止服务失败",
  dismissible: true
}
```

success / error **不得传 `duration: 4200`**。

由官方：

```text
useAnimatedToastStack defaultDuration
```

负责默认持续时间。

不得修改官方：

- position；
- maxVisible；
- status icon morph；
- swipe；
- layout animation；
- reduced-motion；
- defaultDuration。

---

# S01-7：统一 Dashboard 一级分区 32px

## 目标结构

`DashboardPage.tsx` 中建立一个唯一的一级纵向 stack：

```tsx
<main className="dashboard-content">
  <div className="flex flex-col gap-8">
    <header>...</header>
    <section className="dashboard-controls">...</section>
    <section className="metrics-section">...</section>
    <ChartSection ... />
    <SessionSection ... />
  </div>

  <div className="sr-only" ... />
</main>
```

最终一级关系必须是：

```text
Header
↓ 32px
Controls
↓ 32px
Metrics
↓ 32px
Charts
↓ 32px
Sessions
```

## CSS 清理

`index.css`：

删除：

```css
.dashboard-controls {
  margin-top: 32px;
}
```

中的 `margin-top`。

删除：

```css
.metrics-section {
  margin-top: 32px;
}
```

中的 `margin-top`。

类本身若没有其它用途可删除；若保留，不得继续承担一级 spacing。

`ChartSection.tsx`：

```tsx
<section className="mt-4" ...>
```

改为不带一级 `mt-*`：

```tsx
<section ...>
```

`SessionSection.tsx` 同样删除：

```text
mt-4
```

不得修改：

- Chart 内部 `gap-4`；
- Skills 图当前内部 `mt-*`；
- Session 标题与 Table 的 `mb-3`；
- Controls 内部错误提示的 `mt-3`。

这些属于 section 内部 spacing，不是本 Spec 的一级 32px rhythm。

---

# S01-8：清理旧实现

完成业务接线后，做一次只针对 Spec01 的残留扫描。

必须清除：

```text
Header StatefulButton size="md"
ThemeToggle start="top-right"
ThemeToggle rounded-full / bg-transparent / hover:bg-primary/5
UpdateButton 额外版本反馈 span
ActionSwapText 时间值 text-foreground
Toast update duration: 4200
ChartSection 一级 mt-4
SessionSection 一级 mt-4
dashboard-controls 一级 margin-top
metrics-section 一级 margin-top
```

同时检查：

- 没有新增 `UsagiButton` / `HeaderButton` primitive。
- 没有新增自制 Toast。
- 没有新增自制 Theme Toggle。
- 没有复制官方 `sm` class 到业务层。
- 没有改 BeUI 官方 Motion 参数。
- 没有新增 `next-themes` 运行时依赖。

---

# 5. 最小测试标准

原则：只覆盖本 Spec 改动，不为纯视觉细节建立大量脆弱 snapshot。

## T-S01-001：Theme 首帧

自动测试或浏览器脚本至少覆盖：

```text
localStorage 无记录 → html 首帧有 dark
localStorage = dark → html 首帧有 dark
localStorage = light → html 首帧无 dark
非法值 → html 首帧有 dark
```

浏览器人工补验：

- 刷新 Dark 页面无白色首帧。
- 刷新 Light 页面无黑色首帧。

**PASS：首次可见画面直接是目标 Theme。**

## T-S01-002：Theme Toggle

浏览器验收：

1. Dark → Light。
2. Light → Dark。
3. 使用 `circle-blur`。
4. Reveal 从屏幕**底部中间**扩张。
5. Theme Toggle 外观与 BeUI 官方 Preview 一致：
   - rounded-xl
   - subtle border
   - background
   - 20px icon
6. 不再是当前 MU 圆形透明按钮。

**PASS：两方向切换正确，起点和控件视觉均符合上述固定参数。**

## T-S01-003：Header 固定参数

检查 DOM / props：

```text
UpdateButton   primary / sm / ripple=false
SyncButton     outline / sm / ripple=false
ServiceButton  outline / sm / ripple=false
```

视觉人工验收：

- 三个文字按钮均为 BeUI 官方 sm 外观。
- hover / press / state motion 与 BeUI 官方一致。
- `Usagi` 保持 30 / 700 / 36 / normal。
- Header 桌面仍为左右单行布局。

**PASS：无 md 文字按钮、无自制 hover/press。**

## T-S01-004：Update + Last Sync

验证：

- Update 状态仍可到：
  `检查更新 / 检查中… / 已是最新 / 检查失败 / 版本升级`
- 出现新版本时 Header 不显示额外“发现新版本 v...”文本。
- 上次同步格式仍为 `上次同步：HH:mm:ss`。
- 时间变化使用官方 ActionSwapText blur。
- 整行均为 muted 层级，时间值不再单独 foreground。

**PASS：业务状态未回归，视觉层级符合固定规则。**

## T-S01-005：停止服务 Toast

使用现有 mock / test client 验证：

成功：

```text
loading toast
→ 同一 toast id 更新 success
```

失败：

```text
loading toast
→ 同一 toast id 更新 error
```

并断言：

- loading `duration=0`；
- success/error patch 不显式传 `4200`；
- success/error 可 dismiss。

**PASS：没有生成第二个结果 toast，默认 duration 由 BeUI hook 接管。**

## T-S01-006：一级 32px spacing

在桌面 viewport 实际测量以下相邻一级区域 top/bottom 距离：

```text
Header → Controls = 32px
Controls → Metrics = 32px
Metrics → Charts = 32px
Charts → Sessions = 32px
```

允许浏览器像素舍入误差：

```text
31–33px
```

同时确认：

- ChartSection 无额外一级 `mt-4`；
- SessionSection 无额外一级 `mt-4`；
- Controls / Metrics 不再各自维护 `margin-top`。

**PASS：四组一级间距全部一致。**

---

# 6. 必跑命令

在 `frontend/`：

```bash
npm run build
```

必须 PASS。

针对本 Spec 的相关测试：

```bash
npm test -- \
  src/theme/ThemeProvider.test.tsx \
  src/dashboard/DashboardPage.test.tsx \
  src/dashboard/ServiceButton.test.tsx \
  src/ui/beui/button/stateful.test.tsx
```

若施工中新增/修改了 `UpdateButton`、`SyncButton` 的单测，将对应文件加入同一次 Vitest 命令。

不要求在 Spec01 阶段跑整套与 Table / Drawer / Charts 相关的视觉 Gate；这些区域将在各自 Spec 中验收。

---

# 7. Gate S01

Spec01 只有满足以下全部条件才允许进入 Spec02。

## Gate S01-A：源码来源

- Header 使用的 Button / StatefulButton、ActionSwap、ThemeToggle、AnimatedToastStack 已与 BeUI 当前官方源码对齐。
- 除 import path、ThemeProvider bridge、批准的 destructive class 外，没有 primitive 内部自定义。
- 不存在“看起来像官方”但来源不可核验的替代实现。

## Gate S01-B：Theme

- 仅 Dark / Light。
- 默认 Dark。
- localStorage 持久化。
- 首帧无 Theme flash。
- Theme Toggle = `circle-blur + bottom-up`。
- reduced-motion / unsupported fallback 保留官方行为。

## Gate S01-C：Header

- `Usagi`：30 / 700 / 36 / normal。
- Update：`primary / sm / ripple=false`。
- Sync：`outline / sm / ripple=false`。
- Stop：`outline / sm / ripple=false` + 固定 destructive semantic override。
- Update 无额外版本文字。
- Last Sync 整行 muted。
- Theme Toggle 最右，使用官方 Preview 外观。

## Gate S01-D：Toast

- loading → same-toast success/error。
- success/error 不硬编码 4200ms。
- 官方 Toast 的 status morph / swipe / layout / reduced-motion 未被删改。

## Gate S01-E：Layout

四个一级 section gap 全部为 32px：

```text
Header → Controls
Controls → Metrics
Metrics → Charts
Charts → Sessions
```

不得存在一级 margin 与 `gap-8` 叠加。

## Gate S01-F：工程检查

```text
npm run build = PASS
Spec01 targeted tests = PASS
```

任何一项 FAIL，Spec01 不通过，不进入 Spec02。

---

# 8. 施工员禁止事项

1. 禁止为了“更像 BeUI”手写新的 Button / ThemeToggle / Toast / ActionSwap。
2. 禁止简化 BeUI StatefulButton 的逐字 DOM。
3. 禁止把 BeUI `size="sm"` 翻译成 MU 自己的一组 Tailwind class。
4. 禁止修改 BeUI hover / press / spring / easing / duration。
5. 禁止修改已通过的 Neutral Theme 色板。
6. 禁止引入 System Theme。
7. 禁止引入 `next-themes` 替换现有 ThemeProvider。
8. 禁止借 Spec01 修改筛选、KPI、Table、Drawer、图表内部样式。
9. 禁止把 Chart / Session 的内部 spacing 当作一级 32px spacing 一起重写。
10. 禁止新增无法说明官方来源的 `ui/beui/*` primitive。
11. 禁止以“代码和官方很像”“视觉差不多”作为完成依据；必须能说明对应 Registry / 官方源码来源，并通过实际页面验收。
