# Usagi v0.2.0 BeUI 整改实施方案 — Spec05：Drawer

> 施工分支：`feat/v0.2.0-beui-redesign`  
> 前置条件：**Spec04 / Gate S04 已通过。**  
> 本 Spec 只处理 Session Drawer、Drawer 内部 Summary / Main / Subagent、Bouncy Accordion、Receipt、Session ID、时间格式、Loading / Error。  
> 图表不在本 Spec。

---

# 1. Spec05 范围

## 1.1 必须完成

1. Drawer 重新以 BeUI 官方 `@beui/drawer` 为基线同步。
2. Drawer 官方 baseline 上只保留 MU 必要适配：
   - 右侧 480px；
   - `≤480px` 时 `100vw`；
   - 必要 focus trap / first-focus；
   - focus restore 只保留一个 owner，禁止 Drawer 与 controller 双重 restore。
3. 删除 Header 手动刷新按钮及其全部 UI。
4. Drawer Header：
   - Title
   - Session ID
   - Last Active
   - Close
5. Drawer 内所有 Session ID / Thread ID **禁止 Tooltip**。
6. Header root Session ID 正常单行显示。
7. Subagent Thread ID 加宽 value 区，正常 UUID 必须完整显示。
8. Tooltip 使用 Gate S04 已通过的标准 BeUI Tooltip，只用于确实需要的长标题。
9. Divider 改成统一低密度 `ReceiptDivider`：
   - 1px
   - dash 6px
   - gap 8px
   - `--border-strong`
10. 删除 Header 实线 divider，禁止“实线 + 虚线”叠加。
11. 字体不再建立 Drawer 专用 px 字号体系：
   - Header title：Tailwind `text-base`
   - MU receipt / metadata / section heading：Tailwind `text-sm`
   - Bouncy Accordion 内部 typography：完全使用官方默认
12. Drawer composition spacing 收口为：
   - 小：8px
   - 中：16px
   - 大：24px
13. Bouncy Accordion 重新以 BeUI 官方 `@beui/bouncy-accordion` 为基线同步。
14. Main / Subagent 第一阶段 **不允许任何 visual `classNames` override**。
15. 保留两组独立 single-open：
   - Main 内最多 1 个
   - Subagent 内最多 1 个
   - 两组可同时各展开 1 个
16. Summary 4 行 NumberTicker 继续保留，但 Drawer Summary 全部 `blur=false`。
17. 修复 Summary 动画结束后的二次闪烁。
18. Detail Usage Token 全部使用千位分隔。
19. Detail Usage 不使用 NumberTicker。
20. Detail Usage 静态数字使用 `tabular-nums`。
21. Subagent `Last Active` 真正显示到秒。
22. Cost completeness 继续复用 Gate S03 已官方化的 BeUI Gooey Popover。
23. Skeleton 按真实 Summary / Main / Subagent 结构重做。
24. 自动 revision 刷新继续保留；删除的只是用户手动 Refresh action。
25. 已有详情更新失败继续：
   - 保留旧数据；
   - 官方 Animated Toast Stack 显示 `详情更新失败`。

## 1.2 明确不做

- 不修改 Session Table。
- 不修改 Session Detail 后端 API。
- 不修改 Drawer 详情数据口径。
- 不增加 Copy 按钮。
- 不增加手动 Refresh 的替代入口。
- 不增加 Drawer Tabs。
- 不给 Session ID / Thread ID 增加 Tooltip。
- 不修改 BeUI Bouncy Accordion 的 trigger 高度、padding、radius、Chevron、spring。
- 不为了“统一字号”覆盖 Bouncy Accordion 官方 `15px` title / description。
- 不新增自制 Accordion。
- 不新增自制 Tooltip。
- 不给 Drawer 增加 glass / blur card / gradient surface。

---

# 2. BeUI 组件来源与固定边界

## 2.1 Drawer

必须以 BeUI 当前官方 Drawer 完整源码为 baseline。

官方 baseline 必须保留：

```text
AnimatePresence
backdrop = bg-black/40 + backdrop-blur-sm
backdrop fade = 0.25 + EASE_OUT
panel = bg-background
shadow-2xl
right/left semantic border
SPRING_PANEL
Escape close
body scroll lock
reduced-motion
```

MU 只允许：

```text
side="right"
width = 480px
max-width = 100vw
<=480px = 100vw
```

以及第 4 节定义的最小 focus 扩展。

不得因为当前本地 `drawer.tsx` 已经“很像官方”而跳过重同步。

## 2.2 Bouncy Accordion

必须重新以当前官方 `@beui/bouncy-accordion` 为 baseline。

官方必须保留：

```text
item surface = bg-card text-card-foreground
connected group radius = 28px
separated row margin = 12px
trigger min-height = 54px
trigger px = 20px
title = 15px / medium
description = 15px / line-height 24px
ChevronDown
Chevron spring
ROW_TRANSITION
CONTENT_OPEN_TRANSITION
CONTENT_CLOSE_TRANSITION
DESCRIPTION_TRANSITION
ResizeObserver height
single-open controllable state
reduced-motion
```

业务调用禁止：

```tsx
classNames={{ ...visualOverrides }}
```

本 Spec 第一版要求 `classNames` 完全不传。

## 2.3 Tooltip

沿用 Gate S04 已通过的标准 BeUI Tooltip。

Drawer 内只允许：

- Session Title 超长；
- Subagent Title 超长；
- 其它真正需要补充完整文本的非 ID 内容。

明确禁止：

```text
root Session ID Tooltip
Subagent Thread ID Tooltip
```

## 2.4 Popover / Toast / NumberTicker

沿用前置 Gate：

```text
Popover → Gate S03
NumberTicker → Gate S03
AnimatedToastStack → Gate S01
Button → Gate S01
```

Spec05 不再次 fork 这些 primitive。

---

# 3. 允许新增 / 保留的 MU composition

本 Spec 最多允许以下业务 composition：

1. `ReceiptDivider`
2. `UsageReceipt`（可保留现有名称；Main / Subagent 共用）
3. `SummaryReceipt`（现有业务组件）
4. `SubagentReceipt`（现有业务组件）

禁止再拆出：

```text
DrawerCard
DrawerRow
DrawerLabel
DrawerValue
DrawerHeaderButton
DrawerAccordion
DrawerTooltip
```

等纯视觉小组件。

---

# 4. Drawer focus 责任边界

当前 controller 已经维护：

```text
previousFocusRef
close_detail() 时 restoreFocus()
```

因此 Drawer primitive 不再额外做第二次 focus restore。

### Drawer 最小 accessibility patch

官方 Drawer baseline 上只允许增加：

```text
panelRef
tabIndex=-1
open 后 first-focus
Tab / Shift+Tab focus trap
```

打开后：

1. 找 Drawer 内第一个可 focus 控件；
2. 优先 focus Close；
3. 无可 focus 控件则 focus panel。

Tab：

```text
first ↔ last
```

循环。

Escape：

- 继续使用官方 Drawer 自身逻辑；
- 不再另写第二套 Escape listener。

关闭后的 focus restore：

```text
由 useSessionDetailController.close_detail() 负责
```

这样不会出现双重 `.focus()`。

---

# 5. 文件范围

## 5.1 主要修改

```text
frontend/src/dashboard/session/SessionDetailDrawer.tsx
frontend/src/dashboard/session/SessionDetailDrawer.test.tsx
frontend/src/dashboard/session/useSessionDetailController.ts
frontend/src/dashboard/session/useSessionDetailController.test.tsx
frontend/src/dashboard/session/sessionFormat.ts
frontend/src/dashboard/session/sessionFormat.test.ts

frontend/src/dashboard/shared/CostValue.tsx

frontend/src/ui/beui/drawer.tsx
frontend/src/ui/beui/bouncy-accordion.tsx
```

## 5.2 只读依赖

```text
frontend/src/ui/beui/tooltip.tsx
frontend/src/ui/beui/popover.tsx
frontend/src/ui/beui/number-ticker.tsx
frontend/src/ui/beui/animated-toast-stack.tsx
frontend/src/ui/beui/button/**
frontend/src/ui/lib/ease.ts
frontend/src/ui/lib/cn.ts
frontend/src/data/types.ts
```

如果这些前置 Gate primitive 与官方 baseline 不一致，先恢复对应 Gate，不允许在 Drawer 调用层绕过。

## 5.3 禁止扩大

```text
frontend/src/dashboard/charts/**
frontend/src/dashboard/MetricGrid.tsx
frontend/src/dashboard/FilterControls.tsx
frontend/src/dashboard/session/SessionTable*
src/**
```

---

# 6. 实施顺序

# S05-1：重新同步官方 Drawer

先把 `frontend/src/ui/beui/drawer.tsx` 恢复为当前官方源码。

只做：

- import path 机械适配；
- S05-2 的 focus trap patch。

不要保留当前本地 primitive 中其它“增强”。

### 业务调用宽度

`SessionDetailDrawer.tsx`：

```tsx
<Drawer
  open={view.open}
  onOpenChange={(open) => {
    if (!open) view.close_detail();
  }}
  side="right"
  ariaLabel="Session 详情"
  className="w-[480px] max-w-full max-[480px]:w-screen"
/>
```

注意：

官方 baseline 自带 `w-80 max-w-[85vw]`，调用层 width class 必须能够覆盖为 480px。

若 Tailwind class merge 顺序不能稳定覆盖，允许把：

```text
!w-[480px]
max-[480px]:!w-screen
```

作为调用层最小 width override。

禁止修改 Drawer 官方默认 width 以适配 MU。

---

# S05-2：给 Drawer 增加最小 focus trap

只增加：

```text
panelRef
FOCUSABLE selector
first-focus
Tab cycle
tabIndex=-1
```

不增加：

- 第二套 Escape；
- 第二套 body scroll lock；
- Drawer 内 focus restore；
- 自定义 backdrop dismissal。

Drawer close 后由 controller 恢复原 Session row focus。

---

# S05-3：删除手动 Refresh UI

从 `SessionDetailDrawer.tsx` 删除：

```text
RefreshCw import
ActionSwapIcon import
Loader import
refreshing button
“刷新当前详情” Tooltip
```

Header 最终右侧只剩：

```text
[Close]
```

Close：

```tsx
<Tooltip content="关闭" side="bottom">
  <Button
    variant="ghost"
    size="icon"
    aria-label="关闭 Session 详情"
    onClick={view.close_detail}
  >
    <X />
  </Button>
</Tooltip>
```

X 的具体尺寸先服从 Gate S01 Button 组合；Final Gate 再做全站 icon consistency，不在本 Spec另造规则。

---

# S05-4：清理 controller 公开手动刷新接口

当前：

```text
refresh_detail
retry_detail
```

都映射内部 `refreshDetail()`。

删除 UI 后：

- 从 `SessionDetailControllerViewModel` 删除公开 `refresh_detail`；
- return object 删除 `refresh_detail`；
- 保留内部 `refreshDetail()`；
- `retry_detail` 继续指向内部 `refreshDetail()`；
- revision feed 自动刷新仍继续调用 `loadDetail(..., true)`。

不得删除：

```text
自动 revision refresh
retry_detail
refresh_error_code
```

因为它们仍有业务用途。

---

# S05-5：Header 结构与 typography

最终：

```text
Session Title                                   [Close]
Session ID
Last Active
```

Header：

```text
px-5 pt-5 pb-4
```

Title：

```text
text-base
font-semibold
leading-6
text-foreground
truncate
```

Session ID：

```text
text-sm
leading-5
text-muted-foreground
tabular-nums
whitespace-nowrap
```

Last Active：

```text
text-sm
leading-5
text-muted-foreground
tabular-nums
```

Title / ID / time 内部：

```text
space-y-1
```

### 删除

```text
text-[10px]
Header border-b
root Session ID Tooltip
```

Session Title 继续允许标准 Tooltip。

---

# S05-6：Header → Summary Divider

Header 后：

```tsx
<div className="px-5">
  <ReceiptDivider />
</div>
```

`ReceiptDivider` 固定：

```tsx
function ReceiptDivider() {
  return (
    <div
      aria-hidden
      className="h-px w-full"
      style={{
        backgroundImage:
          "repeating-linear-gradient(to right, var(--border-strong) 0 6px, transparent 6px 14px)",
      }}
    />
  );
}
```

精确：

```text
dash = 6px
gap = 8px
height = 1px
color = --border-strong
```

禁止使用普通：

```text
border-dashed
```

来替代本章的 receipt divider，因为浏览器无法保证 dash/gap 密度。

---

# S05-7：Drawer body spacing

Body：

```text
px-5 pt-4 pb-5
```

Ready 内容使用：

```text
Summary
↓ 24px
Main
↓ 24px
Subagent
```

固定规则：

### 小间距 8px

```text
Section heading → Accordion
Receipt row → receipt row
metadata row → metadata row
```

### 中间距 16px

```text
Divider → Summary
Subagent metadata → divider → Usage
```

### 大间距 24px

```text
Summary → Main
Main → Subagent
```

不要再使用顶层：

```text
space-y-6 + heading mb-3
```

造成 24 / 12 混合节奏。

推荐结构：

```tsx
<div>
  <SummaryReceipt />

  <section className="mt-6">
    <h3 className="mb-2 ...">Main ({n})</h3>
    <BouncyAccordion ... />
  </section>

  <section className="mt-6">
    <h3 className="mb-2 ...">Subagent ({n})</h3>
    ...
  </section>
</div>
```

---

# S05-8：Summary Receipt

固定 4 行：

```text
Main Tokens
Subagent Tokens
Total Tokens
Estimated Cost
```

Summary 本身：

- 不套 Card；
- 不再自带 `border-y`;
- 不使用普通 `border-dashed`。

`dl`：

```text
space-y-2
text-sm
```

Label：

```text
普通 → text-muted-foreground
Total / Estimated Cost → font-semibold text-foreground
```

Value：

```text
text-foreground
tabular-nums
```

---

# S05-9：Summary NumberTicker 禁用 blur

三项 Token：

```tsx
<NumberTicker
  value={value}
  locale
  blur={false}
  className="tabular-nums"
/>
```

允许直接省略 `blur`，但为防施工员误开，推荐显式：

```text
blur={false}
```

### Estimated Cost

`CostValue` 当前 `ticker` 会硬编码：

```text
NumberTicker blur
```

修复方式：

优先把 `CostValue` 改为：

```ts
ticker?: boolean
tickerBlur?: boolean
```

默认：

```text
tickerBlur = false
```

内部：

```tsx
<NumberTicker
  ...
  blur={tickerBlur}
/>
```

Drawer Summary：

```tsx
<CostValue
  ...
  ticker
  tickerBlur={false}
/>
```

如果其它现有 caller 明确需要 blur，再显式传 `tickerBlur={true}`。

不得修改 NumberTicker primitive 来修闪烁。

### 验收

动画：

```text
滚动一次
→ 停止
→ 不再二次 blur / flicker
```

---

# S05-10：重新同步官方 Bouncy Accordion

恢复：

```text
@beui/bouncy-accordion
```

Main / Subagent 调用：

```tsx
<BouncyAccordion
  items={items}
  value={open}
  onValueChange={setOpen}
/>
```

删除：

```tsx
classNames={{ description: "text-foreground" }}
```

以及全部其它 visual override。

### Main state

```text
mainOpen: string | null
```

### Subagent state

```text
subagentOpen: string | null
```

切换 root Session：

```text
mainOpen = null
subagentOpen = null
```

详情 revision 自动更新：

```text
不主动 collapse
```

---

# S05-11：Main

Heading：

```text
Main (N)
```

固定：

```text
text-sm
font-semibold
text-foreground
mb-2
```

Accordion collapsed title：

```text
model (reasoning effort)
```

例如：

```text
gpt-5.6-sol (high)
```

Bouncy Accordion 内 title typography 完全官方，不给 `classNames.title`。

Detail：

```text
UsageReceipt
```

---

# S05-12：Detail Usage 千位分隔

保留字段顺序：

1. Total Tokens
2. Input
3. Output
4. Reasoning
5. Cache Read
6. Cache Write
7. Cache Hit Rate
8. Estimated Cost

Token formatter：

```text
formatSessionTokenInteger
formatSessionNullableTokenInteger
```

禁止：

```text
String(usage.total_tokens)
String(usage.input_tokens)
...
```

例如：

```text
61470341
```

必须：

```text
61,470,341
```

静态：

```text
tabular-nums
```

不使用 NumberTicker。

### `UsageReceipt`

Main / Subagent 100% 复用同一组件。

`dl`：

```text
space-y-2
text-sm
```

每行：

```text
flex
items-baseline
justify-between
gap-4
```

---

# S05-13：Subagent collapsed state

Heading：

```text
Subagent (N)
```

collapsed trigger 只显示：

```text
Subagent Title
```

不加入：

- Thread ID
- Model
- Last Active

长 title：

- truncate；
- 允许 Gate S04 官方 Tooltip。

---

# S05-14：Subagent metadata 布局

展开固定三行：

```text
Thread ID
Model
Last Active
```

使用：

```text
grid-cols-[72px_minmax(0,1fr)]
gap-x-4
gap-y-2
text-sm
```

Label：

```text
text-muted-foreground
```

Value：

```text
text-right
text-foreground
```

### Thread ID

固定：

```text
whitespace-nowrap
tabular-nums
```

禁止：

```text
max-w-64
truncate
Tooltip
title tooltip
```

正常 36-char UUID 在 480px Drawer 必须完整显示。

如果实测 72px label 列导致标准 UUID 仍不完整，优先把 label 列降到：

```text
64px
```

不得缩小字体，不得恢复 Tooltip，不得截断 ID。

### Model

允许：

```text
truncate
title={fullModel}
```

因为它不是 Session ID。

---

# S05-15：Subagent Last Active 显示到秒

在 `sessionFormat.ts` 新增：

```ts
formatSessionTimeWithSeconds(...)
```

复用现有 `dateParts()`。

固定输出：

### 同一天

```text
HH:mm:ss
```

### 同一年不同日

```text
MM-DD HH:mm:ss
```

### 跨年

```text
YYYY-MM-DD HH:mm:ss
```

`title`：

```text
YYYY-MM-DD HH:mm:ss
```

Subagent metadata 必须使用这个 formatter。

Header Last Active 继续使用现有 `formatSessionTime()`，不强制到秒。

---

# S05-16：Subagent metadata → Usage Divider

Metadata 后：

```text
16px
ReceiptDivider
16px
UsageReceipt
```

实现：

```tsx
<div className="my-4">
  <ReceiptDivider />
</div>
```

不使用：

```text
border-t border-dashed
```

---

# S05-17：Cost completeness

Summary / Main / Subagent 全部继续共用：

```text
CostValue
```

状态：

### complete

```text
$X.XX
无 warning
```

### partial

```text
$X.XX [!]
Popover: 有部分费用不完整
```

### unknown

```text
— [!]
Popover: 当前费用无法完整估算
```

Popover 必须是 Gate S03 标准 Gooey Popover。

数字本身不变红。

本 Spec 不再重新设计 trigger icon，沿用 Gate S03 已验收实现。

---

# S05-18：Skeleton

首次：

```text
Drawer 立即打开
```

Body Skeleton 模拟真实结构：

```text
Summary
  row
  row
  row
  row

24px

Main
  heading
  collapsed row
  collapsed row

24px

Subagent
  heading
  collapsed row
  collapsed row
```

Skeleton collapsed row：

```text
min-height ≈ 54px
rounded-[28px]
bg-muted
animate-pulse
```

只是 loading placeholder，不模拟 Bouncy Accordion Motion。

不得只保留两根 generic 圆条。

---

# S05-19：首次 error / retry

首次 detail 加载失败：

```text
Session 详情加载失败
[重试]
```

Retry：

```text
Button secondary / sm
```

保留现有 inline error block。

不增加 Toast 作为首次 error 唯一入口。

---

# S05-20：自动更新 / refresh error

虽然手动 Refresh Button 删除：

```text
revision feed
→ 自动刷新当前 detail
```

必须保留。

已有 detail 更新时：

- 旧 detail 保持可见；
- `aria-busy=true` 可保留；
- 不显示全屏 Skeleton；
- Accordion open state 不重置。

更新失败：

```text
旧 detail 保持
AnimatedToastStack:
详情更新失败
```

使用 Gate S01 官方 Toast。

不恢复手动 Refresh Button。

---

# S05-21：清理旧实现

必须删除：

```text
RefreshCw
ActionSwapIcon
Loader
refresh UI Button
root Session ID Tooltip
Thread ID Tooltip
Thread ID max-w-64 / truncate
Header text-[10px]
Header border-b
Summary border-y border-dashed
Subagent border-t border-dashed
BouncyAccordion visual classNames override
Detail Usage String(token)
Summary NumberTicker blur=true
CostValue ticker hardcoded blur=true
top-level space-y-6 + heading mb-3 mismatch
generic 2-row accordion Skeleton
```

---

# 7. 最小测试标准

# T-S05-001：Drawer baseline / width / focus

浏览器：

- Drawer surface 为官方 `bg-background`，不是透明 panel。
- backdrop / shadow / side border 与官方 Drawer 一致。
- desktop width = 480px。
- viewport ≤480px = 100vw。
- 打开后 focus 进入 Drawer。
- Tab 不逃出 Drawer。
- Escape 关闭。
- 关闭后 focus 回到合理 Session row。

源码：

- Drawer baseline 与官方一致；
- 只存在 width 调用层适配 + focus trap patch。

---

# T-S05-002：Header / ID / divider

验证：

- Header 只有 Close，无 Refresh。
- root Session ID 无 Tooltip。
- Header 无实线 divider。
- Header 下只有统一 ReceiptDivider。
- `text-[10px]` 不再存在于 Drawer。

---

# T-S05-003：Summary / NumberTicker

使用现有 fixture：

```text
Main Tokens = 1,801
Subagent Tokens = 1,800
Total Tokens = 3,601
Estimated Cost = $1.20
```

验证：

- 4 行。
- Token NumberTicker `blur=false`。
- Cost ticker `blur=false`。
- 动画完成后不发生第二次闪烁。

二次闪烁属于浏览器人工验收，不为它编写脆弱的动画 timing 单测。

---

# T-S05-004：Accordion

验证：

- Main / Subagent 初始均 collapsed。
- Main 内 single-open。
- Subagent 内 single-open。
- Main 和 Subagent 可同时各展开 1 个。
- 切换 root Session 两组 reset。
- 视觉必须出现官方 Bouncy Accordion：
  - `bg-card`
  - 28px connected radius
  - 54px trigger
  - 官方 Chevron Motion

不接受“ARIA 行为通过但视觉不是官方”的结果。

---

# T-S05-005：Usage / Subagent metadata

展开 Main：

- `1,234` / `1,801` 等 Token 有千位分隔。
- 字段顺序固定。

展开 Subagent：

- Thread ID 无 Tooltip。
- 标准 UUID 完整可见。
- Model 正确。
- Last Active 显示到秒。
- metadata → Usage 使用低密度 ReceiptDivider。

---

# T-S05-006：Loading / error / auto refresh

仅覆盖：

1. 首次 loading Skeleton 有 Summary/Main/Subagent 结构。
2. 首次 error 可 retry。
3. 已有 detail 自动 refresh 时旧数据继续显示。
4. refresh error → `详情更新失败` Toast。
5. UI 中不存在手动 refresh action。

不重复测试 controller 所有缓存 / revision edge case。

---

# 8. 必跑命令

在 `frontend/`：

```bash
npm run build
```

相关测试：

```bash
npm test -- \
  src/dashboard/session/SessionDetailDrawer.test.tsx \
  src/dashboard/session/useSessionDetailController.test.tsx \
  src/dashboard/session/sessionFormat.test.ts
```

如果同步后的 Drawer / Bouncy Accordion 已有本地 primitive tests，运行对应现有测试。

无需 Rust tests。

---

# 9. Gate S05

## Gate S05-A：官方 primitive

- Drawer = 当前官方 BeUI baseline。
- Bouncy Accordion = 当前官方 BeUI baseline。
- Tooltip / Popover / NumberTicker / Toast 沿用前置 Gate。
- 不存在“看起来像官方”的替代实现。

## Gate S05-B：Drawer shell

- panel 非透明。
- 480px / ≤480 100vw。
- 官方 backdrop / shadow / side border / spring。
- focus trap 正常。
- 无双重 focus restore。

## Gate S05-C：Header / ID

- 删除 Refresh。
- Close = BeUI Button icon。
- root Session ID 不用 Tooltip。
- Subagent Thread ID 不用 Tooltip。
- 标准 UUID 完整显示。
- Header 无实线 divider。

## Gate S05-D：Receipt / spacing

- Divider = 6px dash / 8px gap。
- 8 / 16 / 24 composition spacing。
- 无 `text-[10px]` / `text-[11px]` Drawer typography。
- Bouncy Accordion 内部字号 / spacing 完全官方。

## Gate S05-E：数据展示

- Summary 4 行。
- Summary NumberTicker blur=false。
- 无二次闪烁。
- Detail Usage 千位分隔。
- Subagent Last Active 到秒。
- Cost completeness 正确。

## Gate S05-F：Loading / update

- 结构 Skeleton。
- 首次 error retry。
- 自动 revision refresh 保留。
- refresh error 旧数据保留 + Toast。
- 无手动 Refresh UI。

## Gate S05-G：工程

```text
npm run build = PASS
Spec05 targeted tests = PASS
```

全部通过才允许进入 Spec06。

---

# 10. 施工员禁止事项

1. 禁止因为当前 Drawer 源码“接近官方”而不重同步。
2. 禁止修改官方 Drawer backdrop / spring / shadow / surface。
3. 禁止同时由 Drawer 和 controller 做 focus restore。
4. 禁止恢复手动 Refresh Button。
5. 禁止 Session ID / Thread ID Tooltip。
6. 禁止通过缩小到 10px/11px 来塞下 Thread ID。
7. 禁止用 `border-dashed` 冒充本章低密度 ReceiptDivider。
8. 禁止修改 Bouncy Accordion 官方 surface / radius / trigger / Chevron。
9. 禁止传 Bouncy Accordion visual `classNames`。
10. 禁止新建 Drawer 专属 Button / Tooltip / Accordion。
11. 禁止 Detail Usage 使用 NumberTicker。
12. 禁止 Detail Token 使用 `String(...)`。
13. 禁止修改 NumberTicker primitive 来解决二次闪烁。
14. 禁止借 Spec05 修改 Table / Charts。
15. 禁止以“功能能用”代替官方视觉验收。
