# Usagi v0.2.0 BeUI 整改实施方案 — Spec06：图表区域 UI

> 施工分支：`feat/v0.2.0-beui-redesign`  
> 文档可先审核；实际施工顺序按总计划，应在 **Spec05 / Gate S05 通过后**执行。  
> 本 Spec 只处理三张图表的 UI / Motion / Palette / Popover。**Skills parser、Skill locator、parser version、rebuild 全部属于 Spec07，本 Spec 禁止修改。**

---

# 1. Spec06 范围

三张图：

```text
模型分布
项目分布
Skills Used
```

## 1.1 必须完成

1. 新增唯一允许的 MU 图表容器 composition：
   - `ChartSurface`
2. 三张图统一 ChartSurface：
   - `bg-card`
   - 1px `border-border`
   - `rounded-[28px]`
   - `p-5`
   - `text-card-foreground`
3. 模型 / 项目两张 Donut 卡继续共用 `DistributionDonutCard`。
4. Donut 真正改为 140×140。
5. Donut wrapper 同样 140×140。
6. Donut 固定顶部锚定，不随右侧 Legend 行数变化上下位移。
7. Token / 费用切换使用 Spec02 已通过的官方 BeUI Tabs Pill，禁止尺寸覆写。
8. 模型 / 项目 Legend：
   - swatch 10×10
   - 12px
   - name/value/percentage 同级 muted
   - 紧凑 row rhythm
9. 废止 ID hash 配色。
10. 建立固定 Project Folder chart palette：
    - Light
    - Dark
    - Top5 / Top10 按排名 index 映射
    - Other 单独 muted token
11. 自定义 chart focus Motion 改用 BeUI shared Motion token。
12. `Skills used` 改为：
    - `Skills Used`
13. Skills Card 使用同一个 `ChartSurface`。
14. Skills plot 继续保持现有约 168px plot 高度逻辑，Card 高度随 Legend 自动增长。
15. X/Y Axis 字号改为 12px。
16. Skills Legend：
    - 12px
    - swatch 10×10
17. 日期 rich hover 必须改为 Spec03 已通过的标准 BeUI Gooey Popover。
18. Gooey Popover 内文字层级按本文固定。
19. 日期 Plot hit detection 与 Area hover 必须可同时工作。
20. 保留：
    - rolling 7d
    - Top10 + Other
    - niceScale
    - monotone stacked area
    - model/project filter scope
21. Loading Skeleton 使用同一个 ChartSurface。

## 1.2 明确不做

- 不修改 `src/**`。
- 不修改 Skill parser。
- 不修改 `SKILL_USAGE_PARSER_VERSION`。
- 不 rebuild `skill_usage_events`。
- 不修当前 Skill 数量偏少的问题；该问题只在 Spec07。
- 不修改 Skills 7d 业务口径。
- 不把图表迁移到第三方 chart library。
- 不使用 Recharts。
- 不新增另一个 Card primitive。
- 不给 ChartSurface 加 Tilt / hover Motion。
- 不给 ChartSurface 加玻璃效果。
- 不给 ChartSurface 加强黑边。
- 不自制 Tooltip / Popover。
- 不建立图表专属 easing / duration 体系。

---

# 2. 组件与来源

## 2.1 已通过的 BeUI primitives

Spec06 复用前置 Spec 已通过版本，不再次魔改：

| 用途 | 来源 | 来自 |
|---|---|---|
| Token / 费用切换 | `@beui/tabs` | Gate S02 |
| Donut / Skills total | `@beui/number-ticker` | Gate S03 |
| 日期 rich hover | `@beui/popover` | Gate S03 |
| shared Motion | `frontend/src/ui/lib/ease.ts` | BeUI shared token |

如果实际施工时发现这些 primitive 已被后续 Spec 改动，必须先重新与对应 Gate baseline 对齐，不能在图表调用层补丁修复。

## 2.2 唯一新增 composition：`ChartSurface`

建议文件：

```text
frontend/src/dashboard/charts/ChartSurface.tsx
```

固定：

```tsx
export function ChartSurface({
  className,
  children,
  ...articleProps
}: React.HTMLAttributes<HTMLElement>) {
  return (
    <article
      {...articleProps}
      className={cn(
        "min-w-0 rounded-[28px] border border-border bg-card p-5 text-card-foreground",
        className,
      )}
    >
      {children}
    </article>
  );
}
```

用途仅限：

```text
模型分布
项目分布
Skills Used
对应 Skeleton
```

### ChartSurface 明确不是 BeUI Card

文档 / 代码注释不得称其为：

```text
BeUI Card
```

它是：

> MU 最小 composition，复用 BeUI `card / border / foreground` semantic token 和 Bouncy Accordion 类似的 28px 圆角 surface 语言。

### 禁止添加

```text
shadow-xl
backdrop-blur
glass
hover shadow
hover translate
motion.article
tilt
gradient border
```

---

# 3. ChartSurface 固定视觉

三张图统一：

```text
background = bg-card
foreground = text-card-foreground
border = 1px border-border
radius = 28px
padding = 20px
```

边框必须存在。

目标：

```text
可见但低对比度
```

禁止：

```text
无边框
强黑边
border-strong 作为默认外边框
透明 Card
```

---

# 4. 图表区域布局

`ChartSection.tsx` 改为一个统一 Grid：

```tsx
<section aria-label="使用分布图表" aria-busy={view.loading}>
  {error...}

  <div className="grid grid-cols-2 gap-4 max-[1279px]:grid-cols-1">
    <DistributionDonutCard ... />
    <DistributionDonutCard ... />

    <SkillsUsageChart
      className="col-span-2 max-[1279px]:col-span-1"
      ...
    />
  </div>
</section>
```

### 固定

```text
top two cards gap = 16px
Donut row → Skills row gap = 16px
```

删除 `SkillsUsageChart` 自己的：

```text
mt-4
```

Spec01 已负责删除 `ChartSection` 一级 `mt-4`；Spec06 不重新加回来。

---

# 5. 文件范围

## 5.1 主要修改

```text
frontend/src/dashboard/charts/ChartSection.tsx
frontend/src/dashboard/charts/ChartSurface.tsx             # 新增
frontend/src/dashboard/charts/DistributionDonutCard.tsx
frontend/src/dashboard/charts/SkillsUsageChart.tsx
frontend/src/dashboard/charts/chartPalette.ts
frontend/src/dashboard/charts/chartMotion.ts
frontend/src/dashboard/charts/distribution.ts              # 仅 palette index / view model 需要时
frontend/src/dashboard/charts/skillSeries.ts                # 原则上不改 ranking
frontend/src/dashboard/charts/*.test.ts / *.test.tsx
frontend/src/theme/beui.css
```

## 5.2 只读依赖

```text
frontend/src/dashboard/charts/useDashboardChartsController.ts
frontend/src/dashboard/charts/monotoneArea.ts
frontend/src/dashboard/format.ts
frontend/src/dashboard/shared/projectDisplay.ts
frontend/src/data/types.ts

frontend/src/ui/beui/tabs.tsx
frontend/src/ui/beui/number-ticker.tsx
frontend/src/ui/beui/popover.tsx
frontend/src/ui/lib/ease.ts
frontend/src/ui/lib/cn.ts
```

## 5.3 禁止扩大

```text
frontend/src/dashboard/MetricGrid.tsx
frontend/src/dashboard/session/**
frontend/src/dashboard/FilterControls.tsx
src/**
tests/**  # Rust tests不需要
```

---

# 6. 实施顺序

# S06-1：新增统一 ChartSurface

按第 2.2 节创建。

然后：

- Distribution Donut 外壳改用 ChartSurface。
- Skills Ready 外壳改用 ChartSurface。
- Skills Skeleton 外壳改用 ChartSurface。

不得三处复制相同：

```text
rounded / border / bg / p
```

---

# S06-2：重构 ChartSection 布局

用一个 Grid 承载 3 张图。

模型 / 项目：

```text
各 1 column
```

Skills：

```text
desktop col-span-2
<1280 col-span-1
```

统一：

```text
gap-4
```

不再由第三张图自己 `mt-4`。

---

# S06-3：固定 Distribution Card 高度

模型 / 项目两卡保持：

```text
h-[264px]
```

通过：

```tsx
<ChartSurface className="h-[264px]">
```

Skills 不固定同样高度，继续 auto。

“容器一致”指：

```text
surface / border / radius / padding / header baseline
```

不是强行让第三张图也 264px。

---

# S06-4：Donut 改为真实 140×140

固定：

```text
wrapper = 140px × 140px
svg = 140px × 140px
viewBox = 0 0 140 140
cx = 70
cy = 70
r = 65.5
strokeWidth = 8
pathLength = 100
strokeLinecap = butt
```

背景 ring：

```text
stroke = var(--border-strong)
```

### 外层布局

从：

```text
152px
items-center
```

改为：

```text
grid-cols-[140px_minmax(0,1fr)]
items-start
gap-4
```

即：

```tsx
<div className="mt-4 grid min-w-0 grid-cols-[140px_minmax(0,1fr)] items-start gap-4">
```

### 验收目标

右侧 Legend：

```text
1 行
3 行
6 行
```

Donut top 坐标都不变。

---

# S06-5：Distribution Tabs 恢复标准 Pill

使用 Gate S02 已通过的：

```tsx
<Tabs
  value={metric}
  onValueChange={...}
  variant="pill"
>
  <TabsList>
    <TabsTrigger value="tokens">Token</TabsTrigger>
    <TabsTrigger value="cost">费用</TabsTrigger>
  </TabsList>
</Tabs>
```

删除全部：

```text
!p-0.5
!px-2.5
!py-1
text-xs
```

### 注意

BeUI 当前 Tabs **没有 `size="sm"`**。

不得：

- 自己新增 `sm` variant；
- 给 Tabs primitive 加 size prop；
- 声称当前是“官方 sm”。

先使用完整官方 Pill。

---

# S06-6：Distribution Legend

父容器：

```text
flex flex-col gap-0.5
```

每一行固定：

```text
height = 20px
```

推荐 class：

```text
grid h-5 w-full
grid-cols-[10px_minmax(0,1fr)_auto_48px]
items-center
gap-x-2
text-xs
leading-4
text-muted-foreground
```

删除：

```text
px-1.5
py-1
space-y-1
hover:bg-primary/5
rounded-lg
```

### Swatch

```text
10px × 10px
rounded-full
```

### 三个文字列

```text
name        12px muted
value       12px muted tabular-nums
percentage  12px muted tabular-nums
```

percentage：

```text
48px column
text-right
```

静止状态禁止 value 用 `text-foreground` 提高一层。

---

# S06-7：Distribution hover/focus

整行 Legend 仍可使用语义 `button`，但它不是 BeUI Button，不得给它 Card/Button surface；它只是图表可交互 legend item。

Hover / focus：

```text
当前 item opacity = 1
其它 items opacity = 0.22
对应 Donut segment opacity = 1
其它 segments opacity = 0.22
```

保留：

```ts
CHART_FOCUS_OPACITY = 0.22;
```

删除：

```ts
CHART_FOCUS_TRANSITION = { duration: 0.18 };
```

改为直接使用：

```ts
SPRING_LAYOUT
```

来自：

```text
frontend/src/ui/lib/ease.ts
```

调用：

```tsx
transition={reduce ? { duration: 0 } : SPRING_LAYOUT}
```

不得改变：

- segment width；
- dasharray percentage；
- circle radius；
- donut position。

不使用 Tooltip。

---

# S06-8：Donut Center

继续：

```text
NumberTicker
```

主数值固定：

```text
text-lg
font-semibold
leading-6
text-foreground
```

Center label：

```text
text-xs
leading-4
text-muted-foreground
```

正常：

```text
Token
Cost
```

focus segment：

```text
对应 segment value
对应 percentage
```

NumberTicker formatter 继续复用：

```text
formatCompact
formatCompactCost
```

不新增 chart formatter。

---

# S06-9：建立 Project Folder Palette

废止当前：

```text
--chart-1 ... --chart-10 的混合语义
hash(id) % 10
```

改为明确 chart semantic tokens。

## Light

```css
--chart-mint-a: #BBF5DA;
--chart-peach-a: #FDD8AB;
--chart-sky-a: #B9E6FF;
--chart-lavender-a: #DED7FE;
--chart-butter-a: #FEF5D3;

--chart-mint-b: #7CE6B7;
--chart-peach-b: #F5B86F;
--chart-sky-b: #79CBF4;
--chart-lavender-b: #B9A8F6;
--chart-butter-b: #EAD879;

--chart-other: #A3A3A3;
```

## Dark

```css
--chart-mint-a: #5FD0A5;
--chart-peach-a: #E6A766;
--chart-sky-a: #6BBCE8;
--chart-lavender-a: #A99AE8;
--chart-butter-a: #D9C56D;

--chart-mint-b: #36B589;
--chart-peach-b: #C98543;
--chart-sky-b: #429CC8;
--chart-lavender-b: #8271D1;
--chart-butter-b: #B7A143;

--chart-other: #737373;
```

这些值作为本轮**固定施工初值**，施工员不得自行换色。

最终全站视觉 Gate 如果需要微调，只能作为新的明确验收修订，不允许 Luna 自由调整。

---

# S06-10：Palette API 改为排名 index

`chartPalette.ts` 改为 index 映射，不再接受业务 ID hash。

固定数组：

```ts
const CHART_SERIES_COLORS = [
  "var(--chart-mint-a)",
  "var(--chart-peach-a)",
  "var(--chart-sky-a)",
  "var(--chart-lavender-a)",
  "var(--chart-butter-a)",
  "var(--chart-mint-b)",
  "var(--chart-peach-b)",
  "var(--chart-sky-b)",
  "var(--chart-lavender-b)",
  "var(--chart-butter-b)",
] as const;
```

固定：

```ts
export function chartSeriesColor(
  index: number,
  isOther = false
): string {
  if (isOther) return "var(--chart-other)";
  return CHART_SERIES_COLORS[index % CHART_SERIES_COLORS.length];
}
```

### Distribution

`buildDistribution()` 已按：

```text
value DESC
label tie-break
```

得到 Top5 + Other。

render 时：

```text
segment index 0..4
→ palette 0..4
Other → chart-other
```

### Skills

`buildSkillSeries()` 已按完整 7d 总数排序 Top10 + Other。

render：

```text
series index 0..9
→ palette 0..9
Other → chart-other
```

不得根据 model/project/skill name hash。

---

# S06-11：Skills 标题 / Surface

标题：

```text
Skills Used
```

固定：

```text
text-sm
font-medium
text-foreground
```

删除：

```text
Skills used
```

总数：

```text
NumberTicker
text-[28px]
font-semibold
leading-8
tracking-tight
text-foreground
```

Card：

```text
ChartSurface
auto height
```

---

# S06-12：Skills plot 高度与 geometry

继续保留现有核心常量：

```ts
width = 900
top = 14
left = 44
right = 12
plotHeight = 168
axisHeight = 28
```

保留：

```text
stacked area
buildMonotoneAreaPath
niceScale
```

不修改算法。

Y scale：

```text
niceScale
1/2/5 × 10^n
```

Horizontal grid：

```text
stroke = var(--border)
strokeWidth = 1
```

不增加 vertical grid。

---

# S06-13：Axis 统一 12px

Y Axis：

```text
fontSize = 12
fill = var(--muted-foreground)
```

X Axis：

原 SVG `<text fontSize="10">` 删除。

日期改由底部 HTML axis trigger row 渲染，字体：

```text
text-xs
leading-4
text-muted-foreground
```

7 个日期全部显示。

使用现有：

```text
MM-DD / 当前短日期表达
```

本 Spec 不重写 date formatter。

---

# S06-14：日期 hit detection，不再用 SVG 透明 rect

当前 7 个：

```text
<rect fill="transparent" ... />
```

删除。

原因：

- 它覆盖 plot hit region；
- 与 Area path pointer hover 冲突；
- 无法作为标准 BeUI Popover HTMLElement anchor。

### Plot 日期检测

在 `<svg>` 本体加：

```text
onPointerMove
onPointerLeave
```

PointerMove：

1. `getBoundingClientRect()`
2. 将 `clientX` 映射到 viewBox X：

```ts
const viewX =
  ((event.clientX - rect.left) / rect.width) * width;
```

3. clamp：

```text
left ... width-right
```

4. 转成最近的 day index：

```ts
const ratio = (clampedX - left) / plotWidth;
const index = Math.round(
  ratio * (data.days.length - 1)
);
```

5. `setPlotHoveredDay(index)`

PointerLeave：

```text
setPlotHoveredDay(null)
```

Area path 自己的 pointer handlers继续保留，因此：

```text
日期 focus
+
Skill series focus
```

可以同时存在。

---

# S06-15：X Axis 改为 7 个标准 Popover Trigger

在 SVG 容器底部 28px axis band 中放 HTML axis row。

要求：

- 只覆盖 axis band；
- 不覆盖 168px plot area；
- 与 viewBox `left/right` 对齐；
- 7 等分。

每个日期：

```tsx
<Popover
  trigger="hover"
  side="top"
  align="center"
  open={activeDay === index}
  onOpenChange={...}
>
  <PopoverTrigger>
    <button
      type="button"
      className="text-xs leading-4 text-muted-foreground hover:text-foreground focus-visible:text-foreground"
    >
      {shortDate}
    </button>
  </PopoverTrigger>
  <PopoverContent ...>
    ...
  </PopoverContent>
</Popover>
```

### Popover 参数

固定使用官方默认：

```text
sideOffset = 默认 14
panelRadius = 默认 16
gooStrength = 默认 8
```

不得覆盖。

### State

维护：

```text
plotHoveredDay: number | null
popoverDay: number | null
```

有效日期：

```ts
activeDay = plotHoveredDay ?? popoverDay;
```

Plot hover 时：

- Popover 也可被 controlled open；
- 锚点是对应日期 button。

日期 button keyboard focus / hover：

- 由官方 `trigger="hover"` 打开；
- `onOpenChange` 同步 `popoverDay`。

---

# S06-16：Guide line

当：

```text
activeDay !== null
```

SVG 中显示竖线：

```text
x = 对应 day point
y1 = top
y2 = top + plotHeight
stroke = var(--foreground)
strokeOpacity = 0.4
strokeWidth = 1
```

不加动画，不加 glow。

---

# S06-17：标准 Gooey Popover 内容

必须使用 Gate S03 已通过的官方：

```text
@beui/popover
```

禁止当前：

```text
motion.div
rounded-xl
border
bg-popover
shadow-xl
manual left/top/translate
```

这些全部删除。

### PopoverContent 允许的业务 class

只允许控制业务内容宽度：

```text
w-max
max-w-[min(320px,80vw)]
```

不要覆盖：

```text
background
border
radius
shadow
goo
spring
portal
```

### 内容层级

日期：

```text
text-xs
font-medium
text-foreground
```

Skill row：

```text
text-xs
font-normal
text-muted-foreground
```

Count：

```text
text-xs
font-normal
text-muted-foreground
tabular-nums
```

Total：

```text
text-xs
font-semibold
text-foreground
```

Swatch：

```text
10 × 10
rounded-full
```

Grid：

```text
grid-cols-[minmax(0,1fr)_auto]
gap-x-4
gap-y-1
```

separator：

```text
mt-1
border-t border-border
pt-1
```

### 数据排序

继续：

```text
当日 count > 0
count DESC
同 count → skill name ASC
```

不使用 NumberTicker。

---

# S06-18：Skills Legend

固定：

```text
flex flex-wrap
gap-x-4
gap-y-1.5
```

每项：

```text
12px
muted-foreground
```

Swatch：

```text
10×10
```

Skill name：

```text
max-w-44
truncate
```

Hover / focus：

```text
当前 series opacity 1
其它 0.22
```

Motion：

```text
SPRING_LAYOUT
```

不增加 row background。

---

# S06-19：Skills Area 配色

Area render 时使用 series 当前排名 index：

```text
0..9 → Project Folder palette
Other → chart-other
```

保留：

```text
fillOpacity = 0.72
```

若 Dark 下因 palette 变更导致实际可读性问题，留到 Final Gate 统一微调；施工员不得自行调成另一套色。

---

# S06-20：Skills 业务规则不得改

保持：

```text
rolling 7d
```

不随：

```text
今天 / 昨天 / 7d / 30d / 今年
```

切换。

继续接受：

```text
模型筛选
项目筛选
```

排名：

```text
完整 7d 累计 count DESC
Top 10
第 11 名以后 → 其他
```

`buildSkillSeries()` 当前逻辑若已满足，不改。

---

# S06-21：Loading / Rebuilding

### Skills 无旧数据

```text
ChartSurface
├ title skeleton
├ total skeleton
└ 168px plot skeleton
```

不使用 Spinner。

### rebuilding 有旧数据

继续显示旧图。

不切成 Skeleton。

### Distribution

如果现有 controller 只有 empty items：

- 保留当前 empty handling；
- 不为本 Spec 扩大 controller loading protocol。

空态：

```text
暂无数据
```

使用 12px muted。

---

# S06-22：清理旧实现

必须删除：

```text
Distribution article 自己的 rounded-2xl/border/bg-card/p-5
Skills article 自己的 rounded-2xl/border/bg-card/p-5
Skills mt-4
Donut 152×152
viewBox 120 / r44
items-center
Tabs !p-0.5
Tabs !px-2.5 !py-1 text-xs
legend 6px dot
legend text-[11px]
legend px-1.5 py-1
legend hover:bg-primary/5
value text-foreground 高一级
chartColor(id) hash
--chart-1..10 旧混合 palette
CHART_FOCUS_TRANSITION = { duration: 0.18 }
Skills used
Axis fontSize=10
自制 motion.div 日期 Popover
Popover text-[11px]
Skills legend 6px swatch
Skills legend text-[11px]
SVG 透明 date rect
```

---

# 7. 最小测试标准

# T-S06-001：统一 ChartSurface

组件 / DOM：

- 3 张图都经过 `ChartSurface`。
- `rounded-[28px]`
- `border-border`
- `bg-card`
- `p-5`
- Skills desktop `col-span-2`

浏览器：

- 三张 surface 明显一致；
- 边框存在但低对比度；
- 无强黑框；
- Light / Dark 都正确。

---

# T-S06-002：Distribution 固定规格

浏览器至少验证模型分布一张；项目分布复用组件不重复测试。

断言 / 测量：

```text
Donut wrapper = 140×140
SVG = 140×140
Legend swatch = 10×10
Legend = 12px
```

构造：

```text
1 segment
6 segments
```

两种情况下 Donut top 坐标一致，误差 <=1px。

Tabs：

- 标准 BeUI Pill；
- 无 compact override。

---

# T-S06-003：Palette

unit test 只测：

```text
index0 → mint-a
index4 → butter-a
index5 → mint-b
index9 → butter-b
Other → chart-other
```

确认 API 不接收 ID hash。

浏览器人工：

- Light 一次；
- Dark 一次；
- hue family 对应；
- Other 为 neutral。

不为每个 hex 写十几个独立测试。

---

# T-S06-004：Distribution hover/focus

选择任一 item：

- Hover legend → 对应 segment 保持 1，其余 0.22。
- Focus legend → 同样。
- Hover segment → 对应 legend 同步。
- Donut geometry / position 不改变。

源码确认：

```text
无 CHART_FOCUS_TRANSITION duration 0.18
使用 SPRING_LAYOUT
```

---

# T-S06-005：Skills 固定规格

验证：

```text
标题 = Skills Used
plotHeight constant = 168
Y tick = 12px
X date = 12px
Legend = 12px
Legend swatch = 10×10
Card height auto
```

已有 `niceScale` / `buildMonotoneAreaPath` 逻辑测试继续 PASS，不增加重复 geometry test。

---

# T-S06-006：Gooey 日期 Popover

只做一条集成验收：

1. pointer 在 plot 的某一天附近移动。
2. 对应日期 guide line 出现。
3. 标准 BeUI Gooey Popover 从对应日期 trigger 展开。
4. 内容只显示当天 count>0。
5. rows = count DESC / name ASC。
6. Total 正确。
7. 移动到 Area path 时 Skill focus-dim 仍能工作。
8. keyboard focus 日期 trigger 也能打开 Popover。

视觉：

- Popover 是官方 Gooey 形态；
- 不存在旧自制 `motion.div` bubble。

---

# 8. 必跑命令

在 `frontend/`：

```bash
npm run build
```

相关测试：

```bash
npm test -- \
  src/dashboard/charts/distribution.test.ts \
  src/dashboard/charts/skillSeries.test.ts \
  src/dashboard/charts/ChartSection.test.tsx \
  src/dashboard/charts/DistributionDonutCard.test.tsx \
  src/dashboard/charts/SkillsUsageChart.test.tsx
```

实际不存在的 UI test 文件可新增，但只建立本文 6 个验收点所需的最少 case。

不运行 Rust tests。

---

# 9. Gate S06

## Gate S06-A：ChartSurface

- 三张图统一 `ChartSurface`。
- `bg-card + border-border + 28px + p-5`。
- 无强黑边。
- 无透明 Card。
- 无额外 Card primitive。

## Gate S06-B：Distribution

- Donut 真正 140×140。
- wrapper 140×140。
- 位置不随 Legend 数量变化。
- Tabs = 官方 Pill，无尺寸魔改。
- Legend = 10px swatch + 12px muted。
- value/name/% 同级。

## Gate S06-C：Palette / Motion

- 不再 hash ID。
- Top5 / Top10 按 ranking index 映射。
- Project Folder Light / Dark tokens 使用本文固定值。
- Other 使用 neutral token。
- 自定义 Chart Motion 使用 BeUI `SPRING_LAYOUT`。
- 无 MU `duration:0.18`。

## Gate S06-D：Skills

- 标题 `Skills Used`。
- plotHeight=168。
- Axis 12px。
- Legend 12px / 10px swatch。
- niceScale / monotone / rolling7d / Top10+Other 不回归。

## Gate S06-E：Gooey Popover

- 使用 Gate S03 官方 `@beui/popover`。
- 无自制 `motion.div` panel。
- 日期 trigger 是真实 HTMLElement。
- Plot 日期检测不覆盖 Area path。
- Popover 文字层级按本文。
- keyboard 可访问。

## Gate S06-F：范围

确认：

```text
src/** 无改动
SKILL_USAGE_PARSER_VERSION 无改动
skill_usage_events 无 rebuild
```

Skill 数量偏少不得在此 Spec“顺手修”。

## Gate S06-G：工程

```text
npm run build = PASS
Spec06 targeted tests = PASS
```

全部通过才允许进入 Spec07。

---

# 10. 施工员禁止事项

1. 禁止把 `ChartSurface` 宣称成 BeUI Card。
2. 禁止再新增第二个 Chart Card primitive。
3. 禁止删除 ChartSurface 的 subtle border。
4. 禁止用 `border-strong` / 黑色实线做 Card 外边框。
5. 禁止 Donut 保持 152px 空壳。
6. 禁止通过 `items-center` 让 Donut 随 Legend 高度位移。
7. 禁止给 Tabs 写 compact class 或新增 `size="sm"`。
8. 禁止 legend item 增加 hover Card 背景。
9. 禁止 name/value/% 使用不同静态颜色层级。
10. 禁止 hash model/project/skill ID 决定颜色。
11. 禁止自行换 Project Folder palette。
12. 禁止在 chartMotion.ts 新造 duration / easing。
13. 禁止修改 BeUI Tabs / NumberTicker / Popover 官方内部 Motion。
14. 禁止用自制 `motion.div` 替代 Gooey Popover。
15. 禁止把透明 SVG rect 继续作为日期 Popover trigger。
16. 禁止为了日期 hover 覆盖 Area path pointer events。
17. 禁止修改 rolling7d、Top10+Other、niceScale、monotone 业务逻辑。
18. 禁止在 Spec06 修改 Skill parser / parser version / Rust。
19. 禁止因当前 Skill 数量少而在前端伪造/补数据。
20. 禁止过度增加视觉 snapshot tests。
21. 禁止以“看起来像 BeUI”作为通过依据。
